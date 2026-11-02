"""Per-stage simulation of the chain's blocks against the vectors (the
stages after stage 1; `sim.py` dispatches here). Each stage is driven
through a small bench wrapper that takes one record per cycle from the
engine's input stream and routes it to the block's ports, so the compiled
backend (`simx.py`) can feed millions of records without Python in the
loop.
"""

import time

from amaranth.hdl import Elaboratable, Module, Signal, signed

from .phase import SlotPhase
from .simx import engine
from .sim import first_mismatch


# --- stage 3 -------------------------------------------------------------------

class PhaseBench(Elaboratable):
    """Records: kind 0 = an NCO output (at, r0, r1); kind 1 = a head (at,
    word, theta, amp, numbered, number, frame)."""

    def __init__(self, in_bits=6):
        self.dut = SlotPhase(in_bits)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal()
        self.at = Signal(48)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.word = Signal(2)
        self.numbered = Signal()
        self.number = Signal(7)
        self.frame = Signal(32)

    def elaborate(self, platform):
        m = Module()
        m.submodules.dut = d = self.dut
        sym = self.i_valid & self.i_ready & (self.kind == 0)
        head = self.i_valid & self.i_ready & (self.kind == 1)
        m.d.comb += [
            self.i_ready.eq(~d.full),
            d.i_valid.eq(sym), d.i_at.eq(self.at), d.i_r[0].eq(self.a), d.i_r[1].eq(self.b),
            d.h_valid.eq(head), d.h_late.eq(0), d.h_at.eq(self.at), d.h_word.eq(self.word),
            d.h_theta.eq(self.a), d.h_amp.eq(self.b), d.h_numbered.eq(self.numbered),
            d.h_number.eq(self.number), d.h_frame.eq(self.frame),
        ]
        return m

    def ports(self):
        d = self.dut
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "at": self.at,
            "a": self.a, "b": self.b, "word": self.word, "numbered": self.numbered,
            "number": self.number, "frame": self.frame,
            "ov": d.o_valid, "oat": d.o_at, "oj": d.o_j, "ophi": d.o_phi, "og": d.o_gain,
            "oy0": d.o_y[0], "oy1": d.o_y[1], "or0": d.o_r[0], "or1": d.o_r[1],
        }


def head_record(h):
    at, word, _c0, _c1, theta, amp, number, frame = h
    theta_s = theta - (1 << 32) if theta >= 1 << 31 else theta
    return (1, at, theta_s, amp, word, int(number >= 0), max(number, 0), max(frame, 0))


def run_phase(doc, backend):
    heads = doc["heads"]
    r = doc["r"]
    a0 = heads[0][0]
    recs = []
    # the slots' symbols; each head after its window's last symbol (as
    # the model emits it, 24 symbols in), the stream padded past the last
    n = len(r) // 2
    pad = 24 + 256
    hq = {h[0] + 24: h for h in heads}
    for i in range(n + pad):
        at = a0 + i
        if at in hq:
            recs.append(head_record(hq[at]))
        v = (r[2 * i], r[2 * i + 1]) if i < n else (0, 0)
        recs.append((0, at, v[0], v[1], 0, 0, 0, 0))
    b = PhaseBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain2")
    g = e.group("ov", ["oat", "oj", "ophi", "og", "oy0", "oy1", "or0", "or1"])
    t0 = time.time()
    e.run("i_valid", "i_ready", ["kind", "at", "a", "b", "word", "numbered", "number", "frame"],
          recs, tail=9600)
    got = e.read(g)
    cyc = e.cycles()
    dt = time.time() - t0
    exp = [tuple(o) for o in doc["out"]]
    bad = first_mismatch("y", [x[:6] for x in got], exp, key=lambda i, v: f"symbol at {v[0]} (j {v[1]})")
    exp_r = [(r[2 * i], r[2 * i + 1]) for i in range(n)]
    bad += first_mismatch("r", [x[6:] for x in got], exp_r, key=lambda i, v: f"#{i}")
    what = f"{len(got)} symbols corrected over 2 slots; {cyc} cycles in {dt:.1f} s"
    return what, bad


def run(stage, doc, backend, symbols):
    if stage == "2-phase":
        return run_phase(doc, backend)
    raise NotImplementedError(stage)
