"""Stage 3 of the ISDB-S3 chain: each symbol divided by its slot's carrier
phase and amplitude, linearly interpolated between the slot's head and the
next head.

Contract: `isdbs3_phy::chain::phase::SlotPhase` and the module docs of
`isdbs3_phy::chain`, "Stage 3". For the slot of head a, corrected when head
b arrives (only if 9 296 symbols were held since head a: a restart, or the
late head 0 of stage 1, drops a slot):

- `d = theta_b - theta_a` (signed 32 bits), `Delta = round_shift(d * 14 784 741,
  21)`, `phi_j = theta_a + round_shift(Delta * j, 16)` (wrapping);
- `G = round(4 555 500 750 / max(amp, 64))` per head, `Gamma =
  round_shift((G_b - G_a) * 14 784 741, 21)`, `G_j = G_a + round_shift(Gamma *
  j, 16)`;
- `u = round_shift(r_i c + r_q s, 15)`, `w = round_shift(r_q c - r_i s, 15)`
  with (c, s) the cosine table at the nearest entry of phi_j;
- `y = sat16(round_shift(u G_j, 17))`, likewise for w: Q.10.

Implementation:

- **Slot ring.** Every NCO output r (with its chain index) is written into
  a ring of N = 9 296 + 24 + 192 = 9 512 entries (2 x 11 bits for 6-bit
  input: 209 kbit, the plan's one slot of delay). A head's ring address is
  the last write address minus (last index - head index). The reader runs
  behind: it starts reading slot a's first symbol once head b's slot setup
  is done (<= 75 cycles after head b's record), one symbol per clock, so it
  is never more than 9 296 + 24 + 75 symbols behind the writer, and the
  writer never overwrites an entry not yet read. `full` (to the input
  hold) guards a stalled reader (`o_ready` low) anyway: it is high while
  the writer is within 64 entries of the oldest unread one.
- **Slot setup** (once per head, sequential, no DSP): G_b by a restoring
  divider (33-bit numerator, 35 cycles), then Delta and Gamma by two
  bit-serial multiplies over the 24-bit constant (24 cycles each, in
  parallel), queued as the slot (theta_a, Delta, G_a, Gamma, ring address
  of head a, head a's word, number and frame). A one-entry queue holds the
  next slot while the reader finishes the current one.
- **The products per symbol** are exact running sums: `Delta * j` and
  `Gamma * j` accumulate Delta and Gamma (48-bit adders), so no multiplier
  for the interpolation; the rotation is four products of r (11 bits) by
  the table (16 bits) and the gain two products of u, w (12 bits) by G_j
  (27 bits): 4 + 4 DSP48E1 (the gain products split in two each).

Reader schedule per symbol (one per clock, `o_ready` permitting):

  R0  ring address, phi_j and G_j (registered from the accumulators)
  R1  r out of the ring; the table addressed by phi_j (nearest entry)
  R2  c, s; the four products
  R3  u, w (round_shift 15)
  R4  u G_j, w G_j
  R5  y (round_shift 17, saturated): `o_valid` with the record
      (`o_at`, `o_j`, `o_r`, `o_phi`, `o_gain`, `o_y`, and head a's
      `o_word`, `o_numbered`, `o_number`, `o_frame`)
"""

from amaranth.hdl import Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import (
    GAIN_FRAC, GAIN_NUM, INTERP_FRAC, MIN_AMP, SLOT, SLOT_RECIP, SLOTS, DivRound, SeqMul,
    cos_table, r_bits, round_shift, sat16,
)

AT_BITS = 48
RING = SLOT + 24 + 192
GAIN_BITS = 28  # G <= 4 555 500 750 / 64 < 2**27, signed
STEP_BITS = 40  # |Delta| <= 2**31 * 2**37 / 9 296 / 2**21 < 2**34; |Gamma| < 2**27


class SlotPhase(Elaboratable):
    def __init__(self, in_bits=6, table=None, snap_bits=1):
        self.snap_bits = snap_bits
        self.h_snap = Signal(snap_bits)  # the chain's status snapshot now (after head b's decision)
        # the snapshot taken with the last slot numbered 0 (a frame's status:
        # the model's state when that slot was corrected, which completes the
        # previous frame's last symbol at the FIR's centre)
        self.o_snap = Signal(snap_bits)
        self.rb = r_bits(in_bits)
        self.table = list(table) if table is not None else cos_table()
        # the NCO stream
        self.i_valid = Signal()
        self.i_at = Signal(AT_BITS)
        self.i_r = [Signal(signed(self.rb), name=f"i_r{k}") for k in range(2)]
        self.full = Signal()
        # head records (stage 1)
        self.h_valid = Signal()
        self.h_late = Signal()
        self.h_at = Signal(AT_BITS)
        self.h_word = Signal(2)
        self.h_theta = Signal(32)
        self.h_amp = Signal(signed(26))
        self.h_numbered = Signal()
        self.h_number = Signal(range(SLOTS))
        self.h_frame = Signal(32)
        self.restart = Signal()
        self.o_restart = Signal()  # the restart, in stream order after the slots before it
        # corrected symbols
        self.o_ready = Signal(init=1)
        self.o_valid = Signal()
        self.o_at = Signal(AT_BITS)
        self.o_j = Signal(range(SLOT))
        self.o_r = [Signal(signed(self.rb), name=f"o_r{k}") for k in range(2)]
        self.o_phi = Signal(32)
        self.o_gain = Signal(signed(GAIN_BITS))
        self.o_y = [Signal(signed(16), name=f"o_y{k}") for k in range(2)]
        self.o_word = Signal(2)
        self.o_numbered = Signal()
        self.o_number = Signal(range(SLOTS))
        self.o_frame = Signal(32)

    def elaborate(self, platform):
        m = Module()
        rb = self.rb

        # ---- ring writer -------------------------------------------------
        ring = Memory(shape=signed(2 * rb), depth=RING, init=[])
        m.submodules.ring = ring
        wp = ring.write_port()
        rp = ring.read_port()
        wa = Signal(range(RING))  # next write address
        last_at = Signal(AT_BITS)  # index of the last symbol written
        m.d.comb += [wp.addr.eq(wa), wp.data.eq((self.i_r[1] << rb) | self.i_r[0].as_unsigned()),
                     wp.en.eq(self.i_valid)]
        with m.If(self.i_valid):
            m.d.sync += [wa.eq(Mux(wa == RING - 1, 0, wa + 1)), last_at.eq(self.i_at)]

        # ---- head records -> slot setup ----------------------------------
        prev_ok = Signal()  # a head a is held whose slot can be corrected
        a_at = Signal(AT_BITS)
        a_addr = Signal(range(RING))
        a_theta = Signal(32)
        a_gain = Signal(GAIN_BITS)
        a_word = Signal(2)
        a_numbered = Signal()
        a_number = Signal(range(SLOTS))
        a_frame = Signal(32)
        b_at = Signal(AT_BITS)
        b_addr = Signal(range(RING))
        b_theta = Signal(32)
        b_word = Signal(2)
        b_numbered = Signal()
        b_number = Signal(range(SLOTS))
        b_frame = Signal(32)
        b_amp = Signal(26)
        # the slot queue (one pending entry)
        q_valid = Signal()
        q_at = Signal(AT_BITS)
        q_addr = Signal(range(RING))
        q_theta = Signal(32)
        q_delta = Signal(signed(STEP_BITS))
        q_gain = Signal(GAIN_BITS)
        q_gamma = Signal(signed(STEP_BITS))
        q_word = Signal(2)
        q_numbered = Signal()
        q_number = Signal(range(SLOTS))
        q_frame = Signal(32)
        q_snap = Signal(self.snap_bits)
        take = Signal()

        m.submodules.gdiv = gdiv = DivRound(33, 26)
        m.submodules.mdelta = mdelta = SeqMul(33, 24)
        m.submodules.mgamma = mgamma = SeqMul(GAIN_BITS + 1, 24)
        m.d.sync += [gdiv.start.eq(0), mdelta.start.eq(0), mgamma.start.eq(0)]
        dist = (last_at - self.h_at)[:16]
        h_addr = Signal(signed(17))
        m.d.comb += h_addr.eq(wa - 1 - dist)

        with m.If(take):
            m.d.sync += q_valid.eq(0)
        # A restart closes the open slot only: the slots whose two heads
        # came before it are the model's output already (it corrects a slot
        # inside the push that brings its second head), so the setup in
        # progress, the queue and the reader finish, and the restart goes
        # downstream after them (`o_restart`).
        rpend = Signal()
        opend = Signal()
        with m.If(self.restart):
            m.d.sync += [rpend.eq(1), opend.eq(1)]
        with m.FSM(name="setup") as setup:
            with m.State("IDLE"):
                with m.If(rpend):
                    m.d.sync += [prev_ok.eq(0), rpend.eq(0)]
                with m.Elif(self.h_valid & self.h_late):
                    m.d.sync += prev_ok.eq(0)
                with m.Elif(self.h_valid):
                    m.d.sync += [
                        b_at.eq(self.h_at),
                        b_addr.eq(Mux(h_addr < 0, h_addr + RING, h_addr)),
                        b_theta.eq(self.h_theta),
                        b_amp.eq(self.h_amp),
                        b_word.eq(self.h_word),
                        b_numbered.eq(self.h_numbered),
                        b_number.eq(self.h_number),
                        b_frame.eq(self.h_frame),
                        gdiv.n.eq(GAIN_NUM),
                        gdiv.d.eq(Mux(self.h_amp < MIN_AMP, MIN_AMP, self.h_amp)),
                        gdiv.start.eq(1),
                    ]
                    m.next = "GAIN"
            with m.State("GAIN"):
                with m.If(gdiv.done):
                    whole = prev_ok & ((b_at - a_at) == SLOT)
                    with m.If(whole):
                        m.d.sync += [
                            mdelta.a.eq((b_theta - a_theta)[:32].as_signed()),
                            mdelta.b.eq(SLOT_RECIP), mdelta.start.eq(1),
                            mgamma.a.eq(gdiv.q[:GAIN_BITS].as_signed() - a_gain.as_signed()),
                            mgamma.b.eq(SLOT_RECIP), mgamma.start.eq(1),
                        ]
                        m.next = "STEPS"
                    with m.Else():
                        m.next = "SHIFT"
            with m.State("STEPS"):
                with m.If(mdelta.done):
                    m.next = "QUEUE"
            with m.State("QUEUE"):
                with m.If(~q_valid | take):
                    m.d.sync += [
                        q_valid.eq(1), q_at.eq(a_at), q_addr.eq(a_addr), q_theta.eq(a_theta),
                        q_delta.eq(round_shift(mdelta.p, 37 - INTERP_FRAC)),
                        q_gain.eq(a_gain),
                        q_gamma.eq(round_shift(mgamma.p, 37 - INTERP_FRAC)),
                        q_word.eq(a_word), q_numbered.eq(a_numbered), q_number.eq(a_number),
                        q_frame.eq(a_frame), q_snap.eq(self.h_snap),
                    ]
                    m.next = "SHIFT"
            with m.State("SHIFT"):
                # head b becomes head a of the next slot
                m.d.sync += [
                    prev_ok.eq(1), a_at.eq(b_at), a_addr.eq(b_addr), a_theta.eq(b_theta),
                    a_gain.eq(gdiv.q[:GAIN_BITS]), a_word.eq(b_word), a_numbered.eq(b_numbered),
                    a_number.eq(b_number), a_frame.eq(b_frame),
                ]
                m.next = "IDLE"

        # ---- reader --------------------------------------------------------
        busy = Signal()
        j = Signal(range(SLOT + 1))
        ra = Signal(range(RING))
        s_at = Signal(AT_BITS)
        s_theta = Signal(32)
        s_delta = Signal(signed(STEP_BITS))
        s_gain = Signal(GAIN_BITS)
        s_gamma = Signal(signed(STEP_BITS))
        s_word = Signal(2)
        s_numbered = Signal()
        s_number = Signal(range(SLOTS))
        s_frame = Signal(32)
        s_snap = Signal(self.snap_bits)
        pd = Signal(signed(STEP_BITS + 15))  # Delta * j
        pg = Signal(signed(STEP_BITS + 15))  # Gamma * j
        adv = Signal()
        m.d.comb += [adv.eq(busy & self.o_ready), take.eq(q_valid & (~busy | (adv & (j == SLOT - 1))))]
        m.d.comb += [rp.addr.eq(ra), rp.en.eq(adv)]
        # R0 -> R1 registers
        v1 = Signal()
        at1 = Signal(AT_BITS)
        j1 = Signal(range(SLOT))
        phi1 = Signal(32)
        g1 = Signal(signed(GAIN_BITS))
        meta1 = Signal(2 + 1 + 7 + 32)
        m.d.sync += v1.eq(adv)
        with m.If(take):
            m.d.sync += [
                busy.eq(1), j.eq(0), ra.eq(q_addr), s_at.eq(q_at), s_theta.eq(q_theta),
                s_delta.eq(q_delta), s_gain.eq(q_gain), s_gamma.eq(q_gamma), s_word.eq(q_word),
                s_numbered.eq(q_numbered), s_number.eq(q_number), s_frame.eq(q_frame),
                pd.eq(0), pg.eq(0), s_snap.eq(q_snap),
            ]
        with m.Elif(adv):
            with m.If(j == SLOT - 1):
                m.d.sync += busy.eq(0)
        # a frame ends (in the model) in the push whose symbol completes the
        # FIR's 5 look-ahead after its last one: the next frame's slot 0,
        # corrected at the head after it, whose snapshot that is
        with m.If(take & q_numbered & (q_number == 0)):
            m.d.sync += self.o_snap.eq(q_snap)
        with m.If(adv & ~take):
            m.d.sync += [j.eq(j + 1), ra.eq(Mux(ra == RING - 1, 0, ra + 1)),
                         pd.eq(pd + s_delta), pg.eq(pg + s_gamma)]
        with m.If(adv):
            m.d.sync += [
                at1.eq(s_at + j),
                j1.eq(j),
                phi1.eq(s_theta + round_shift(pd, INTERP_FRAC)[:32]),
                g1.eq(s_gain.as_signed() + round_shift(pg, INTERP_FRAC)),
                meta1.eq(s_word | (s_numbered << 2) | (s_number << 3) | (s_frame << 10)),
            ]
        # R1: ring data, table address
        cos = Memory(shape=signed(16), depth=1024, init=self.table)
        m.submodules.cos = cos
        rc = cos.read_port()
        rs = cos.read_port()
        idx = (phi1 + (1 << 21))[22:32]
        m.d.comb += [rc.addr.eq(idx), rs.addr.eq(idx + 768), rc.en.eq(1), rs.en.eq(1)]
        rd = rp.data
        ri = rd[:rb].as_signed()
        rq = rd[rb:2 * rb].as_signed()
        v2 = Signal()
        at2 = Signal(AT_BITS)
        j2 = Signal(range(SLOT))
        phi2 = Signal(32)
        g2 = Signal(signed(GAIN_BITS))
        meta2 = Signal(len(meta1))
        r2 = [Signal(signed(rb), name=f"r2_{k}") for k in range(2)]
        m.d.sync += [v2.eq(v1), at2.eq(at1), j2.eq(j1), phi2.eq(phi1), g2.eq(g1), meta2.eq(meta1),
                     r2[0].eq(ri), r2[1].eq(rq)]

        def stage(vals, n):
            regs = []
            for i, v in enumerate(vals):
                r = Signal.like(v, name=f"{v.name}_p{n}")
                m.d.sync += r.eq(v)
                regs.append(r)
            return regs

        # R2: products
        pw = rb + 16
        p = [Signal(signed(pw), name=f"pp{k}") for k in range(4)]
        m.d.sync += [
            p[0].eq(r2[0] * rc.data), p[1].eq(r2[1] * rs.data),
            p[2].eq(r2[1] * rc.data), p[3].eq(r2[0] * rs.data),
        ]
        v3, at3, j3, phi3, g3, meta3, r3a, r3b = stage([v2, at2, j2, phi2, g2, meta2, r2[0], r2[1]], 3)
        # R3: u, w
        uw = rb + 2
        u = Signal(signed(uw))
        w = Signal(signed(uw))
        m.d.sync += [u.eq(round_shift(p[0] + p[1], 15)), w.eq(round_shift(p[2] - p[3], 15))]
        v4, at4, j4, phi4, g4, meta4, r4a, r4b = stage([v3, at3, j3, phi3, g3, meta3, r3a, r3b], 4)
        # R4: gain products
        yu = Signal(signed(uw + GAIN_BITS))
        yw = Signal(signed(uw + GAIN_BITS))
        m.d.sync += [yu.eq(u * g4), yw.eq(w * g4)]
        v5, at5, j5, phi5, g5, meta5, r5a, r5b = stage([v4, at4, j4, phi4, g4, meta4, r4a, r4b], 5)
        # R5: output
        m.d.sync += [
            self.o_valid.eq(v5),
            self.o_at.eq(at5), self.o_j.eq(j5), self.o_phi.eq(phi5), self.o_gain.eq(g5),
            self.o_r[0].eq(r5a), self.o_r[1].eq(r5b),
            self.o_y[0].eq(sat16(round_shift(yu, GAIN_FRAC))),
            self.o_y[1].eq(sat16(round_shift(yw, GAIN_FRAC))),
            self.o_word.eq(meta5[0:2]), self.o_numbered.eq(meta5[2]),
            self.o_number.eq(meta5[3:10]), self.o_frame.eq(meta5[10:42]),
        ]
        # the restart marker after the last symbol of the slots before it
        drained = setup.ongoing("IDLE") & ~rpend & ~q_valid & ~busy & ~v1 & ~v2 & ~v3 & ~v4 & ~v5
        m.d.sync += self.o_restart.eq(0)
        with m.If(opend & drained & ~self.restart):
            m.d.sync += [opend.eq(0), self.o_restart.eq(1)]

        # ---- the writer's guard against a stalled reader ------------------
        # entries written and not yet read: from the oldest unread (the
        # reader's address, or head a's while it waits) to the writer
        oldest = Mux(busy, ra, Mux(q_valid, q_addr, a_addr))
        used = Signal(signed(16))
        m.d.comb += used.eq(wa - oldest)
        m.d.comb += self.full.eq((busy | q_valid | prev_ok) &
                                 (Mux(used < 0, used + RING, used) >= RING - 64))
        return m
