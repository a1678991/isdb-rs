"""How long `finish.FastBch` takes on a word, in symbol clocks: from the
word's last beat to `done`, by error count (the slot word of 93/120 and the
TMCC word), so the result FIFO's headroom (results.py) can be checked
against the BCH's budget per corrected slot.

python tools/bch_cycles.py [--backend pysim|cxxrtl|verilator] [--errors 0,1,6,12]
"""

import os
import random
import sys

from amaranth.hdl import Module, Signal

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import gf  # noqa: E402
from isdb_fpga.chain.bchsim import SLOT_N, TMCC_N, BchBench, beats  # noqa: E402
from isdb_fpga.chain.simx import compiled_backend, engine  # noqa: E402


class Timed(BchBench):
    """The BCH bench with a cycle counter and the last beat as an event."""

    def __init__(self, lengths):
        super().__init__(lengths)
        self.cyc = Signal(32)
        self.lastbeat = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.bench = super().elaborate(platform)
        m.d.sync += self.cyc.eq(self.cyc + 1)
        # registered, as `done` is (both then read with the counter of the cycle after)
        m.d.sync += self.lastbeat.eq(self.i_valid & self.i_ready & (self.kind == 1) & self.last)
        return m

    def ports(self):
        p = super().ports()
        p.update({"cyc": self.cyc, "lastbeat": self.lastbeat})
        return p


def measure(n, errors, backend, seed=1):
    r = random.Random(seed)
    w = gf.encode([r.randrange(2) for _ in range(n - gf.PARITY)])
    for p in r.sample(range(n), errors):
        w[p] ^= 1
    b = Timed([TMCC_N, SLOT_N])
    e = engine(b, b.ports(), backend, tag="bch_cycles")
    g_last = e.group("lastbeat", ["cyc"])
    g_done = e.group("done", ["cyc", "ok", "cor"])
    bs = beats(w)
    recs = [(0, [TMCC_N, SLOT_N].index(n), 0)] + [(1, v, int(i == len(bs) - 1)) for i, v in enumerate(bs)]
    # a few idle symbol clocks first: an input before the first edge reaches
    # the fast domain only in its last fast cycle (simulation start only)
    e.set("i_valid", 0)
    e.tick(4)
    e.run("i_valid", "i_ready", ["kind", "v", "last"], recs, tail=60_000)
    (t_last,), (t_done, ok, cor) = e.read(g_last)[0], e.read(g_done)[0]
    return len(bs), t_done - t_last, ok, cor


def main(argv):
    backend = argv[argv.index("--backend") + 1] if "--backend" in argv else (compiled_backend() or "pysim")
    errs = [int(x) for x in argv[argv.index("--errors") + 1].split(",")] if "--errors" in argv else [0, 1, 6, 12]
    print(f"backend {backend}; symbol clocks from the last beat to done")
    for n, name in ((SLOT_N, "slot 93/120"), (TMCC_N, "TMCC")):
        for k in errs:
            nb, dt, ok, cor = measure(n, k, backend)
            print(f"{name:12s} n {n:5d} ({nb} beats)  errors {k:2d}: {dt:6d}  (ok {ok}, corrected {cor})",
                  flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
