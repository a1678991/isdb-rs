"""The demapper's frame events in a whole-chain run (the LDPC stub's
bench, `stages.ChainBench`): each cycle with f_swap (a decoded frame's
last symbol), f_end (the same, the pipeline drained: the noise division
starts), f_start (a frame with a layout starts) or f_drop, with the noise
count, whether the noise division is idle and n0, so the order of a
frame's end and the next frame's start at the demapper can be read.

python tools/frame_events.py build/rec7/chain-rec-6-chain.json [--taps chain-rec-3-eq.json] [--symbols N] [--paced]

--paced: the symbols at the front end's 33.756 Msymbol/s in the 40 MHz
clock (`run_chain_real`'s idle cycles), else one per cycle. At the end the
demapper's `late` (K not in force in time) is printed.
"""

import json
import os
import sys

from amaranth.hdl import Module, Signal

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain.simx import compiled_backend, engine  # noqa: E402
from isdb_fpga.chain.stages import C_SYM, ChainBench, chain_records  # noqa: E402


class Events(ChainBench):
    def __init__(self):
        super().__init__()
        self.cyc = Signal(32)
        self.ev = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.bench = super().elaborate(platform)
        dm = self.ch.back.demap
        m.d.sync += self.cyc.eq(self.cyc + 1)
        m.d.comb += self.ev.eq(dm.f_swap | dm.f_end | dm.f_start | dm.f_drop)
        return m


def main(argv):
    path = argv[1]
    doc = json.load(open(path))
    tp = argv[argv.index("--taps") + 1] if "--taps" in argv else path.replace("6-chain", "3-eq")
    taps = json.load(open(tp))["taps"] if os.path.exists(tp) else None
    n = int(argv[argv.index("--symbols") + 1]) if "--symbols" in argv else len(doc["input"]) // 2
    b = Events()
    dm = b.ch.back.demap
    p = b.ports()
    p.update({"cyc": b.cyc, "ev": b.ev, "f_swap": dm.f_swap, "f_end": dm.f_end, "f_start": dm.f_start,
              "f_drop": dm.f_drop, "ncnt": dm.dbg_ncnt, "nidle": dm.dbg_noise_idle, "n0": dm.n0,
              "frame": dm.frame})
    e = engine(b, p, compiled_backend() or "pysim", tag="frame_events")
    e.stall_limit = 3_000_000
    g = e.group("ev", ["cyc", "f_swap", "f_end", "f_start", "f_drop", "ncnt", "nidle", "n0", "frame"])
    recs = chain_records(doc, taps, n)
    gaps = None
    if "--paced" in argv:
        gaps, acc = [], 0.0
        for r in recs:
            if r[0] == C_SYM:
                acc += 40e6 / 33.7561e6 - 1
                gaps.append(int(acc))
                acc -= int(acc)
            else:
                gaps.append(0)
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=200_000, gaps=gaps)
    print("cycle      swap end start drop  ncnt   idle  n0      frame")
    for cyc, sw, en, st, dr, nc, idle, n0, fr in e.read(g):
        print(f"{cyc:10d} {sw:4d} {en:3d} {st:5d} {dr:4d}  {nc:6d} {idle:4d}  {n0:6d}  {fr}")
    print(f"late: {e.get('late')}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
