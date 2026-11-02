"""Stage 6 of the ISDB-S3 chain: the max-log demapper, the de-interleaving
into the LDPC decoder's input, the pilot-learned constellation and the
noise estimate.

Contract: `isdbs3_phy::chain::demap` (`demap`, `Learn`, `InForce`) and
`frame::Back::symbol` / `finish`, module docs "Stage 6":

- `m(p) = |p|^2 - 2 (y_i p_i + y_q p_q)` per point of the codeword's
  constellation (Q.10 points, Q.20 metrics); for label bit b (MSB first)
  `Delta_b = min_{bit 1} m - min_{bit 0} m`, LLR `sat127(round_shift(Delta_b K,
  32))`;
- main symbol s of a valid slot's codeword (the codewords of a frame's
  valid slots cover its main symbols in slot order) gives LLR b to
  position `col rows + s` (col = b forward or bits - 1 - b backward, Table
  3.7-2) or `s bits + b` (pi/2-BPSK, QPSK);
- the constellation in force for a frame: per modulation, the points
  learned on the previous decoded frame (round(sum / count) of its pilots
  with the PRBS sign removed, saturating, label t mod 2^bits) or, for a
  modulation that frame had no pilots of, the nominal points of the
  codeword's rate; the noise: n0 = round(sum (|y|^2 + min m) / count) over
  every 16th symbol of each codeword of the previous decoded frame (else
  the n0 before; 52 556 at the start), K = round(2^32 / max(n0, 256)).

Implementation: one main symbol per symbol clock (`sync`); the metrics in
the 4x clock (`fast`, phase-aligned with `sync`: four fast cycles per
symbol clock, phase 0 the one after the common edge), 8 points per fast
cycle:

- **Point sets**: two sets of 32 points (re, im 16 bits; |p|^2 truncated
  to 24 bits as before), one in use and one loaded for the next codeword,
  in eight LUT RAMs (lane l: points 8 p + l of both sets, address {set,
  p}), written in the fast domain: the loader reads one point per symbol
  clock (the learned RAM or the nominal ROM, `nominal.py`) and the fast
  domain's squarer (one DSP48E1, shared with |y|^2) makes |p|^2 on phases
  2 and 3 and writes the lane on the next phase 0.
- **Metrics**, per fast cycle (the symbol arrives at A, its z and set held
  in the symbol clock's registers through A+1; phase p of A+1 reads lane
  address {set, p}): F0 the eight points; F1 16 products (16 x 16 bits,
  16 DSP48E1, truncated to 29 bits as the 64-product design did); F2 the
  eight metrics (labels >= 2^bits masked to 2^31 - 1); F3 eight pair
  minima; F4 per LSB-based bit position 0..2 and value a minimum of four
  (shared pairs: {0,2,4,6}, {1,3,5,7}, {0,1,4,5}, {2,3,6,7}, {0,1,2,3},
  {4,5,6,7}); F5 those into running minima (reset on phase 0), the minimum
  of all eight; F6 that into the running minima of positions 3 and 4 (the
  eight points of phase p share bits 3, 4 = p & 1, p >> 1). The max-log
  minimum over a label set is the same in any visiting order, so these
  are the 32-leaf trees' values exactly. After phase 3's update: the five
  differences and the overall minimum (F7), the five products by K on two
  multipliers (25 x 33 bits, two DSP48E1 each; positions 0-2 on one,
  3-4 on the other, one per fast cycle), the rounding and saturation:
  the LLRs are in registers by phase 2 of A+4, |y|^2 (the squarer on
  phases 0, 1 of A+1) by A+2, the minimum by A+4; the symbol clock takes
  them there and delays them to the 64-product design's schedule, so
  everything after is unchanged: the noise sample at A+8, the beat at A+9.
  21 DSP48E1 in all (the 64-product design: 95).
- **The beat** at A+9 (`o_*`: up to 5 positions and LLRs, `o_first` /
  `o_last` of the codeword, the 64-bit tag: ldpc_if.py) to the decoder's
  port; the positions col * rows + s from a constant table (rows =
  44 880 / bits), s * bits + b for bits 1, 2 by a shift.
- **Codeword cursor**: the frame's valid slots in order, from the layout
  in force (`FrameCtl`'s second read port), the next one looked up when a
  codeword starts; its points loaded into the free set then (36 cycles:
  the frame's first codeword is current before the frame's 57th symbol,
  its first main one).
- **Pilots**: 32 slot accumulators by label (21 bits; a LUT RAM with a
  valid bit per label, one adder), added into the frame's sums (one RAM
  entry per modulation and label, 32 bits per component, one read port
  shared by the slot flush and the learning, which never overlap) after
  each slot's pilots; the count of a label is the slots of its modulation
  times 32 / 2^bits. After the last slot's pilots (9 240 symbols before
  the frame's end) each label's round(sum / count) by a sequential divider
  into the point RAM's other bank.
- **Noise**: a 48-bit sum and a count; at a decoded frame's end n0 and K
  by one 3-bit-per-cycle divider, one after the other (about 37 cycles from
  the frame end's event, which is 12 after its last symbol: the next
  frame's first main symbol reaches the K product 57 symbols later at the
  earliest).
  `late` counts a K not ready in time (never on a stream with the
  frame's 56 leading sync and pilot symbols, cycle count above).
- At a decoded frame's end the banks swap (the learned points and n0 are
  in force from the next frame); a dropped frame changes nothing.

Clock crossing (fpga/tezuka/isdb_slots.xdc): the fast domain reads the
symbol clock's registers on all four phases (they change only on the
common edge) and the symbol clock reads the fast registers one symbol
clock after they settle (written by phase 2 at the latest); the phase is
found from a toggle of the symbol clock (`phase.py`'s `Phase4`).
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import LLR_SHIFT, N0_DEFAULT, N0_MIN, NOISE_STEP, PILOTS, SLOTS, Phase4, SeqDiv, SeqDivR
from .common import sat16, sat127
from .ldpc_if import LANES
from .nominal import NominalPort, NominalRom
from .tables import MOD_BITS, pilot_sign_rom

N_LDPC = 44880
CTX_BITS = 74
INF = (1 << 31) - 1  # a masked metric (larger than any real one)
LANES8 = 8  # points per fast cycle


def k_of(n0):
    n = max(n0, N0_MIN)
    return (2 * (1 << LLR_SHIFT) + n) // (2 * n)


def direction(mod, rate_code):
    """`interleave::direction`: 0 none, 1 forward, 2 backward."""
    from .tables import RATES

    r = RATES[rate_code - 1]
    if mod in (1, 2):
        return 0
    if r <= 49:
        return 2
    if mod == 5 and r == 93:
        return 2
    return 1


def smin(m, dom, a, b, name):
    """A registered minimum of two signed values (in domain `dom`)."""
    r = Signal(signed(max(len(a), len(b))), name=name)
    dom += r.eq(Mux(a < b, a, b))
    return r


class Demap(Elaboratable):
    def __init__(self, nominal=None):
        """`nominal`: a `NominalPort` of a shared `NominalRom` (back.py), or
        None for one of its own."""
        self.nominal = nominal
        # the equaliser's output, routed (aligned with z)
        self.valid = Signal()
        self.z = [Signal(signed(16), name=f"z{k}") for k in range(2)]
        self.is_main = Signal()  # a main symbol of an active frame with a layout
        self.is_pilot = Signal()  # a pilot of an active frame with a layout
        self.k = Signal(range(SLOTS))
        self.t = Signal(5)
        self.slot_mod = Signal(4)
        self.frame = Signal(32)
        self.f_start = Signal()  # a frame with a layout starts (its first symbol, before it)
        self.f_swap = Signal()  # it ended whole (after its last symbol)
        self.f_end = Signal()  # the same, once the pipeline is drained
        self.f_drop = Signal()  # it was dropped
        # the layout in force (FrameCtl's second read port, one cycle)
        self.lay_addr = Signal(range(SLOTS))
        self.lay_data = Signal(49)  # frame.LAY_BITS
        # the decoder's input port
        self.o_valid = Signal()
        self.o_n = Signal(range(LANES + 1))
        self.o_pos = [Signal(16, name=f"o_pos{k}") for k in range(LANES)]
        self.o_llr = [Signal(signed(8), name=f"o_llr{k}") for k in range(LANES)]
        self.o_first = Signal()
        self.o_last = Signal()
        self.o_tag = Signal(64)
        self.o_rate = Signal(4)
        # in force (status)
        self.n0 = Signal(32, init=N0_DEFAULT)
        self.kk = Signal(32, init=k_of(N0_DEFAULT))
        self.learned_from = Signal(32, init=0xFFFFFFFF)
        self.learned_valid = Signal(5)  # per modulation (bits - 1), the bank in force
        self.st_prev = Signal()  # read the bank in force before the last swap
        self.st_addr = Signal(8)  # (mod - 1) * 32 + label
        self.st_data = Signal(32)
        self.st_valid = Signal(5)  # that bank's learned modulations
        self.prev_n0 = Signal(32, init=N0_DEFAULT)
        self.prev_k = Signal(32, init=k_of(N0_DEFAULT))
        self.prev_from = Signal(32, init=0xFFFFFFFF)
        self.late = Signal(16)
        # the next frame's (verification)
        self.nx_valid = Signal()
        self.nx_n0 = Signal(32)
        self.nx_k = Signal(32)
        self.lp_valid = Signal()  # a learned point written
        self.lp_addr = Signal(8)
        self.lp_data = Signal(32)

    def elaborate(self, platform):
        m = Module()
        fast = m.d.fast
        bitsa = Array(Const(MOD_BITS.get(i, 1), 3) for i in range(16))
        m.submodules.ph = ph4 = Phase4()
        ph = ph4.phase  # the fast cycle's phase (0 .. 3)

        # ---- point RAM (2 banks x 8 modulations x 32 labels) and nominal ROM
        pts = Memory(shape=32, depth=512, init=[])
        m.submodules.pts = pts
        pw = pts.write_port()
        pr = pts.read_port()
        pst = pts.read_port()
        if self.nominal is None:
            m.submodules.nom = nrom = NominalRom(1)
            nr = nrom.ports[0]
        else:
            nr = self.nominal
        fb = Signal()  # the bank in force
        valid_b = [Signal(5, name=f"lvalid{b}") for b in range(2)]
        m.d.comb += [self.learned_valid.eq(Mux(fb, valid_b[1], valid_b[0])),
                     pst.addr.eq(Cat(self.st_addr, fb ^ self.st_prev)), self.st_data.eq(pst.data),
                     self.st_valid.eq(Mux(fb ^ self.st_prev, valid_b[1], valid_b[0]))]

        NB = [Signal(3, name=f"nb{s}") for s in range(2)]  # bits of the set's modulation

        # ---- codeword cursor --------------------------------------------------
        cw_ok = Signal()  # a codeword is current
        cw_set = Signal()
        cw_slot = Signal(7)
        cw_mode = Signal(3)
        cw_mod = Signal(4)
        cw_rate = Signal(4)
        # a codeword's symbols and the index in it: up to 44 880 (pi/2-BPSK)
        cw_syms = Signal(16)
        sym = Signal(16)
        nx_ok = Signal()  # the next codeword found (and its points loaded)
        nx_slot = Signal(7)
        nx_mode = Signal(3)
        nx_mod = Signal(4)
        nx_rate = Signal(4)
        nx_syms = Signal(16)
        scan = Signal(range(SLOTS + 1))
        frame = Signal(32)

        # loader: label counter, from the RAM (learned) or the ROM (nominal);
        # the point reaches its lane LUT RAM three symbol clocks after its
        # read (the fast squarer), so the loader counts 2 more before idle
        ld_busy = Signal()
        ld_i = Signal(6)
        ld_set = Signal()
        ld_mod = Signal(4)
        ld_rate = Signal(4)
        ld_learned = Signal()
        ld_v = Signal()
        ld_lab = Signal(5)
        m.d.comb += [
            pr.addr.eq(Cat(ld_i[:5], (ld_mod - 1)[:3], fb)),
            nr.mod.eq(Mux(ld_mod > 5, 1, ld_mod)), nr.rate.eq(ld_rate), nr.label.eq(ld_i[:5]),
        ]
        m.d.sync += [ld_v.eq(ld_busy & (ld_i < 32)), ld_lab.eq(ld_i[:5])]
        lre = Mux(ld_learned, pr.data[:16].as_signed(), nr.re)
        lim = Mux(ld_learned, pr.data[16:].as_signed(), nr.im)
        # the point in hand for the fast domain (one symbol clock)
        lq_v = Signal()
        lq_set = Signal()
        lq_lab = Signal(5)
        lq = [Signal(signed(16), name=f"lq{c}") for c in range(2)]
        m.d.sync += [lq_v.eq(ld_v), lq_set.eq(ld_set), lq_lab.eq(ld_lab), lq[0].eq(lre), lq[1].eq(lim)]
        with m.If(ld_busy):
            m.d.sync += ld_i.eq(ld_i + 1)
            with m.If(ld_i == 34):
                m.d.sync += ld_busy.eq(0)

        def load(set_, mod, rate):
            mi = (mod - 1)[:3]
            m.d.sync += [ld_busy.eq(1), ld_i.eq(0), ld_set.eq(set_), ld_mod.eq(mod), ld_rate.eq(rate),
                         ld_learned.eq((Mux(fb, valid_b[1], valid_b[0]) >> mi)[0])]
            for s in range(2):
                with m.If(set_ == s):
                    m.d.sync += NB[s].eq(bitsa[mod])

        # layout scan: the next valid slot from `scan`
        ld = self.lay_data
        l_mode, l_mod, l_rate, l_dummy, l_syms = ld[0:3], ld[3:7], ld[7:11], ld[11], ld[33:49]
        m.d.comb += self.lay_addr.eq(Mux(scan < SLOTS, scan, 0)[:7])
        main = self.valid & self.is_main
        end_cw = main & cw_ok & (sym == cw_syms - 1)
        with m.FSM(name="cursor"):
            with m.State("IDLE"):
                with m.If(self.f_start):
                    m.d.sync += [scan.eq(0), cw_ok.eq(0), nx_ok.eq(0), frame.eq(self.frame)]
                    m.next = "SCAN"
            with m.State("SCAN"):
                # the layout entry of slot `scan` is read; CHK sees it
                with m.If(self.f_drop):
                    m.d.sync += [cw_ok.eq(0), nx_ok.eq(0)]
                    m.next = "IDLE"
                with m.Elif(scan >= SLOTS):
                    m.next = "LAST"
                with m.Else():
                    m.next = "CHK"
            with m.State("CHK"):
                with m.If(self.f_drop):
                    m.d.sync += [cw_ok.eq(0), nx_ok.eq(0)]
                    m.next = "IDLE"
                with m.Elif(~l_dummy):
                    m.d.sync += [nx_slot.eq(scan), nx_mode.eq(l_mode), nx_mod.eq(l_mod),
                                 nx_rate.eq(l_rate), nx_syms.eq(l_syms)]
                    m.next = "LOAD"
                with m.Else():
                    m.d.sync += scan.eq(scan + 1)
                    m.next = "SCAN"
            with m.State("LOAD"):
                load(~Mux(cw_ok, cw_set, 1), nx_mod, nx_rate)
                m.next = "LOADING"
            with m.State("LOADING"):
                with m.If(self.f_drop):
                    m.d.sync += [cw_ok.eq(0), nx_ok.eq(0)]
                    m.next = "IDLE"
                with m.Elif(~ld_busy):
                    m.d.sync += nx_ok.eq(1)
                    with m.If(~cw_ok):
                        # the frame's first codeword becomes current
                        m.d.sync += [cw_ok.eq(1), cw_set.eq(0), cw_slot.eq(nx_slot), cw_mode.eq(nx_mode),
                                     cw_mod.eq(nx_mod), cw_rate.eq(nx_rate), cw_syms.eq(nx_syms), sym.eq(0),
                                     nx_ok.eq(0), scan.eq(nx_slot + 1)]
                        m.next = "SCAN"
                    with m.Else():
                        m.next = "WAIT"
            with m.State("WAIT"):
                with m.If(self.f_drop):
                    m.d.sync += [cw_ok.eq(0), nx_ok.eq(0)]
                    m.next = "IDLE"
                with m.Elif(end_cw):
                    m.d.sync += [cw_set.eq(~cw_set), cw_slot.eq(nx_slot), cw_mode.eq(nx_mode),
                                 cw_mod.eq(nx_mod), cw_rate.eq(nx_rate), cw_syms.eq(nx_syms),
                                 nx_ok.eq(0), scan.eq(nx_slot + 1)]
                    m.next = "SCAN"
            with m.State("LAST"):
                with m.If(self.f_drop | (end_cw)):
                    m.d.sync += cw_ok.eq(0)
                    m.next = "IDLE"
        with m.If(main & cw_ok):
            m.d.sync += sym.eq(Mux(end_cw, 0, sym + 1))

        # ---- the symbol clock's side of the metric pipeline -------------------
        # A: arrival (main & cw_ok)
        a_v = main & cw_ok
        v1 = Signal()
        z1 = [Signal(signed(16), name=f"z1_{c}") for c in range(2)]
        s1 = Signal()
        ctx = Signal(CTX_BITS)
        dirn = Array(Const(direction(md, rc) if 1 <= md <= 5 and 1 <= rc <= 11 else 0, 2)
                     for md in range(16) for rc in range(16))[Cat(cw_rate, cw_mod)]
        ctx_now = Cat(sym, end_cw, sym == 0, Mux(cw_set, NB[1], NB[0]), cw_mod, cw_rate, cw_slot,
                      cw_mode, dirn, (sym % NOISE_STEP) == 0, frame)
        m.d.sync += [v1.eq(a_v), z1[0].eq(self.z[0]), z1[1].eq(self.z[1]), s1.eq(cw_set), ctx.eq(ctx_now)]
        # the context word: sym 0-15, last 16, first 17, bits 18-20, mod 21-24,
        # rate 25-28, slot 29-35, mode 36-38, direction 39-40, noise 41, the
        # codeword's frame 42-73 (the tag's: `frame` itself moves on at the
        # next frame's start, which can come before a frame's last beats
        # leave)
        cp = [ctx]
        vp = [v1]
        for d in range(2, 10):
            nc = Signal(CTX_BITS, name=f"ctx{d}")
            nv = Signal(name=f"v{d}")
            m.d.sync += [nc.eq(cp[-1]), nv.eq(vp[-1])]
            cp.append(nc)
            vp.append(nv)
        # cp[i] / vp[i] hold the symbol during A+1+i
        nb1 = ctx[18:21]

        # ---- the fast domain: lanes, squarer, metrics --------------------------
        lanes = []
        for la in range(LANES8):
            mem = Memory(shape=56, depth=8, init=[])
            m.submodules[f"lane{la}"] = mem
            lanes.append((mem.write_port(domain="fast"), mem.read_port(domain="comb")))
        # the squarer: phase 0 z_re^2, 1 z_im^2 (the symbol of A+1), 2 re^2,
        # 3 im^2 (the loader's point)
        # the operand registered a fast cycle ahead (`sq_op`: the multiplier's
        # input from a register, the crossing from the symbol clock's
        # registers through one select): in phase 3 the next symbol clock's
        # z_re, which z1 takes at the coming edge (the eq's output register)
        sq_op = Signal(signed(16))
        fast += sq_op.eq(Array([z1[1], lq[0], lq[1], self.z[0]])[ph])
        sq = Signal(signed(32))
        fast += sq.eq(sq_op * sq_op)
        sq_a = Signal(signed(32))
        zz_f = Signal(32)
        w_v = Signal()
        w_set = Signal()
        w_lab = Signal(5)
        w_p = [Signal(signed(16), name=f"w_p{c}") for c in range(2)]
        with m.If(ph == 1):
            fast += sq_a.eq(sq)  # z_re^2
        with m.If(ph == 2):
            fast += zz_f.eq(sq_a + sq)  # + z_im^2
        with m.If(ph == 3):
            fast += [sq_a.eq(sq), w_v.eq(lq_v), w_set.eq(lq_set), w_lab.eq(lq_lab),
                     w_p[0].eq(lq[0]), w_p[1].eq(lq[1])]  # re^2
        ppw = Signal(signed(24))
        m.d.comb += ppw.eq(sq_a + sq)  # in phase 0: re^2 + im^2, truncated to 24 bits
        for la, (wp, _rp) in enumerate(lanes):
            m.d.comb += [wp.addr.eq(Cat(w_lab[3:5], w_set)),
                         wp.data.eq(Cat(w_p[0], w_p[1], ppw)),
                         wp.en.eq((ph == 0) & w_v & (w_lab[:3] == la))]

        # F0: the eight points of phase p of the symbol in hand
        e_v = Signal()
        e_p = Signal(2)
        e_nb = Signal(3)
        e_z = [Signal(signed(16), name=f"e_z{c}") for c in range(2)]
        e_pt = [[Signal(signed(16), name=f"e_p{la}_{c}") for c in range(2)] for la in range(LANES8)]
        e_pp = [Signal(signed(24), name=f"e_pp{la}") for la in range(LANES8)]
        fast += [e_v.eq(v1), e_p.eq(ph), e_nb.eq(nb1), e_z[0].eq(z1[0]), e_z[1].eq(z1[1])]
        for la, (_wp, rp) in enumerate(lanes):
            m.d.comb += rp.addr.eq(Cat(ph, s1))
            fast += [e_pt[la][0].eq(rp.data[0:16].as_signed()), e_pt[la][1].eq(rp.data[16:32].as_signed()),
                     e_pp[la].eq(rp.data[32:56].as_signed())]
        # F1: the products (truncated to 29 bits), |p|^2
        f1_v, f1_p, f1_nb = Signal(), Signal(2), Signal(3)
        prod = [[Signal(signed(29), name=f"zp{la}_{c}") for c in range(2)] for la in range(LANES8)]
        pp1 = [Signal(signed(24), name=f"pp1_{la}") for la in range(LANES8)]
        fast += [f1_v.eq(e_v), f1_p.eq(e_p), f1_nb.eq(e_nb)]
        for la in range(LANES8):
            fast += [prod[la][0].eq(e_z[0] * e_pt[la][0]), prod[la][1].eq(e_z[1] * e_pt[la][1]),
                     pp1[la].eq(e_pp[la])]
        # F2: the metrics, labels past 2^bits masked
        f2_v, f2_p = Signal(), Signal(2)
        met = [Signal(signed(32), name=f"met{la}") for la in range(LANES8)]
        fast += [f2_v.eq(f1_v), f2_p.eq(f1_p)]
        for la in range(LANES8):
            ok = Cat(Const(la, 3), f1_p) < (Const(1, 6) << f1_nb)
            fast += met[la].eq(Mux(ok, pp1[la] - ((prod[la][0] + prod[la][1]) << 1), INF))
        # F3: pair minima
        f3_v, f3_p = Signal(), Signal(2)
        fast += [f3_v.eq(f2_v), f3_p.eq(f2_p)]
        pm = {}
        for a, b in ((0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (4, 6), (1, 3), (5, 7)):
            pm[a, b] = smin(m, fast, met[a], met[b], f"pm{a}{b}")
        # F4: per position 0..2 and value, the minimum of four
        f4_v, f4_p = Signal(), Signal(2)
        fast += [f4_v.eq(f3_v), f4_p.eq(f3_p)]
        q4 = {
            (0, 0): smin(m, fast, pm[0, 2], pm[4, 6], "q00"), (0, 1): smin(m, fast, pm[1, 3], pm[5, 7], "q01"),
            (1, 0): smin(m, fast, pm[0, 1], pm[4, 5], "q10"), (1, 1): smin(m, fast, pm[2, 3], pm[6, 7], "q11"),
            (2, 0): smin(m, fast, pm[0, 1], pm[2, 3], "q20"), (2, 1): smin(m, fast, pm[4, 5], pm[6, 7], "q21"),
        }
        # F5: running minima of positions 0..2 (the symbol's phases in order),
        # the minimum of all eight
        f5_v, f5_p = Signal(), Signal(2)
        fast += [f5_v.eq(f4_v), f5_p.eq(f4_p)]
        all8 = smin(m, fast, q4[2, 0], q4[2, 1], "all8")
        run = {}
        fin = {}
        for pos in range(3):
            for v in range(2):
                r = Signal(signed(32), name=f"run{pos}{v}")
                fi = Signal(signed(32), name=f"fin{pos}{v}")
                nxt = Mux(f4_p == 0, q4[pos, v], Mux(q4[pos, v] < r, q4[pos, v], r))
                with m.If(f4_v):
                    fast += r.eq(nxt)
                    with m.If(f4_p == 3):
                        fast += fi.eq(nxt)
                run[pos, v], fin[pos, v] = r, fi
        # F6: positions 3 (value p & 1) and 4 (value p >> 1)
        f6_v = Signal()
        fast += f6_v.eq(f5_v & (f5_p == 3))
        for pos, sel in ((3, lambda p, v: p[0] == v), (4, lambda p, v: p[1] == v)):
            for v in range(2):
                r = Signal(signed(32), name=f"run{pos}{v}")
                fi = Signal(signed(32), name=f"fin{pos}{v}")
                first_p = v if pos == 3 else 2 * v
                nxt = Mux(f5_p == first_p, all8, Mux(all8 < r, all8, r))
                with m.If(f5_v & sel(f5_p, v)):
                    fast += r.eq(nxt)
                with m.If(f5_v & (f5_p == 3)):
                    fast += fi.eq(Mux(sel(f5_p, v), nxt, r))
                run[pos, v], fin[pos, v] = r, fi
        # F7: the differences and the overall minimum, as soon as their
        # minima are final (positions 0-2 after F5 of phase 3, 3-4 after F6)
        dlt = [Signal(signed(33), name=f"dlt{p}") for p in range(5)]
        best_f = Signal(signed(32))
        g0 = f5_v & (f5_p == 3)
        with m.If(g0):
            fast += [dlt[p].eq(fin[p, 1] - fin[p, 0]) for p in range(3)]
            fast += best_f.eq(Mux(fin[0, 0] < fin[0, 1], fin[0, 0], fin[0, 1]))
        with m.If(f6_v):
            fast += [dlt[p].eq(fin[p, 1] - fin[p, 0]) for p in range(3, 5)]
        # F8..: the products by K, positions 0-2 on unit 0 (from F7 of 0-2) and
        # 3-4 on unit 1, one per fast cycle: K <= 2^24 (= round(2^32 / max(n0,
        # 256))), so Delta * K[:25] is Delta << 24 when bit 24 is set (the rest
        # is then 0), else Delta * K[:24]: Delta split at bit 17, two 25 x 18
        # products (two DSP48E1) summed a cycle later; then the rounding and
        # saturation into llr_f
        k25 = Cat(self.kk[:24], Const(0, 1)).as_signed()
        k_top = self.kk[24]
        llr_f = [Signal(signed(8), name=f"llr_f{p}") for p in range(5)]
        for u, (start, poss) in enumerate(((g0, (0, 1, 2)), (f6_v, (3, 4)))):
            kc = Signal(2, name=f"kc{u}")
            k_go = Signal(name=f"k_go{u}")
            with m.If(start):
                fast += [kc.eq(0), k_go.eq(1)]
            with m.Elif(k_go):
                fast += kc.eq(kc + 1)
                with m.If(kc == len(poss) - 1):
                    fast += k_go.eq(0)
            dsel = Array(dlt[p] for p in poss)[kc]
            p_lo = Signal(signed(43), name=f"p_lo{u}")
            p_hi = Signal(signed(41), name=f"p_hi{u}")
            # the rounding's half (round_shift) added with the products and
            # Delta << 24, so that the last stage is a shift and the
            # saturation: dk = Delta K + 2^(LLR_SHIFT - 1)
            half = 1 << (LLR_SHIFT - 1)
            d_r = Signal(signed(59), name=f"d_r{u}")
            p_v, p_i = Signal(name=f"p_v{u}"), Signal(2, name=f"p_i{u}")
            fast += [p_lo.eq(k25 * Cat(dsel[:17], Const(0, 1)).as_signed() + half),
                     p_hi.eq(k25 * dsel[17:33].as_signed()),
                     d_r.eq((dsel << 24) + half), p_v.eq(k_go), p_i.eq(kc)]
            dk = Signal(signed(59), name=f"dk{u}")
            dk_v, dk_i = Signal(name=f"dk_v{u}"), Signal(2, name=f"dk_i{u}")
            fast += [dk.eq(Mux(k_top, d_r, p_lo + (p_hi << 17))), dk_v.eq(p_v), dk_i.eq(p_i)]
            v8 = sat127(dk >> LLR_SHIFT)
            for i, p in enumerate(poss):
                with m.If(dk_v & (dk_i == i)):
                    fast += llr_f[p].eq(v8)

        # ---- back in the symbol clock: the 64-product design's schedule ---------
        # llr_f settles by phase 2 of A+4, best_f by phase 3 of A+3, zz_f by
        # phase 2 of A+1
        llr_d = [llr_f]
        for d in range(5, 10):
            regs = [Signal(signed(8), name=f"llr{d}_{p}") for p in range(5)]
            m.d.sync += [regs[p].eq(llr_d[-1][p]) for p in range(5)]
            llr_d.append(regs)
        llr9 = llr_d[-1]  # A+9
        best_d = [best_f]
        for d in range(4, 9):
            r = Signal(signed(32), name=f"best{d}")
            m.d.sync += r.eq(best_d[-1])
            best_d.append(r)
        zz_d = [zz_f]
        for d in range(2, 9):
            r = Signal(32, name=f"zz{d}")
            m.d.sync += r.eq(zz_d[-1])
            zz_d.append(r)
        best8, zz8 = best_d[-1], zz_d[-1]
        v8, ctx8 = vp[7], cp[7]
        v9, ctx9 = vp[8], cp[8]
        # noise sample at A+8
        nsum = Signal(48)
        ncnt = Signal(20)
        with m.If(v8 & ctx8[41]):
            m.d.sync += [nsum.eq(nsum + zz8 + best8), ncnt.eq(ncnt + 1)]
        # A+9: the beat
        s9 = ctx9[0:16]
        nb9 = ctx9[18:21]
        mod9 = ctx9[21:25]
        rate9 = ctx9[25:29]
        slot9 = ctx9[29:36]
        mode9 = ctx9[36:39]
        dir9 = ctx9[39:41]
        # col * rows (rows = 44 880 / bits) for col < bits <= 5
        crows = Array(Const(c * (N_LDPC // b) if 1 <= b <= 5 and c < b else 0, 16)
                      for b in range(8) for c in range(8))
        m.d.sync += [self.o_valid.eq(v9), self.o_n.eq(nb9), self.o_last.eq(ctx9[16]), self.o_first.eq(ctx9[17]),
                     self.o_tag.eq(Cat(slot9, Const(0, 1), rate9, mod9, mode9, Const(0, 13), ctx9[42:74])),
                     self.o_rate.eq(rate9)]
        for b in range(5):
            # label bit b (MSB first) is LSB position bits - 1 - b
            pos_lsb = (nb9 - 1 - b)[:3]
            llr = Array(llr9)[pos_lsb]
            col = Mux(dir9 == 2, nb9 - 1 - b, b)[:3]
            p_int = crows[Cat(col, nb9)] + s9
            # s * bits + b: bits is 1 or 2 where it is used (direction 0)
            p_id = (Mux(nb9 == 2, s9 << 1, s9) + b)[:16]
            m.d.sync += [self.o_llr[b].eq(Mux(b < nb9, llr, 0)),
                         self.o_pos[b].eq(Mux(b < nb9, Mux(dir9 == 0, p_id, p_int[:16]), 0))]

        # ---- pilots -------------------------------------------------------------
        srom = Memory(shape=1, depth=4096, init=pilot_sign_rom())
        m.submodules.signs = srom
        sp = srom.read_port(domain="comb")
        m.d.comb += sp.addr.eq(self.k * PILOTS + self.t)
        neg = sp.data[0]

        def sneg(v):
            return Mux(neg, Mux(v == -32768, 32767, -v), v)

        pv = self.valid & self.is_pilot
        nbits = bitsa[self.slot_mod]
        lab = self.t & ((Const(1, 6) << nbits) - 1)[:5]
        # the slot accumulators: a LUT RAM (re, im 24 bits) and a valid bit per
        # label (cleared at a frame's start and by the flush)
        sacc = Memory(shape=48, depth=32, init=[])
        m.submodules.sacc = sacc
        sa_w = sacc.write_port()
        sa_r = sacc.read_port(domain="comb")
        sa_f = sacc.read_port(domain="comb")
        sval = Signal(32)
        m.d.comb += sa_r.addr.eq(lab)
        old_v = Mux(sval.bit_select(lab, 1), sa_r.data, 0)
        m.d.comb += [sa_w.addr.eq(lab),
                     sa_w.data.eq(Cat((old_v[:24].as_signed() + sneg(self.z[0]))[:24],
                                      (old_v[24:].as_signed() + sneg(self.z[1]))[:24])),
                     sa_w.en.eq(pv)]
        # the slot's pilots into the frame sums
        fs = Memory(shape=64, depth=256, init=[])
        m.submodules.fsums = fs
        fw = fs.write_port()
        fr_ = fs.read_port()  # the flush's read-modify-write and the learning
        slots_of = [Signal(8, name=f"slots_of{i}") for i in range(5)]
        fl_busy = Signal()
        fl_i = Signal(6)
        fl_mod = Signal(4)
        fl_n = Signal(6)
        fl_v = Signal()
        fl_lab = Signal(5)
        last_pilot = pv & (self.t == PILOTS - 1)
        fl_first = Signal()
        with m.If(last_pilot):
            m.d.sync += [fl_busy.eq(1), fl_i.eq(0), fl_mod.eq(self.slot_mod),
                         fl_n.eq((Const(1, 6) << nbits)[:6])]
            m.d.sync += fl_first.eq(Array(slots_of)[(self.slot_mod - 1)[:3]] == 0)
            for i in range(5):
                with m.If(self.slot_mod == i + 1):
                    m.d.sync += slots_of[i].eq(slots_of[i] + 1)
        m.d.sync += [fl_v.eq(fl_busy & (fl_i < fl_n)), fl_lab.eq(fl_i[:5])]
        with m.If(fl_busy & ~last_pilot):
            m.d.sync += fl_i.eq(fl_i + 1)
            with m.If(fl_i == fl_n):
                m.d.sync += fl_busy.eq(0)
        m.d.comb += sa_f.addr.eq(fl_lab)
        sf = Mux(sval.bit_select(fl_lab, 1), sa_f.data, 0)
        sa_re = sf[:24].as_signed()
        sa_im = sf[24:].as_signed()
        old_re = fr_.data[:32].as_signed()
        old_im = fr_.data[32:].as_signed()
        # the first slot of a modulation in the frame writes its sums (no clearing pass)
        m.d.comb += [fw.addr.eq(Cat(fl_lab, (fl_mod - 1)[:3])),
                     fw.data.eq(Cat(Mux(fl_first, sa_re, old_re + sa_re)[:32], Mux(fl_first, sa_im, old_im + sa_im)[:32])),
                     fw.en.eq(fl_v)]
        # the valid bits: set by an accumulation, cleared by the flush (the next
        # pilots are a slot later) and at a frame's start
        with m.If(self.f_start):
            m.d.sync += sval.eq(0)
        with m.Else():
            for x in range(32):
                with m.If((pv & (lab == x)) | (fl_v & (fl_lab == x))):
                    m.d.sync += sval[x].eq(pv & (lab == x))

        # ---- learning: after the last slot's pilots ---------------------------------
        # labels of every modulation with slots, divided into the other bank
        m.submodules.ldiv = ldiv = SeqDiv(36, 20)
        lr_go = Signal()
        lr_mod = Signal(3)  # 0 .. 4
        lr_lab = Signal(6)
        lr_comp = Signal()
        lr_re = Signal(signed(16))
        lr_busy = Signal()
        lr_done = Signal()
        nb_l = Array(Const(1 << (i + 1), 6) for i in range(8))[lr_mod]
        cnt_l = Signal(16)
        # slots of the modulation x 32 / 2^bits
        m.d.comb += cnt_l.eq(Array(slots_of)[lr_mod] << Array(Const(4 - i if i < 5 else 0, 3)
                                                             for i in range(8))[lr_mod])
        m.d.comb += fr_.addr.eq(Mux(lr_busy, Cat(lr_lab[:5], lr_mod), Cat(fl_i[:5], (fl_mod - 1)[:3])))
        rd2 = fr_
        num = Signal(signed(36))
        m.d.sync += [ldiv.start.eq(0), self.lp_valid.eq(0)]
        with m.If(pv & (self.k == SLOTS - 1) & (self.t == PILOTS - 1)):
            m.d.sync += lr_go.eq(1)
        nfb = ~fb
        with m.FSM(name="learn"):
            with m.State("IDLE"):
                with m.If(lr_go & ~fl_busy & ~last_pilot):
                    m.d.sync += [lr_go.eq(0), lr_mod.eq(0), lr_lab.eq(0), lr_comp.eq(0), lr_busy.eq(1)]
                    m.next = "MOD"
            with m.State("MOD"):
                with m.If(lr_mod == 5):
                    m.d.sync += [lr_busy.eq(0), lr_done.eq(1)]
                    m.next = "IDLE"
                with m.Elif(Array(slots_of)[lr_mod] == 0):
                    for b in range(2):
                        with m.If(nfb == b):
                            m.d.sync += valid_b[b].eq(valid_b[b] & ~(Const(1, 5) << lr_mod))
                    m.d.sync += lr_mod.eq(lr_mod + 1)
                with m.Else():
                    for b in range(2):
                        with m.If(nfb == b):
                            m.d.sync += valid_b[b].eq(valid_b[b] | (Const(1, 5) << lr_mod))
                    m.d.sync += [lr_lab.eq(0), lr_comp.eq(0)]
                    m.next = "RD"
            with m.State("RD"):
                m.next = "DIV"
            with m.State("DIV"):
                v = Mux(lr_comp, rd2.data[32:].as_signed(), rd2.data[:32].as_signed())
                nn = (v << 1) + cnt_l
                m.d.sync += [num.eq(nn), ldiv.n.eq(Mux(nn < 0, -nn, nn)), ldiv.d.eq(cnt_l << 1),
                             ldiv.start.eq(1)]
                m.next = "WAITDIV"
            with m.State("WAITDIV"):
                with m.If(ldiv.done):
                    q = Mux(num < 0, -(ldiv.q + (ldiv.r != 0)), ldiv.q)
                    qs = sat16(q.as_signed())
                    with m.If(~lr_comp):
                        m.d.sync += [lr_re.eq(qs), lr_comp.eq(1)]
                        m.next = "RD"
                    with m.Else():
                        m.d.sync += [self.lp_valid.eq(1), self.lp_addr.eq(Cat(lr_lab[:5], lr_mod)),
                                     self.lp_data.eq(Cat(lr_re[:16], qs[:16])), lr_comp.eq(0),
                                     lr_lab.eq(lr_lab + 1)]
                        with m.If(lr_lab + 1 == nb_l):
                            m.d.sync += lr_mod.eq(lr_mod + 1)
                            m.next = "MOD"
                        with m.Else():
                            m.next = "RD"
        m.d.comb += [pw.addr.eq(Cat(self.lp_addr, nfb)), pw.data.eq(self.lp_data), pw.en.eq(self.lp_valid)]

        # ---- frame end: n0, K, the swap ------------------------------------------------
        # one divider for n0 and then K (17 cycles each)
        m.submodules.ndiv = ndiv = SeqDivR(50, 34, 3)
        kdiv = ndiv
        nn0 = Signal(32)
        k_pending = Signal()
        with m.If(a_v & k_pending):
            m.d.sync += self.late.eq(self.late + 1)
        m.d.sync += [ndiv.start.eq(0), self.nx_valid.eq(0)]
        with m.FSM(name="noise"):
            with m.State("IDLE"):
                with m.If(self.f_end):
                    m.d.sync += k_pending.eq(1)
                    with m.If(ncnt == 0):
                        m.d.sync += nn0.eq(self.n0)
                        m.next = "K"
                    with m.Else():
                        # round(S / c) = floor((2S + c) / 2c)
                        m.d.sync += [ndiv.n.eq((nsum << 1) + ncnt), ndiv.d.eq(ncnt << 1), ndiv.start.eq(1)]
                        m.next = "N0"
                with m.Elif(self.f_start | self.f_drop):
                    m.d.sync += [nsum.eq(0), ncnt.eq(0)]
            with m.State("N0"):
                with m.If(ndiv.done):
                    m.d.sync += nn0.eq(ndiv.q[:32])
                    m.next = "K"
            with m.State("K"):
                d = Mux(nn0 < N0_MIN, N0_MIN, nn0)
                m.d.sync += [kdiv.n.eq((Const(1, 35) << (LLR_SHIFT + 1)) + d), kdiv.d.eq(d << 1),
                             kdiv.start.eq(1)]
                m.next = "KW"
            with m.State("KW"):
                with m.If(kdiv.done):
                    m.d.sync += [self.n0.eq(nn0), self.kk.eq(kdiv.q[:32]),
                                 self.prev_n0.eq(self.n0), self.prev_k.eq(self.kk), k_pending.eq(0),
                                 self.nx_valid.eq(1), self.nx_n0.eq(nn0),
                                 self.nx_k.eq(kdiv.q[:32]), nsum.eq(0), ncnt.eq(0)]
                    m.next = "IDLE"
        # a decoded frame's end: the learned points (ready since its last
        # slot's pilots) in force at once; n0 and K follow from the noise
        # division (f_end, 12 cycles later)
        with m.If(self.f_swap):
            m.d.sync += [fb.eq(~fb), self.prev_from.eq(self.learned_from), self.learned_from.eq(frame)]
        # frame start: the per-modulation slot counts zero (the slot
        # accumulators' valid bits above)
        with m.If(self.f_start):
            m.d.sync += [lr_go.eq(0), lr_done.eq(0)]
            m.d.sync += [s.eq(0) for s in slots_of]
        return m
