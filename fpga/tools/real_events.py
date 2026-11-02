"""The decoder port's events in a real-decoder chain run (stages.RealChainBench):
each codeword's first and last beat (slot or TMCC), the TMCC burst's
BEGIN / DONE, each result header leaving the bridge, the decoder's overrun
count, with the symbol-clock cycle; for finding where the codeword queue
overruns.

python tools/real_events.py build/chain-7-hdl.json [--backend verilator|cxxrtl] [--from CYCLE]

Writes build/real-events.txt (every event) and prints those from --from on.
"""

import json
import os
import sys

from amaranth.hdl import Signal

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain.ldpc_if import OP_BEGIN, OP_DONE  # noqa: E402
from isdb_fpga.chain.simx import engine  # noqa: E402
from isdb_fpga.chain.stages import C_LOSS, C_SYM, C_TAP, C_TAPNOW, RealChainBench  # noqa: E402


class Bench(RealChainBench):
    def __init__(self):
        super().__init__()
        self.cyc = Signal(32)
        self.ev_in = Signal()
        self.ev_out = Signal()

    def elaborate(self, platform):
        m = super().elaborate(platform)
        p = self.ch.port
        m.d.sync += self.cyc.eq(self.cyc + 1)
        m.d.comb += [
            self.ev_in.eq(p.in_valid & p.in_ready & (p.in_first | p.in_last | (p.in_op == OP_BEGIN)
                                                     | (p.in_op == OP_DONE))),
            self.ev_out.eq(p.out_valid & p.out_ready & p.out_head),
        ]
        return m


def main(argv):
    path = argv[1]
    backend = argv[argv.index("--backend") + 1] if "--backend" in argv else "verilator"
    start = int(argv[argv.index("--from") + 1]) if "--from" in argv else 0
    doc = json.load(open(path))
    taps = json.load(open(path.replace("7-hdl", "3-eq")))["taps"] if "7-hdl" in path else None
    x = doc["input"]
    losses = {at: k for at, k in doc.get("losses", [])}
    recs, gaps = [], []
    if taps is not None:
        recs = [(C_TAP, i, taps[2 * i], taps[2 * i + 1]) for i in range(11)] + [(C_TAPNOW, 0, 0, 0)]
        gaps = [0] * len(recs)
    acc = 0.0
    for i in range(len(x) // 2):
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0))
            gaps.append(0)
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0))
        acc += 40e6 / 33.7561e6 - 1
        gaps.append(int(acc))
        acc -= int(acc)
    b = Bench()
    p = b.ch.port
    ports = b.ports()
    ports.update({"cyc": b.cyc, "ev_in": b.ev_in, "ev_out": b.ev_out, "op": p.in_op, "ctx": p.in_ctx,
                  "first": p.in_first, "last": p.in_last, "tag": p.in_tag, "otag": p.out_tag,
                  "oit": p.out_iterations, "oconv": p.out_converged, "ovr": b.br.out_overrun})
    e = engine(b, ports, backend, tag="realevents")
    e.stall_limit = 3_000_000
    gi = e.group("ev_in", ["cyc", "op", "ctx", "first", "last", "tag", "dec_overruns"])
    go = e.group("ev_out", ["cyc", "otag", "oit", "oconv", "ovr", "dec_overruns"])
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], recs, tail=1_500_000, gaps=gaps)
    ev = []
    for c, op, ctx, fi, la, tag, ov in e.read(gi):
        what = ("TMCC BEGIN" if op == OP_BEGIN else "TMCC DONE" if op == OP_DONE
                else ("first" if fi else "") + (" last" if la else ""))
        ev.append((c, f"in  {what:12s} frame {tag >> 32} slot {tag & 0x7F} ctx {ctx} overruns {ov}"))
    for c, tag, it, conv, ovr, ov in e.read(go):
        kind = "TMCC" if tag >> 7 & 1 else f"slot {tag & 0x7F}"
        ev.append((c, f"out result frame {tag >> 32} {kind} it {it} conv {conv} dropped {ovr} overruns {ov}"))
    ev.sort()
    out = os.path.join(os.path.dirname(__file__), "..", "build", "real-events.txt")
    with open(out, "w") as f:
        for c, s in ev:
            f.write(f"{c:9d} {s}\n")
    for c, s in ev:
        if c >= start:
            print(f"{c:9d} {s}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
