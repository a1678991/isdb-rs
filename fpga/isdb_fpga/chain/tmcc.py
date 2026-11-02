"""Stage 5 of the ISDB-S3 chain: the TMCC soft bits, the frame's TMCC
codeword into the LDPC decoder, and the layout from the decoded TMCC.

Contract: `isdbs3_phy::chain::tmcc` and the module docs of
`isdbs3_phy::chain`, "Stage 5"; `isdbs3_core::tmcc::TmccInfo::parse`,
`allocation_ok` and `isdbs3_core::slots::layout`:

- TMCC symbol k of slot i is TMCC signal bit b = 264 i + k; its soft value
  `sat127(round_shift(z_i + z_q, 5))` at an even slot position,
  `sat127(round_shift(z_q - z_i, 5))` at an odd one;
- the shortened 61/120 word: bit b at position b + 1 870 (b < 9 614) or b +
  13 200, the known zeros +127 (filled by the decoder: `ldpc_if.py`);
- the TMCC of every whole frame is decoded (with or without a layout in
  force); its layout, when the TMCC's BCH succeeded on a converged word
  and the allocation is consistent (`allocation_ok`), is the one for
  frame + lag (2).

`TmccSoft` (the input side):

- **Staging buffer**: 31 680 x 8 bits (the sent positions in the model's
  TMCC order, one RAM of 7 RAMB36), written at TMCC symbol b of the open
  frame (the eq's output, `t_*`).
- **Burst**: when the frame ends whole (`frame_done`), OP_BEGIN on context
  1, then the 31 680 values in order b = 0 .. 31 679 (position
  `tmcc_position(b)`, one per beat, `in_n` = 1), then OP_DONE (rate 61,
  shortened, tag TMCC | frame). The port is shared with the demapper cycle
  by cycle (`grant`: the demapper has priority; the burst takes the cycles
  it leaves, at least the 320 sync, pilot and TMCC positions of every
  slot, so the 31 680 beats need at most 99 of the next frame's slots).
- **No overtaking**: the next frame's TMCC writes start at its slot 0,
  position 192 and advance 4 per 140 positions; the burst has read 56
  values by then (positions 0 .. 55 leave the port free) and reads at
  least 4 per 140 positions after, so the writer never reaches an unread
  value. `overrun` counts it if it ever did (status).
- Latency is free: the lag-2 rule needs frame N's layout only for frame
  N + 2.

`TmccLayout` (on the result side, fed by the finish stage with the decoded
information bits' first 200 bits, the change indicator and the eight
modes): `allocation_ok` (the used modes first, the rest exactly unused,
the slots summing to 120 in multiples of 5, (modulation, rate) strictly
decreasing), then the 120 slot entries (mode, modulation, rate, dummy =
slot j of its mode has j mod 5 >= the modulation's valid slots per unit,
the codeword's first main symbol, 44 880 / bits symbols) written into a
free bank of `FrameCtl` and committed for frame + lag; 1 slot per clock.
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed

from amaranth.lib.memory import Memory

from .common import LAYOUT_LAG, SLOTS, TMCC_SHIFT, round_shift, sat127
from .frame import LAY_BITS, lay_pack
from .ldpc_if import OP_BEGIN, OP_DONE, OP_WRITE, TAG_TMCC, TMCC_RATE_CODE
from .tables import lfsr_state

TMCC_BITS = 31680
TMCC_DATA = 9614
NULL_HEAD = 1870
NULL_TAIL = 11330
TMCC_RATE = 61


class TmccSoft(Elaboratable):
    def __init__(self, max_iter=50, offset=1):
        self.max_iter = max_iter
        self.offset = offset
        # TMCC symbols of the open frame (from the equaliser and FrameCtl)
        self.t_valid = Signal()
        self.t_z = [Signal(signed(16), name=f"t_z{k}") for k in range(2)]
        self.t_odd = Signal()
        self.t_idx = Signal(15)
        self.frame_done = Signal()
        self.frame = Signal(32)
        # the shared decoder port (request side)
        self.b_valid = Signal()
        self.b_op = Signal(2)
        self.b_pos = Signal(16)
        self.b_llr = Signal(signed(8))
        self.b_tag = Signal(64)
        self.grant = Signal()  # the beat is taken this cycle
        self.overrun = Signal(16)
        self.busy = Signal()
        # verification
        self.s_valid = Signal()
        self.s_idx = Signal(15)
        self.s_soft = Signal(signed(8))

    def elaborate(self, platform):
        m = Module()
        mem = Memory(
            shape=signed(8), depth=TMCC_BITS, init=[])
        m.submodules.stage = mem
        wp = mem.write_port()
        rp = mem.read_port()
        zi, zq = self.t_z
        s = Mux(self.t_odd, zq - zi, zi + zq)
        soft = Signal(signed(8))
        m.d.comb += soft.eq(sat127(round_shift(s, TMCC_SHIFT)))
        m.d.comb += [wp.addr.eq(Mux(self.t_valid, self.t_idx, 0)), wp.data.eq(soft), wp.en.eq(self.t_valid)]
        m.d.sync += [self.s_valid.eq(self.t_valid), self.s_idx.eq(self.t_idx), self.s_soft.eq(soft)]

        r = Signal(15)
        fr = Signal(32)
        take = self.grant
        with m.FSM(name="burst") as fsm:
            with m.State("IDLE"):
                with m.If(self.frame_done):
                    m.d.sync += [fr.eq(self.frame), r.eq(0), self.busy.eq(1)]
                    m.next = "BEGIN"
            with m.State("BEGIN"):
                m.d.comb += [self.b_valid.eq(1), self.b_op.eq(OP_BEGIN)]
                with m.If(take):
                    m.next = "WRITE"
            with m.State("WRITE"):
                m.d.comb += [
                    self.b_valid.eq(1), self.b_op.eq(OP_WRITE),
                    self.b_pos.eq(r + Mux(r < TMCC_DATA, NULL_HEAD, NULL_HEAD + NULL_TAIL)),
                    self.b_llr.eq(rp.data),
                ]
                with m.If(take):
                    m.d.sync += r.eq(r + 1)
                    with m.If(r == TMCC_BITS - 1):
                        m.next = "DONE"
            with m.State("DONE"):
                m.d.comb += [self.b_valid.eq(1), self.b_op.eq(OP_DONE)]
                with m.If(take):
                    m.d.sync += self.busy.eq(0)
                    m.next = "IDLE"
        m.d.comb += [
            self.b_tag.eq(Cat(Const(TAG_TMCC | TMCC_RATE_CODE << 8, 32), fr)),
            rp.addr.eq(Mux(fsm.ongoing("WRITE"), Mux(take & (r != TMCC_BITS - 1), r + 1, r), 0)),
            rp.en.eq(1),
        ]
        # the writer must never reach a value the burst has not read
        with m.If(self.t_valid & fsm.ongoing("WRITE") & (self.t_idx >= r)):
            m.d.sync += self.overrun.eq(self.overrun + 1)
        return m


VALID_PER_UNIT = {1: 1, 2: 2, 3: 3, 4: 4, 5: 5}
SYMBOLS_PER_CW = {1: 44880, 2: 22440, 3: 14960, 4: 11220, 5: 8976}


class TmccLayout(Elaboratable):
    """The 200 mode bits (change, then 8 x (modulation 4, rate 4, slots 8,
    obo 8), MSB first) in `bits` with `go` (one pulse, `valid` = the TMCC's
    BCH succeeded on a converged word, `frame` the TMCC's frame): writes the
    layout through the FrameCtl write port (`lw_*`) and commits it for
    frame + lag; `o_done` with `o_ok` (allocation_ok and valid)."""

    def __init__(self, lag=LAYOUT_LAG):
        self.lag = lag
        self.go = Signal()
        self.valid = Signal()
        self.bits = Signal(200)
        self.frame = Signal(32)
        self.lw_open = Signal()
        self.lw_en = Signal()
        self.lw_slot = Signal(range(SLOTS))
        self.lw_data = Signal(LAY_BITS)
        self.lw_commit = Signal()
        self.lw_frame = Signal(32)
        self.lw_from = Signal(32)
        self.o_done = Signal()
        self.o_ok = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()

        def field(off, n):
            # MSB first: bit `off` is the field's top bit
            return Cat(*[self.bits[off + n - 1 - k] for k in range(n)])

        mod = [field(8 + 24 * i, 4) for i in range(8)]
        rate = [field(12 + 24 * i, 4) for i in range(8)]
        slots = [field(16 + 24 * i, 8) for i in range(8)]
        obo = [field(24 + 24 * i, 8) for i in range(8)]
        used = [(slots[i] > 0) & (mod[i] >= 1) & (mod[i] <= 5) & (rate[i] >= 1) & (rate[i] <= 11)
                for i in range(8)]
        unused = [(mod[i] == 15) & (rate[i] == 15) & (slots[i] == 0) & (obo[i] == 0) for i in range(8)]
        n_used = sum(u.as_unsigned() for u in used)
        cond1 = Cat(*[(n_used > i) | unused[i] for i in range(8)]).all()
        total = sum(Mux(used[i], slots[i], 0) for i in range(8))
        mult5 = Cat(*[~used[i] | (slots[i] % 5 == 0) for i in range(8)]).all()
        order = Cat(*[~used[i + 1] | (mod[i] > mod[i + 1]) | ((mod[i] == mod[i + 1]) & (rate[i] > rate[i + 1]))
                      for i in range(7)]).all()
        ok = Signal()
        m.d.comb += ok.eq(cond1 & (total == SLOTS) & mult5 & order)

        mi = Signal(3)
        j = Signal(8)
        k = Signal(range(SLOTS + 1))
        u5 = Signal(3)
        sym = Signal(21)
        fr = Signal(32)
        cmod = Array(mod)[mi]
        crate = Array(rate)[mi]
        cslots = Array(slots)[mi]
        vpu = cmod  # valid slots per unit = the modulation code (1 .. 5)
        spc = Array(Const(SYMBOLS_PER_CW.get(c, 0), 16) for c in range(16))[cmod]
        dummy = u5 >= vpu
        m.d.sync += [self.lw_en.eq(0), self.lw_commit.eq(0), self.o_done.eq(0), self.lw_open.eq(0)]
        with m.FSM(name="layout"):
            with m.State("IDLE"):
                with m.If(self.go):
                    m.d.sync += [fr.eq(self.frame), mi.eq(0), j.eq(0), k.eq(0), u5.eq(0), sym.eq(0), self.lw_open.eq(self.valid & ok),
                                 self.busy.eq(1)]
                    with m.If(self.valid & ok):
                        m.next = "SLOT"
                    with m.Else():
                        m.d.sync += [self.o_done.eq(1), self.o_ok.eq(0), self.busy.eq(0)]
            with m.State("SLOT"):
                m.d.sync += [
                    self.lw_en.eq(1), self.lw_slot.eq(k[:7]),
                    self.lw_data.eq(lay_pack(mi, cmod, crate, dummy, sym, Mux(dummy, 0, spc))),
                    sym.eq(sym + Mux(dummy, 0, spc)), k.eq(k + 1),
                    u5.eq(Mux(u5 == 4, 0, u5 + 1)),
                ]
                with m.If(j + 1 == cslots):
                    m.d.sync += [mi.eq(mi + 1), j.eq(0), u5.eq(0)]
                with m.Else():
                    m.d.sync += j.eq(j + 1)
                with m.If(k == SLOTS - 1):
                    m.next = "COMMIT"
            with m.State("COMMIT"):
                m.d.sync += [self.lw_commit.eq(1), self.lw_frame.eq(fr + self.lag), self.lw_from.eq(fr),
                             self.o_done.eq(1), self.o_ok.eq(1), self.busy.eq(0)]
                m.next = "IDLE"
        return m


class Realign(Elaboratable):
    """Bits [skip, skip + take) of a stream of 16-bit beats (position
    order: bit b of beat q = position 16 q + b) as 16-bit beats again, after
    `lead` zeros (the BCH unit's alignment), the last beat zero-padded.
    `start` with the parameters; then `i_valid` beats with their index
    `i_q`; `o_valid`, `o_bits`, `o_last`. One beat in and at most one out
    per clock; the bits left over wait in a 32-bit accumulator and leave
    on an idle input cycle."""

    def __init__(self):
        self.start = Signal()
        self.skip = Signal(16)
        self.take = Signal(16)
        self.lead = Signal(4)
        self.i_valid = Signal()
        self.i_bits = Signal(16)
        self.i_q = Signal(12)
        self.o_valid = Signal()
        self.o_bits = Signal(16)
        self.o_last = Signal()

    def elaborate(self, platform):
        m = Module()
        acc = Signal(32)
        nacc = Signal(6)
        left = Signal(17)  # bits still to output (lead + take)
        skip = Signal(16)
        end = Signal(17)
        with m.If(self.start):
            m.d.sync += [acc.eq(0), nacc.eq(self.lead), left.eq(self.lead + self.take),
                         skip.eq(self.skip), end.eq(self.skip + self.take)]
        base = Signal(17)
        m.d.comb += base.eq(self.i_q << 4)
        lo = Signal(5)
        hi = Signal(5)
        m.d.comb += [
            lo.eq(Mux(skip > base, Mux(skip - base > 16, 16, skip - base), 0)),
            hi.eq(Mux(end > base, Mux(end - base > 16, 16, end - base), 0)),
        ]
        n_in = Signal(5)
        m.d.comb += n_in.eq(Mux(hi > lo, hi - lo, 0))
        part = Signal(16)
        m.d.comb += part.eq((self.i_bits >> lo) & ((Const(1, 17) << n_in) - 1)[:16])
        merged = Signal(48)
        m.d.comb += merged.eq(acc | (part << nacc))
        tot = Signal(7)
        m.d.comb += tot.eq(nacc + n_in)
        m.d.sync += [self.o_valid.eq(0), self.o_last.eq(0)]
        with m.If(~self.start & (left > 0)):
            with m.If(self.i_valid):
                with m.If((tot >= 16) | ((tot > 0) & (tot >= left))):
                    m.d.sync += [self.o_valid.eq(1), self.o_bits.eq(merged[:16]),
                                 self.o_last.eq(left <= 16), left.eq(Mux(left > 16, left - 16, 0)),
                                 acc.eq(merged[16:48]), nacc.eq(Mux(tot >= 16, tot - 16, 0))]
                with m.Else():
                    m.d.sync += [acc.eq(merged[:32]), nacc.eq(tot)]
            with m.Elif((nacc >= 16) | ((nacc > 0) & (nacc >= left))):
                m.d.sync += [self.o_valid.eq(1), self.o_bits.eq(acc[:16]), self.o_last.eq(left <= 16),
                             left.eq(Mux(left > 16, left - 16, 0)), acc.eq(acc >> 16),
                             nacc.eq(Mux(nacc >= 16, nacc - 16, 0))]
        return m


class Prbs15(Elaboratable):
    """The X^15 + X^14 + 1 generator (last stage out), 16 output bits per
    `step` (bit b = the b-th in order; `count` 1 .. 16 advances that many)."""

    def __init__(self, init):
        self.init = init
        self.reset = Signal()
        self.step = Signal()
        self.count = Signal(5, init=16)
        self.bits = Signal(16)

    def elaborate(self, platform):
        m = Module()
        reg = Signal(15, init=lfsr_state(self.init))
        cur = [reg[i] for i in range(15)]
        after = [cur]
        outs = []
        for _ in range(16):
            outs.append(cur[14])
            fb = cur[13] ^ cur[14]
            cur = [fb] + cur[:14]
            after.append(cur)
        m.d.comb += self.bits.eq(Cat(*outs))
        with m.If(self.reset):
            m.d.sync += reg.eq(lfsr_state(self.init))
        with m.Elif(self.step):
            with m.Switch(self.count):
                for c in range(1, 17):
                    with m.Case(c):
                        m.d.sync += reg.eq(Cat(*after[c]))
        return m


INFO_BITS = 9422
INFO_BEATS = (INFO_BITS + 15) // 16  # 589


class TmccResult(Elaboratable):
    """The TMCC's LDPC result into its information bits: the decoder's
    first 22 814 hard decisions (`r_*`: a header with iterations and
    convergence, then 1 426 beats) -> data bits 1 870 .. 11 483 (`Realign`)
    -> the TMCC dispersal (X^15 + X^14 + 1 from 100000000001110) -> a
    601-beat buffer, and realigned by 2 leading zeros into the BCH unit
    (`bch_*`; the finish stage owns it and shares it with the slots: the
    results arrive one at a time) -> after the BCH the 9 422 information
    bits out (`o_*`, 589 beats; corrected when the BCH succeeded, whatever
    the LDPC did, as `chain::tmcc::decode`; the BCH result itself counts
    only for a converged word: `o_bch_ok`), the first 200 into
    `TmccLayout`."""

    def __init__(self, lag=LAYOUT_LAG):
        self.layout = TmccLayout(lag)
        self.r_valid = Signal()
        self.r_ready = Signal()
        self.r_head = Signal()
        self.r_bits = Signal(16)
        self.r_last = Signal()
        self.r_iterations = Signal(8)
        self.r_converged = Signal()
        self.frame = Signal(32)  # the TMCC's frame (with the header)
        # the BCH unit (outside)
        self.bch_start = Signal()
        self.bch_valid = Signal()
        self.bch_bits = Signal(16)
        self.bch_last = Signal()
        self.bch_done = Signal()
        self.bch_ok = Signal()
        self.bch_corrected = Signal(4)
        self.bch_q = Signal(12)
        self.bch_mask = Signal(16)
        # out
        self.o_head = Signal()
        self.o_frame = Signal(32)
        self.o_iter = Signal(8)
        self.o_conv = Signal()
        self.o_bch_ok = Signal()
        self.o_bch = Signal(4)
        self.o_valid = Signal()
        self.o_bits = Signal(16)
        self.o_last = Signal()
        self.busy = Signal()
        self.o_go = Signal(init=1)  # in: stream the information bits now

    def elaborate(self, platform):
        m = Module()
        m.submodules.layout = lay = self.layout
        m.submodules.ext = ext = Realign()
        m.submodules.al = al = Realign()
        m.submodules.prbs = prbs = Prbs15("100000000001110")
        buf = Memory(shape=16, depth=608, init=[])
        m.submodules.buf = buf
        wp = buf.write_port()
        rp = buf.read_port()
        q = Signal(12)
        nout = Signal(10)
        iters = Signal(8)
        conv = Signal()
        fr = Signal(32)
        dis = Signal(16)
        m.d.comb += [
            dis.eq(ext.o_bits ^ prbs.bits),
            ext.skip.eq(NULL_HEAD), ext.take.eq(TMCC_DATA), ext.lead.eq(0),
            al.skip.eq(0), al.take.eq(TMCC_DATA), al.lead.eq((-TMCC_DATA) % 16),
            ext.i_bits.eq(self.r_bits), ext.i_q.eq(q),
            prbs.step.eq(ext.o_valid), prbs.count.eq(Mux(ext.o_last, TMCC_DATA % 16, 16)),
            wp.addr.eq(nout), wp.data.eq(dis), wp.en.eq(ext.o_valid),
            al.i_valid.eq(ext.o_valid), al.i_bits.eq(dis), al.i_q.eq(nout),
            self.bch_valid.eq(al.o_valid), self.bch_bits.eq(al.o_bits), self.bch_last.eq(al.o_last),
        ]
        with m.If(ext.o_valid):
            m.d.sync += nout.eq(nout + 1)
        m.d.sync += [
                     lay.go.eq(0), self.o_head.eq(0), self.o_valid.eq(0), self.o_last.eq(0)]
        rq = Signal(10)
        okl = Signal()
        modes = Signal(208)
        # the BCH may finish before the stream does (the data bits end at
        # beat 717 of 1 426): its result is latched
        bdone = Signal()
        bok_l = Signal()
        bcor_l = Signal(4)
        with m.If(self.bch_done):
            m.d.sync += [bdone.eq(1), bok_l.eq(self.bch_ok), bcor_l.eq(self.bch_corrected)]
        bok = Mux(self.bch_done, self.bch_ok, bok_l)
        bcor = Mux(self.bch_done, self.bch_corrected, bcor_l)
        with m.FSM(name="tmcc_result"):
            with m.State("IDLE"):
                m.d.comb += self.r_ready.eq(1)
                with m.If(self.r_valid & self.r_head):
                    m.d.sync += [iters.eq(self.r_iterations), conv.eq(self.r_converged), fr.eq(self.frame),
                                 q.eq(0), nout.eq(0), self.busy.eq(1)]
                    # the next cycle may bring the first beat: start now
                    m.d.comb += [ext.start.eq(1), al.start.eq(1), prbs.reset.eq(1), self.bch_start.eq(1)]
                    m.next = "BITS"
            with m.State("BITS"):
                m.d.comb += [self.r_ready.eq(1), ext.i_valid.eq(self.r_valid)]
                with m.If(self.r_valid):
                    m.d.sync += q.eq(q + 1)
                    with m.If(self.r_last):
                        m.next = "WAIT_BCH"
            with m.State("WAIT_BCH"):
                with m.If(bdone | self.bch_done):
                    m.d.sync += [okl.eq(bok), rq.eq(0), bdone.eq(0),
                                 self.o_head.eq(1), self.o_frame.eq(fr), self.o_iter.eq(iters),
                                 self.o_conv.eq(conv), self.o_bch_ok.eq(bok & conv),
                                 self.o_bch.eq(bcor)]
                    m.next = "HOLD"
            with m.State("HOLD"):
                # the consumer (the finish stage) sends the block's first words
                with m.If(self.o_go):
                    m.next = "READ"
            with m.State("READ"):
                m.d.comb += rp.addr.eq(rq)
                m.d.sync += rq.eq(rq + 1)
                m.next = "OUT"
            with m.State("OUT"):
                # the read port has beat rq - 1; its flips from the BCH unit
                m.d.comb += [rp.addr.eq(rq), self.bch_q.eq(rq - 1)]
                v = Signal(16)
                m.d.comb += v.eq(rp.data ^ Mux(okl, self.bch_mask, 0))
                last = rq - 1 == INFO_BEATS - 1
                m.d.sync += [self.o_valid.eq(1),
                             self.o_bits.eq(Mux(last, v & ((1 << (INFO_BITS % 16)) - 1), v)),
                             self.o_last.eq(last), rq.eq(rq + 1)]
                for b in range(13):
                    with m.If(rq - 1 == b):
                        m.d.sync += modes[16 * b:16 * b + 16].eq(v)
                with m.If(last):
                    m.next = "LAYOUT"
            with m.State("LAYOUT"):
                m.d.sync += [lay.go.eq(1), lay.valid.eq(okl & conv), lay.bits.eq(modes[:200]),
                             lay.frame.eq(fr)]
                m.next = "LAYOUT_WAIT"
            with m.State("LAYOUT_WAIT"):
                with m.If(lay.o_done):
                    m.d.sync += self.busy.eq(0)
                    m.next = "IDLE"
        return m
