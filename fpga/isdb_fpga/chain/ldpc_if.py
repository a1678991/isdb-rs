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
  ends; the TMCC burst uses OP_BEGIN and OP_DONE; OP_ABORT gives up the
  context's open codeword, `ldpc.py` "Abandoned codewords": the back end
  sends it on context 0 after a frame drop or a restart, once the
  demapper's pipeline has emptied): decode as rate `in_rate` (numerator r of r/120; 61
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

OP_BEGIN, OP_WRITE, OP_DONE, OP_ABORT = 0, 1, 2, 3


def _block_fifo(width, depth, w_domain, r_domain):
    """An AsyncFIFO whose storage is a block RAM (`ram_style = "block"`):
    Vivado otherwise maps the bridge's 512 x 216-bit input FIFO to LUTRAM
    (about 1 700 LUTs, a deep read mux behind a read address that depends on
    the synchronised write pointer: -1.0 ns at 200 MHz in the first full
    build); on block RAM the address meets a handful of BRAM pins and the
    read data is registered inside them."""
    import functools

    import amaranth.lib.fifo as fifo_mod
    from amaranth.lib.fifo import AsyncFIFO

    class BlockAsyncFIFO(AsyncFIFO):
        def elaborate(self, platform):
            orig = fifo_mod.Memory
            fifo_mod.Memory = functools.partial(orig, attrs={"ram_style": "block"})
            try:
                return super().elaborate(platform)
            finally:
                fifo_mod.Memory = orig

    return BlockAsyncFIFO(width=width, depth=depth, w_domain=w_domain, r_domain=r_domain)
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

    - **in**: an AsyncFIFO of beats (IN_DEPTH = 64 beats of up to 5 LLRs,
      211 bits: LUT RAM; 512 took three RAMB36)
      into the decoder's domain, where a serialiser pops a beat and gives
      the decoder one LLR per cycle (`in_valid`, `in_pos`, `in_llr`,
      `in_rate` = the rate index, `in_ctx`), with `begin` on the cycle of a
      codeword's first LLR (or alone, OP_BEGIN) and `start` (`st_rate`,
      `st_max_iter`, `st_offset`, `st_short`: the closing beat's
      `in_max_iter`, `in_offset`, `in_shortened`, carried in the FIFO with
      the beat) on the cycle after its last (or alone, OP_DONE). The slot beats have priority on the chain side
      (`back.py`), so the TMCC burst takes the free cycles and never
      starves them. Rates: a 32APSK main symbol per 40 MHz clock is 5
      LLRs, exactly the decoder's 200 MHz; the front end's 33.76 Msymbol/s
      with 3.4 % non-main positions makes 163 M LLR/s for 32APSK, 130 M
      for 16APSK (the decoder's 0.81 share), plus the TMCC's 31 680 per
      frame (1 M/s): the FIFO absorbs only the bursts of a stream that
      momentarily runs at one symbol per clock (a beat of n LLRs takes n + 1
      decoder cycles, 6 for 32APSK against the 5 of a symbol clock: after a
      100-cycle hold of stage 1 the backlog grows by about 17 beats). A beat
      written while the FIFO is full is counted (`in_lost`; never in a
      simulation here).
    - **tags**: the decoder returns results in commit order without a tag;
      each `start` pushes its tag into a FIFO (TAG_DEPTH = 16, beyond the
      decoder's queue and its banks) that each `res_valid` pops.
    - **abort**: an OP_ABORT beat becomes `abort` (one cycle, `d_ctx`), in
      order with the beats around it.
    - **health**: the decoder's `overruns`, `queue_overflow`, `held`,
      `aborted` and `reused` (`h_*`, ldpc domain) as one snapshot into the
      chain's domain (`health_*`, the HEALTH registers of `SlotsRx`): a
      toggle handshake (each side's toggle through two registers; the
      snapshot is taken when the request arrives and read by the chain
      after the acknowledgement, so it is stable for at least two symbol
      clocks before it is sampled), a new one about every 100 ns.
    - **out**: the record (`res_*` with the tag) and the decoder's hard-bit
      words (`o_data`, `o_last`; all n bits, the first k in natural order)
      through an AsyncFIFO (OUT_DEPTH = 64) back to the chain's domain as
      the port's `out_*` stream (`out_head` the record; a dropped codeword,
      `res_overrun`, has no words). `o_ready` (a register) keeps room for one record, so
      a `res_valid` (one cycle, no handshake) always has a place.
    """

    IN_DEPTH = 64
    OUT_DEPTH = 64
    TAG_DEPTH = 16
    # the invariant monitors (monitors.py; `monitors=True`, simulation only)
    MONITORS = {
        "in_fifo_overflow": ("sync", "no beat written into the full input FIFO (it would be lost)"),
        "out_fifo_overflow": ("ldpc", "no record or word written into the full output FIFO"),
        "tag_overflow": ("ldpc", "no tag pushed into the full tag queue"),
        "tag_underflow": ("ldpc", "every result record pops a tag (one per commit)"),
    }
    monitor = None

    def __init__(self, port, max_iter=None, offset=None, monitors=False):
        # the limit and offset come with each codeword's closing beat
        # (`in_max_iter`, `in_offset` of the port); the arguments are unused
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
        self.abort = Signal()
        # the decoder's health (ldpc domain) and its snapshot (chain side)
        self.h_overruns = Signal(16)
        self.h_overflow = Signal()
        self.h_held = Signal(4)
        self.h_aborted = Signal(16)
        self.h_reused = Signal(16)
        self.health_overruns = Signal(16)
        self.health_overflow = Signal()
        self.health_held = Signal(4)
        self.health_aborted = Signal(16)
        self.health_reused = Signal(16)
        self.in_lost = Signal(16)
        self.out_overrun = Signal()  # chain side: the record's overrun flag
        # verification: the input FIFO's and tag queue's levels (ldpc
        # domain), the output FIFO's (chain side)
        self.v_in_level = Signal(range(self.IN_DEPTH + 1))
        self.v_tags = Signal(range(self.TAG_DEPTH + 1))
        self.v_out_level = Signal(range(self.OUT_DEPTH + 1))
        if monitors:
            from .monitors import enable

            enable(self)

    def elaborate(self, platform):
        from amaranth.lib.fifo import SyncFIFO

        m = Module()
        p = self.p
        # ---- in -------------------------------------------------------------
        fields = [p.in_op, p.in_ctx, p.in_n, p.in_first, p.in_last, p.in_rate, p.in_shortened,
                  p.in_tag, p.in_max_iter, p.in_offset, *p.in_pos, *p.in_llr]
        width = sum(len(f) for f in fields)
        fin = _block_fifo(width, self.IN_DEPTH, "sync", "ldpc")
        m.submodules.fin = fin
        m.d.comb += [fin.w_data.eq(Cat(*fields)), fin.w_en.eq(p.in_valid), p.in_ready.eq(1)]
        with m.If(p.in_valid & ~fin.w_rdy):
            m.d.sync += self.in_lost.eq(self.in_lost + 1)
        # the serialiser: a beat of n LLRs gives n decoder cycles (one, the
        # `begin` or `abort`, for a beat of none), back to back with the
        # beat before: the next beat is popped on the cycle of this one's
        # last lane (`at_last`), or with the `start` that closes it, so the
        # load cycle of a beat overlaps the previous beat's last cycle.
        # `at_last` and `ends` (the beat closes a codeword) are registers,
        # so the FIFO's read enable is a function of three registers.
        cur = Signal(width)
        have = Signal()
        lane = Signal(3)
        at_last = Signal()  # this cycle gives the beat's last lane
        ends = Signal()  # the beat closes its codeword: `start` follows
        o = 0
        at = {}  # a field's bits in a FIFO word
        for name, sig in zip(["op", "ctx", "n", "first", "last", "rate", "short", "tag", "max_iter", "offset"],
                             fields[:10]):
            at[name] = slice(o, o + len(sig))
            o += len(sig)
        f = {name: cur[sl] for name, sl in at.items()}
        pos = [cur[o + 16 * k:o + 16 * k + 16] for k in range(LANES)]
        o += 16 * LANES
        llr = [cur[o + 8 * k:o + 8 * k + 8].as_signed() for k in range(LANES)]
        tags = SyncFIFO(width=64, depth=self.TAG_DEPTH)
        m.submodules.tags = DomainRenamer("ldpc")(tags)

        def decode(word):
            """(opens, closes, n) of a FIFO word"""
            op = word[at["op"]]
            is_write = op == OP_WRITE
            return (Mux(is_write, word[at["first"]], op == OP_BEGIN),
                    Mux(is_write, word[at["last"]], op == OP_DONE), Mux(is_write, word[at["n"]], 0))

        opens, _, n = decode(cur)
        _, r_closes, r_n = decode(fin.r_data)
        need_start = Signal()
        load = Signal()
        # the tag is pushed in the `need_start` cycle itself (a cycle before
        # `start` reaches the decoder): the next beat may replace `cur` at
        # its end
        m.d.comb += [fin.r_en.eq(~have | (at_last & ~ends)), load.eq(fin.r_en & fin.r_rdy),
                     tags.w_data.eq(f["tag"]), tags.w_en.eq(need_start)]
        m.d.ldpc += [self.begin.eq(0), self.d_valid.eq(0), self.start.eq(0), self.abort.eq(0)]
        rate_idx = (f["rate"] - 1)[:4]
        with m.If(need_start):
            m.d.ldpc += [self.start.eq(1), self.d_ctx.eq(f["ctx"]), self.st_rate.eq(rate_idx),
                         self.st_max_iter.eq(f["max_iter"]), self.st_offset.eq(f["offset"]),
                         self.st_short.eq(f["short"]), need_start.eq(0)]
        with m.If(have):
            m.d.ldpc += [self.d_ctx.eq(f["ctx"]), self.d_rate.eq(rate_idx)]
            with m.If(lane == 0):
                m.d.ldpc += [self.begin.eq(opens), self.abort.eq(f["op"] == OP_ABORT)]
            with m.If(lane < n):
                m.d.ldpc += [self.d_valid.eq(1), self.d_pos.eq(Array(pos)[lane]),
                             self.d_llr.eq(Array(llr)[lane])]
            with m.If(at_last):
                m.d.ldpc += [have.eq(0), need_start.eq(ends)]
            with m.Else():
                m.d.ldpc += [lane.eq(lane + 1), at_last.eq(lane + 2 >= n)]
        # (after the lanes: a beat popped on the last lane keeps `have`)
        with m.If(load):
            m.d.ldpc += [cur.eq(fin.r_data), have.eq(1), lane.eq(0), at_last.eq(r_n <= 1),
                         ends.eq(r_closes)]
        # ---- out ------------------------------------------------------------
        # entry: kind (1: record), then the record (tag 64, iterations 8,
        # converged, overrun) or the word (data 16, last)
        fout = _block_fifo(1 + 64 + 8 + 1 + 1, self.OUT_DEPTH, "ldpc", "sync")
        m.submodules.fout = fout
        # the writes registered (the tag FIFO's output and the mux off the
        # FIFO's write port), and o_ready a register: up to two more words
        # and a record arrive after the level crosses, so it keeps four free
        fw_en = Signal()
        fw_data = Signal(len(fout.w_data))
        m.d.comb += [tags.r_en.eq(self.res_valid), fout.w_en.eq(fw_en), fout.w_data.eq(fw_data)]
        m.d.ldpc += [
            self.o_ready.eq(fout.w_level < self.OUT_DEPTH - 4),
            fw_en.eq(self.res_valid | (self.o_valid & self.o_ready)),
            fw_data.eq(Mux(self.res_valid,
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
        m.d.comb += [self.v_in_level.eq(fin.r_level), self.v_tags.eq(tags.level),
                     self.v_out_level.eq(fout.r_level)]
        # ---- health -----------------------------------------------------------
        from amaranth.lib.cdc import FFSynchronizer

        h_in = Cat(self.h_overruns, self.h_overflow, self.h_held, self.h_aborted, self.h_reused)
        h_out = Cat(self.health_overruns, self.health_overflow, self.health_held, self.health_aborted,
                    self.health_reused)
        snap = Signal(len(h_in))  # ldpc: taken when a request arrives
        req = Signal()  # chain side's toggle
        ack = Signal()  # decoder side's toggle
        req_l = Signal()
        ack_s = Signal()
        m.submodules.h_req = FFSynchronizer(req, req_l, o_domain="ldpc")
        m.submodules.h_ack = FFSynchronizer(ack, ack_s, o_domain="sync")
        with m.If(req_l != ack):
            m.d.ldpc += [snap.eq(h_in), ack.eq(req_l)]
        with m.If(ack_s == req):
            m.d.sync += [h_out.eq(snap), req.eq(~req)]
        if self.monitor is not None:
            from . import monitors

            monitors.fifo_overflow(m, self.monitor, "in_fifo_overflow", fin)
            monitors.fifo_overflow(m, self.monitor, "out_fifo_overflow", fout)
            monitors.fifo_overflow(m, self.monitor, "tag_overflow", tags)
            monitors.fifo_underflow(m, self.monitor, "tag_underflow", tags)
        return m


def tmcc_position(b):
    """`chain::tmcc::word_position`: TMCC signal bit b's position in the
    shortened word."""
    return b + 1870 if b < 9614 else b + 13200


def connect_decoder(m, br, dec):
    """`LdpcBridge` (its decoder side, the `ldpc` domain) to `LdpcDecoder`."""
    m.d.comb += [
        dec.begin.eq(br.begin), dec.in_ctx.eq(br.d_ctx), dec.in_valid.eq(br.d_valid),
        dec.in_pos.eq(br.d_pos), dec.in_llr.eq(br.d_llr), dec.in_rate.eq(br.d_rate),
        dec.start.eq(br.start), dec.st_rate.eq(br.st_rate), dec.st_max_iter.eq(br.st_max_iter),
        dec.st_offset.eq(br.st_offset), dec.st_short.eq(br.st_short),
        br.load_ready.eq(dec.load_ready),
        br.o_valid.eq(dec.o_valid), br.o_data.eq(dec.o_data), br.o_last.eq(dec.o_last),
        dec.o_ready.eq(br.o_ready),
        br.res_valid.eq(dec.res_valid), br.res_iterations.eq(dec.res_iterations),
        br.res_converged.eq(dec.res_converged), br.res_unsat.eq(dec.res_unsat),
        br.res_overrun.eq(dec.res_overrun),
        dec.abort.eq(br.abort),
        br.h_overruns.eq(dec.overruns), br.h_overflow.eq(dec.queue_overflow), br.h_held.eq(dec.held),
        br.h_aborted.eq(dec.aborted), br.h_reused.eq(dec.reused),
    ]
