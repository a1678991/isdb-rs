"""The frame bookkeeping of stages 4-6 (`isdbs3_phy::chain::frame::Back`:
`symbol`, `start`, `finish` and the layout queue), at the equaliser's
centre: every equalised symbol is routed by its slot position and the
frame it belongs to.

Rules (the model's):

- a symbol without a frame number (before the first frame head) is ignored;
- slot 0, position 0 starts frame f: an open frame is dropped (`o_dropped`),
  the layout queued for f becomes the one in force (queued layouts for
  frames before f are discarded);
- a symbol belongs to the open frame only if its frame number is the open
  frame's (`active`); `seen` counts them;
- slot 119, position 9 295 of the open frame ends it: complete when all
  1 115 520 symbols were seen (`o_done`), else dropped;
- the layout decoded from frame N's TMCC is queued for frame N + lag
  (`lw_*`, from the TMCC stage); a restart drops the open frame and the
  queue.

Layout memory: three banks of 120 slot entries (mode, modulation code, rate
code, dummy, the codeword's first main symbol in the frame and its symbol
count: 3 + 4 + 4 + 1 + 21 + 15 = 48 bits), one RAMB18 in all. A bank is
"in force" (taken at a frame start), "queued" (applies_to frame), or free;
the TMCC stage writes a free bank (`lw_bank` says which) and commits it.
With lag 2 at most one bank is in force, one queued and one being written.

Part decoding (Fig. 3.3-1): counters advanced with j (positions arrive in
order within a slot): the run position p (0 .. 139: 136 main, 4 TMCC) and
the main and TMCC indices of the slot. Outputs at the centre cycle
(combinational from the centre's meta and the registered state): `active`,
`has_layout`, `is_sync`, `is_pilot`, `is_main`, `is_tmcc`, `t` (sync /
pilot index), `mi` (main index in the frame), `ti` (TMCC index in the
frame). The slot's layout entry (`s_*`) is read at position 0 and valid
from position 1 on (the pilots, main and TMCC symbols use it).
"""

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal
from amaranth.lib.memory import Memory

from .common import FRAME, MAIN_RUN, PILOTS, SLOT, SLOTS, SYNC, TMCC_RUN

LAY_BITS = 48


def lay_pack(mode, mod, rate, dummy, start, symbols):
    return mode | mod << 3 | rate << 7 | dummy << 11 | start << 12 | symbols << 33


class FrameCtl(Elaboratable):
    def __init__(self):
        # the centre symbol
        self.cv = Signal()
        self.c_numbered = Signal()
        self.c_number = Signal(range(SLOTS))
        self.c_frame = Signal(32)
        self.c_j = Signal(range(SLOT))
        self.c_at = Signal(48)
        self.restart = Signal()
        # layout writes (TMCC stage)
        self.lw_bank = Signal(2)  # a free bank (output)
        self.lw_en = Signal()
        self.lw_slot = Signal(range(SLOTS))
        self.lw_data = Signal(LAY_BITS)
        self.lw_commit = Signal()
        self.lw_frame = Signal(32)  # the frame it applies to
        self.lw_from = Signal(32)  # the frame whose TMCC gave it
        # routing of the centre symbol (combinational)
        self.active = Signal()
        self.start = Signal()
        self.has_layout = Signal()
        self.layout_from = Signal(32)
        self.is_sync = Signal()
        self.is_pilot = Signal()
        self.is_main = Signal()
        self.is_tmcc = Signal()
        self.t = Signal(range(32))
        self.mi = Signal(21)
        self.ti = Signal(15)
        self.last = Signal()  # the frame's last symbol
        # the current slot's layout entry
        self.s_mode = Signal(3)
        self.s_mod = Signal(4)
        self.s_rate = Signal(4)
        self.s_dummy = Signal()
        self.s_start = Signal(21)
        self.s_symbols = Signal(15)
        # frame events (one cycle after the centre cycle)
        self.o_done = Signal()
        self.o_dropped = Signal()
        self.o_frame = Signal(32)
        self.o_at = Signal(48)
        self.o_has_layout = Signal()
        self.o_layout_from = Signal(32)
        # the open frame
        self.cur_valid = Signal()
        self.cur_frame = Signal(32)

    def elaborate(self, platform):
        m = Module()
        lay = Memory(shape=LAY_BITS, depth=4 * 128, init=[])
        m.submodules.lay = lay
        wp = lay.write_port()
        rp = lay.read_port()
        bank_force = Signal(2)
        q_valid = Signal(3)
        q_frame = [Signal(32, name=f"q_frame{b}") for b in range(3)]
        q_from = [Signal(32, name=f"q_from{b}") for b in range(3)]
        cur_at = Signal(48)
        cur_has = Signal()
        cur_from = Signal(32)
        seen = Signal(range(FRAME + 1))
        # a free bank: neither in force (while a frame with a layout is open) nor queued
        used = Signal(3)
        m.d.comb += used.eq(q_valid | Mux(self.cur_valid & cur_has, 1 << bank_force, 0))
        m.d.comb += self.lw_bank.eq(Mux(~used[0], 0, Mux(~used[1], 1, 2)))
        m.d.comb += [wp.addr.eq(Cat(self.lw_slot, self.lw_bank)), wp.data.eq(self.lw_data),
                     wp.en.eq(self.lw_en)]

        c = self
        first = c.cv & c.c_numbered & (c.c_number == 0) & (c.c_j == 0)
        m.d.comb += c.start.eq(first)
        # the layout queued for c_frame
        hit = [q_valid[b] & (q_frame[b] == c.c_frame) for b in range(3)]
        hit_any = hit[0] | hit[1] | hit[2]
        hit_bank = Mux(hit[0], 0, Mux(hit[1], 1, 2))
        hit_from = Mux(hit[0], q_from[0], Mux(hit[1], q_from[1], q_from[2]))
        open_now = Signal()
        m.d.comb += open_now.eq(Mux(first, 1, c.cur_valid & (c.cur_frame == c.c_frame)))
        m.d.comb += [
            c.active.eq(c.cv & c.c_numbered & open_now),
            c.has_layout.eq(Mux(first, hit_any, cur_has)),
            c.layout_from.eq(Mux(first, hit_from, cur_from)),
        ]
        # part counters (positions in order within a slot)
        p = Signal(range(MAIN_RUN + TMCC_RUN))
        mslot = Signal(range(SLOT))
        tslot = Signal(range(SLOT))
        kbase_m = Signal(21)
        kbase_t = Signal(15)
        j = c.c_j
        in_runs = j >= SYNC + PILOTS
        pp = Mux(j == SYNC + PILOTS, 0, p)
        mm = Mux(j == SYNC + PILOTS, 0, mslot)
        tt = Mux(j == SYNC + PILOTS, 0, tslot)
        is_m = in_runs & (pp < MAIN_RUN)
        m.d.comb += [
            c.is_sync.eq(j < SYNC),
            c.is_pilot.eq((j >= SYNC) & (j < SYNC + PILOTS)),
            c.is_main.eq(is_m),
            c.is_tmcc.eq(in_runs & ~(pp < MAIN_RUN)),
            c.t.eq(Mux(j < SYNC, j, j - SYNC)),
            c.mi.eq(kbase_m + mm),
            c.ti.eq(kbase_t + tt),
            c.last.eq((c.c_number == SLOTS - 1) & (j == SLOT - 1)),
        ]
        with m.If(c.cv & in_runs):
            m.d.sync += [
                p.eq(Mux(pp == MAIN_RUN + TMCC_RUN - 1, 0, pp + 1)),
                mslot.eq(mm + is_m),
                tslot.eq(tt + ~is_m),
            ]
        # slot entry read at position 0 (address: the bank in force, k)
        bank_now = Mux(first, hit_bank, bank_force)
        m.d.comb += [rp.addr.eq(Cat(c.c_number, bank_now)), rp.en.eq(c.cv & (j == 0))]
        rd = rp.data
        m.d.comb += [
            c.s_mode.eq(rd[0:3]), c.s_mod.eq(rd[3:7]), c.s_rate.eq(rd[7:11]),
            c.s_dummy.eq(rd[11]), c.s_start.eq(rd[12:33]), c.s_symbols.eq(rd[33:48]),
        ]
        with m.If(c.cv & (j == 0)):
            m.d.sync += [kbase_m.eq(c.c_number * (66 * MAIN_RUN)), kbase_t.eq(c.c_number * (66 * TMCC_RUN))]

        m.d.sync += [c.o_done.eq(0), c.o_dropped.eq(0)]
        with m.If(c.restart):
            with m.If(c.cur_valid):
                m.d.sync += [c.o_dropped.eq(1), c.o_frame.eq(c.cur_frame), c.o_at.eq(cur_at),
                             c.o_has_layout.eq(cur_has), c.o_layout_from.eq(cur_from)]
            m.d.sync += [c.cur_valid.eq(0), q_valid.eq(0)]
        with m.Else():
            with m.If(first):
                with m.If(c.cur_valid):
                    m.d.sync += [c.o_dropped.eq(1), c.o_frame.eq(c.cur_frame), c.o_at.eq(cur_at),
                                 c.o_has_layout.eq(cur_has), c.o_layout_from.eq(cur_from)]
                keep = Cat(*[q_valid[b] & (q_frame[b] > c.c_frame) for b in range(3)])
                m.d.sync += [
                    c.cur_valid.eq(1), c.cur_frame.eq(c.c_frame), cur_at.eq(c.c_at),
                    cur_has.eq(hit_any), cur_from.eq(hit_from), bank_force.eq(hit_bank),
                    seen.eq(1), q_valid.eq(keep),
                ]
            with m.Elif(c.active):
                m.d.sync += seen.eq(seen + 1)
                with m.If(c.last):
                    m.d.sync += [
                        c.cur_valid.eq(0),
                        c.o_done.eq(seen + 1 == FRAME), c.o_dropped.eq(seen + 1 != FRAME),
                        c.o_frame.eq(c.cur_frame), c.o_at.eq(cur_at),
                        c.o_has_layout.eq(cur_has), c.o_layout_from.eq(cur_from),
                    ]
            with m.If(self.lw_commit):
                for b in range(3):
                    with m.If(self.lw_bank == b):
                        m.d.sync += [q_valid[b].eq(1), q_frame[b].eq(self.lw_frame),
                                     q_from[b].eq(self.lw_from)]
        return m
