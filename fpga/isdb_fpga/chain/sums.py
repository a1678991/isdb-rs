"""The `Slots` sums block (format 2, block type 4): a decoded frame's
training sums sent in the stream right after its status block, read from
the equaliser's latched accumulator bank.

Contract: `isdb_frontend::slots` ("sums", `SumsBlock`) and the model's
`isdbs3_phy::chain::out::blocks` (a frame whose sums the equaliser latched
at its end gets the block after its status block): 465 payload words, the
frame summed (2 words, low first; the header's frame), the count (1), then
the 77 accumulators in the equaliser's entry order (R's 66 upper-triangle
entries row by row, then p's 11: `eq.entries()`, the order of the
registers `SUMS_R` and `SUMS_P`), each as six words: re bits 0-15, 16-31,
32-47, im bits 0-15, 16-31, 32-47 (the accumulator RAM's 96-bit word, re in
bits 0-47 and im in 48-95, so the six words are its six 16-bit slices in
order: the 48-bit two's-complement values need no sign handling).

`SumsReader` (in `Finish`, which starts it after a frame's status block):

- `due`: the block is sent for the frame `Finish` has in hand (`frame`,
  `decoded`) when it was decoded and the equaliser's latched sums are that
  frame's (`sums_frame == frame`): the latch at the frame's end. With the
  host's SUMS_CAPTURE hold the latch keeps an earlier frame, and that
  frame gets no block (the model has no capture; a host reading the stream
  arms none).
- `go` (one cycle, with the packer's block accepted): the payload words
  then leave through `w_valid` / `w_data` / `w_ready` (the packer's payload
  handshake, one word per clock at most), `last` on the 465th. The header
  words come from `frame` and `sums_count` (taken at `go`). Each entry is
  read through the equaliser's second readback request (`rd_req`,
  `rd_entry` -> `rd_ack`, `rd_data` 96 bits 3 cycles later: eq.py takes the
  accumulator RAM's read port for one cycle, as the host's register READ
  does, a MAC due then waiting a cycle), then its six words leave by a 6:1
  select of 16 bits. About 10 cycles per entry, 800 per block: far inside
  the frame the latched bank stays readable (the next latch is at the next
  decoded frame's end; the status block itself leaves within a frame of
  its frame's end, finish.py).
"""

from amaranth.hdl import Array, Cat, Elaboratable, Module, Signal

from .slots import STATUS, SUMS, SUMS_ENTRIES, SUMS_PAYLOAD

NONE = 0xFFFFFFFF


class SumsReader(Elaboratable):
    # the invariant monitors (monitors.py)
    MONITORS = {
        "words": ("sync", "a sums block's payload is 465 words: its last word is the 465th taken "
                          "since its start, and no word is offered outside a block"),
        "follows_status": ("sync", "a sums block (packer type 4) is accepted only right after its "
                                   "frame's status block (the block before it of type 3, the same "
                                   "frame)"),
        "latch_steady": ("sync", "the equaliser's latched sums stay those of the frame being sent "
                                 "until the block's last word (a latch moving mid-block would mix "
                                 "two frames' sums under one CRC)"),
    }
    monitor = None

    def __init__(self):
        # Finish's side
        self.frame = Signal(32)  # the frame in hand (its status block just left)
        self.decoded = Signal()
        self.due = Signal()
        self.go = Signal()
        self.busy = Signal()
        self.w_valid = Signal()
        self.w_data = Signal(16)
        self.w_ready = Signal()
        self.last = Signal()
        # the equaliser's latched sums (eq.py)
        self.sums_frame = Signal(32, init=NONE)
        self.sums_count = Signal(16)
        self.rd_req = Signal()
        self.rd_entry = Signal(7)
        self.rd_ack = Signal()
        self.rd_data = Signal(96)
        # the packer's block acceptance (the monitors)
        self.pk_accept = Signal()
        self.pk_kind = Signal(4)
        self.pk_frame = Signal(32)

    def elaborate(self, platform):
        m = Module()
        m.d.comb += self.due.eq(self.decoded & (self.sums_frame == self.frame))
        hdr = Signal(48)  # the frame and the count, taken at go
        idx = Signal(2)  # header word
        ent = Signal(range(SUMS_ENTRIES))
        sub = Signal(range(6))
        take = self.w_valid & self.w_ready
        m.d.comb += [self.rd_req.eq(0), self.rd_entry.eq(ent), self.w_valid.eq(0), self.last.eq(0)]
        with m.FSM(name="sums") as fsm:
            with m.State("IDLE"):
                with m.If(self.go):
                    m.d.sync += [hdr.eq(Cat(self.frame, self.sums_count)), idx.eq(0), ent.eq(0)]
                    m.next = "HEAD"
            with m.State("HEAD"):
                m.d.comb += [self.w_valid.eq(1), self.w_data.eq(Array([hdr[0:16], hdr[16:32], hdr[32:48]])[idx])]
                with m.If(take):
                    m.d.sync += idx.eq(idx + 1)
                    with m.If(idx == 2):
                        m.next = "REQ"
            with m.State("REQ"):
                m.d.comb += self.rd_req.eq(1)
                m.next = "WAIT"
            with m.State("WAIT"):
                with m.If(self.rd_ack):
                    m.d.sync += sub.eq(0)
                    m.next = "WORDS"
            with m.State("WORDS"):
                d = self.rd_data
                m.d.comb += [self.w_valid.eq(1),
                             self.w_data.eq(Array([d[16 * k:16 * k + 16] for k in range(6)])[sub]),
                             self.last.eq((sub == 5) & (ent == SUMS_ENTRIES - 1))]
                with m.If(take):
                    m.d.sync += sub.eq(sub + 1)
                    with m.If(sub == 5):
                        with m.If(ent == SUMS_ENTRIES - 1):
                            m.next = "IDLE"
                        with m.Else():
                            m.d.sync += ent.eq(ent + 1)
                            m.next = "REQ"
        m.d.comb += self.busy.eq(~fsm.ongoing("IDLE"))
        if self.monitor is not None:
            sync = m.d.sync
            mon = self.monitor
            n = Signal(range(SUMS_PAYLOAD + 2), name="mon_sums_n")
            with m.If(self.go):
                sync += n.eq(0)
            with m.Elif(take):
                sync += n.eq(n + 1)
            mon.watch(m, "words", (take & self.last & (n != SUMS_PAYLOAD - 1))
                      | (take & ~self.last & (n >= SUMS_PAYLOAD - 1))
                      | (self.w_valid & fsm.ongoing("IDLE")))
            pk_kind = Signal(4, name="mon_prev_kind")
            pk_frame = Signal(32, name="mon_prev_frame")
            with m.If(self.pk_accept):
                sync += [pk_kind.eq(self.pk_kind), pk_frame.eq(self.pk_frame)]
            mon.watch(m, "follows_status", self.pk_accept & (self.pk_kind == SUMS)
                      & ((pk_kind != STATUS) | (pk_frame != self.pk_frame)))
            sent = Signal(32, name="mon_sums_sent")
            with m.If(self.go):
                sync += sent.eq(self.frame)
            mon.watch(m, "latch_steady", self.busy & (self.sums_frame != sent))
        return m
