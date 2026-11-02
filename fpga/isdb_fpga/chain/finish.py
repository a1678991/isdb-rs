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
