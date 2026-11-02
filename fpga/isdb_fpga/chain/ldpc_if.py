"""The interface between the chain and the LDPC decoder (stage 7, written
on another branch as `isdb_fpga/ldpc.py`; plan piece 2). The chain drives
the input side and consumes the result side; both are in the chain's
clock domain (the symbol clock, `util_ad9361_divclk`, 40 MHz). The decoder
runs in its own faster domain (100 or 200 MHz) behind asynchronous FIFOs
on both sides, which belong to the decoder's wrapper: every crossing is a
FIFO, nothing else crosses.

**Input (chain -> decoder)**, one beat per clock when `in_valid & in_ready`:

- `in_op`: OP_BEGIN opens the codeword buffer of context `in_ctx` (0: the
  slot codewords, 1: the frame's TMCC codeword); OP_WRITE writes `in_n`
  (1 .. 5) LLRs, lane l at codeword position `in_pos[l]` (< 44 880) with
  value `in_llr[l]` (i8, -127 .. 127) into the open buffer of `in_ctx`;
  OP_DONE closes it (a write beat may also carry `in_first`, which opens
  the buffer first, and `in_last`, which closes it after: the demapper's
  form, one beat per main symbol with no beat to spare at a codeword's
  ends; the TMCC burst uses OP_BEGIN and OP_DONE): decode as rate `in_rate` (numerator r of r/120; 61
  for the TMCC), at most `in_max_iter` iterations, offset `in_offset`,
  `in_shortened` (the TMCC's shortened 61/120 word: positions 0 .. 1 869
  and 11 484 .. 22 813 are known zeros the decoder fills with +127 itself;
  they are never written), with the 64-bit `in_tag` returned with the
  result (bits 0-6 the slot index, bit 7 TMCC, 8-11 the rate code, 12-15
  the modulation code, 16-18 the transmission mode, 32-63 the frame
  counter: what the finish stage needs of the slot; the decoder itself
  returns no tag, the bridge keeps them in commit order).
- A slot codeword is written by the demapper, one beat per main symbol
  (its 1 .. 5 label bits at their de-interleaved positions); the TMCC
  codeword is burst from the TMCC stage's staging buffer (5 positions per
  beat) on the cycles the demapper leaves free. The two contexts share the
  port cycle by cycle; the decoder keeps one open buffer per context.
  Every position of a slot codeword is written exactly once; the TMCC's
  31 680 sent positions likewise.

**Result (decoder -> chain)**, in the order the codewords were closed
(the decoder's queue; plan stage 7: a result depends only on the
codeword, the code, the schedule and the limit):

- a header beat (`out_head` high) with `out_tag`, `out_iterations` (the
  limit when not converged) and `out_converged`;
- then the hard decisions of the first k positions (k = 374 r; for the
  TMCC, 22 814) in position order, 16 per beat (`out_bits`, bit b =
  position 16 q + b; the last beat zero-padded), `out_last` on the last;
- `out_ready` from the chain (the finish stage may stall the stream).

**The stub** (`LdpcStub` in the simulations, `stages.py`): the chain's
vectors carry the decoder's results (each slot's iterations and the
decoded slot bits in the `Slots` words, the TMCC's iterations and
information bits), not its output bits, so the stub rebuilds them: for a
converged codeword with no BCH correction (all of the synthetic sets) the
decoder's first k hard decisions are the transmitted ones, i.e. the
header and data bits the vectors give, BCH-encoded (gf.encode), with the
six stuffing ones, then dispersed (the slot PRBS across the frame's valid
slots; for the TMCC the TMCC PRBS over its 9 614 data bits, the known
zeros 0). It checks the input side against the vectors where they carry
it (chain-4: every TMCC position and value; chain-5: every LLR of two
slots and their positions) and answers each OP_DONE with that header and
those bits, in order. It is a stand-in for the decoder's output, not a
model of the decoder: a codeword the stub cannot rebuild (not converged,
or BCH-corrected) is reported as a gap in the comparison.
"""

from amaranth.hdl import Array, Cat, Const, DomainRenamer, Elaboratable, Module, Mux, Signal, signed

OP_BEGIN, OP_WRITE, OP_DONE = 0, 1, 2
LANES = 5
TAG_TMCC = 1 << 7
TMCC_RATE_CODE = 3  # 61/120


class LdpcPort:
    """The signals of both sides (the chain's view)."""

    def __init__(self, name="ldpc"):
        self.in_valid = Signal(name=f"{name}_in_valid")
        self.in_ready = Signal(name=f"{name}_in_ready", init=1)
        self.in_op = Signal(2, name=f"{name}_in_op")
        self.in_ctx = Signal(name=f"{name}_in_ctx")
        self.in_n = Signal(range(LANES + 1), name=f"{name}_in_n")
        self.in_pos = [Signal(16, name=f"{name}_in_pos{k}") for k in range(LANES)]
        self.in_llr = [Signal(signed(8), name=f"{name}_in_llr{k}") for k in range(LANES)]
        self.in_rate = Signal(7, name=f"{name}_in_rate")
        self.in_max_iter = Signal(8, name=f"{name}_in_max_iter")
        self.in_offset = Signal(3, name=f"{name}_in_offset")
        self.in_shortened = Signal(name=f"{name}_in_shortened")
        self.in_first = Signal(name=f"{name}_in_first")
        self.in_last = Signal(name=f"{name}_in_last")
        self.in_tag = Signal(64, name=f"{name}_in_tag")
        self.out_valid = Signal(name=f"{name}_out_valid")
        self.out_ready = Signal(name=f"{name}_out_ready")
        self.out_head = Signal(name=f"{name}_out_head")
        self.out_tag = Signal(64, name=f"{name}_out_tag")
        self.out_iterations = Signal(8, name=f"{name}_out_iterations")
        self.out_converged = Signal(name=f"{name}_out_converged")
        self.out_bits = Signal(16, name=f"{name}_out_bits")
        self.out_last = Signal(name=f"{name}_out_last")


class LdpcBridge(Elaboratable):
    """The chain's port (`LdpcPort`, sync domain = the 40 MHz symbol clock)
    to the decoder's ports (`isdb_fpga/ldpc.py`, domain `ldpc`, 200 MHz):

    - **in**: an AsyncFIFO of beats (IN_DEPTH = 512 beats of up to 5 LLRs)
      into the decoder's domain, where a serialiser pops a beat and gives
      the decoder one LLR per cycle (`in_valid`, `in_pos`, `in_llr`,
      `in_rate` = the rate index, `in_ctx`), with `begin` on the cycle of a
      codeword's first LLR (or alone, OP_BEGIN) and `start` (`st_rate`,
      `st_max_iter`, `st_offset`, `st_short`) on the cycle after its last
      (or alone, OP_DONE). The slot beats have priority on the chain side
      (`back.py`), so the TMCC burst takes the free cycles and never
      starves them. Rates: a 32APSK main symbol per 40 MHz clock is 5
      LLRs, exactly the decoder's 200 MHz; the front end's 33.76 Msymbol/s
      with 3.4 % non-main positions makes 163 M LLR/s for 32APSK, 130 M
      for 16APSK (the decoder's 0.81 share), plus the TMCC's 31 680 per
      frame (1 M/s): the FIFO absorbs only the bursts of a stream that
      momentarily runs at one symbol per clock. A beat written while the
      FIFO is full is counted (`in_lost`; never in a simulation here).
    - **tags**: the decoder returns results in commit order without a tag;
      each `start` pushes its tag into a FIFO (TAG_DEPTH = 16, beyond the
      decoder's queue and its banks) that each `res_valid` pops.
    - **out**: the record (`res_*` with the tag) and the decoder's hard-bit
      words (`o_data`, `o_last`; all n bits, the first k in natural order)
      through an AsyncFIFO (OUT_DEPTH = 64) back to the chain's domain as
      the port's `out_*` stream (`out_head` the record; a dropped codeword,
      `res_overrun`, has no words). `o_ready` keeps room for one record, so
      a `res_valid` (one cycle, no handshake) always has a place.
    """

    IN_DEPTH = 512
    OUT_DEPTH = 64
    TAG_DEPTH = 16

    def __init__(self, port, max_iter=16, offset=1):
        self.p = port
        self.max_iter = max_iter
        self.offset = offset
        # the decoder's ports (ldpc domain)
        self.begin = Signal()
        self.d_ctx = Signal()
        self.d_valid = Signal()
        self.d_pos = Signal(16)
        self.d_llr = Signal(signed(8))
        self.d_rate = Signal(4)
        self.start = Signal()
        self.st_rate = Signal(4)
        self.st_max_iter = Signal(8)
        self.st_offset = Signal(7)
        self.st_short = Signal()
        self.load_ready = Signal()
        self.o_valid = Signal()
        self.o_data = Signal(16)
        self.o_last = Signal()
        self.o_ready = Signal()
        self.res_valid = Signal()
        self.res_iterations = Signal(8)
        self.res_converged = Signal()
        self.res_unsat = Signal(16)
        self.res_overrun = Signal()
        self.in_lost = Signal(16)
        self.out_overrun = Signal()  # chain side: the record's overrun flag

    def elaborate(self, platform):
        from amaranth.lib.fifo import AsyncFIFO, SyncFIFO

        m = Module()
        p = self.p
        # ---- in -------------------------------------------------------------
        fields = [p.in_op, p.in_ctx, p.in_n, p.in_first, p.in_last, p.in_rate, p.in_shortened,
                  p.in_tag, *p.in_pos, *p.in_llr]
        width = sum(len(f) for f in fields)
        fin = AsyncFIFO(width=width, depth=self.IN_DEPTH, w_domain="sync", r_domain="ldpc")
        m.submodules.fin = fin
        m.d.comb += [fin.w_data.eq(Cat(*fields)), fin.w_en.eq(p.in_valid), p.in_ready.eq(1)]
        with m.If(p.in_valid & ~fin.w_rdy):
            m.d.sync += self.in_lost.eq(self.in_lost + 1)
        # the serialiser
        cur = Signal(width)
        have = Signal()
        lane = Signal(3)
        o = 0
        f = {}
        for name, sig in zip(["op", "ctx", "n", "first", "last", "rate", "short", "tag"], fields[:8]):
            f[name] = cur[o:o + len(sig)]
            o += len(sig)
        pos = [cur[o + 16 * k:o + 16 * k + 16] for k in range(LANES)]
        o += 16 * LANES
        llr = [cur[o + 8 * k:o + 8 * k + 8].as_signed() for k in range(LANES)]
        tags = SyncFIFO(width=64, depth=self.TAG_DEPTH)
        m.submodules.tags = DomainRenamer("ldpc")(tags)
        is_write = f["op"] == OP_WRITE
        opens = Mux(is_write, f["first"], f["op"] == OP_BEGIN)
        closes = Mux(is_write, f["last"], f["op"] == OP_DONE)
        n = Mux(is_write, f["n"], 0)
        need_start = Signal()
        m.d.comb += [fin.r_en.eq(~have & ~need_start), tags.w_data.eq(f["tag"])]
        m.d.ldpc += [self.begin.eq(0), self.d_valid.eq(0), self.start.eq(0), tags.w_en.eq(0)]
        rate_idx = (f["rate"] - 1)[:4]
        with m.If(need_start):
            m.d.ldpc += [self.start.eq(1), self.d_ctx.eq(f["ctx"]), self.st_rate.eq(rate_idx),
                         self.st_max_iter.eq(self.max_iter), self.st_offset.eq(self.offset),
                         self.st_short.eq(f["short"]), tags.w_en.eq(1), need_start.eq(0)]
        with m.Elif(~have & fin.r_rdy):
            m.d.ldpc += [cur.eq(fin.r_data), have.eq(1), lane.eq(0)]
        with m.Elif(have):
            m.d.ldpc += [self.d_ctx.eq(f["ctx"]), self.d_rate.eq(rate_idx)]
            with m.If(lane == 0):
                m.d.ldpc += self.begin.eq(opens)
            with m.If(lane < n):
                m.d.ldpc += [self.d_valid.eq(1), self.d_pos.eq(Array(pos)[lane]),
                             self.d_llr.eq(Array(llr)[lane])]
            last_lane = (lane + 1 >= n)
            with m.If(last_lane):
                m.d.ldpc += [have.eq(0), need_start.eq(closes)]
            with m.Else():
                m.d.ldpc += lane.eq(lane + 1)
        # ---- out ------------------------------------------------------------
        # entry: kind (1: record), then the record (tag 64, iterations 8,
        # converged, overrun) or the word (data 16, last)
        fout = AsyncFIFO(width=1 + 64 + 8 + 1 + 1, depth=self.OUT_DEPTH, w_domain="ldpc", r_domain="sync")
        m.submodules.fout = fout
        m.d.comb += [
            tags.r_en.eq(self.res_valid),
            self.o_ready.eq(fout.w_level < self.OUT_DEPTH - 2),
            fout.w_en.eq(self.res_valid | (self.o_valid & self.o_ready)),
            fout.w_data.eq(Mux(self.res_valid,
                               Cat(Const(1, 1), tags.r_data, self.res_iterations, self.res_converged,
                                   self.res_overrun),
                               Cat(Const(0, 1), self.o_data, self.o_last))),
        ]
        e = fout.r_data
        m.d.comb += [
            p.out_valid.eq(fout.r_rdy), fout.r_en.eq(p.out_ready),
            p.out_head.eq(e[0]), p.out_tag.eq(e[1:65]), p.out_iterations.eq(e[65:73]),
            p.out_converged.eq(e[73]), self.out_overrun.eq(e[74]),
            p.out_bits.eq(e[1:17]), p.out_last.eq(e[17]),
        ]

        return m


def tmcc_position(b):
    """`chain::tmcc::word_position`: TMCC signal bit b's position in the
    shortened word."""
    return b + 1870 if b < 9614 else b + 13200
