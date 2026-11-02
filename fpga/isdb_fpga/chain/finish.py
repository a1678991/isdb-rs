"""Stage 8 of the ISDB-S3 chain: the slot dispersal and the BCH decoder
(the TMCC's BCH through the same unit).

Contract: `isdbs3_phy::main_signal::finish_frame` (stage 8 of the chain,
module docs "Stage 8") and `isdb_fec::bch::decode` (gf.py is its Python
port, the unit tests' reference):

- the slot PRBS X^25 + X^22 + 1 from `1010000000000000000011010` (last
  stage out) runs across the frame's valid slots in slot order, k bits per
  slot (header, data, BCH parity, stuffing; LDPC-failed slots too):
  `SlotPrbs`, 16 bits per step;
- BCH on the first k - 6 bits of a converged slot (header + data + 192
  parity), on the 9 614 TMCC data bits after the TMCC dispersal: syndromes
  S_1 .. S_24 of r(x) = sum word[j] x^(n-1-j), Berlekamp-Massey, the Chien
  search over the n positions present; the result is the number of roots
  when it equals the locator's degree L <= 12, else uncorrectable. The
  roots found are flipped in the word whatever the result (the Rust
  decoder corrects in place before it compares the count; the chain's
  slot blocks carry those bits, and its TMCC block the uncorrected ones).

`BchUnit` schedule (one unit for the frame's slots and its TMCC):

- **syndromes**, 16 bits per clock as the word streams in: S_i <- S_i
  alpha^(16 i) + sum_b bit_b alpha^((15 - b) i) for the 12 odd i (one XOR
  network each), the even ones by squaring (S_2i = S_i^2) after the last
  beat. The word must end on a beat: the caller sends z = (-n) mod 16
  leading zeros (they do not change r(x));
- if every syndrome is 0: done (corrected 0) one cycle later;
- **Berlekamp-Massey**, 24 steps on one GF(2^16) multiplier (combinational,
  256 AND + XOR reduction): the discrepancy (L products), the update (up to
  13 products), and at a length change the inverse of the new b by
  a^(2^16 - 2) (15 squarings and products): at most about 1 300 cycles;
  L > 12 at any step fails at once (the model's final check, exact since L
  never decreases);
- **Chien**: R_i = c_i alpha^(-(n-1) i) (alpha^(-(n-1)) from a ROM of the
  word lengths used, 24 products), then one position per clock j = 0 .. n -
  1: root where 1 + sum R_i = 0, R_i <- R_i alpha^i (12 constant
  networks); the first 12 roots are kept (more would be more than L: a
  failure). At most 44 880 cycles, only for a word with errors.

Throughput: a slot word's syndromes take n / 16 cycles (2 548 for 93/120);
the Chien search, when needed, n cycles: at 40 MHz a frame of 96 slots all
with errors would need 3.9 M cycles of Chien, more than a frame (1.1 M
symbols); on the lab signal no slot has needed BCH after a converged LDPC
(0 corrections, plan "Results"). A clock of 4x, or 4 positions per clock,
covers the worst case (open question in fpga/README.md).

`o_mask` (of `q_beat`): the 16-bit flip mask of beat q_beat of the word
in position order (bit b = word[16 q + b]; not the aligned input beats)
from the kept roots: 12 comparators.
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal

from . import gf
from .tables import SLOT_INIT, lfsr_state

GF_BITS = 16


def gf_const_mul(x, c):
    """x * c for a constant c: an XOR network (gf.matrix)."""
    cols = gf.matrix(c)
    bits = []
    for o in range(GF_BITS):
        terms = [x[k] for k in range(GF_BITS) if cols[k] >> o & 1]
        acc = Const(0, 1)
        for t in terms:
            acc = acc ^ t
        bits.append(acc)
    return Cat(*bits)


def gf_square(x):
    cols = gf.square_matrix()
    bits = []
    for o in range(GF_BITS):
        acc = Const(0, 1)
        for k in range(GF_BITS):
            if cols[k] >> o & 1:
                acc = acc ^ x[k]
        bits.append(acc)
    return Cat(*bits)


def gf_mul(a, b):
    """a * b in GF(2^16): the carry-less product and its reduction."""
    prod = [Const(0, 1)] * (2 * GF_BITS - 1)
    for i in range(GF_BITS):
        for j in range(GF_BITS):
            prod[i + j] = prod[i + j] ^ (a[i] & b[j])
    out = list(prod[:GF_BITS])
    for k in range(GF_BITS, 2 * GF_BITS - 1):
        red = gf.mul(1 << (k - GF_BITS), 1 << GF_BITS) if False else _xpow(k)
        for o in range(GF_BITS):
            if red >> o & 1:
                out[o] = out[o] ^ prod[k]
    return Cat(*out)


def _xpow(k):
    """x^k mod the field polynomial."""
    v = 1
    for _ in range(k):
        v <<= 1
        if v & (1 << 16):
            v ^= gf.POLY
    return v


def beat_terms(i):
    """For syndrome S_i: the constants alpha^(16 i) and alpha^((15 - b) i)."""
    return gf.alpha(16 * i), [gf.alpha((15 - b) * i) for b in range(16)]


class BchUnit(Elaboratable):
    def __init__(self, lengths):
        self.lengths = sorted(set(lengths))
        self.start = Signal()
        self.n_sel = Signal(range(len(self.lengths)))  # index of the word length
        self.i_valid = Signal()
        self.i_bits = Signal(16)
        self.i_last = Signal()
        self.done = Signal()
        self.ok = Signal()
        self.corrected = Signal(range(gf.T + 1))
        self.nroots = Signal(range(gf.T + 2))
        self.roots = [Signal(16, name=f"root{k}") for k in range(gf.T)]
        self.busy = Signal()
        self.q_beat = Signal(12)
        self.o_mask = Signal(16)

    def elaborate(self, platform):
        m = Module()
        T = gf.T
        ns = Array(Const(n, 16) for n in self.lengths)
        n = Signal(16)
        z = Signal(4)
        s = [Signal(16, name=f"s{i}") for i in range(2 * T + 1)]  # s[1..24]
        # ---- syndromes ------------------------------------------------------
        with m.If(self.start):
            m.d.sync += [s[i].eq(0) for i in range(1, 2 * T + 1)]
            m.d.sync += [n.eq(ns[self.n_sel]), z.eq((-ns[self.n_sel])[:4])]
        with m.Elif(self.i_valid):
            for i in range(1, 2 * T + 1, 2):
                c16, cb = beat_terms(i)
                acc = gf_const_mul(s[i], c16)
                for b in range(16):
                    acc = acc ^ Mux(self.i_bits[b], Const(cb[b], 16), 0)
                m.d.sync += s[i].eq(acc)
        # ---- BM / Chien --------------------------------------------------------
        c = [Signal(16, name=f"c{i}") for i in range(T + 1)]
        bp = [Signal(16, name=f"bp{i}") for i in range(T + 1)]
        tc = [Signal(16, name=f"tc{i}") for i in range(T + 1)]
        ll = Signal(range(2 * T + 2))
        mm = Signal(range(2 * T + 2))
        b = Signal(16)
        binv = Signal(16)
        k = Signal(range(2 * T + 1))
        i = Signal(range(2 * T + 2))
        d = Signal(16)
        coef = Signal(16)
        ma = Signal(16)
        mb = Signal(16)
        prod = Signal(16)
        m.d.comb += prod.eq(gf_mul(ma, mb))
        sa = Array(s)
        ca = Array(c)
        bpa = Array(bp)
        xsq = Signal(16)
        r_inv = Signal(16)
        cnt = Signal(5)
        R = [Signal(16, name=f"R{i}") for i in range(T + 1)]
        apow = Signal(16)
        a_n = Array(Const(gf.inv(gf.alpha(nn - 1)), 16) for nn in self.lengths)
        nsel = Signal(range(len(self.lengths)))
        jpos = Signal(16)
        nroots = self.nroots
        rootsa = Array(self.roots)
        m.d.sync += self.done.eq(0)
        with m.If(self.start):
            m.d.sync += nsel.eq(self.n_sel)
        with m.FSM(name="bch"):
            with m.State("IN"):
                with m.If(self.i_valid & self.i_last):
                    m.d.sync += [i.eq(1), self.busy.eq(1)]
                    m.next = "EVEN"
            with m.State("EVEN"):
                # S_2i = S_i^2 for 2i = 2, 4, ... 24, in order
                for e in range(2, 2 * T + 1, 2):
                    with m.If(i == e // 2):
                        m.d.sync += s[e].eq(gf_square(s[e // 2]))
                m.d.sync += i.eq(i + 1)
                with m.If(i == T):
                    m.next = "CHECK"
            with m.State("CHECK"):
                anyv = Cat(*[s[x] != 0 for x in range(1, 2 * T + 1)]).any()
                with m.If(~anyv):
                    m.d.sync += [self.ok.eq(1), self.corrected.eq(0), nroots.eq(0), self.done.eq(1),
                                 self.busy.eq(0)]
                    m.next = "IN"
                with m.Else():
                    m.d.sync += [c[0].eq(1), bp[0].eq(1), ll.eq(0), mm.eq(1), b.eq(1), binv.eq(1),
                                 k.eq(0)]
                    m.d.sync += [c[x].eq(0) for x in range(1, T + 1)]
                    m.d.sync += [bp[x].eq(0) for x in range(1, T + 1)]
                    m.next = "BM_D0"
            with m.State("BM_D0"):
                m.d.sync += [d.eq(sa[k + 1]), i.eq(1)]
                m.next = "BM_D"
            with m.State("BM_D"):
                # d ^= c_i s_{k+1-i}, i = 1 .. L
                with m.If(i <= ll):
                    m.d.comb += [ma.eq(ca[i[:4]]), mb.eq(sa[(k + 1 - i)[:5]])]
                    m.d.sync += [d.eq(d ^ prod), i.eq(i + 1)]
                with m.Else():
                    m.next = "BM_E"
            with m.State("BM_E"):
                with m.If(d == 0):
                    m.d.sync += mm.eq(mm + 1)
                    m.next = "BM_NEXT"
                with m.Else():
                    m.d.comb += [ma.eq(d), mb.eq(binv)]
                    m.d.sync += [coef.eq(prod), i.eq(0)]
                    m.d.sync += [tc[x].eq(c[x]) for x in range(T + 1)]
                    m.next = "BM_U"
            with m.State("BM_U"):
                # c[i + m] ^= coef bp[i] for i + m <= 12
                with m.If(i + mm <= T):
                    m.d.comb += [ma.eq(coef), mb.eq(bpa[i[:4]])]
                    for x in range(T + 1):
                        with m.If(i + mm == x):
                            m.d.sync += c[x].eq(c[x] ^ prod)
                    m.d.sync += i.eq(i + 1)
                with m.Else():
                    with m.If(2 * ll <= k):
                        nl = k + 1 - ll
                        m.d.sync += [ll.eq(nl), b.eq(d), mm.eq(1)]
                        m.d.sync += [bp[x].eq(tc[x]) for x in range(T + 1)]
                        with m.If(nl > T):
                            m.next = "FAIL"
                        with m.Else():
                            # binv = d^(2^16 - 2)
                            m.d.sync += [xsq.eq(d), r_inv.eq(1), cnt.eq(0)]
                            m.next = "INV"
                    with m.Else():
                        m.d.sync += mm.eq(mm + 1)
                        m.next = "BM_NEXT"
            with m.State("INV"):
                sq = gf_square(xsq)
                m.d.comb += [ma.eq(r_inv), mb.eq(sq)]
                m.d.sync += [xsq.eq(sq), r_inv.eq(prod), cnt.eq(cnt + 1)]
                with m.If(cnt == 14):
                    m.d.sync += binv.eq(prod)
                    m.next = "BM_NEXT"
            with m.State("BM_NEXT"):
                m.d.sync += k.eq(k + 1)
                with m.If(k == 2 * T - 1):
                    m.d.sync += [i.eq(1), apow.eq(a_n[nsel])]
                    m.next = "CH_INIT"
                with m.Else():
                    m.next = "BM_D0"
            with m.State("CH_INIT"):
                # R_i = c_i a^i with a = alpha^-(n-1); apow holds a^i
                with m.If(i <= T):
                    m.d.comb += [ma.eq(ca[i[:4]]), mb.eq(apow)]
                    for x in range(1, T + 1):
                        with m.If(i == x):
                            m.d.sync += R[x].eq(Mux(i <= ll, prod, 0))
                    m.d.sync += i.eq(i + 1)
                    m.next = "CH_POW"
                with m.Else():
                    m.d.sync += [jpos.eq(0), nroots.eq(0)]
                    m.next = "CHIEN"
            with m.State("CH_POW"):
                m.d.comb += [ma.eq(apow), mb.eq(a_n[nsel])]
                m.d.sync += apow.eq(prod)
                m.next = "CH_INIT"
            with m.State("CHIEN"):
                v = Const(1, 16)
                for x in range(1, T + 1):
                    v = v ^ R[x]
                with m.If(v == 0):
                    with m.If(nroots < T):
                        m.d.sync += rootsa[nroots[:4]].eq(jpos)
                    m.d.sync += nroots.eq(Mux(nroots == T + 1, T + 1, nroots + 1))
                for x in range(1, T + 1):
                    m.d.sync += R[x].eq(gf_const_mul(R[x], gf.alpha(x)))
                m.d.sync += jpos.eq(jpos + 1)
                with m.If(jpos == n - 1):
                    m.next = "RESULT"
            with m.State("RESULT"):
                ok = nroots == ll
                m.d.sync += [self.ok.eq(ok), self.corrected.eq(Mux(ok, ll, 0)), self.done.eq(1),
                             self.busy.eq(0)]
                m.next = "IN"
            with m.State("FAIL"):
                m.d.sync += [self.ok.eq(0), self.corrected.eq(0), nroots.eq(0), self.done.eq(1),
                             self.busy.eq(0)]
                m.next = "IN"
        # flip mask of an aligned beat
        mask = Const(0, 16)
        for x in range(T):
            aj = self.roots[x]
            hit = (x < nroots) & (aj[4:] == self.q_beat)
            mask = mask | Mux(hit, Const(1, 16) << aj[:4], 0)
        m.d.comb += self.o_mask.eq(mask)
        return m


class SlotPrbs(Elaboratable):
    """The slot dispersal PRBS (X^25 + X^22 + 1, last stage out), 16 output
    bits per `step` (bit b = the b-th bit in order), or 1..16 with `count`
    (the last beat of a slot: the generator advances by `count` only).
    `reset` at a frame's start."""

    def __init__(self):
        self.reset = Signal()
        self.step = Signal()
        self.count = Signal(5, init=16)
        self.bits = Signal(16)

    def elaborate(self, platform):
        m = Module()
        reg = Signal(25, init=lfsr_state(SLOT_INIT))
        # unroll 16 steps symbolically: states after s steps
        states = [reg]
        outs = []
        cur = [reg[i] for i in range(25)]
        after = [cur]
        for _ in range(16):
            outs.append(cur[24])
            fb = cur[21] ^ cur[24]
            cur = [fb] + cur[:24]
            after.append(cur)
        m.d.comb += self.bits.eq(Cat(*outs))
        with m.If(self.reset):
            m.d.sync += reg.eq(lfsr_state(SLOT_INIT))
        with m.Elif(self.step):
            with m.Switch(self.count):
                for cnum in range(1, 17):
                    with m.Case(cnum):
                        m.d.sync += reg.eq(Cat(*after[cnum]))
        del states
        return m


RATES = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109]
TMCC_N = 9614
BCH_LENGTHS = sorted({374 * r - 6 for r in RATES} | {TMCC_N})


class Finish(Elaboratable):
    """The result side of stages 7-9: the decoder's results (`r_*`, the
    LDPC port's out stream) into `Slots` blocks through the packer, in the
    model's order: per whole frame its slots (decoded frames: all 120, the
    dummies included), its TMCC, its status.

    - **Frames** (`f_*`): at each whole frame's end the back end pushes
      (frame, decoded) and a status snapshot (`snap_*`, `packer.status_layout`
      less the counters, which are this stage's). One snapshot register: a
      frame's status leaves before the next frame ends (its TMCC result
      comes within the next frame).
    - **Slot results** (tag bit 7 clear): the slot PRBS (reset at a frame's
      first slot result) over the first k positions, k = 374 r (an overrun
      codeword, no words, advances it by k too and carries zeros); the
      words into a 2 560-word buffer and, realigned, the first k - 6 bits
      into the BCH unit (converged words only); then the slot block: w0, w1,
      w2 and the 176 + D header and data bits with the Chien roots flipped
      (whatever the result, as the model). Before a valid slot k the dummy
      blocks of the slots since the last valid one (mode, modulation and
      rate of that valid slot: the dummies of a unit follow its valid
      slots); after the frame's last valid slot, at the TMCC result, the
      rest up to slot 119.
    - **TMCC results** (tag bit 7): `TmccResult` (dispersal, BCH, layout),
      the TMCC block (BCH, iterations, the frame its layout applies from,
      the 9 422 bits), the counters, then the status block (`StatusSource`).
    - **Order**: the blocks are the model's when the results come in frame
      order (a frame's slot results, then its TMCC result). The decoder
      returns them in commit order, in which frame N's TMCC (its burst ends
      about 3 slots into frame N + 1) follows frame N + 1's first 2 or 3
      slot results: reproducing the model's order then needs those slot
      results held (about 3 x 2 548 words) until frame N's status is out.
      Not built: fpga/README.md "The ISDB-S3 chain", what is left. The
      chain-6 simulation's LDPC stub answers in frame order.
    """

    def __init__(self, lag=2):
        from .packer import Packer, StatusSource
        from .tmcc import TmccResult

        self.tmcc = TmccResult(lag)
        self.bch = BchUnit(BCH_LENGTHS)
        self.prbs = SlotPrbs()
        self.packer = Packer()
        self.status = StatusSource()
        self.lag = lag
        # results
        self.r_valid = Signal()
        self.r_ready = Signal()
        self.r_head = Signal()
        self.r_tag = Signal(64)
        self.r_iterations = Signal(8)
        self.r_converged = Signal()
        self.r_overrun = Signal()
        self.r_bits = Signal(16)
        self.r_last = Signal()
        # frames
        self.f_valid = Signal()
        self.f_frame = Signal(32)
        self.f_decoded = Signal()
        # the demapper's point RAM for the status (bank select: the frame's)
        self.pt_prev = Signal()
        # counters
        names = ("frames", "undecoded", "tmcc_failed", "slots", "ldpc_failed", "bch_failed")
        self.counters = {n: Signal(32, name=f"cnt_{n}") for n in names}
        self.iterations = Signal(64)
        self.o_valid = self.packer.o_valid
        self.o_word = self.packer.o_word

    def elaborate(self, platform):
        from amaranth.lib.fifo import SyncFIFO
        from amaranth.lib.memory import Memory

        from .tmcc import Realign

        m = Module()
        m.submodules.tmcc = tr = self.tmcc
        m.submodules.bch = bch = self.bch
        m.submodules.prbs = prbs = self.prbs
        m.submodules.packer = pk = self.packer
        m.submodules.status = st = self.status
        m.submodules.al = al = Realign()
        m.submodules.frames = ff = SyncFIFO(width=33, depth=4)
        cnt = self.counters
        m.d.comb += [ff.w_data.eq(Cat(self.f_frame, self.f_decoded)), ff.w_en.eq(self.f_valid)]
        fr_frame = ff.r_data[:32]
        fr_dec = ff.r_data[32]

        tag = self.r_tag
        is_tmcc = tag[7]
        slot = tag[0:7]
        rate = tag[8:12]
        mod = tag[12:16]
        mode = tag[16:19]
        tframe = tag[32:64]
        k_of = Array(Const(374 * RATES[i - 1] if 1 <= i <= 11 else 0, 16) for i in range(16))
        nsel_of = Array(Const(BCH_LENGTHS.index(374 * RATES[i - 1] - 6) if 1 <= i <= 11 else 0,
                              range(len(BCH_LENGTHS))) for i in range(16))
        tmcc_sel = BCH_LENGTHS.index(TMCC_N)

        # the slot in hand
        s_slot = Signal(7)
        s_mode = Signal(3)
        s_mod = Signal(4)
        s_rate = Signal(4)
        s_it = Signal(8)
        s_conv = Signal()
        s_k = Signal(16)
        s_q = Signal(12)
        s_run = Signal()  # the BCH runs on this slot
        last_valid = Signal(8, init=0xFF)  # the last slot emitted (0xFF: none in this frame)
        d_mode = Signal(3)
        d_mod = Signal(4)
        d_rate = Signal(4)
        dslot = Signal(8)
        dend = Signal(8)
        prev_frame = Signal(32)
        have_prev = Signal()

        buf = Memory(shape=16, depth=2560, init=[])
        m.submodules.buf = buf
        bw = buf.write_port()
        brd = buf.read_port()
        remaining = Signal(17)
        cntw = Signal(5)
        m.d.comb += cntw.eq(Mux(remaining >= 16, 16, remaining))
        dis = self.r_bits ^ prbs.bits
        keep = ((Const(1, 17) << cntw) - 1)[:16]

        # BCH sharing: the slot path or TmccResult
        tmode = Signal()
        slot_start = Signal()
        m.d.comb += [
            bch.start.eq(Mux(tmode, tr.bch_start, slot_start)),
            bch.n_sel.eq(Mux(tmode, tmcc_sel, nsel_of[rate])),
            bch.i_valid.eq(Mux(tmode, tr.bch_valid, al.o_valid & s_run)),
            bch.i_bits.eq(Mux(tmode, tr.bch_bits, al.o_bits)),
            bch.i_last.eq(Mux(tmode, tr.bch_last, al.o_last)),
            tr.bch_done.eq(bch.done & tmode), tr.bch_ok.eq(bch.ok), tr.bch_corrected.eq(bch.corrected),
            tr.bch_mask.eq(bch.o_mask),
            al.skip.eq(0), al.take.eq(k_of[rate] - 6), al.lead.eq((-(k_of[rate] - 6))[:4]),
            al.start.eq(slot_start),
            al.i_bits.eq(dis & keep), al.i_q.eq(s_q),
            tr.r_bits.eq(self.r_bits), tr.r_last.eq(self.r_last), tr.r_head.eq(self.r_head),
            tr.r_iterations.eq(self.r_iterations), tr.r_converged.eq(self.r_converged & ~self.r_overrun),
            tr.frame.eq(fr_frame),
        ]
        q_out = Signal(12)
        m.d.comb += bch.q_beat.eq(Mux(tmode, tr.bch_q, q_out))
        bch_done_l = Signal()
        bch_ok_l = Signal()
        bch_cor_l = Signal(4)
        with m.If(bch.done & ~tmode):
            m.d.sync += [bch_done_l.eq(1), bch_ok_l.eq(bch.ok), bch_cor_l.eq(bch.corrected)]

        m.d.sync += [pk.b_valid.eq(0), st.begin.eq(0), st.next.eq(0)]
        m.d.comb += [prbs.step.eq(0), prbs.count.eq(cntw), bw.en.eq(0), al.i_valid.eq(0),
                     tr.r_valid.eq(0), tr.o_go.eq(0)]
        nb = Signal(12)  # payload bit words of the slot
        hd_bits = Signal(17)
        word_i = Signal(12)
        tw = Signal(2)
        tmcc_hdr = Signal(64)
        pace = Signal()

        def block(kind, frame, length):
            m.d.sync += [pk.b_valid.eq(1), pk.b_kind.eq(kind), pk.b_frame.eq(frame), pk.b_len.eq(length)]

        s_frame = Signal(32)
        frame_of_slot = s_frame
        m.d.comb += pk.w_valid.eq(0)
        with m.FSM(name="finish"):
            with m.State("IDLE"):
                with m.If(self.r_valid & self.r_head & is_tmcc & ff.r_rdy):
                    m.d.sync += [tmode.eq(1), dslot.eq(Mux(have_prev & (prev_frame == fr_frame), last_valid + 1, 0)),
                                 dend.eq(Mux(fr_dec, 120, 0))]
                    m.next = "TMCC_DUMMIES"
                with m.Elif(self.r_valid & self.r_head & ~is_tmcc):
                    new_frame = ~have_prev | (prev_frame != tframe)
                    with m.If(new_frame):
                        m.d.sync += [have_prev.eq(1), prev_frame.eq(tframe)]
                        m.d.comb += prbs.reset.eq(1)
                    m.d.comb += slot_start.eq(1)
                    with m.If(1):
                        m.d.sync += [s_slot.eq(slot), s_mode.eq(mode), s_mod.eq(mod), s_rate.eq(rate),
                                     s_it.eq(self.r_iterations), s_conv.eq(self.r_converged & ~self.r_overrun),
                                     s_k.eq(k_of[rate]), remaining.eq(k_of[rate]), s_q.eq(0),
                                     s_run.eq(self.r_converged & ~self.r_overrun), bch_done_l.eq(0),
                                     dslot.eq(Mux(new_frame, 0, last_valid + 1)), s_frame.eq(tframe)]
                        m.d.comb += self.r_ready.eq(1)
                        with m.If(self.r_overrun):
                            m.next = "OVERRUN"
                        with m.Else():
                            m.next = "WORDS"
            with m.State("WORDS"):
                m.d.comb += self.r_ready.eq(1)
                with m.If(self.r_valid):
                    with m.If(remaining > 0):
                        m.d.comb += [prbs.step.eq(1), bw.addr.eq(s_q), bw.data.eq(dis & keep), bw.en.eq(1),
                                     al.i_valid.eq(1)]
                        m.d.sync += [remaining.eq(remaining - cntw), s_q.eq(s_q + 1)]
                    with m.If(self.r_last):
                        m.next = "BCH"
            with m.State("OVERRUN"):
                with m.If(remaining > 0):
                    m.d.comb += [prbs.step.eq(1), bw.addr.eq(s_q), bw.data.eq(0), bw.en.eq(1)]
                    m.d.sync += [remaining.eq(remaining - cntw), s_q.eq(s_q + 1)]
                with m.Else():
                    m.next = "BCH"
            with m.State("BCH"):
                with m.If(~s_run | bch_done_l):
                    m.next = "DUMMIES"
            with m.State("DUMMIES"):
                # the dummy slots before this one
                with m.If(dslot < s_slot):
                    with m.If(pk.b_ready & ~pk.b_valid):
                        block(1, frame_of_slot, 3)
                        m.d.sync += word_i.eq(0)
                        m.next = "DUMMY_W"
                with m.Else():
                    with m.If(pk.b_ready & ~pk.b_valid):
                        hb = s_k - 198
                        m.d.sync += [hd_bits.eq(hb), nb.eq((hb + 15) >> 4), word_i.eq(0), q_out.eq(0)]
                        block(1, frame_of_slot, 3 + ((hb + 15) >> 4))
                        m.next = "SLOT_W"
            with m.State("DUMMY_W"):
                ws = Array([Cat(dslot[:8], d_mode, Const(1, 1), Const(0, 4)), Cat(d_mod, d_rate, Const(0, 8)),
                            Const(0xFFFF, 16)])
                m.d.comb += [pk.w_valid.eq(1), pk.w_data.eq(ws[word_i[:2]])]
                with m.If(pk.w_ready):
                    m.d.sync += word_i.eq(word_i + 1)
                    with m.If(word_i == 2):
                        m.d.sync += dslot.eq(dslot + 1)
                        m.next = "DUMMIES"
            with m.State("SLOT_W"):
                bok = s_run & bch_ok_l
                w0 = Cat(s_slot, Const(0, 1), s_mode, Const(0, 1), s_conv, bok, Const(0, 2))
                w1 = Cat(s_mod, s_rate, s_it)
                w2 = Mux(bok, bch_cor_l, 0xFFFF)
                m.d.comb += brd.addr.eq(q_out)
                with m.If(word_i < 3):
                    m.d.comb += [pk.w_valid.eq(1), pk.w_data.eq(Array([w0, w1, w2])[word_i[:2]])]
                    with m.If(pk.w_ready):
                        m.d.sync += word_i.eq(word_i + 1)
                        with m.If(word_i == 2):
                            m.d.sync += pace.eq(0)
                with m.Else():
                    # two clocks per word: the buffer read, then the word
                    with m.If(~pace):
                        m.d.sync += pace.eq(1)
                    with m.Else():
                        q = word_i - 3
                        lastw = q == nb - 1
                        rem = hd_bits[:4]
                        v = brd.data ^ Mux(s_run, bch.o_mask, 0)
                        v = Mux(lastw & (rem != 0), v & ((Const(1, 17) << rem) - 1)[:16], v)
                        m.d.comb += [bch.q_beat.eq(q), pk.w_valid.eq(1), pk.w_data.eq(v)]
                        with m.If(pk.w_ready):
                            m.d.sync += [word_i.eq(word_i + 1), q_out.eq(q_out + 1), pace.eq(0)]
                            with m.If(lastw):
                                m.next = "SLOT_DONE"
            with m.State("SLOT_DONE"):
                m.d.sync += [last_valid.eq(s_slot), d_mode.eq(s_mode), d_mod.eq(s_mod), d_rate.eq(s_rate),
                             cnt["slots"].eq(cnt["slots"] + 1)]
                with m.If(~s_conv):
                    m.d.sync += cnt["ldpc_failed"].eq(cnt["ldpc_failed"] + 1)
                with m.Else():
                    m.d.sync += self.iterations.eq(self.iterations + s_it)
                    with m.If(~bch_ok_l):
                        m.d.sync += cnt["bch_failed"].eq(cnt["bch_failed"] + 1)
                m.next = "IDLE"
            # ---- the TMCC result: the frame's remaining dummies, then its blocks
            with m.State("TMCC_DUMMIES"):
                with m.If(dslot < dend):
                    with m.If(pk.b_ready & ~pk.b_valid):
                        block(1, fr_frame, 3)
                        m.d.sync += word_i.eq(0)
                        m.next = "TDUMMY_W"
                with m.Else():
                    m.next = "TMCC_RUN"
            with m.State("TDUMMY_W"):
                ws = Array([Cat(dslot[:8], d_mode, Const(1, 1), Const(0, 4)), Cat(d_mod, d_rate, Const(0, 8)),
                            Const(0xFFFF, 16)])
                m.d.comb += [pk.w_valid.eq(1), pk.w_data.eq(ws[word_i[:2]])]
                with m.If(pk.w_ready):
                    m.d.sync += word_i.eq(word_i + 1)
                    with m.If(word_i == 2):
                        m.d.sync += dslot.eq(dslot + 1)
                        m.next = "TMCC_DUMMIES"
            with m.State("TMCC_RUN"):
                # TmccResult consumes the result stream
                m.d.comb += [tr.r_valid.eq(self.r_valid), self.r_ready.eq(tr.r_ready)]
                with m.If(tr.o_head):
                    bok = tr.o_bch_ok
                    m.d.sync += tmcc_hdr.eq(Cat(Mux(bok, tr.o_bch, 0xFFFF)[:16],
                                                Mux(tr.o_conv, tr.o_iter, 0xFFFF)[:16],
                                                (fr_frame + self.lag)[:32]))
                    m.d.sync += [cnt["frames"].eq(cnt["frames"] + 1)]
                    with m.If(~bok):
                        m.d.sync += cnt["tmcc_failed"].eq(cnt["tmcc_failed"] + 1)
                    with m.If(~fr_dec):
                        m.d.sync += cnt["undecoded"].eq(cnt["undecoded"] + 1)
                    m.next = "TMCC_HDR"
            with m.State("TMCC_HDR"):
                with m.If(pk.b_ready & ~pk.b_valid):
                    block(2, fr_frame, 4 + 589)
                    m.d.sync += word_i.eq(0)
                    m.next = "TMCC_W"
            with m.State("TMCC_W"):
                m.d.comb += [pk.w_valid.eq(1), pk.w_data.eq(Array([tmcc_hdr[16 * i:16 * i + 16] for i in range(4)])[word_i[:2]])]
                with m.If(pk.w_ready):
                    m.d.sync += word_i.eq(word_i + 1)
                    with m.If(word_i == 3):
                        m.next = "TMCC_GO"
            with m.State("TMCC_GO"):
                m.d.comb += tr.o_go.eq(1)
                m.next = "TMCC_BITS"
            with m.State("TMCC_BITS"):
                # the packer takes a word per clock in its payload state
                m.d.comb += [pk.w_valid.eq(tr.o_valid), pk.w_data.eq(tr.o_bits)]
                with m.If(tr.o_valid & tr.o_last):
                    m.next = "TMCC_WAIT"
            with m.State("TMCC_WAIT"):
                with m.If(~tr.busy):
                    m.d.sync += [tmode.eq(0), st.begin.eq(1)]
                    m.next = "STATUS_HDR"
            with m.State("STATUS_HDR"):
                with m.If(pk.b_ready & ~pk.b_valid & ~st.begin):
                    block(3, fr_frame, st.length)
                    m.d.sync += pace.eq(0)
                    m.next = "STATUS_W"
            with m.State("STATUS_W"):
                with m.If(~pace):
                    m.d.sync += pace.eq(1)
                with m.Else():
                    m.d.comb += [pk.w_valid.eq(1), pk.w_data.eq(st.word)]
                    with m.If(pk.w_ready):
                        m.d.sync += [st.next.eq(1), pace.eq(0)]
                        with m.If(st.last):
                            m.next = "FRAME_DONE"
            with m.State("FRAME_DONE"):
                m.d.comb += ff.r_en.eq(1)
                m.next = "IDLE"
        # the counters in the status
        for n_, sig in cnt.items():
            m.d.comb += st.snap[n_].eq(sig)
        m.d.comb += st.snap["iterations"].eq(self.iterations)
        return m
