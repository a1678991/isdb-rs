"""Shared pieces of the ISDB-S3 chain HDL (stages 1-6 and 8-9 of
docs/plans/isdbs3-fpga-rx.md): the model's constants, the rounding helpers
and the small sequential arithmetic units the slow paths (once per slot
head, once per frame) share instead of spending DSP slices.

The reference is the Rust model `isdbs3_phy::chain` (its module docs are
the specification). Every constant here is the model's; the tests check
them against the `"chain"` object every vectors file carries.
"""

import math

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed, unsigned

# --- the model's constants (isdbs3_phy::chain, isdbs3_core::params) --------
SLOT = 9296  # modulation slot, symbols (Fig. 3.3-1)
SLOTS = 120
FRAME = SLOT * SLOTS  # 1 115 520
SYNC = 24  # sync symbols per slot
PILOTS = 32
MAIN_RUN, TMCC_RUN = 136, 4
LEAK_SHIFT = 3
LOCK_RATIO = 3
LOCK_PERIODS = 4
LOSS_PERIODS = 8
MAJORITY_MAX = 8
MAJORITY_MIN = 2
FRAME_MISS_LIMIT = 2
ROT_SHIFT = 11
CARRIER_MAX = (1 << 19) - 1
CARRIER_STEPS = 60
CARRIER_GAIN = 246_412
CARRIER_SHIFT = 39
CORDIC_ATAN = [
    536_870_912, 316_933_406, 167_458_907, 85_004_756, 42_667_331, 21_354_465,
    10_679_838, 5_340_245, 2_670_163, 1_335_087, 667_544, 333_772, 166_886,
    83_443, 41_722, 20_861,
]
CORDIC_INV_GAIN = 39_797
CORDIC_PRESCALE = 8
SLOT_RECIP = 14_784_741
INTERP_FRAC = 16
GAIN_NUM = 4_555_500_750
GAIN_FRAC = 17
MIN_AMP = 64
TAP_FRAC = 14
TAPS = 11
SYNC_REF = 724
EQ_APPLY_FRAMES = 2
TMCC_SHIFT = 5
KNOWN_ZERO = 127
LLR_SHIFT = 32
N0_MIN = 256
N0_DEFAULT = 52_556
NOISE_STEP = 16
LAYOUT_LAG = 2
FSYNC = 0x52F866
SSYNC = 0x36715A

# Word codes of a head record (the vectors' `word`).
W_SSYNC, W_FSYNC, W_NFSYNC = 0, 1, 2

# Vivado synthesis attribute: map an adder to fabric, not a DSP48E1 (as
# the front end does).
NO_DSP = {"use_dsp": "no"}


def round_half_away(v):
    """Rust's f64::round (halves away from zero)."""
    return int(math.floor(v + 0.5)) if v >= 0 else -int(math.floor(-v + 0.5))


def cos_table():
    """`isdb_frontend::tables::cos_table`: round(32767 cos(2 pi k / 1024))."""
    return [round_half_away(32767 * math.cos(2 * math.pi * k / 1024)) for k in range(1024)]


def word_symbols(word):
    """`chain::sync::word_symbols`: the 24 sync symbols of `word` (MSB
    first) as (+-1, +-1): bit 0 at 45 deg, bit 1 at -135 deg, odd k turned
    by 90 deg."""
    out = []
    for k in range(SYNC):
        bit = (word >> (SYNC - 1 - k)) & 1
        if k % 2 == 0:
            out.append((1, 1) if bit == 0 else (-1, -1))
        else:
            out.append((-1, 1) if bit == 0 else (1, -1))
    return out


def model_constants():
    """The constants under the names of the vectors' "chain" object."""
    return {
        "slot": SLOT, "slots": SLOTS, "leak_shift": LEAK_SHIFT, "lock_ratio": LOCK_RATIO,
        "lock_periods": LOCK_PERIODS, "loss_periods": LOSS_PERIODS,
        "majority_max": MAJORITY_MAX, "majority_min": MAJORITY_MIN,
        "frame_miss_limit": FRAME_MISS_LIMIT, "rot_shift": ROT_SHIFT,
        "carrier_max": CARRIER_MAX, "carrier_steps": CARRIER_STEPS,
        "carrier_gain": CARRIER_GAIN, "carrier_shift": CARRIER_SHIFT,
        "cordic_atan": CORDIC_ATAN, "cordic_inv_gain": CORDIC_INV_GAIN,
        "cordic_prescale": CORDIC_PRESCALE, "slot_recip": SLOT_RECIP,
        "interp_frac": INTERP_FRAC, "gain_num": GAIN_NUM, "gain_frac": GAIN_FRAC,
        "min_amp": MIN_AMP, "tap_frac": TAP_FRAC, "sync_ref": SYNC_REF,
        "eq_apply_frames": EQ_APPLY_FRAMES, "tmcc_shift": TMCC_SHIFT,
        "known_zero": KNOWN_ZERO, "llr_shift": LLR_SHIFT, "n0_min": N0_MIN,
        "n0_default": N0_DEFAULT, "noise_step": NOISE_STEP, "layout_lag": LAYOUT_LAG,
        "fsync": [v for s in word_symbols(FSYNC) for v in s],
        "ssync": [v for s in word_symbols(SSYNC) for v in s],
        "cos_table": cos_table(),
    }


def r_bits(in_bits):
    """Width of the NCO output r for `in_bits`-bit input components:
    |r| <= 2**(in_bits - 1) * sqrt(2) * 32767 / 2**11 + 1 < 2**(in_bits + 4),
    so in_bits + 5 bits signed (11 for the PlutoSky's 6-bit symbols, 13 for
    8: the model's "13 bits signed")."""
    return in_bits + 5


# --- expression helpers ----------------------------------------------------

def round_shift(v, s):
    """round(v / 2**s) for a constant s: add half, arithmetic shift
    (`fx::round_shift`)."""
    if s == 0:
        return v
    return (v + (1 << (s - 1))) >> s


def sat(v, lo, hi):
    """Clamp `v` to [lo, hi]."""
    return Mux(v > hi, hi, Mux(v < lo, lo, v))


def sat16(v):
    return sat(v, -(1 << 15), (1 << 15) - 1)


def sat127(v):
    return sat(v, -127, 127)


def absv(v):
    """|v| of a signed value (unsigned, as wide as v)."""
    return Mux(v < 0, -v, v).as_unsigned()[: len(v)]


def mag_approx(re, im):
    """`fx::mag_approx`: max(|re|, |im|) + (min(|re|, |im|) >> 1)."""
    a, b = absv(re), absv(im)
    return Mux(a > b, a, b) + (Mux(a > b, b, a) >> 1)


# --- the 4x clock -------------------------------------------------------------

class Phase4(Elaboratable):
    """The phase of the `fast` domain (4x the symbol clock `sync`, phase-
    aligned: every edge of `sync` is an edge of `fast`; fpga/tezuka/
    isdb_slots.xdc): `phase` is 0 in the fast cycle after the common edge,
    then 1, 2, 3; `late` is the phase one fast cycle late (3, 0, 1, 2: for a
    block whose symbol-clock inputs cross through a fast register, which
    holds a symbol clock's values in the fast cycles 1, 2, 3 and the next
    0). Both are fast registers (nothing of the symbol clock's in their
    paths): a counter, kept in step by a toggle of the symbol clock that
    the fast domain registers twice (the two registers differ in phase 1
    only); it counts from 3, so it is right from the first edge on (both
    clocks start with a common edge), and a wrong start is corrected in the
    first symbol clock."""

    def __init__(self):
        self.phase = Signal(2)
        self.late = Signal(2)

    def elaborate(self, platform):
        m = Module()
        tg = Signal()
        tq = Signal()
        tq2 = Signal()
        ph = Signal(2, init=3)
        m.d.sync += tg.eq(~tg)
        m.d.fast += [tq.eq(tg), tq2.eq(tq), ph.eq(Mux(tq ^ tq2, 2, ph + 1)), self.late.eq(ph)]
        m.d.comb += self.phase.eq(ph)
        return m


# --- sequential units -------------------------------------------------------

class SeqMul(Elaboratable):
    """p = a * b, bit-serial over b (shift and add): `start` samples a and
    b; `len(b)` cycles later `done` pulses for one cycle with `p` valid
    (held until the next start). One adder, no DSP. b is two's complement
    when `b_signed` (its top bit's term subtracted)."""

    def __init__(self, wa, wb, b_signed=False):
        self.wa, self.wb, self.b_signed = wa, wb, b_signed
        self.start = Signal()
        self.a = Signal(signed(wa))
        self.b = Signal(signed(wb) if b_signed else unsigned(wb))
        self.p = Signal(signed(wa + wb))
        self.done = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()
        w = self.wa + self.wb
        sh = Signal(signed(w))
        bb = Signal(self.wb)
        i = Signal(range(self.wb + 1))
        m.d.sync += self.done.eq(0)
        with m.If(self.start):
            m.d.sync += [self.p.eq(0), sh.eq(self.a), bb.eq(self.b), i.eq(0), self.busy.eq(1)]
        with m.Elif(self.busy):
            term = Mux(bb[0], sh, 0)
            last = i == self.wb - 1
            if self.b_signed:
                m.d.sync += self.p.eq(Mux(last, self.p - term, self.p + term))
            else:
                m.d.sync += self.p.eq(self.p + term)
            m.d.sync += [sh.eq(sh << 1), bb.eq(bb >> 1), i.eq(i + 1)]
            with m.If(last):
                m.d.sync += [self.busy.eq(0), self.done.eq(1)]
        return m


class SeqDiv(Elaboratable):
    """Unsigned restoring division q = n // d, r = n % d: `start` samples n
    and d; `wn` cycles later `done` pulses with q and r valid (held). d = 0
    gives all ones (not used: callers clamp d)."""

    def __init__(self, wn, wd):
        self.wn, self.wd = wn, wd
        self.start = Signal()
        self.n = Signal(wn)
        self.d = Signal(wd)
        self.q = Signal(wn)
        self.r = Signal(wd)
        self.done = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()
        rem = Signal(self.wd + 1)
        dd = Signal(self.wd)
        i = Signal(range(self.wn + 1))
        m.d.sync += self.done.eq(0)
        with m.If(self.start):
            m.d.sync += [self.q.eq(self.n), rem.eq(0), dd.eq(self.d), i.eq(0), self.busy.eq(1)]
        with m.Elif(self.busy):
            r2 = Cat(self.q[-1], rem[: self.wd])
            ge = r2 >= dd
            m.d.sync += [
                rem.eq(Mux(ge, r2 - dd, r2)),
                self.q.eq(Cat(ge, self.q[:-1])),
                i.eq(i + 1),
            ]
            with m.If(i == self.wn - 1):
                m.d.sync += [self.busy.eq(0), self.done.eq(1)]
        m.d.comb += self.r.eq(rem[: self.wd])
        return m


class SeqDivR(Elaboratable):
    """Unsigned restoring division q = n // d with `k` quotient bits per
    clock (k chained compare-subtract steps): ceil(wn / k) cycles after
    `start` `done` pulses with q valid (held). For the frame-boundary
    divisions that must be short (demap.py: n0 and its reciprocal)."""

    def __init__(self, wn, wd, k=3):
        self.wn = -(-wn // k) * k
        self.wd, self.k = wd, k
        self.start = Signal()
        self.n = Signal(wn)
        self.d = Signal(wd)
        self.q = Signal(self.wn)
        self.done = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()
        rem = Signal(self.wd + 1)
        dd = Signal(self.wd)
        steps = self.wn // self.k
        i = Signal(range(steps + 1))
        m.d.sync += self.done.eq(0)
        with m.If(self.start):
            m.d.sync += [self.q.eq(self.n), rem.eq(0), dd.eq(self.d), i.eq(0), self.busy.eq(1)]
        with m.Elif(self.busy):
            r, q = rem, self.q
            for _ in range(self.k):
                r2 = Cat(q[-1], r[: self.wd])
                ge = r2 >= dd
                r = Mux(ge, r2 - dd, r2)[: self.wd + 1]
                q = Cat(ge, q[:-1])
            m.d.sync += [rem.eq(r), self.q.eq(q), i.eq(i + 1)]
            with m.If(i == steps - 1):
                m.d.sync += [self.busy.eq(0), self.done.eq(1)]
        return m


class DivRound(Elaboratable):
    """`fx::div_round` for a > 0 numerators: round(n / d) = floor((2n + d) /
    2d) on `SeqDiv` (wn + 2 cycles after `start`, `done` pulses, `q` held).
    n, d unsigned; d > 0."""

    def __init__(self, wn, wd):
        self.wn, self.wd = wn, wd
        self.start = Signal()
        self.n = Signal(wn)
        self.d = Signal(wd)
        self.q = Signal(wn)
        self.done = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.div = div = SeqDiv(self.wn + 2, self.wd + 1)
        m.d.comb += [
            div.start.eq(self.start),
            div.n.eq((self.n << 1) + self.d),
            div.d.eq(self.d << 1),
            self.q.eq(div.q),
            self.done.eq(div.done),
            self.busy.eq(div.busy),
        ]
        return m


class Cordic(Elaboratable):
    """`fx::cordic`: 16 vectoring iterations, one per clock, then the
    magnitude by the gain compensation 39 797 (Q0.16) bit-serially
    (`SeqMul`, 16 cycles) and round_shift 24. Inputs |re|, |im| < 2**22.
    `done` pulses 35 cycles after `start` (theta and mag held after)."""

    IN_BITS = 23

    def __init__(self):
        self.start = Signal()
        self.re = Signal(signed(self.IN_BITS))
        self.im = Signal(signed(self.IN_BITS))
        self.theta = Signal(32)
        self.mag = Signal(signed(26))
        self.done = Signal()
        self.busy = Signal()

    def elaborate(self, platform):
        m = Module()
        W = 34
        x = Signal(signed(W))
        y = Signal(signed(W))
        z = Signal(32)
        i = Signal(range(17))
        m.submodules.mul = mul = SeqMul(W, 16)
        atan = Array(Const(a, 32) for a in CORDIC_ATAN)
        m.d.sync += [mul.start.eq(0), self.done.eq(0)]
        with m.FSM(name="cordic"):
            with m.State("IDLE"):
                with m.If(self.start):
                    xs = self.re.as_signed() << CORDIC_PRESCALE
                    ys = self.im.as_signed() << CORDIC_PRESCALE
                    neg = self.re < 0
                    m.d.sync += [
                        x.eq(Mux(neg, -xs, xs)),
                        y.eq(Mux(neg, -ys, ys)),
                        z.eq(Mux(neg, 1 << 31, 0)),
                        i.eq(0),
                        self.busy.eq(1),
                    ]
                    m.next = "ITER"
            with m.State("ITER"):
                dx = y >> i
                dy = x >> i
                up = y > 0
                m.d.sync += [
                    x.eq(Mux(up, x + dx, x - dx)),
                    y.eq(Mux(up, y - dy, y + dy)),
                    z.eq(Mux(up, z + atan[i], z - atan[i])),
                    i.eq(i + 1),
                ]
                with m.If(i == 15):
                    m.next = "GAIN"
            with m.State("GAIN"):
                m.d.sync += [mul.a.eq(x), mul.b.eq(CORDIC_INV_GAIN), mul.start.eq(1)]
                m.next = "WAIT"
            with m.State("WAIT"):
                with m.If(mul.done):
                    m.d.sync += [
                        self.theta.eq(z),
                        self.mag.eq(round_shift(mul.p, 16 + CORDIC_PRESCALE)),
                        self.done.eq(1),
                        self.busy.eq(0),
                    ]
                    m.next = "IDLE"
        return m
