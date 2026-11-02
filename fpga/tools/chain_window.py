"""Run the chain (the LDPC stub's bench, `stages.ChainBench`) over a window
of a whole-chain set's input, with the hold signals observed, to find where
a run stalls (a record not accepted): which of stage 1's hold, its loss
drain, stage 3's ring guard holds the input.

python tools/chain_window.py build/chain-8-loss.json START END [--every N]

Prints, every N cycles of the run (default 1 000) once the input stalls,
the hold sources; the window's losses (the set's, inside it) are fed.
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain.simx import engine  # noqa: E402
from isdb_fpga.chain.stages import C_LOSS, C_SYM, ChainBench  # noqa: E402


def main(argv):
    doc = json.load(open(argv[1]))
    a, b = int(argv[2]), int(argv[3])
    x = doc["input"]
    losses = {at: k for at, k in doc.get("losses", [])}
    recs = []
    for i in range(a, min(b, len(x) // 2)):
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0, 0, 0))
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0, 0, 0))
    bn = ChainBench()
    ch = bn.ch
    sf, sy, ph = ch.front, ch.front.sync, ch.phase
    ports = bn.ports()
    ports.update({"hold": sy.hold, "ext_hold": sf.ext_hold, "full": ph.full, "ready": ch.i_ready,
                  "locked": sy.st_locked, "pv": ph.o_valid, "headv": sy.o_head_valid,
                  "headat": sy.o_head_at, "late_h": sy.o_head_late, "restart": sf.o_restart,
                  "cnt_in": sy.dbg_cnt_in, "ipos": sy.dbg_ipos, "pos": sy.dbg_pos, "idle": sy.dbg_idle})
    e = engine(bn, ports, "cxxrtl", tag="chainwin")
    e.stall_limit = 200_000
    gh = e.group("headv", ["headat", "late_h", "locked"])
    gr = e.group("restart", ["locked"])
    try:
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=10)
        print("ran through")
    except RuntimeError as err:
        print(err)
    for k in ("hold", "ext_hold", "full", "ready", "locked", "cnt_in", "ipos", "pos", "idle"):
        print(k, e.get(k))
    print("heads", e.read(gh)[-8:])
    print("restarts", e.read(gr))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
