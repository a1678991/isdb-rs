"""Stage 2a (the NCO) and stage 2b (the carrier loop) of the ISDB-S3 chain.

Contract: `isdbs3_phy::chain::sync::Nco` and the module docs of
`isdbs3_phy::chain`, "Stage 2a" and "Stage 2b":

- rotation `r_i = round_shift(x_i c + x_q s, 11)`, `r_q = round_shift(x_q c
  - x_i s, 11)` with (c, s) the front end's 1 024-entry cosine table at the
  nearest entry of phi (`idx = ((phi + 2**21) >> 22) mod 1024`, sine at
  idx + 768), then `phi += f` (both 32 bits, wrapping);
- at each SSync head after the first since the (re)start, `delta = theta_k -
  theta_{k-2}` (wrapping difference read as signed 32 bits) added to a
  38-bit sum; after 60 steps `f = clamp(f + round_shift(sum * 246 412, 39),
  +-(2**19 - 1))` and the sum restarts. A restart drops the steps and keeps
  f and phi.

Schedule (one symbol per clock; "A" = the cycle a symbol is accepted,
`i_valid & i_ready`):

  A    x registered; the table's two read ports addressed by phi (cos at
       idx, sin at idx + 768: one RAMB18, both ports); phi += f
  A+1  c, s out of the table; the four products x*c, x*s (registered)
  A+2  the sums, round_shift 11 (registered): `o_valid`, `o_r` at A+3,
       with `o_phase` = the phi the symbol was rotated with

The carrier loop runs on head records (`h_valid`, one pulse, with `h_ssync`
and `h_theta`) and answers each with `h_done` (one pulse): 1 cycle for a
head that does not update f, 1 + 18 + 1 cycles for the 60th step (the
product sum * 246 412 bit-serially over the 18-bit constant: no DSP). The
new f is used from the next phi update. The caller (stage 1, `sync.py`)
holds the input (`i_ready` low) from the symbol that completes a head's
correlation window until the head is answered, so the update lands
exactly where the model's does: the model updates f inside the push of
that symbol, and the symbol after it is rotated with the phi the old f
gave and advances phi by the new f.

Ports `o_f_valid`, `o_f` report every register update (the vectors'
`carrier`).
"""

from amaranth.hdl import Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import CARRIER_GAIN, CARRIER_MAX, CARRIER_SHIFT, CARRIER_STEPS, ROT_SHIFT
from .common import SeqMul, cos_table, r_bits, round_shift

SUM_BITS = 38


class Nco(Elaboratable):
    def __init__(self, in_bits=6, carrier=0, table=None):
        self.in_bits = in_bits
        self.rb = r_bits(in_bits)
        self.table = list(table) if table is not None else cos_table()
        self.carrier = carrier
        # input stream
        self.i_valid = Signal()
        self.i_ready = Signal()  # driven by the caller: high = may accept
        self.i_x = [Signal(signed(in_bits), name=f"i_x{k}") for k in range(2)]
        self.accept = Signal()  # i_valid & i_ready
        # output stream (no back-pressure)
        self.o_valid = Signal()
        self.o_r = [Signal(signed(self.rb), name=f"o_r{k}") for k in range(2)]
        self.o_phase = Signal(32)
        self.o_x = [Signal(signed(in_bits), name=f"o_x{k}") for k in range(2)]  # the input, with o_r
        self.i_at = Signal(48)  # the chain index of the symbol offered (piped to o_at)
        self.o_at = Signal(48)
        # carrier loop
        self.h_valid = Signal()
        self.h_ssync = Signal()
        self.h_theta = Signal(32)
        self.h_done = Signal()
        self.restart = Signal()
        self.o_f_valid = Signal()
        self.f = Signal(signed(32), init=carrier)
        self.phi = Signal(32)  # phi before the next accepted symbol

    def elaborate(self, platform):
        m = Module()
        f, phi = self.f, self.phi

        # --- rotation ------------------------------------------------------
        m.d.comb += self.accept.eq(self.i_valid & self.i_ready)
        cos = Memory(shape=signed(16), depth=1024, init=self.table)
        m.submodules.cos = cos
        rc = cos.read_port()
        rs = cos.read_port()
        idx = Signal(10)
        m.d.comb += [
            idx.eq((phi + (1 << 21))[22:32]),
            rc.addr.eq(idx),
            rs.addr.eq(idx + 768),
            rc.en.eq(self.accept),
            rs.en.eq(self.accept),
        ]
        x1 = [Signal(signed(self.in_bits), name=f"x1_{k}") for k in range(2)]
        v1 = Signal()
        ph1 = Signal(32)
        at1 = Signal(48)
        at2 = Signal(48)
        m.d.sync += v1.eq(self.accept)
        with m.If(self.accept):
            m.d.sync += [x1[0].eq(self.i_x[0]), x1[1].eq(self.i_x[1]), ph1.eq(phi), phi.eq(phi + f),
                         at1.eq(self.i_at)]
        m.d.sync += [at2.eq(at1), self.o_at.eq(at2)]
        # A+1: products
        pw = self.in_bits + 16
        p = [Signal(signed(pw), name=f"p{k}") for k in range(4)]
        v2 = Signal()
        ph2 = Signal(32)
        x2 = [Signal(signed(self.in_bits), name=f"x2_{k}") for k in range(2)]
        m.d.sync += [x2[k].eq(x1[k]) for k in range(2)]
        m.d.sync += [self.o_x[k].eq(x2[k]) for k in range(2)]
        m.d.sync += [
            v2.eq(v1),
            ph2.eq(ph1),
            p[0].eq(x1[0] * rc.data),
            p[1].eq(x1[1] * rs.data),
            p[2].eq(x1[1] * rc.data),
            p[3].eq(x1[0] * rs.data),
        ]
        # A+2: sums, rounding
        m.d.sync += [
            self.o_valid.eq(v2),
            self.o_phase.eq(ph2),
            self.o_r[0].eq(round_shift(p[0] + p[1], ROT_SHIFT)),
            self.o_r[1].eq(round_shift(p[2] - p[3], ROT_SHIFT)),
        ]

        # --- carrier loop (stage 2b) ---------------------------------------
        last = Signal(32)
        have_last = Signal()
        acc = Signal(signed(SUM_BITS))
        steps = Signal(range(CARRIER_STEPS + 1))
        m.submodules.mul = mul = SeqMul(SUM_BITS, 18)
        m.d.sync += [self.h_done.eq(0), self.o_f_valid.eq(0), mul.start.eq(0)]
        step = (self.h_theta - last)[:32].as_signed()
        with m.FSM(name="carrier"):
            with m.State("IDLE"):
                with m.If(self.restart):
                    m.d.sync += [have_last.eq(0), acc.eq(0), steps.eq(0)]
                with m.Elif(self.h_valid):
                    with m.If(~self.h_ssync):
                        m.d.sync += self.h_done.eq(1)
                    with m.Elif(~have_last):
                        m.d.sync += [last.eq(self.h_theta), have_last.eq(1), self.h_done.eq(1)]
                    with m.Elif(steps == CARRIER_STEPS - 1):
                        m.d.sync += [
                            last.eq(self.h_theta),
                            mul.a.eq(acc + step),
                            mul.b.eq(CARRIER_GAIN),
                            mul.start.eq(1),
                        ]
                        m.next = "MUL"
                    with m.Else():
                        m.d.sync += [
                            last.eq(self.h_theta),
                            acc.eq(acc + step),
                            steps.eq(steps + 1),
                            self.h_done.eq(1),
                        ]
            with m.State("MUL"):
                with m.If(mul.done):
                    nf = Signal(signed(34))
                    m.d.comb += nf.eq(f + round_shift(mul.p, CARRIER_SHIFT))
                    m.d.sync += [
                        f.eq(Mux(nf > CARRIER_MAX, CARRIER_MAX, Mux(nf < -CARRIER_MAX, -CARRIER_MAX, nf))),
                        acc.eq(0),
                        steps.eq(0),
                        self.h_done.eq(1),
                        self.o_f_valid.eq(1),
                    ]
                    m.next = "IDLE"
        return m


