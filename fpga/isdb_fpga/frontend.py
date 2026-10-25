"""Single-carrier front end (IQ -> Symbols), format version 2.

The FPGA implementation of `isdb_frontend::fixed::Frontend` (Rust,
crates/isdb-frontend/src/fixed.rs). The model's module documentation is
the specification; this file implements it and must reproduce it bit for
bit: every step uses the model's widths, rounding (add half, arithmetic
shift) and saturation.

Timing: one input sample per `i_valid` strobe, at least CYCLES (4) clock
cycles apart (the AD9361 ADC clock runs at 4x the sample rate on the
PlutoSky R2, LVDS 2R2T). The input sample enters at cycle A; the mixer
output is ready at D0 = A + 2, the cycle that shifts it into the delay
line, decides whether a symbol is due (model step 3), commits the loop
update and advances t_acc. For a due symbol ("Dk" is k cycles after D0;
a value "at Dk" is held in a register during Dk):

  D1..D4   coefficient read and tap selection, cycle j: tap j*L + l (lane l)
  D2..D5   products (h, g and the output bank k);  D3..D6 lane sums;
  D4..D7 accumulation
  D9  y, dy, yk   D10 *G   D11 z, dz, zk   D12 q (from zk), TED (z, dz)
  and power (zk) products
  D13 e, err (the symbol's internals, `s_*`)   D14 kp*e, ki*e, AGC step
  D15 rounded loop terms;  D16 queued for the commit of symbol k + 6

Symbols are at least one sample (4 cycles) apart, so successive symbols
occupy the stages without overlap; each carries its context (phase, gain,
period) down the pipeline. The commit of symbol k's errors happens at the
decision of symbol k + LOOP_DELAY (6), at least 24 cycles after its D0: the
queue entry (D16) and the 3-stage shadow pipeline that precomputes the
next loop state (ready 3 cycles after any commit) both fit.
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed, unsigned
from amaranth.lib.memory import Memory

# Constants of format version 3 (crates/isdb-frontend/src/params.rs).
FORMAT_VERSION = 3
PHASES = 64
PHASE_BITS = 6
FRAC_BITS = 24
ONE = 1 << FRAC_BITS
NCO_LUT_BITS = 10
COEF_BITS = 18
FILTER_SHIFT = 14
GAIN_FRAC_BITS = 10
GAIN_ACC_EXTRA_BITS = 8
GAIN_ACC_BITS = 24
POWER_SHIFT = 12
KI_FRAC = 16
LOOP_DELAY = 6
E_MAX = (1 << 23) - 1
HEADER_WORDS = 9
SYNC = (0x1ACF, 0xFC1D)
CYCLES = 4  # clock cycles per input sample (MAC time multiplexing)
PIPE = 17  # stage strobes D1..D16


def sat(v, lo, hi):
    """Clamp `v` to [lo, hi]."""
    return Mux(v > hi, hi, Mux(v < lo, lo, v))


def round_shift_const(v, s):
    """round(v / 2**s) for a constant s: add half, arithmetic shift."""
    if s == 0:
        return v
    return (v + (1 << (s - 1))) >> s


def round_shift_var(v, s):
    """round(v / 2**s) for an unsigned signal s: ((v >> (s-1)) + 1) >> 1
    when s >= 1, which equals (v + 2**(s-1)) >> s."""
    s1 = (s - 1)[: len(s)]
    return Mux(s == 0, v, ((v >> s1) + 1) >> 1)


# CRC-32C (Castagnoli) polynomial 0x1EDC6F41, bit-reversed for the
# reflected shift register of CRC-32/ISCSI.
CRC32C_POLY_REFLECTED = 0x82F63B78


def crc32c_word_terms():
    """The parallel next-state equations of the reflected CRC-32C register
    for one 16-bit word sent as its two little-endian bytes (bit 0 of the
    word first, as `isdb_frontend::format::crc32c` feeds it), derived by
    symbolic simulation of the bit-serial register: entry i is the set of
    ("c", j) / ("w", j) inputs XORed into bit i."""
    state = [{("c", i)} for i in range(32)]
    for bit in range(16):
        fb = state[0] ^ {("w", bit)}
        new = [set(state[i + 1]) for i in range(31)] + [set()]
        for i in range(32):
            if CRC32C_POLY_REFLECTED >> i & 1:
                new[i] = new[i] ^ fb
        state = new
    return state


def crc32c_word(crc, word):
    """One CRC-32C step of the 32-bit register `crc` over the 16-bit
    `word` (no initial value or final XOR applied): a one-level XOR
    network per output bit (`crc32c_word_terms`)."""
    outs = []
    for terms in crc32c_word_terms():
        acc = Const(0, 1)
        for kind, i in sorted(terms):
            acc = acc ^ (crc[i] if kind == "c" else word[i])
        outs.append(acc)
    return Cat(*outs)


class Registers:
    """The run-time parameters (`isdb_frontend::params::Params`) as
    signals; the coefficient banks are written through the core's load
    port."""

    def __init__(self):
        self.nco_step = Signal(32)
        self.period_nominal = Signal(32)
        self.period_min = Signal(32)
        self.period_max = Signal(32)
        self.ted_shift = Signal(5)
        self.kp = Signal(signed(16))
        self.kp_shift = Signal(6)
        self.ki = Signal(signed(16))
        self.ki_shift = Signal(6)
        self.integrator_max = Signal(22)
        self.agc_init = Signal(GAIN_ACC_BITS)
        self.agc_target = Signal(32)
        self.agc_shift = Signal(5)
        self.bits = Signal(4)
        self.q_shift = Signal(4)
        self.block_log2 = Signal(4)

    def fields(self):
        return {k: v for k, v in vars(self).items() if isinstance(v, Signal)}


class SymbolFrontend(Elaboratable):
    """IQ (12-bit components in 16) in, `Symbols` words out.

    Ports:
      start                  one-cycle pulse: reset the state to its initial
                             values (set the registers first);
      i_valid, i_i, i_q      input sample;
      o_valid, o_word        output words (at most one per cycle);
      c_we_h, c_we_g, c_we_k, c_phase, c_tap, c_h, c_g, c_k
                             coefficient load (h, g and/or k of one tap per
                             cycle);
      s_valid, s_*           the internals of each symbol (verification).
    """

    def __init__(self, taps, cos_table, h=None, g=None, k=None):
        assert taps % CYCLES == 0 and CYCLES <= taps <= 64
        self.taps = taps
        self.lanes = taps // CYCLES
        self.cos_table = list(cos_table)
        self.h = h
        self.g = g
        # Without an output bank the core starts with k = h (format 1's
        # output); the host loads k with the trim.
        self.k = k if k is not None else h
        self.regs = Registers()
        self.start = Signal()
        self.i_valid = Signal()
        self.i_i = Signal(signed(16))
        self.i_q = Signal(signed(16))
        self.o_valid = Signal()
        self.o_word = Signal(16)
        self.c_we_h = Signal()
        self.c_we_g = Signal()
        self.c_we_k = Signal()
        self.c_phase = Signal(PHASE_BITS)
        self.c_tap = Signal(6)
        self.c_h = Signal(signed(COEF_BITS))
        self.c_g = Signal(signed(COEF_BITS))
        self.c_k = Signal(signed(COEF_BITS))
        self.s_valid = Signal()
        self.s_phase = Signal(PHASE_BITS)
        self.s_y = [Signal(signed(16), name=f"s_y{i}") for i in range(2)]
        self.s_dy = [Signal(signed(16), name=f"s_dy{i}") for i in range(2)]
        self.s_yk = [Signal(signed(16), name=f"s_yk{i}") for i in range(2)]
        self.s_gain = Signal(16)
        self.s_period = Signal(32)
        self.s_z = [Signal(signed(16), name=f"s_z{i}") for i in range(2)]
        self.s_dz = [Signal(signed(16), name=f"s_dz{i}") for i in range(2)]
        self.s_zk = [Signal(signed(16), name=f"s_zk{i}") for i in range(2)]
        self.s_q = [Signal(signed(8), name=f"s_q{i}") for i in range(2)]
        self.s_e = Signal(signed(24))
        self.s_err = Signal(signed(33))

    def elaborate(self, platform):
        m = Module()
        r = self.regs
        L = self.lanes

        def delayed(sig, n, name):
            """`sig` delayed by n registers."""
            prev = sig
            for i in range(n):
                nxt = Signal.like(prev, name=f"{name}_{i}")
                m.d.sync += nxt.eq(prev)
                prev = nxt
            return prev

        # ---- mixer (model step 1): A = i_valid, A+1 products, D0 = A+2 --
        cos = Memory(shape=signed(16), depth=1 << NCO_LUT_BITS, init=self.cos_table)
        m.submodules.cos = cos
        rp_c = cos.read_port()
        rp_s = cos.read_port()
        nco = Signal(32)
        idx = nco[32 - NCO_LUT_BITS:]
        m.d.comb += [rp_c.addr.eq(idx), rp_s.addr.eq((idx + 768)[:NCO_LUT_BITS])]
        x1 = [Signal(signed(16), name=f"x1_{i}") for i in range(2)]
        v1 = Signal()
        m.d.sync += v1.eq(self.i_valid)
        with m.If(self.i_valid):
            m.d.sync += [x1[0].eq(self.i_i), x1[1].eq(self.i_q), nco.eq(nco + r.nco_step)]
        mp = [Signal(signed(32), name=f"mp{i}") for i in range(4)]
        d0 = Signal()
        m.d.sync += [
            d0.eq(v1),
            mp[0].eq(x1[0] * rp_c.data),
            mp[1].eq(x1[1] * rp_s.data),
            mp[2].eq(x1[0] * rp_s.data),
            mp[3].eq(x1[1] * rp_c.data),
        ]
        mix_i = round_shift_const(mp[0] - mp[1], 15)
        mix_q = round_shift_const(mp[2] + mp[3], 15)

        # ---- loop state -----------------------------------------------
        line_i = Array(Signal(signed(14), name=f"li{t}") for t in range(self.taps))
        line_q = Array(Signal(signed(14), name=f"lq{t}") for t in range(self.taps))
        t_acc = Signal(signed(32))
        v = Signal(signed(40))
        period = Signal(32)
        gain_acc = Signal(GAIN_ACC_BITS)
        nsym = Signal(range(LOOP_DELAY + 1))

        # ---- error queue (model step 3.6) and shadow commit (3.2) -----
        qdepth = 8
        q_ki = Array(Signal(signed(40), name=f"q_ki{i}") for i in range(qdepth))
        q_kp = Array(Signal(signed(40), name=f"q_kp{i}") for i in range(qdepth))
        q_ag = Array(Signal(signed(33), name=f"q_ag{i}") for i in range(qdepth))
        q_rd = Signal(3)
        q_wr = Signal(3)
        vmax = Signal(signed(40))
        m.d.comb += vmax.eq(r.integrator_max << KI_FRAC)
        sh_vsum = Signal(signed(41))
        sh_gsum = Signal(signed(34))
        sh_kp1 = Signal(signed(40))
        m.d.sync += [
            sh_vsum.eq(v + q_ki[q_rd]),
            sh_gsum.eq(gain_acc + q_ag[q_rd]),
            sh_kp1.eq(q_kp[q_rd]),
        ]
        sh_v2 = Signal(signed(40))
        sh_g2 = Signal(GAIN_ACC_BITS)
        sh_kp2 = Signal(signed(40))
        m.d.sync += [
            sh_v2.eq(sat(sh_vsum, -vmax, vmax)),
            sh_g2.eq(sat(sh_gsum, 0, (1 << GAIN_ACC_BITS) - 1)),
            sh_kp2.eq(sh_kp1),
        ]
        sh_v = Signal(signed(40))
        sh_g = Signal(GAIN_ACC_BITS)
        sh_p = Signal(32)
        psum = Signal(signed(42))
        m.d.comb += psum.eq(r.period_nominal + (sh_v2 >> KI_FRAC) + sh_kp2)
        m.d.sync += [sh_v.eq(sh_v2), sh_g.eq(sh_g2), sh_p.eq(sat(psum, r.period_min, r.period_max))]

        # ---- decision at D0 (model steps 2, 3, 4) ----------------------
        due = Signal()
        m.d.comb += due.eq(d0 & (t_acc < ONE))
        commit = Signal()
        m.d.comb += commit.eq(due & (nsym == LOOP_DELAY))
        period_eff = Signal(32)
        gain_eff = Signal(GAIN_ACC_BITS)
        m.d.comb += [
            period_eff.eq(Mux(commit, sh_p, period)),
            gain_eff.eq(Mux(commit, sh_g, gain_acc)),
        ]
        # symbol context, held from D1 until the next decision
        k_phase = Signal(PHASE_BITS)
        k_gain = Signal(16)
        k_period = Signal(32)
        with m.If(d0):
            for t in range(self.taps - 1, 0, -1):
                m.d.sync += [line_i[t].eq(line_i[t - 1]), line_q[t].eq(line_q[t - 1])]
            m.d.sync += [line_i[0].eq(mix_i), line_q[0].eq(mix_q)]
            with m.If(due):
                m.d.sync += [
                    t_acc.eq(t_acc + period_eff - ONE),
                    k_phase.eq(t_acc[FRAC_BITS - PHASE_BITS:FRAC_BITS]),
                    k_gain.eq(gain_eff >> GAIN_ACC_EXTRA_BITS),
                    k_period.eq(period_eff),
                ]
                with m.If(commit):
                    m.d.sync += [v.eq(sh_v), period.eq(sh_p), gain_acc.eq(sh_g), q_rd.eq(q_rd + 1)]
                with m.Else():
                    m.d.sync += nsym.eq(nsym + 1)
            with m.Else():
                m.d.sync += t_acc.eq(t_acc - ONE)

        # stage strobes: stage[k] is high during Dk+1 (k = 0..PIPE-1)
        stage = Signal(PIPE)
        m.d.sync += stage.eq(Cat(due, stage[:-1]))

        def at(k):
            """High during Dk of a due symbol."""
            return stage[k - 1]

        # ---- MAC (model step 3.3) --------------------------------------
        j = Signal(2)
        with m.If(due):
            m.d.sync += j.eq(0)
        with m.Else():
            m.d.sync += j.eq(j + 1)
        tap_i = [Signal(signed(14), name=f"tap_i{l}") for l in range(L)]
        tap_q = [Signal(signed(14), name=f"tap_q{l}") for l in range(L)]
        prods = []
        for lane in range(L):
            init = []
            for ph in range(PHASES):
                for jj in range(CYCLES):
                    t = jj * L + lane
                    hv = self.h[ph][t] if self.h else 0
                    gv = self.g[ph][t] if self.g else 0
                    kv = self.k[ph][t] if self.k else 0
                    init.append(
                        (hv & 0x3FFFF) | ((gv & 0x3FFFF) << 18) | ((kv & 0x3FFFF) << 36)
                    )
            mem = Memory(shape=unsigned(54), depth=PHASES * CYCLES, init=init)
            m.submodules[f"coef{lane}"] = mem
            rp = mem.read_port()
            m.d.comb += rp.addr.eq(Cat(j, k_phase))
            sel_i = Array(line_i[jj * L + lane] for jj in range(CYCLES))
            sel_q = Array(line_q[jj * L + lane] for jj in range(CYCLES))
            m.d.sync += [tap_i[lane].eq(sel_i[j]), tap_q[lane].eq(sel_q[j])]
            wp = mem.write_port(granularity=18)
            hit = ((self.c_tap % L) == lane) & (self.c_tap < self.taps)
            m.d.comb += [
                wp.en.eq(Cat(self.c_we_h & hit, self.c_we_g & hit, self.c_we_k & hit)),
                wp.addr.eq(Cat((self.c_tap // L)[:2], self.c_phase)),
                wp.data.eq(Cat(self.c_h, self.c_g, self.c_k)),
            ]
            hc = rp.data[:18].as_signed()
            gc = rp.data[18:36].as_signed()
            kc = rp.data[36:].as_signed()
            pr = [Signal(signed(32), name=f"pr{lane}_{k}") for k in range(6)]
            m.d.sync += [
                pr[0].eq(hc * tap_i[lane]),
                pr[1].eq(hc * tap_q[lane]),
                pr[2].eq(gc * tap_i[lane]),
                pr[3].eq(gc * tap_q[lane]),
                pr[4].eq(kc * tap_i[lane]),
                pr[5].eq(kc * tap_q[lane]),
            ]
            prods.append(pr)
        part = [Signal(signed(40), name=f"part{k}") for k in range(6)]
        for k in range(6):
            total = prods[0][k]
            for lane in range(1, L):
                total = total + prods[lane][k]
            m.d.sync += part[k].eq(total)
        acc = [Signal(signed(44), name=f"acc{k}") for k in range(6)]
        with m.If(at(4)):
            m.d.sync += [acc[k].eq(part[k]) for k in range(6)]
        with m.Elif(at(5) | at(6) | at(7)):
            m.d.sync += [acc[k].eq(acc[k] + part[k]) for k in range(6)]

        # Context down the pipeline: k_* hold a symbol's values from D1 to
        # its successor's D0 (>= D4); a free-running delay line of n
        # registers shows during D(n+1) what k_* held during D1.
        gain_d9 = delayed(k_gain, 8, "g9")
        phase_d13 = delayed(k_phase, 12, "ph13")
        gain_d13 = delayed(k_gain, 12, "g13")
        period_d13 = delayed(k_period, 12, "p13")

        # ---- D9: y, dy ---------------------------------------------------
        y = [Signal(signed(16), name=f"y{i}") for i in range(2)]
        dy = [Signal(signed(16), name=f"dy{i}") for i in range(2)]
        yk = [Signal(signed(16), name=f"yk{i}") for i in range(2)]
        with m.If(at(8)):
            for i in range(2):
                m.d.sync += [
                    y[i].eq(sat(round_shift_const(acc[i], FILTER_SHIFT), -32768, 32767)),
                    dy[i].eq(sat(round_shift_const(acc[2 + i], FILTER_SHIFT), -32768, 32767)),
                    yk[i].eq(sat(round_shift_const(acc[4 + i], FILTER_SHIFT), -32768, 32767)),
                ]
        # ---- D10: gain products -------------------------------------------
        gp = [Signal(signed(34), name=f"gp{i}") for i in range(6)]
        gain_s = Cat(gain_d9, Const(0, 1)).as_signed()
        with m.If(at(9)):
            m.d.sync += [
                gp[0].eq(y[0] * gain_s), gp[1].eq(y[1] * gain_s),
                gp[2].eq(dy[0] * gain_s), gp[3].eq(dy[1] * gain_s),
                gp[4].eq(yk[0] * gain_s), gp[5].eq(yk[1] * gain_s),
            ]
        # ---- D11: z, dz, zk -------------------------------------------------
        z = [Signal(signed(16), name=f"z{i}") for i in range(2)]
        dz = [Signal(signed(16), name=f"dz{i}") for i in range(2)]
        zk = [Signal(signed(16), name=f"zk{i}") for i in range(2)]
        with m.If(at(10)):
            for i in range(2):
                m.d.sync += [
                    z[i].eq(sat(round_shift_const(gp[i], GAIN_FRAC_BITS), -32768, 32767)),
                    dz[i].eq(sat(round_shift_const(gp[2 + i], GAIN_FRAC_BITS), -32768, 32767)),
                    zk[i].eq(sat(round_shift_const(gp[4 + i], GAIN_FRAC_BITS), -32768, 32767)),
                ]
        # ---- D12: quantiser (zk), TED (z, dz) and power (zk) products ---
        lim = Array(Const(1 << max(b - 1, 0), signed(10)) for b in range(16))[r.bits]
        qv = [Signal(signed(8), name=f"q{i}") for i in range(2)]
        tp = [Signal(signed(32), name=f"tp{i}") for i in range(2)]
        pw = [Signal(signed(32), name=f"pw{i}") for i in range(2)]
        with m.If(at(11)):
            for i in range(2):
                m.d.sync += [
                    qv[i].eq(sat(round_shift_var(zk[i], r.q_shift), -lim, lim - 1)),
                    tp[i].eq(z[i] * dz[i]),
                    pw[i].eq(zk[i] * zk[i]),
                ]
        # ---- D13: e, err ---------------------------------------------------
        e = Signal(signed(24))
        err = Signal(signed(33))
        ted = Signal(signed(33))
        powv = Signal(signed(34))
        m.d.comb += [ted.eq(tp[0] + tp[1]), powv.eq((pw[0] + pw[1]) >> POWER_SHIFT)]
        with m.If(at(12)):
            m.d.sync += [e.eq(sat(ted >> r.ted_shift, -E_MAX, E_MAX)), err.eq(r.agc_target - powv)]
        # ---- D14, D15: loop terms; queued at the end of D15 ----------------
        kpe = Signal(signed(40))
        kie = Signal(signed(40))
        ags = Signal(signed(33))
        with m.If(at(13)):
            m.d.sync += [kpe.eq(r.kp * e), kie.eq(r.ki * e), ags.eq(err >> r.agc_shift)]
        kpe2 = Signal(signed(40))
        kie2 = Signal(signed(40))
        ags2 = Signal(signed(33))
        with m.If(at(14)):
            m.d.sync += [
                kpe2.eq(round_shift_var(kpe, r.kp_shift)),
                kie2.eq(round_shift_var(kie, r.ki_shift)),
                ags2.eq(ags),
            ]
        with m.If(at(15)):
            m.d.sync += [q_ki[q_wr].eq(kie2), q_kp[q_wr].eq(kpe2), q_ag[q_wr].eq(ags2), q_wr.eq(q_wr + 1)]

        # ---- per-symbol internals, valid at D13 ---------------------------
        m.d.comb += [
            self.s_valid.eq(at(13)),
            self.s_phase.eq(phase_d13),
            self.s_gain.eq(gain_d13),
            self.s_period.eq(period_d13),
            self.s_e.eq(e),
            self.s_err.eq(err),
        ]
        # y (D9..D12) and z (D11..D14) are replaced by the next symbol's
        # values as early as D12 and D14: read them through delay lines.
        for i in range(2):
            m.d.comb += [
                self.s_y[i].eq(delayed(y[i], 4, f"sy{i}")),
                self.s_dy[i].eq(delayed(dy[i], 4, f"sdy{i}")),
                self.s_yk[i].eq(delayed(yk[i], 4, f"syk{i}")),
                self.s_z[i].eq(delayed(z[i], 2, f"sz{i}")),
                self.s_dz[i].eq(delayed(dz[i], 2, f"sdz{i}")),
                self.s_zk[i].eq(delayed(zk[i], 2, f"szk{i}")),
                self.s_q[i].eq(delayed(qv[i], 1, f"sq{i}")),
            ]

        # ---- packer (isdb_frontend::format::Packer), fed at D13 ------------
        pq_depth = 8
        pq_sym = Array(Signal(16, name=f"pq_sym{i}") for i in range(pq_depth))
        pq_gain = Array(Signal(16, name=f"pq_gain{i}") for i in range(pq_depth))
        pq_per = Array(Signal(32, name=f"pq_per{i}") for i in range(pq_depth))
        pq_rd = Signal(3)
        pq_wr = Signal(3)
        mask = Array(Const((1 << b) - 1, 8) for b in range(16))[r.bits]
        sym_word = Signal(16)
        m.d.comb += sym_word.eq(
            (self.s_q[0].as_unsigned() & mask) | ((self.s_q[1].as_unsigned() & mask) << r.bits)
        )
        # qv holds the symbol during D12..D15: delayed by one to D13
        with m.If(at(13)):
            m.d.sync += [
                pq_sym[pq_wr].eq(sym_word),
                pq_gain[pq_wr].eq(gain_d13),
                pq_per[pq_wr].eq(period_d13),
                pq_wr.eq(pq_wr + 1),
            ]
        bacc = Signal(32)
        nacc = Signal(5)
        in_block = Signal(16)
        seq = Signal(32)
        crc = Signal(32)
        crc_out = Signal(32)
        m.d.comb += crc_out.eq(~crc)
        tr_idx = Signal(1)
        hdr_idx = Signal(4)
        hdr_gain = Signal(16)
        hdr_per = Signal(32)
        fmt = Signal(16)
        m.d.comb += fmt.eq(FORMAT_VERSION | ((r.bits - 1)[:4] << 4) | (r.block_log2 << 8))
        check = Signal(16)
        hsum = fmt + seq[:16] + seq[16:] + hdr_gain + hdr_per[:16] + hdr_per[16:]
        m.d.comb += check.eq((0xFFFF - hsum)[:16])
        hw = Array([
            Const(SYNC[0], 16), Const(SYNC[1], 16), fmt, seq[:16], seq[16:],
            hdr_gain, hdr_per[:16], hdr_per[16:], check,
        ])
        block_len = Array(Const(1 << b, 17) for b in range(16))[r.block_log2]
        newacc = Signal(32)
        m.d.comb += newacc.eq(bacc | (pq_sym[pq_rd] << nacc))
        nbits = Signal(6)
        m.d.comb += nbits.eq(nacc + (r.bits << 1))
        m.d.sync += self.o_valid.eq(0)
        with m.FSM(name="packer"):
            with m.State("IDLE"):
                with m.If(pq_rd != pq_wr):
                    with m.If(in_block == 0):
                        m.d.sync += [hdr_idx.eq(0), hdr_gain.eq(pq_gain[pq_rd]), hdr_per.eq(pq_per[pq_rd])]
                        m.next = "HEADER"
                    with m.Else():
                        m.next = "SYMBOL"
            with m.State("HEADER"):
                m.d.sync += [self.o_valid.eq(1), self.o_word.eq(hw[hdr_idx]), hdr_idx.eq(hdr_idx + 1)]
                # the trailer's CRC-32C starts from ~0 over the sequence
                # number's two words (low first), during the header
                with m.If(hdr_idx == 0):
                    m.d.sync += crc.eq(crc32c_word(Const(0xFFFFFFFF, 32), seq[:16]))
                with m.Elif(hdr_idx == 1):
                    m.d.sync += crc.eq(crc32c_word(crc, seq[16:]))
                with m.If(hdr_idx == HEADER_WORDS - 1):
                    m.next = "SYMBOL"
            with m.State("SYMBOL"):
                m.d.sync += [pq_rd.eq(pq_rd + 1), in_block.eq(in_block + 1)]
                with m.If(nbits >= 16):
                    m.d.sync += [
                        self.o_valid.eq(1),
                        self.o_word.eq(newacc[:16]),
                        crc.eq(crc32c_word(crc, newacc[:16])),
                        bacc.eq(newacc >> 16),
                        nacc.eq(nbits - 16),
                    ]
                with m.Else():
                    m.d.sync += [bacc.eq(newacc), nacc.eq(nbits)]
                with m.If(in_block + 1 == block_len):
                    m.next = "TRAILER"
                with m.Else():
                    m.next = "IDLE"
            with m.State("TRAILER"):
                # two words, low first: the CRC with the final XOR
                m.d.sync += [self.o_valid.eq(1), tr_idx.eq(tr_idx + 1)]
                with m.If(tr_idx == 0):
                    m.d.sync += self.o_word.eq(crc_out[:16])
                with m.Else():
                    m.d.sync += [self.o_word.eq(crc_out[16:]), seq.eq(seq + 1), in_block.eq(0)]
                    m.next = "IDLE"

        # ---- start: initial state (Frontend::new) ------------------------
        with m.If(self.start):
            m.d.sync += [
                nco.eq(0), t_acc.eq(0), v.eq(0), period.eq(r.period_nominal),
                gain_acc.eq(r.agc_init), nsym.eq(0), q_rd.eq(0), q_wr.eq(0),
                pq_rd.eq(0), pq_wr.eq(0), bacc.eq(0), nacc.eq(0), in_block.eq(0),
                seq.eq(0), crc.eq(0), tr_idx.eq(0), stage.eq(0),
            ]
            m.d.sync += [line_i[t].eq(0) for t in range(self.taps)]
            m.d.sync += [line_q[t].eq(0) for t in range(self.taps)]
        return m
