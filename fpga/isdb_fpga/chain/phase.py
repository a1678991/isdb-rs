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

- **Slot ring.** The NCO's *input* x (2 x 6 bits), not its output r (2 x
  11), is delayed: three symbols per 36-bit word, a ring of 3 171 words
  (9 513 symbols >= 9 296 + 24 + 192: the plan's one slot of delay), 3 072
  of them in three RAMB36 (1K x 36) and the last 99 in a LUT RAM, so 3
  BRAM36 instead of 16 RAMB18. The reader re-applies the NCO: r =
  round_shift(x e^{-j phi_nco}, 11) with phi_nco recomputed as the NCO
  did (`nco.py`: phi advances by the carrier register f per symbol, and f
  changes only in a decision window: a head's carrier update applies from
  the head's symbol + 24 on), a pure function of phi and x, so r is the
  NCO's to the bit (the vectors' r are compared). Per head the writer's
  64-entry history (LUT RAM: the NCO phase and the ring position of each
  symbol written) gives phi_nco(a), f(a) = phi_nco(a + 1) - phi_nco(a) and
  the ring position of the head's symbol (a head's record comes 23 symbols
  after its symbol: the input is held for the decision), and the carrier
  register at the record (`h_f`, after the head's carrier update) is f
  from a + 24 on to the slot's end (the next update is the next head's).
  The reader runs behind: it starts reading slot a's first symbol once
  head b's slot setup is done (<= 75 cycles after head b's record), one
  symbol per clock, so it is never more than 9 296 + 24 + 75 symbols
  behind the writer, and the writer never overwrites an entry not yet
  read. `full` (to the input hold) guards a stalled reader (`o_ready`
  low) anyway: it is high while the writer is within 22 words (66
  symbols) of the oldest unread one.
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
  (27 bits): 4 + 4 DSP48E1 (the gain products split in two each); the
  NCO's rotation again four products of x (6 bits) by its table: 4 more.

Reader schedule per symbol (one per clock, `o_ready` permitting):

  R0  ring address, phi_j and G_j (registered from the accumulators),
      phi_nco
  N1  x out of the ring (the word's lane); the NCO table addressed by
      phi_nco
  N2  the NCO's four products
  R1  r (round_shift 11); the table addressed by phi_j (nearest entry)
  R2  c, s; the four products
  R3  u, w (round_shift 15)
  R4  u G_j, w G_j
  R5  y (round_shift 17, saturated): `o_valid` with the record
      (`o_at`, `o_j`, `o_r`, `o_phi`, `o_gain`, `o_y`, and head a's
      `o_word`, `o_numbered`, `o_number`, `o_frame`)
"""

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import (
    GAIN_FRAC, GAIN_NUM, INTERP_FRAC, MIN_AMP, ROT_SHIFT, SLOT, SLOT_RECIP, SLOTS, DivRound, SeqMul,
    cos_table, r_bits, round_shift, sat16,
)

AT_BITS = 48
RING_WORDS = 3171  # 3 symbols per word: 9 513 >= SLOT + 24 + 192
RING_MAIN = 3072  # in block RAM (1K x 36 each); the rest in LUT RAM
RING = 3 * RING_WORDS
HIST = 64  # the writer's history of (NCO phase, ring position)
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
        # the NCO stream: its input x and phase (the symbol's r is
        # recomputed after the delay)
        self.in_bits = in_bits
        self.i_valid = Signal()
        self.i_at = Signal(AT_BITS)
        self.i_x = [Signal(signed(in_bits), name=f"i_x{k}") for k in range(2)]
        self.i_phi = Signal(32)
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
        self.h_f = Signal(signed(32))  # the carrier register at the record
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
        xb = 2 * self.in_bits
        main = Memory(shape=3 * xb, depth=RING_MAIN, init=[])
        tail = Memory(shape=3 * xb, depth=128, init=[], attrs={"ram_style": "distributed"})
        hist = Memory(shape=32 + 12 + 2, depth=HIST, init=[], attrs={"ram_style": "distributed"})
        m.submodules.ring = main
        m.submodules.ring_tail = tail
        m.submodules.hist = hist
        wp = main.write_port()
        wpt = tail.write_port()
        hw = hist.write_port()
        hr0 = hist.read_port(domain="comb")
        hr1 = hist.read_port(domain="comb")
        ww = Signal(range(RING_WORDS))  # the word being filled
        wl = Signal(2)  # its next lane
        st = [Signal(xb, name=f"wst{k}") for k in range(2)]
        hc = Signal(6)  # the next history entry
        last_at = Signal(AT_BITS)  # index of the last symbol written
        xin = Signal(xb)
        m.d.comb += xin.eq((self.i_x[1].as_unsigned() << self.in_bits) | self.i_x[0].as_unsigned())
        word = Cat(st[0], st[1], xin)
        m.d.comb += [wp.addr.eq(Mux(ww < RING_MAIN, ww, 0)), wp.data.eq(word), wp.en.eq(self.i_valid & (wl == 2) & (ww < RING_MAIN)),
                     wpt.addr.eq((ww - RING_MAIN)[:7]), wpt.data.eq(word),
                     wpt.en.eq(self.i_valid & (wl == 2) & (ww >= RING_MAIN)),
                     hw.addr.eq(hc), hw.data.eq(Cat(self.i_phi, ww, wl)), hw.en.eq(self.i_valid)]
        with m.If(self.i_valid):
            m.d.sync += [last_at.eq(self.i_at), hc.eq(hc + 1)]
            with m.If(wl == 2):
                m.d.sync += [wl.eq(0), ww.eq(Mux(ww == RING_WORDS - 1, 0, ww + 1))]
            with m.Else():
                m.d.sync += wl.eq(wl + 1)
                for k in range(2):
                    with m.If(wl == k):
                        m.d.sync += st[k].eq(xin)

        # ---- head records -> slot setup ----------------------------------
        prev_ok = Signal()  # a head a is held whose slot can be corrected
        a_at = Signal(AT_BITS)
        a_w = Signal(range(RING_WORDS))
        a_l = Signal(2)
        a_phi = Signal(32)
        a_f0 = Signal(signed(32))
        a_f1 = Signal(signed(32))
        a_theta = Signal(32)
        a_gain = Signal(GAIN_BITS)
        a_word = Signal(2)
        a_numbered = Signal()
        a_number = Signal(range(SLOTS))
        a_frame = Signal(32)
        b_at = Signal(AT_BITS)
        b_w = Signal(range(RING_WORDS))
        b_l = Signal(2)
        b_phi = Signal(32)
        b_f0 = Signal(signed(32))
        b_f1 = Signal(signed(32))
        b_theta = Signal(32)
        b_word = Signal(2)
        b_numbered = Signal()
        b_number = Signal(range(SLOTS))
        b_frame = Signal(32)
        b_amp = Signal(26)
        # the slot queue (one pending entry)
        q_valid = Signal()
        q_at = Signal(AT_BITS)
        q_w = Signal(range(RING_WORDS))
        q_l = Signal(2)
        q_phi = Signal(32)
        q_f0 = Signal(signed(32))
        q_f1 = Signal(signed(32))
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
        # the head's symbol in the history (written 23 symbols before its
        # record; the late head 0, 9 296 before, is never corrected)
        dist = (last_at - self.h_at)[:6]
        m.d.comb += [hr0.addr.eq(hc - 1 - dist), hr1.addr.eq(hc - dist)]
        h_phi = hr0.data[:32]

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
                        b_w.eq(hr0.data[32:44]), b_l.eq(hr0.data[44:46]), b_phi.eq(h_phi),
                        b_f0.eq((hr1.data[:32] - h_phi)[:32].as_signed()), b_f1.eq(self.h_f),
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
                        q_valid.eq(1), q_at.eq(a_at), q_w.eq(a_w), q_l.eq(a_l), q_phi.eq(a_phi),
                        q_f0.eq(a_f0), q_f1.eq(a_f1), q_theta.eq(a_theta),
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
                    prev_ok.eq(1), a_at.eq(b_at), a_w.eq(b_w), a_l.eq(b_l), a_phi.eq(b_phi),
                    a_f0.eq(b_f0), a_f1.eq(b_f1), a_theta.eq(b_theta),
                    a_gain.eq(gdiv.q[:GAIN_BITS]), a_word.eq(b_word), a_numbered.eq(b_numbered),
                    a_number.eq(b_number), a_frame.eq(b_frame),
                ]
                m.next = "IDLE"

        # ---- reader --------------------------------------------------------
        busy = Signal()
        j = Signal(range(SLOT + 1))
        rw = Signal(range(RING_WORDS))
        rl = Signal(2)
        nphi = Signal(32)  # the NCO phase of symbol j
        s_f0 = Signal(signed(32))
        s_f1 = Signal(signed(32))
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
        rp = main.read_port()
        rpt = tail.read_port()
        m.d.comb += [rp.addr.eq(Mux(rw < RING_MAIN, rw, 0)), rp.en.eq(adv), rpt.addr.eq((rw - RING_MAIN)[:7]), rpt.en.eq(adv)]
        # R0 -> R1 registers
        v1 = Signal()
        at1 = Signal(AT_BITS)
        j1 = Signal(range(SLOT))
        phi1 = Signal(32)
        g1 = Signal(signed(GAIN_BITS))
        meta1 = Signal(2 + 1 + 7 + 32)
        nphi1 = Signal(32)
        rl1 = Signal(2)
        tail1 = Signal()
        m.d.sync += v1.eq(adv)
        with m.If(take):
            m.d.sync += [
                busy.eq(1), j.eq(0), rw.eq(q_w), rl.eq(q_l), nphi.eq(q_phi), s_f0.eq(q_f0), s_f1.eq(q_f1),
                s_at.eq(q_at), s_theta.eq(q_theta),
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
            m.d.sync += [j.eq(j + 1), rl.eq(Mux(rl == 2, 0, rl + 1)),
                         nphi.eq(nphi + Mux(j < 24, s_f0, s_f1)),
                         pd.eq(pd + s_delta), pg.eq(pg + s_gamma)]
            with m.If(rl == 2):
                m.d.sync += rw.eq(Mux(rw == RING_WORDS - 1, 0, rw + 1))
        with m.If(adv):
            m.d.sync += [
                at1.eq(s_at + j),
                j1.eq(j),
                phi1.eq(s_theta + round_shift(pd, INTERP_FRAC)[:32]),
                g1.eq(s_gain.as_signed() + round_shift(pg, INTERP_FRAC)),
                meta1.eq(s_word | (s_numbered << 2) | (s_number << 3) | (s_frame << 10)),
                nphi1.eq(nphi), rl1.eq(rl), tail1.eq(rw >= RING_MAIN),
            ]
        def stage(vals, n):
            regs = []
            for i, v in enumerate(vals):
                r = Signal.like(v, name=f"{v.name}_p{n}")
                m.d.sync += r.eq(v)
                regs.append(r)
            return regs

        # N1: x out of the ring; the NCO's table addressed by its phase
        ncos = Memory(shape=signed(16), depth=1024, init=self.table)
        m.submodules.ncos = ncos
        nrc = ncos.read_port()
        nrs = ncos.read_port()
        nidx = (nphi1 + (1 << 21))[22:32]
        m.d.comb += [nrc.addr.eq(nidx), nrs.addr.eq(nidx + 768)]
        wd = Mux(tail1, rpt.data, rp.data)
        xs = Signal(xb)
        m.d.comb += xs.eq(Mux(rl1 == 0, wd[:xb], Mux(rl1 == 1, wd[xb:2 * xb], wd[2 * xb:])))
        xn = [Signal(signed(self.in_bits), name=f"xn{k}") for k in range(2)]
        m.d.sync += [xn[0].eq(xs[:self.in_bits].as_signed()), xn[1].eq(xs[self.in_bits:].as_signed())]
        vA, atA, jA, phiA, gA, metaA = stage([v1, at1, j1, phi1, g1, meta1], "a")
        # N2: the NCO's products
        npw = self.in_bits + 16
        npp = [Signal(signed(npw), name=f"npp{k}") for k in range(4)]
        m.d.sync += [npp[0].eq(xn[0] * nrc.data), npp[1].eq(xn[1] * nrs.data),
                     npp[2].eq(xn[1] * nrc.data), npp[3].eq(xn[0] * nrs.data)]
        vB, atB, jB, phiB, gB, metaB = stage([vA, atA, jA, phiA, gA, metaA], "b")

        # R1: r; the table addressed by phi_j
        cos = Memory(shape=signed(16), depth=1024, init=self.table)
        m.submodules.cos = cos
        rc = cos.read_port()
        rs = cos.read_port()
        idx = (phiB + (1 << 21))[22:32]
        m.d.comb += [rc.addr.eq(idx), rs.addr.eq(idx + 768), rc.en.eq(1), rs.en.eq(1)]
        ri = round_shift(npp[0] + npp[1], ROT_SHIFT)
        rq = round_shift(npp[2] - npp[3], ROT_SHIFT)
        v2 = Signal()
        at2 = Signal(AT_BITS)
        j2 = Signal(range(SLOT))
        phi2 = Signal(32)
        g2 = Signal(signed(GAIN_BITS))
        meta2 = Signal(len(meta1))
        r2 = [Signal(signed(rb), name=f"r2_{k}") for k in range(2)]
        m.d.sync += [v2.eq(vB), at2.eq(atB), j2.eq(jB), phi2.eq(phiB), g2.eq(gB), meta2.eq(metaB),
                     r2[0].eq(ri), r2[1].eq(rq)]

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
        drained = setup.ongoing("IDLE") & ~rpend & ~q_valid & ~busy & ~v1 & ~vA & ~vB & ~v2 & ~v3 & ~v4 & ~v5
        m.d.sync += self.o_restart.eq(0)
        with m.If(opend & drained & ~self.restart):
            m.d.sync += [opend.eq(0), self.o_restart.eq(1)]

        # ---- the writer's guard against a stalled reader ------------------
        # entries written and not yet read: from the oldest unread (the
        # reader's address, or head a's while it waits) to the writer
        oldest = Mux(busy, rw, Mux(q_valid, q_w, a_w))
        used = Signal(signed(14))
        m.d.comb += used.eq(ww - oldest)
        m.d.comb += self.full.eq((busy | q_valid | prev_ok) &
                                 (Mux(used < 0, used + RING_WORDS, used) >= RING_WORDS - 22))
        return m
