"""The formal harness of `Packer`'s sequence numbers (`chain/packer.py`:
"The sequence number counts from 0 at reset; `start` restarts it at the
next block boundary"; PR #39's defect, 1817079).

The block under test is the `Packer`, unchanged; every input is free
(`start`, `b_valid` with any kind, frame and payload length, `w_valid`
with any data).

The spec model: at each block accepted (`b_valid` while `b_ready`), the
block's sequence number is 0 when it is the first block since reset or
a `start` came in the cycle of its acceptance or after the previous
block's acceptance, else the previous block's + 1.

Assumptions: no reset (the initial state is the reset state). Nothing
else: a `start` and a block offer may come in any cycle, payloads of any
length.

Properties (Assert):

- from the cycle after a block's acceptance until it has left (the
  packer busy, `b_ready` low), `seq` is the block's number;
- the block's header carries it: its fourth and fifth output words are
  the number's low and high halves;
- a block offered while the packer is idle (`b_ready`) is taken: the
  packer is busy the cycle after;
- helper invariants (they make the properties inductive, for the prove
  task): the model's state against the packer's (`restart`, `idx`, the
  FSM state): `seq` while idle is 0 after a `start`, else the last
  block's + 1; the header words leave on the cycles the model counts.

Covers: a block accepted with a `start` in the same cycle, a `start`
while a block is in its header, a second block numbered 0 after such a
`start`, two blocks numbered 0 and 1.
"""

from amaranth.hdl import Assert, Assume, Cover, Elaboratable, Module, Mux, ResetSignal, Signal

from ..chain.packer import Packer
from . import elaborate


class PackerHarness(Elaboratable):
    TRACE = ["start", "b_valid", "b_ready", "b_len", "w_valid", "o_valid", "o_word", "seq", "p_expect",
             "p_pending", "p_hdr"]

    def __init__(self):
        self.start = Signal()
        self.b_valid = Signal()
        self.b_kind = Signal(4)
        self.b_frame = Signal(32)
        self.b_len = Signal(16)
        self.w_valid = Signal()
        self.w_data = Signal(16)

    def elaborate(self, platform):
        m = Module()
        pk = Packer()
        frag, t = elaborate(pk, ["restart", "idx", "packer_state"])
        restart, idx, state = t["restart"], t["idx"], t["packer_state"]
        HEADER, PAYLOAD = 1, 2  # (Amaranth numbers an FSM's states in order of definition)
        m.submodules.pk = frag
        m.d.comb += [pk.start.eq(self.start), pk.b_valid.eq(self.b_valid), pk.b_kind.eq(self.b_kind),
                     pk.b_frame.eq(self.b_frame), pk.b_len.eq(self.b_len), pk.w_valid.eq(self.w_valid),
                     pk.w_data.eq(self.w_data)]
        m.d.comb += Assume(~ResetSignal())

        accept = pk.b_valid & pk.b_ready
        # the model: the number of the block accepted last, a start pending
        expect = Signal(32, name="p_expect")
        have = Signal(name="p_have")  # a block was accepted since reset
        pending = Signal(name="p_pending")  # a start since the last acceptance
        with m.If(accept):
            m.d.sync += [expect.eq(Mux(~have | pending | pk.start, 0, expect + 1)), have.eq(1),
                         pending.eq(0)]
        with m.Elif(pk.start):
            m.d.sync += pending.eq(1)
        # the header words of the block left to see (9 after acceptance)
        hdr = Signal(range(10), name="p_hdr")
        with m.If(accept):
            m.d.sync += hdr.eq(9)
        with m.Elif(pk.o_valid & (hdr != 0)):
            m.d.sync += hdr.eq(hdr - 1)

        busy = ~pk.b_ready
        acc_d = Signal(name="p_accepted")
        m.d.sync += acc_d.eq(accept)
        m.d.comb += [
            Assert(~busy | (pk.seq == expect), "a block's seq while it leaves is its number"),
            Assert(~(pk.o_valid & (hdr == 6)) | (pk.o_word == expect[:16]), "header word 3 = seq low"),
            Assert(~(pk.o_valid & (hdr == 5)) | (pk.o_word == expect[16:]), "header word 4 = seq high"),
            Assert(~acc_d | busy, "a block offered while idle is taken: busy the cycle after"),
        ]
        # helper invariants: the model against the packer's state
        m.d.comb += [
            Assert(hdr <= 9, "model: header count"),
            Assert(~busy | have, "model: a block accepted before the packer is busy"),
            Assert(~restart | pending, "model: a latched start is pending"),
            Assert(~busy | (restart == pending), "model: a start while busy is latched"),
            Assert(busy | restart | ~(pending | ~have) | (pk.seq == 0), "model: seq 0 when idle after start"),
            Assert(busy | ~have | pending | (pk.seq == (expect + 1)[:32]), "model: seq the last + 1 when idle"),
            Assert((state == HEADER) == (hdr >= 2), "model: the header leaves while hdr >= 2"),
            Assert(~(hdr == 9) | ((idx == 0) & ~pk.o_valid) | ((idx == 1) & pk.o_valid),
                   "model: header start"),
            Assert(~((hdr >= 2) & (hdr <= 8)) | (pk.o_valid & (idx == 10 - hdr)), "model: header words"),
            Assert(~(hdr == 1) | (pk.o_valid & (state == PAYLOAD) & (idx == 0)), "model: header end"),
        ]

        st_hdr = Signal(name="c_start_in_header")
        with m.If(accept):
            m.d.sync += st_hdr.eq(0)
        with m.Elif(pk.start & (hdr != 0)):
            m.d.sync += st_hdr.eq(1)
        m.d.comb += [
            Cover(accept & pk.start & have),
            Cover(pk.start & (hdr > 3)),
            Cover(accept & st_hdr),
            Cover(busy & have & (expect == 1)),
        ]
        return m
