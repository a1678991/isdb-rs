"""The decoder's result stream into the finish stage through a FIFO
(`ResultFifo`), so that a slot whose BCH needs the Chien search (the finish
stage then works ~10 000 symbol clocks on it, `finish.FastBch`) never holds
the decoder's output, and with it a codeword bank: the decoder hands its
results over at the stream's pace while the finish stage catches up.

Entries of 18 bits in a 4 096-deep `SyncFIFOBuffered` (two RAMB36 in 2K x
18): a result header as five entries (bit 16 set: the 64-bit tag, then the
iterations, convergence and the bridge's overrun flag), a data word as one
(bit 17 its `last`). Only the words the finish stage uses are kept: the
first k bits of a slot codeword (k = 374 r, rate from the tag), the first
22 814 of the TMCC's (1 426 words); the rest of the n = 44 880 are taken
from the decoder and dropped, `last` on the last one kept.

Capacity: 4 096 entries are 1.88 results of 93/120 (2 174 words + 5).
The finish stage needs about 2 200 symbol clocks to take a result, about
2 200 to send its slot block and nothing more when the BCH check passes,
about 9 000 more for a word with errors (Berlekamp-Massey, its product
steps two fast cycles each, and the Chien search in the 4x clock: 8 993
at 12 errors, tools/bch_cycles.py), about 13 400 in all. A 16APSK frame
brings its 4 valid slots in 5 at 11 000 symbol clocks: with every one
corrected the finish stage falls 2 400 behind per slot, 7 200 after the
fourth, and catches up in the dummy slot (55 000 per five slots against
53 600 of work), so a result waits while the one before is finished and
the next has not come: at most one result held (2 179 entries) and the
decoder never held more than it was without a correction. (32APSK, five
valid slots in five at 11 000, keeps up while at most about 7 in 10
slots need correcting: 0.73 x 13 400 + 0.27 x 4 400 = 11 000.)
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal
from amaranth.lib.fifo import SyncFIFOBuffered

from .tables import RATES

DEPTH = 4096
TMCC_WORDS = (374 * 61 + 15) // 16  # 1 426: the shortened TMCC word's k bits


class ResultFifo(Elaboratable):
    def __init__(self):
        # in: the LDPC port's result side (`LdpcPort.out_*`, the bridge's overrun flag)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.i_head = Signal()
        self.i_tag = Signal(64)
        self.i_iterations = Signal(8)
        self.i_converged = Signal()
        self.i_overrun = Signal()
        self.i_bits = Signal(16)
        self.i_last = Signal()
        # out: the finish stage's `r_*`
        self.o_valid = Signal()
        self.o_ready = Signal()
        self.o_head = Signal()
        self.o_tag = Signal(64)
        self.o_iterations = Signal(8)
        self.o_converged = Signal()
        self.o_overrun = Signal()
        self.o_bits = Signal(16)
        self.o_last = Signal()
        self.level = Signal(range(DEPTH + 2))
        self.max_level = Signal(range(DEPTH + 2))  # verification: the most entries held

    def elaborate(self, platform):
        m = Module()
        m.submodules.fifo = f = SyncFIFOBuffered(width=18, depth=DEPTH)
        m.d.comb += self.level.eq(f.level)
        with m.If(f.level > self.max_level):
            m.d.sync += self.max_level.eq(f.level)
        # ---- writer ---------------------------------------------------------
        chunk = Signal(3)  # 0: idle; 1..4: header chunks 1..4 to write
        rec = Signal(80)
        kw = Signal(12)  # words of the result to keep
        idx = Signal(12)
        kwords = Array(Const((374 * r + 15) // 16, 12) for r in RATES + [0] * 5)
        m.d.comb += f.w_en.eq(0)
        with m.If(chunk != 0):
            m.d.comb += [f.w_data.eq(Cat(Array(rec[16 * k:16 * k + 16] for k in range(5))[chunk], 1, 0)),
                         f.w_en.eq(1)]
            with m.If(f.w_rdy):
                m.d.sync += chunk.eq(Mux(chunk == 4, 0, chunk + 1))
        with m.Else():
            # a header needs room for its five entries
            room = f.level <= DEPTH - 5
            m.d.comb += self.i_ready.eq(Mux(self.i_head, room, f.w_rdy | (idx >= kw)))
            with m.If(self.i_valid & self.i_ready):
                with m.If(self.i_head):
                    r = Cat(self.i_tag, self.i_iterations, self.i_converged, self.i_overrun, Const(0, 6))
                    m.d.sync += [rec.eq(r), chunk.eq(1), idx.eq(0),
                                 kw.eq(Mux(self.i_tag[7], TMCC_WORDS, kwords[(self.i_tag[8:12] - 1)[:4]]))]
                    m.d.comb += [f.w_data.eq(Cat(r[:16], 1, 0)), f.w_en.eq(1)]
                with m.Else():
                    m.d.sync += idx.eq(idx + 1)
                    with m.If(idx < kw):
                        m.d.comb += [f.w_data.eq(Cat(self.i_bits, 0, self.i_last | (idx == kw - 1))),
                                     f.w_en.eq(1)]
        # ---- reader ---------------------------------------------------------
        got = Signal(3)  # header chunks gathered
        hd = Signal(80)
        e = f.r_data
        is_rec = e[16]
        have = Signal()  # a whole header in `hd`
        m.d.comb += f.r_en.eq(0)
        with m.If(~have & f.r_rdy & is_rec):
            m.d.comb += f.r_en.eq(1)
            for k in range(5):
                with m.If(got == k):
                    m.d.sync += hd[16 * k:16 * k + 16].eq(e[:16])
            m.d.sync += got.eq(Mux(got == 4, 0, got + 1))
            with m.If(got == 4):
                m.d.sync += have.eq(1)
        m.d.comb += [
            self.o_valid.eq(have | (f.r_rdy & ~is_rec)),
            self.o_head.eq(have),
            self.o_tag.eq(hd[:64]), self.o_iterations.eq(hd[64:72]), self.o_converged.eq(hd[72]),
            self.o_overrun.eq(hd[73]),
            self.o_bits.eq(e[:16]), self.o_last.eq(e[17]),
        ]
        with m.If(have):
            with m.If(self.o_ready):
                m.d.sync += have.eq(0)
        with m.Elif(f.r_rdy & ~is_rec & self.o_ready):
            m.d.comb += f.r_en.eq(1)
        return m
