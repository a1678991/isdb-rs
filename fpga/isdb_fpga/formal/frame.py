"""The formal harness of `FrameCtl`'s frame boundaries (`chain/frame.py`:
the rules of `isdbs3_phy::chain::frame::Back`).

The block under test is the `FrameCtl`, unchanged; its inputs (the
centre symbol, `restart`, the TMCC stage's layout writes) are free but
for the assumptions below.

Assumptions:

- no reset (the initial state is the reset state);
- a symbol's slot number is below 120 and its position below 9 296 (the
  values a slot position can take);
- frame heads come in increasing frame order: the frame number of each
  frame head (slot 0, position 0, numbered) is above every earlier
  head's (the front end numbers its frames with a counter that START does
  not reset; the monitor `frame_order` relies on the same).

Properties (Assert):

- the frame numbers at `o_done` / `o_dropped` strictly increase;
- `o_done` and `o_dropped` never together, and each reports the frame
  that was open the cycle before (`o_frame` = that `cur_frame`);
- a restart closes the open frame: the cycle after, no frame is open,
  `o_dropped` reports the frame that was open (if one was) and no frame
  is done;
- a frame head (not in a restart's cycle) drops the open frame (if one)
  and opens its own;
- a restart drops the queued layouts: a frame head after a restart, with
  no layout committed since, has no layout;
- helper invariants (they make the properties inductive, for the prove
  task): an open frame's number is the last head's; the last reported
  frame (counting a report on the outputs now) is below the open frame
  and at most the last head's; after a restart and no commit, the
  layout queue (`q_valid`) is empty.

Covers: a frame dropped by a restart, a frame dropped by the next head,
a frame dropped by an incomplete end, two reports in a row, a head with
a queued layout. Not reachable in a bounded run: `o_done` (a frame of
1 115 520 symbols); the prove task covers it, as every other state the
properties' induction admits.
"""

from amaranth.hdl import Assert, Assume, Cover, Elaboratable, Module, Mux, ResetSignal, Signal

from ..chain.common import SLOT, SLOTS
from ..chain.frame import LAY_BITS, FrameCtl
from . import elaborate


class FrameHarness(Elaboratable):
    TRACE = ["cv", "c_numbered", "c_number", "c_j", "c_frame", "restart", "lw_commit", "start", "cur_valid",
             "cur_frame", "o_done", "o_dropped", "o_frame", "p_last", "p_head"]

    def __init__(self):
        self.cv = Signal()
        self.c_numbered = Signal()
        self.c_number = Signal(range(SLOTS))
        self.c_frame = Signal(32)
        self.c_j = Signal(range(SLOT))
        self.c_at = Signal(48)
        self.restart = Signal()
        self.lw_open = Signal()
        self.lw_en = Signal()
        self.lw_slot = Signal(range(SLOTS))
        self.lw_data = Signal(LAY_BITS)
        self.lw_commit = Signal()
        self.lw_frame = Signal(32)
        self.lw_from = Signal(32)

    def elaborate(self, platform):
        m = Module()
        fc = FrameCtl()
        frag, t = elaborate(fc, ["q_valid"])
        m.submodules.fc = frag
        for n in ("cv", "c_numbered", "c_number", "c_frame", "c_j", "c_at", "restart", "lw_open", "lw_en",
                  "lw_slot", "lw_data", "lw_commit", "lw_frame", "lw_from"):
            m.d.comb += getattr(fc, n).eq(getattr(self, n))
        head = fc.start  # slot 0, position 0 of a numbered symbol
        ev = fc.o_done | fc.o_dropped

        # the model: the last head's frame, the last frame reported
        p_head = Signal(32, name="p_head")
        have_head = Signal(name="p_have_head")
        with m.If(head):
            m.d.sync += [p_head.eq(fc.c_frame), have_head.eq(1)]
        p_last = Signal(32, name="p_last")
        have_last = Signal(name="p_have_last")
        with m.If(ev):
            m.d.sync += [p_last.eq(fc.o_frame), have_last.eq(1)]

        m.d.comb += [
            Assume(~ResetSignal()),
            Assume(fc.c_number < SLOTS),
            Assume(fc.c_j < SLOT),
            Assume(~head | ~have_head | (fc.c_frame > p_head)),
        ]

        # the cycle before
        cv_d = Signal(name="p_cv_d")
        cf_d = Signal(32, name="p_cf_d")
        rs_d = Signal(name="p_restart_d")
        hd_d = Signal(name="p_head_d")
        hf_d = Signal(32, name="p_head_frame_d")
        m.d.sync += [cv_d.eq(fc.cur_valid), cf_d.eq(fc.cur_frame), rs_d.eq(fc.restart),
                     hd_d.eq(head & ~fc.restart), hf_d.eq(fc.c_frame)]
        # a restart, and no layout committed since (outside a restart's cycle)
        cleared = Signal(name="p_cleared")
        with m.If(fc.restart):
            m.d.sync += cleared.eq(1)
        with m.Elif(fc.lw_commit):
            m.d.sync += cleared.eq(0)

        m.d.comb += [
            Assert(~ev | ~have_last | (fc.o_frame > p_last), "frames reported in increasing order"),
            Assert(~(fc.o_done & fc.o_dropped), "done and dropped never together"),
            Assert(~ev | (cv_d & (fc.o_frame == cf_d)), "a report is of the frame open the cycle before"),
            Assert(~rs_d | (~fc.cur_valid & (fc.o_dropped == cv_d) & ~fc.o_done),
                   "a restart closes the open frame"),
            Assert(~hd_d | ((fc.o_dropped == cv_d) & ~fc.o_done & fc.cur_valid & (fc.cur_frame == hf_d)),
                   "a head drops the open frame and opens its own"),
            Assert(~(head & ~fc.restart & cleared) | ~fc.has_layout, "a restart drops the queued layouts"),
        ]
        # helper invariants
        last = Mux(ev, fc.o_frame, p_last)
        have = ev | have_last
        m.d.comb += [
            Assert(~fc.cur_valid | (have_head & (fc.cur_frame == p_head)), "model: the open frame is the last head's"),
            Assert(~(fc.cur_valid & have) | (last < fc.cur_frame), "model: reported frames below the open one"),
            Assert(~have | (have_head & (last <= p_head)), "model: reported frames at most the last head's"),
            Assert(~cleared | (t["q_valid"] == 0), "model: no layout queued since the restart"),
        ]

        m.d.comb += [
            Cover(rs_d & cv_d),
            Cover(hd_d & cv_d),
            Cover(fc.o_dropped & ~rs_d & ~hd_d),
            Cover(ev & have_last & (p_last + 1 == fc.o_frame)),
            Cover(head & ~fc.restart & fc.has_layout),
        ]
        return m
