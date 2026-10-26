"""Single-carrier front end (IQ -> Symbols), format version 4.

The FPGA implementation of `isdb_frontend::fixed::Frontend` (Rust,
crates/isdb-frontend/src/fixed.rs). The model's module documentation is
the specification; this file implements it and must reproduce it bit for
bit: every step uses the model's widths, rounding (add half, arithmetic
shift) and saturation.

Timing: one input sample per `i_valid` strobe, as often as every clock
cycle. On the PlutoSky R2 the block runs on `util_ad9361_divclk` = l_clk /
4 = the sample rate (40 MHz at 40 Msps; fpga/README.md "Clock"), and
`util_wfifo` delivers samples in bursts of eight on consecutive cycles, so
the core takes one sample per clock and at most one symbol per sample. The
input sample enters at cycle A; the mixer output is ready at D0 = A + 2,
the cycle that shifts it into the delay line, decides whether a symbol is
due (model step 3), applies the loop update (3.2) and advances t_acc. For a
due symbol ("Dk" is k cycles after D0; a value "at Dk" is in a register
during Dk), fully pipelined (a new symbol may start every cycle; each
stage carries its own copy of the symbol's context):

  D1  delay-line snapshot and coefficient read (one ROM per tap, by
      phase), both ready at D2
  D2  products: output bank k on every tap, h and g on the centre
      TED_TAPS taps (the model's detector banks are zero elsewhere)
  D3..D7  adder trees (k: 32 -> 1 in five levels; h, g: 8 -> 1 in three,
          delayed to match); the sums are ready at D8
  D8  y, dy, yk   D9 *G   D10 z, dz, zk   D11 q (from zk), TED (z, dz) and
  power (zk) products   D12 e, err   D13 kp*e, ki*e, AGC step   D14
  rounded loop terms, queued at its end for the update at the decision of
  symbol k + LOOP_DELAY (24). ("At Dk" here: computed during Dk, in a
  register from Dk+1; the symbol's internals `s_*` are all valid at D13.)

Symbol k + 24's decision is at least 24 cycles after symbol k's (at most
one symbol per sample, at most one sample per cycle), so the queue entry
written at the end of D14 is there in time. The loop update itself (model
step 3.2) is combinational at D0: v + ki*e, its saturation, the period and
the AGC sum, one add-and-clamp chain each (within one 62.5 MHz cycle).
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed, unsigned
from amaranth.lib.memory import Memory

# Constants of format version 4 (crates/isdb-frontend/src/params.rs).
FORMAT_VERSION = 4
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
LOOP_DELAY = 24
TED_TAPS = 8
E_MAX = (1 << 23) - 1
HEADER_WORDS = 9
SYNC = (0x1ACF, 0xFC1D)
TREE = 5  # adder-tree levels of the output bank (32 taps); 2**TREE >= taps
LAST = 15  # the loop terms are in their registers during this stage
# Vivado synthesis attribute: map an adder to fabric, not a DSP48E1.
NO_DSP = {"use_dsp": "no"}


def ted_range(taps):
    """The centre TED_TAPS taps (`isdb_frontend::params::ted_tap`)."""
    lo = max(taps - TED_TAPS, 0) // 2
    return range(lo, lo + min(TED_TAPS, taps))


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
      i_valid, i_i, i_q      input sample (at most one per cycle);
      o_valid, o_word        output words (at most one per cycle);
      c_we_h, c_we_g, c_we_k, c_phase, c_tap, c_h, c_g, c_k
                             coefficient load (h, g and/or k of one tap per
                             cycle; h and g only on the centre TED_TAPS
                             taps, writes elsewhere are ignored, as the
                             model requires them zero there);
      s_valid, s_*           the internals of each symbol (verification).
    """

    def __init__(self, taps, cos_table, h=None, g=None, k=None):
        assert taps % 2 == 0 and 4 <= taps <= 1 << TREE
        self.taps = taps
        self.ted = ted_range(taps)
        self.cos_table = list(cos_table)
        self.h = h
        self.g = g
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
        taps = self.taps

        def pipe(sig, n, name):
            """`sig` delayed by n registers (a free-running pipeline)."""
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
        line_i = [Signal(signed(14), name=f"li{t}") for t in range(taps)]
        line_q = [Signal(signed(14), name=f"lq{t}") for t in range(taps)]
        t_acc = Signal(signed(32), attrs=NO_DSP)
        v = Signal(signed(40), attrs=NO_DSP)
        period = Signal(32)
        gain_acc = Signal(GAIN_ACC_BITS)
        nsym = Signal(range(LOOP_DELAY + 1))

        # ---- error queue (model step 3.6) and the update (3.2) ---------
        # one entry per symbol in flight between its D14 and the decision
        # of symbol k + 24 (at most 24 + 15 entries): a distributed RAM of
        # 64 entries, read asynchronously by the update at D0
        qbits = 6
        qmem = Memory(shape=unsigned(40 + 40 + 33), depth=1 << qbits, init=[])
        m.submodules.errq = qmem
        q_rp = qmem.read_port(domain="comb")
        q_wp = qmem.write_port()
        q_rd = Signal(qbits)
        q_wr = Signal(qbits)
        m.d.comb += q_rp.addr.eq(q_rd)
        q_ki_h = q_rp.data[:40].as_signed()
        q_kp_h = q_rp.data[40:80].as_signed()
        q_ag_h = q_rp.data[80:].as_signed()
        vmax = Signal(signed(40))
        m.d.comb += vmax.eq(r.integrator_max << KI_FRAC)
        new_v = Signal(signed(40))
        new_p = Signal(32)
        new_g = Signal(GAIN_ACC_BITS)
        vsum = Signal(signed(41), attrs=NO_DSP)
        gsum = Signal(signed(34), attrs=NO_DSP)
        psum = Signal(signed(42), attrs=NO_DSP)
        m.d.comb += [
            vsum.eq(v + q_ki_h),
            new_v.eq(sat(vsum, -vmax, vmax)),
            psum.eq(r.period_nominal + (new_v >> KI_FRAC) + q_kp_h),
            new_p.eq(sat(psum, r.period_min, r.period_max)),
            gsum.eq(gain_acc + q_ag_h),
            new_g.eq(sat(gsum, 0, (1 << GAIN_ACC_BITS) - 1)),
        ]

        # ---- decision at D0 (model steps 2, 3, 4) ----------------------
        due = Signal()
        m.d.comb += due.eq(d0 & (t_acc < ONE))
        commit = Signal()
        m.d.comb += commit.eq(due & (nsym == LOOP_DELAY))
        period_eff = Signal(32)
        gain_eff = Signal(GAIN_ACC_BITS)
        m.d.comb += [
            period_eff.eq(Mux(commit, new_p, period)),
            gain_eff.eq(Mux(commit, new_g, gain_acc)),
        ]
        # symbol context at D1 (one register stage after the decision)
        c_phase = Signal(PHASE_BITS)
        c_gain = Signal(16)
        c_period = Signal(32)
        v_d1 = Signal()
        m.d.sync += v_d1.eq(due)
        with m.If(d0):
            for t in range(taps - 1, 0, -1):
                m.d.sync += [line_i[t].eq(line_i[t - 1]), line_q[t].eq(line_q[t - 1])]
            m.d.sync += [line_i[0].eq(mix_i), line_q[0].eq(mix_q)]
            with m.If(due):
                m.d.sync += t_acc.eq(t_acc + period_eff - ONE)
                with m.If(commit):
                    m.d.sync += [v.eq(new_v), period.eq(new_p), gain_acc.eq(new_g), q_rd.eq(q_rd + 1)]
                with m.Else():
                    m.d.sync += nsym.eq(nsym + 1)
            with m.Else():
                m.d.sync += t_acc.eq(t_acc - ONE)
        m.d.sync += [
            c_phase.eq(t_acc[FRAC_BITS - PHASE_BITS:FRAC_BITS]),
            c_gain.eq(gain_eff >> GAIN_ACC_EXTRA_BITS),
            c_period.eq(period_eff),
        ]
        # Note: the delay line updates at D0 together with the decision, so
        # during D1 it holds m[n] .. m[n - taps + 1] for the due sample n,
        # which is what the model's step 3.3 reads.

        # ---- D1: coefficient read and delay-line snapshot ---------------
        snap_i = [Signal(signed(14), name=f"snap_i{t}") for t in range(taps)]
        snap_q = [Signal(signed(14), name=f"snap_q{t}") for t in range(taps)]
        m.d.sync += [snap_i[t].eq(line_i[t]) for t in range(taps)]
        m.d.sync += [snap_q[t].eq(line_q[t]) for t in range(taps)]
        hit_t = lambda t: (self.c_tap == t)  # noqa: E731
        kc = []
        hc = {}
        gc = {}
        for t in range(taps):
            init_k = [((self.k[ph][t] if self.k else 0) & 0x3FFFF) for ph in range(PHASES)]
            mk = Memory(shape=unsigned(COEF_BITS), depth=PHASES, init=init_k)
            m.submodules[f"coef_k{t}"] = mk
            rk = mk.read_port()
            m.d.comb += rk.addr.eq(c_phase)
            wk = mk.write_port()
            m.d.comb += [wk.en.eq(self.c_we_k & hit_t(t)), wk.addr.eq(self.c_phase), wk.data.eq(self.c_k)]
            kc.append(rk.data.as_signed())
            if t in self.ted:
                for name, bank, we, val, store in (
                    ("h", self.h, self.c_we_h, self.c_h, hc),
                    ("g", self.g, self.c_we_g, self.c_g, gc),
                ):
                    init = [((bank[ph][t] if bank else 0) & 0x3FFFF) for ph in range(PHASES)]
                    mem = Memory(shape=unsigned(COEF_BITS), depth=PHASES, init=init)
                    m.submodules[f"coef_{name}{t}"] = mem
                    rp = mem.read_port()
                    m.d.comb += rp.addr.eq(c_phase)
                    wp = mem.write_port()
                    m.d.comb += [wp.en.eq(we & hit_t(t)), wp.addr.eq(self.c_phase), wp.data.eq(val)]
                    store[t] = rp.data.as_signed()

        # ---- D2: products ------------------------------------------------
        pk = [[Signal(signed(32), name=f"pk{t}_{i}") for i in range(2)] for t in range(taps)]
        for t in range(taps):
            m.d.sync += [pk[t][0].eq(kc[t] * snap_i[t]), pk[t][1].eq(kc[t] * snap_q[t])]
        ph_ = {}
        pg_ = {}
        for t in self.ted:
            ph_[t] = [Signal(signed(32), name=f"ph{t}_{i}") for i in range(2)]
            pg_[t] = [Signal(signed(32), name=f"pg{t}_{i}") for i in range(2)]
            m.d.sync += [
                ph_[t][0].eq(hc[t] * snap_i[t]), ph_[t][1].eq(hc[t] * snap_q[t]),
                pg_[t][0].eq(gc[t] * snap_i[t]), pg_[t][1].eq(gc[t] * snap_q[t]),
            ]

        def tree(leaves, levels, name):
            """Registered pairwise adder tree over exactly `levels` stages
            (padded with zeros); 44-bit sums (the model's accumulators)."""
            cur = list(leaves)
            for lvl in range(levels):
                nxt = []
                for i in range(0, max(len(cur), 1), 2):
                    a = cur[i]
                    b = cur[i + 1] if i + 1 < len(cur) else Const(0, signed(44))
                    # adders in fabric: Vivado would otherwise spend a DSP48
                    # post-adder on each, and the block's multipliers already
                    # take 112 of the xc7z020's 220
                    s = Signal(signed(44), name=f"{name}_l{lvl}_{i // 2}", attrs=NO_DSP)
                    m.d.sync += s.eq(a + b)
                    nxt.append(s)
                cur = nxt
            assert len(cur) == 1
            return cur[0]

        # D3..D7: TREE levels; the detector's 8 leaves need 3 levels plus
        # TREE - 3 delays so all sums land at D7.
        acc_k = [tree([pk[t][i] for t in range(taps)], TREE, f"tk{i}") for i in range(2)]
        ted_levels = max((len(self.ted) - 1).bit_length(), 0)
        acc_h = [
            pipe(tree([ph_[t][i] for t in self.ted], ted_levels, f"th{i}"), TREE - ted_levels, f"dh{i}")
            for i in range(2)
        ]
        acc_g = [
            pipe(tree([pg_[t][i] for t in self.ted], ted_levels, f"tg{i}"), TREE - ted_levels, f"dg{i}")
            for i in range(2)
        ]

        # context down the pipeline: c_* hold the symbol's values during D1
        valid = [v_d1]  # valid[k-1] is high during Dk
        for kk in range(2, LAST + 1):
            nv = Signal(name=f"valid_d{kk}")
            m.d.sync += nv.eq(valid[-1])
            valid.append(nv)

        def at(k):
            return valid[k - 1]

        # c_* are the symbol's during D1; pipe(c, n) shows them at D(1+n)
        gain_d9 = pipe(c_gain, 8, "g9")
        phase_d13 = pipe(c_phase, 12, "ph13")
        gain_d13 = pipe(c_gain, 12, "g13")
        period_d13 = pipe(c_period, 12, "p13")

        # ---- D8: y, dy, yk (tree outputs are valid during D7) ------------
        def filt(a):
            return sat(round_shift_const(a, FILTER_SHIFT), -32768, 32767)

        y = [Signal(signed(16), name=f"y{i}") for i in range(2)]
        dy = [Signal(signed(16), name=f"dy{i}") for i in range(2)]
        yk = [Signal(signed(16), name=f"yk{i}") for i in range(2)]
        for i in range(2):
            m.d.sync += [y[i].eq(filt(acc_h[i])), dy[i].eq(filt(acc_g[i])), yk[i].eq(filt(acc_k[i]))]
        # ---- D9: gain products --------------------------------------------
        gp = [Signal(signed(34), name=f"gp{i}") for i in range(6)]
        gain_s = Cat(gain_d9, Const(0, 1)).as_signed()
        m.d.sync += [
            gp[0].eq(y[0] * gain_s), gp[1].eq(y[1] * gain_s),
            gp[2].eq(dy[0] * gain_s), gp[3].eq(dy[1] * gain_s),
            gp[4].eq(yk[0] * gain_s), gp[5].eq(yk[1] * gain_s),
        ]
        # ---- D10: z, dz, zk -------------------------------------------------
        z = [Signal(signed(16), name=f"z{i}") for i in range(2)]
        dz = [Signal(signed(16), name=f"dz{i}") for i in range(2)]
        zk = [Signal(signed(16), name=f"zk{i}") for i in range(2)]
        for i in range(2):
            m.d.sync += [
                z[i].eq(sat(round_shift_const(gp[i], GAIN_FRAC_BITS), -32768, 32767)),
                dz[i].eq(sat(round_shift_const(gp[2 + i], GAIN_FRAC_BITS), -32768, 32767)),
                zk[i].eq(sat(round_shift_const(gp[4 + i], GAIN_FRAC_BITS), -32768, 32767)),
            ]
        # ---- D11: quantiser (zk), TED (z, dz) and power (zk) products ---
        lim = Array(Const(1 << max(b - 1, 0), signed(10)) for b in range(16))[r.bits]
        qv = [Signal(signed(8), name=f"q{i}") for i in range(2)]
        tp = [Signal(signed(32), name=f"tp{i}") for i in range(2)]
        pw = [Signal(signed(32), name=f"pw{i}") for i in range(2)]
        for i in range(2):
            m.d.sync += [
                qv[i].eq(sat(round_shift_var(zk[i], r.q_shift), -lim, lim - 1)),
                tp[i].eq(z[i] * dz[i]),
                pw[i].eq(zk[i] * zk[i]),
            ]
        # ---- D12: e, err ---------------------------------------------------
        e = Signal(signed(24))
        err = Signal(signed(33))
        ted = Signal(signed(33))
        powv = Signal(signed(34))
        m.d.comb += [ted.eq(tp[0] + tp[1]), powv.eq((pw[0] + pw[1]) >> POWER_SHIFT)]
        m.d.sync += [e.eq(sat(ted >> r.ted_shift, -E_MAX, E_MAX)), err.eq(r.agc_target - powv)]
        # ---- D13, D14: loop terms; queued at the end of D14 ----------------
        kpe = Signal(signed(40))
        kie = Signal(signed(40))
        ags = Signal(signed(33))
        m.d.sync += [kpe.eq(r.kp * e), kie.eq(r.ki * e), ags.eq(err >> r.agc_shift)]
        kpe2 = Signal(signed(40))
        kie2 = Signal(signed(40))
        ags2 = Signal(signed(33))
        m.d.sync += [
            kpe2.eq(round_shift_var(kpe, r.kp_shift)),
            kie2.eq(round_shift_var(kie, r.ki_shift)),
            ags2.eq(ags),
        ]
        m.d.comb += [
            q_wp.addr.eq(q_wr),
            q_wp.data.eq(Cat(kie2.as_unsigned(), kpe2.as_unsigned(), ags2.as_unsigned())),
            q_wp.en.eq(at(LAST)),
        ]
        with m.If(at(LAST)):
            m.d.sync += q_wr.eq(q_wr + 1)

        # ---- per-symbol internals, valid at D13 ---------------------------
        sq = [Signal(signed(8), name=f"sq{i}") for i in range(2)]
        m.d.sync += [sq[i].eq(qv[i]) for i in range(2)]
        m.d.comb += [
            self.s_valid.eq(at(13)),
            self.s_phase.eq(phase_d13),
            self.s_gain.eq(gain_d13),
            self.s_period.eq(period_d13),
            self.s_e.eq(e),
            self.s_err.eq(err),
        ]
        for i in range(2):
            m.d.comb += [
                self.s_y[i].eq(pipe(y[i], 4, f"sy{i}")),
                self.s_dy[i].eq(pipe(dy[i], 4, f"sdy{i}")),
                self.s_yk[i].eq(pipe(yk[i], 4, f"syk{i}")),
                self.s_z[i].eq(pipe(z[i], 2, f"sz{i}")),
                self.s_dz[i].eq(pipe(dz[i], 2, f"sdz{i}")),
                self.s_zk[i].eq(pipe(zk[i], 2, f"szk{i}")),
                self.s_q[i].eq(sq[i]),
            ]

        # ---- packer (isdb_frontend::format::Packer), fed at D13 ------------
        # Symbols arrive at most one per cycle; the packer emits at most one
        # word per cycle and needs HEADER_WORDS + 2 extra cycles per block
        # (header and trailer). A FIFO of symbols absorbs those cycles: at
        # 40 Msps a symbol takes at least 1.06 samples (period_min), so the
        # average symbol rate (<= 0.94 per cycle) leaves room for the
        # block's overhead and the FIFO drains.
        pq_bits = 6
        pqmem = Memory(shape=unsigned(16 + 16 + 32), depth=1 << pq_bits, init=[])
        m.submodules.symq = pqmem
        pq_rp = pqmem.read_port(domain="comb")
        pq_wp = pqmem.write_port()
        pq_rd = Signal(pq_bits)
        pq_wr = Signal(pq_bits)
        m.d.comb += pq_rp.addr.eq(pq_rd)
        pq_sym_h = pq_rp.data[:16]
        pq_gain_h = pq_rp.data[16:32]
        pq_per_h = pq_rp.data[32:]
        mask = Array(Const((1 << b) - 1, 8) for b in range(16))[r.bits]
        sym_word = Signal(16)
        m.d.comb += sym_word.eq(
            (self.s_q[0].as_unsigned() & mask) | ((self.s_q[1].as_unsigned() & mask) << r.bits)
        )
        m.d.comb += [
            pq_wp.addr.eq(pq_wr),
            pq_wp.data.eq(Cat(sym_word, gain_d13, period_d13)),
            pq_wp.en.eq(at(13)),
        ]
        with m.If(at(13)):
            m.d.sync += pq_wr.eq(pq_wr + 1)
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
        m.d.comb += newacc.eq(bacc | (pq_sym_h << nacc))
        nbits = Signal(6)
        m.d.comb += nbits.eq(nacc + (r.bits << 1))
        avail = Signal()
        m.d.comb += avail.eq(pq_rd != pq_wr)
        m.d.sync += self.o_valid.eq(0)
        with m.FSM(name="packer"):
            with m.State("IDLE"):
                with m.If(avail):
                    with m.If(in_block == 0):
                        m.d.sync += [hdr_idx.eq(0), hdr_gain.eq(pq_gain_h), hdr_per.eq(pq_per_h)]
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
                # one symbol per cycle while the FIFO has one
                with m.If(avail):
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
                seq.eq(0), crc.eq(0), tr_idx.eq(0), v_d1.eq(0),
            ]
            m.d.sync += [vv.eq(0) for vv in valid[1:]]
            m.d.sync += [line_i[t].eq(0) for t in range(taps)]
            m.d.sync += [line_q[t].eq(0) for t in range(taps)]
        return m
