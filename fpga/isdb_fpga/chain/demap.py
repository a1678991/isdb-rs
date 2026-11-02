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

Implementation (one main symbol per clock):

- **Metrics**: two register sets of 32 points (re, im, |p|^2), one in use
  and one loaded for the next codeword (32 cycles from the point RAM or the
  nominal ROM, |p|^2 by two multipliers), the set selected per symbol; 64
  products (16 x 12 bits: 64 DSP48E1 at the symbol clock, the plan's upper
  figure; 16 at 4x is not done here), 32 metric adders, labels beyond
  2^bits masked; per label bit position (LSB-based) and value a 16-leaf
  min tree (10 trees, 4 levels), the overall minimum for the noise, the 5
  differences, 5 products by K (31 x 24 bits), rounding and saturation.
  Schedule from the symbol's arrival (A, the equaliser's output cycle):
  A+1 products, A+2 metrics, A+3..A+6 min trees, A+7 differences, A+8 x K,
  A+9 the beat (`o_*`: up to 5 positions and LLRs, `o_first` / `o_last`
  of the codeword, the 32-bit tag) to the decoder's port.
- **Codeword cursor**: the frame's valid slots in order, from the layout
  in force (`FrameCtl`'s second read port), the next one looked up when a
  codeword starts; its points loaded into the free register set then.
- **Pilots**: 32 slot accumulators by label (21 bits), added into the
  frame's sums (one RAM entry per modulation and label, 32 bits per
  component) after each slot's pilots; the count of a label is the slots
  of its modulation times 32 / 2^bits. After the last slot's pilots (9 240
  symbols before the frame's end) each label's round(sum / count) by a
  sequential divider into the point RAM's other bank.
- **Noise**: a 48-bit sum and a count; at a decoded frame's end n0 and K
  by two 3-bit-per-cycle dividers (about 30 cycles: the next frame's first
  main symbol reaches the K product 57 symbols later at the earliest).
  `late` counts a K not ready in time (never on a stream with the
  frame's 56 leading sync and pilot symbols, cycle count above).
- At a decoded frame's end the banks swap (the learned points and n0 are
  in force from the next frame); a dropped frame changes nothing.
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import LLR_SHIFT, N0_DEFAULT, N0_MIN, NOISE_STEP, PILOTS, SLOTS, SeqDiv, SeqDivR
from .common import round_shift, sat16, sat127
from .ldpc_if import LANES
from .tables import MOD_BITS, nominal_rom, pilot_signs

N_LDPC = 44880
INF = (1 << 31) - 1  # a masked metric (larger than any real one)


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


class Demap(Elaboratable):
    def __init__(self):
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
        self.lay_data = Signal(48)
        # the decoder's input port
        self.o_valid = Signal()
        self.o_n = Signal(range(LANES + 1))
        self.o_pos = [Signal(16, name=f"o_pos{k}") for k in range(LANES)]
        self.o_llr = [Signal(signed(8), name=f"o_llr{k}") for k in range(LANES)]
        self.o_first = Signal()
        self.o_last = Signal()
        self.o_tag = Signal(32)
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
        bitsa = Array(Const(MOD_BITS.get(i, 1), 3) for i in range(16))

        # ---- point RAM (2 banks x 8 modulations x 32 labels) and nominal ROM
        pts = Memory(shape=32, depth=512, init=[])
        m.submodules.pts = pts
        pw = pts.write_port()
        pr = pts.read_port()
        pst = pts.read_port()
        rom = nominal_rom()
        rom = rom + [(0, 0)] * (4096 - len(rom))
        nom = Memory(shape=32, depth=4096, init=[(a & 0xFFFF) | (b & 0xFFFF) << 16 for a, b in rom])
        m.submodules.nom = nom
        nr = nom.read_port()
        fb = Signal()  # the bank in force
        valid_b = [Signal(5, name=f"lvalid{b}") for b in range(2)]
        m.d.comb += [self.learned_valid.eq(Mux(fb, valid_b[1], valid_b[0])),
                     pst.addr.eq(Cat(self.st_addr, fb ^ self.st_prev)), self.st_data.eq(pst.data),
                     self.st_valid.eq(Mux(fb ^ self.st_prev, valid_b[1], valid_b[0]))]

        # ---- the two point register sets ------------------------------------
        P = [[[Signal(signed(16), name=f"p{s}_{x}_{c}") for c in range(2)] for x in range(32)] for s in range(2)]
        PP = [[Signal(signed(24), name=f"pp{s}_{x}") for x in range(32)] for s in range(2)]
        NB = [Signal(3, name=f"nb{s}") for s in range(2)]  # bits of the set's modulation

        # ---- codeword cursor --------------------------------------------------
        cw_ok = Signal()  # a codeword is current
        cw_set = Signal()
        cw_slot = Signal(7)
        cw_mode = Signal(3)
        cw_mod = Signal(4)
        cw_rate = Signal(4)
        cw_syms = Signal(15)
        sym = Signal(15)
        nx_ok = Signal()  # the next codeword found (and its points loaded)
        nx_slot = Signal(7)
        nx_mode = Signal(3)
        nx_mod = Signal(4)
        nx_rate = Signal(4)
        nx_syms = Signal(15)
        scan = Signal(range(SLOTS + 1))
        frame = Signal(32)

        # loader: label counter, from the RAM (learned) or the ROM (nominal)
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
            nr.addr.eq(Cat(ld_i[:5], (ld_rate - 1)[:4], Mux(ld_mod > 5, 0, ld_mod - 1)[:3])),
        ]
        m.d.sync += [ld_v.eq(ld_busy & (ld_i < 32)), ld_lab.eq(ld_i[:5])]
        lw = Mux(ld_learned, pr.data, nr.data)
        lre = lw[:16].as_signed()
        lim = lw[16:].as_signed()
        with m.If(ld_v):
            for s in range(2):
                for x in range(32):
                    with m.If((ld_set == s) & (ld_lab == x)):
                        m.d.sync += [P[s][x][0].eq(lre), P[s][x][1].eq(lim), PP[s][x].eq(lre * lre + lim * lim)]
        with m.If(ld_busy):
            m.d.sync += ld_i.eq(ld_i + 1)
            with m.If(ld_i == 32):
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
        l_mode, l_mod, l_rate, l_dummy, l_syms = ld[0:3], ld[3:7], ld[7:11], ld[11], ld[33:48]
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

        # ---- the metric pipeline ---------------------------------------------
        # A: arrival (main & cw_ok)
        a_v = main & cw_ok
        v1 = Signal()
        z1 = [Signal(signed(16), name=f"z1_{c}") for c in range(2)]
        s1 = Signal()
        nb1 = Signal(3)
        ctx_w = 16 + 15 + 1 + 1 + 3 + 4 + 4 + 7 + 3 + 1  # pos base etc.
        ctx = Signal(64)
        dirn = Array(Const(direction(md, rc) if 1 <= md <= 5 and 1 <= rc <= 11 else 0, 2)
                     for md in range(16) for rc in range(16))[Cat(cw_rate, cw_mod)]
        ctx_now = Cat(sym, end_cw, sym == 0, Mux(cw_set, NB[1], NB[0]), cw_mod, cw_rate, cw_slot,
                      cw_mode, dirn, (sym % NOISE_STEP) == 0)
        del ctx_w
        m.d.sync += [v1.eq(a_v), z1[0].eq(self.z[0]), z1[1].eq(self.z[1]), s1.eq(cw_set), ctx.eq(ctx_now)]
        # A+1: products
        prod = [[Signal(signed(29), name=f"zp{x}_{c}") for c in range(2)] for x in range(32)]
        ppx = [Signal(signed(24), name=f"ppx{x}") for x in range(32)]
        for x in range(32):
            pi_ = Mux(s1, P[1][x][0], P[0][x][0])
            pq_ = Mux(s1, P[1][x][1], P[0][x][1])
            m.d.sync += [prod[x][0].eq(z1[0] * pi_), prod[x][1].eq(z1[1] * pq_),
                         ppx[x].eq(Mux(s1, PP[1][x], PP[0][x]))]
        zz = Signal(32)
        m.d.sync += zz.eq(z1[0] * z1[0] + z1[1] * z1[1])
        v2 = Signal()
        ctx2 = Signal(64)
        m.d.sync += [v2.eq(v1), ctx2.eq(ctx)]
        nb2 = ctx2[17:20]
        # A+2: metrics
        met = [Signal(signed(32), name=f"met{x}") for x in range(32)]
        for x in range(32):
            ok = x < (Const(1, 6) << nb2)
            m.d.sync += met[x].eq(Mux(ok, ppx[x] - ((prod[x][0] + prod[x][1]) << 1), INF))
        v3 = Signal()
        ctx3 = Signal(64)
        zz3 = Signal(32)
        m.d.sync += [v3.eq(v2), ctx3.eq(ctx2), zz3.eq(zz)]

        # A+3 .. A+6: min trees per LSB position and value
        def mintree(vals, name):
            cur = vals
            lvl = 0
            while len(cur) > 1:
                nxt = []
                for i in range(0, len(cur), 2):
                    r = Signal(signed(32), name=f"{name}_{lvl}_{i // 2}")
                    m.d.sync += r.eq(Mux(cur[i] < cur[i + 1], cur[i], cur[i + 1]))
                    nxt.append(r)
                cur = nxt
                lvl += 1
            return cur[0], lvl

        mins = {}
        for pos in range(5):
            for v in range(2):
                mins[pos, v], depth = mintree([met[x] for x in range(32) if (x >> pos) & 1 == v],
                                              f"mn{pos}{v}")
        vp = [v3]
        cp = [ctx3]
        zp = [zz3]
        for d in range(depth):
            nv = Signal(name=f"vm{d}")
            nc = Signal(64, name=f"cm{d}")
            nz = Signal(32, name=f"zm{d}")
            m.d.sync += [nv.eq(vp[-1]), nc.eq(cp[-1]), nz.eq(zp[-1])]
            vp.append(nv)
            cp.append(nc)
            zp.append(nz)
        v7, ctx7, zz7 = vp[-1], cp[-1], zp[-1]
        # A+7: differences, the overall minimum
        dlt = [Signal(signed(33), name=f"dlt{p}") for p in range(5)]
        best = Signal(signed(32))
        m.d.sync += [dlt[p].eq(mins[p, 1] - mins[p, 0]) for p in range(5)]
        m.d.sync += best.eq(Mux(mins[0, 0] < mins[0, 1], mins[0, 0], mins[0, 1]))
        v8 = Signal()
        ctx8 = Signal(64)
        zz8 = Signal(32)
        m.d.sync += [v8.eq(v7), ctx8.eq(ctx7), zz8.eq(zz7)]
        # A+8: x K
        dk = [Signal(signed(58), name=f"dk{p}") for p in range(5)]
        m.d.sync += [dk[p].eq(dlt[p] * self.kk[:25]) for p in range(5)]
        v9 = Signal()
        ctx9 = Signal(64)
        m.d.sync += [v9.eq(v8), ctx9.eq(ctx8)]
        # noise sample at A+8
        nsum = Signal(48)
        ncnt = Signal(20)
        with m.If(v8 & ctx8[40]):
            m.d.sync += [nsum.eq(nsum + zz8 + best), ncnt.eq(ncnt + 1)]
        # A+9: the beat
        s9 = ctx9[0:15]
        nb9 = ctx9[17:20]
        mod9 = ctx9[20:24]
        rate9 = ctx9[24:28]
        slot9 = ctx9[28:35]
        mode9 = ctx9[35:38]
        dir9 = ctx9[38:40]
        rows = Array(Const(N_LDPC // b if b else 0, 16) for b in range(8))[nb9]
        m.d.sync += [self.o_valid.eq(v9), self.o_n.eq(nb9), self.o_last.eq(ctx9[15]), self.o_first.eq(ctx9[16]),
                     self.o_tag.eq(Cat(slot9, Const(0, 1), frame[:8], rate9, mod9, mode9)),
                     self.o_rate.eq(rate9)]
        llr_pos = [sat127(round_shift(dk[p], LLR_SHIFT)) for p in range(5)]
        for b in range(5):
            # label bit b (MSB first) is LSB position bits - 1 - b
            pos_lsb = (nb9 - 1 - b)[:3]
            llr = Array(llr_pos)[pos_lsb]
            col = Mux(dir9 == 2, nb9 - 1 - b, b)[:3]
            p_int = col * rows + s9
            p_id = (s9 * nb9 + b)[:16]
            m.d.sync += [self.o_llr[b].eq(Mux(b < nb9, llr, 0)),
                         self.o_pos[b].eq(Mux(b < nb9, Mux(dir9 == 0, p_id, p_int[:16]), 0))]
        # these products are by constants in practice (rows per modulation)

        # ---- pilots -------------------------------------------------------------
        signs = [1 if s < 0 else 0 for s in pilot_signs()]
        srom = Memory(shape=1, depth=4096, init=signs + [0] * (4096 - len(signs)))
        m.submodules.signs = srom
        sp = srom.read_port(domain="comb")
        m.d.comb += sp.addr.eq(self.k * PILOTS + self.t)
        neg = sp.data[0]

        def sneg(v):
            return Mux(neg, Mux(v == -32768, 32767, -v), v)

        pv = self.valid & self.is_pilot
        nbits = bitsa[self.slot_mod]
        lab = self.t & ((Const(1, 6) << nbits) - 1)[:5]
        sacc = [[Signal(signed(24), name=f"sacc{x}_{c}") for c in range(2)] for x in range(32)]
        with m.If(pv):
            for x in range(32):
                with m.If(lab == x):
                    m.d.sync += [sacc[x][0].eq(sacc[x][0] + sneg(self.z[0])),
                                 sacc[x][1].eq(sacc[x][1] + sneg(self.z[1]))]
        # the slot's pilots into the frame sums
        fs = Memory(shape=64, depth=256, init=[])
        m.submodules.fsums = fs
        fw = fs.write_port()
        fr_ = fs.read_port()
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
        m.d.comb += fr_.addr.eq(Cat(fl_i[:5], (fl_mod - 1)[:3]))
        m.d.sync += [fl_v.eq(fl_busy & (fl_i < fl_n)), fl_lab.eq(fl_i[:5])]
        with m.If(fl_busy & ~last_pilot):
            m.d.sync += fl_i.eq(fl_i + 1)
            with m.If(fl_i == fl_n):
                m.d.sync += fl_busy.eq(0)
        sa_re = Array(s[0] for s in sacc)[fl_lab]
        sa_im = Array(s[1] for s in sacc)[fl_lab]
        old_re = fr_.data[:32].as_signed()
        old_im = fr_.data[32:].as_signed()
        # the first slot of a modulation in the frame writes its sums (no clearing pass)
        m.d.comb += [fw.addr.eq(Cat(fl_lab, (fl_mod - 1)[:3])),
                     fw.data.eq(Cat(Mux(fl_first, sa_re, old_re + sa_re)[:32], Mux(fl_first, sa_im, old_im + sa_im)[:32])),
                     fw.en.eq(fl_v)]
        with m.If(fl_v):
            for x in range(32):
                with m.If(fl_lab == x):
                    m.d.sync += [sacc[x][0].eq(0), sacc[x][1].eq(0)]

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
        m.d.comb += cnt_l.eq(Array(slots_of)[lr_mod] * (Array(Const(32 >> (i + 1), 6) for i in range(8))[lr_mod]))
        rd2 = fs.read_port()
        m.d.comb += rd2.addr.eq(Cat(lr_lab[:5], lr_mod))
        num = Signal(signed(36))
        anum = Signal(36)
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
        m.submodules.ndiv = ndiv = SeqDivR(50, 20, 3)
        m.submodules.kdiv = kdiv = SeqDivR(35, 34, 3)
        nn0 = Signal(32)
        k_pending = Signal()
        with m.If(a_v & k_pending):
            m.d.sync += self.late.eq(self.late + 1)
        m.d.sync += [ndiv.start.eq(0), kdiv.start.eq(0), self.nx_valid.eq(0)]
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
        # frame start: the frame sums cleared, the per-modulation slot counts zero
        with m.If(self.f_start):
            m.d.sync += [lr_go.eq(0), lr_done.eq(0)]
            m.d.sync += [s.eq(0) for s in slots_of]
            m.d.sync += [sacc[x][c].eq(0) for x in range(32) for c in range(2)]
        return m
