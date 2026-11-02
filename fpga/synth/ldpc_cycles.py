"""Cycles per iteration of the LDPC decoder's static schedule
(isdb_fpga/ldpcsched.py) for each rate and S, and the iterations per
frame they allow: one frame (33.05 ms) carries at most 120 codewords plus
the TMCC's, and every codeword must be decodable at the iteration limit
(the plan's worst case), so the iterations per codeword are
f * 33.05 ms / 121 / (cycles per pass), less the passes a codeword always
runs on top of its iterations (one: the syndrome of the last iteration is
read in the next pass) and the output phase.

python synth/ldpc_cycles.py [S ...] [--clock MHZ ...]
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga import ldpcmodel, ldpcsched  # noqa: E402

FRAME_S = 33.05e-3
CODEWORDS = 121
# the HDL pipeline's figures (ldpc.py asserts them)
WRITE_LATENCY = 8
DECIDE = 12


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    clocks = [100.0, 200.0]
    if "--clock" in argv:
        clocks = [float(argv[argv.index("--clock") + 1])]
        args = [a for a in args if float(a) != clocks[0]]
    units = [int(a) for a in args] or [11, 17, 22, 34, 187]
    tables = ldpcmodel.load_tables()
    print("| S | rate | layer degree | cycles per pass | " + " | ".join(
        f"iterations per codeword at {c:.0f} MHz" for c in clocks) + " |")
    print("|---|---|---|---|" + "---|" * len(clocks))
    for s in units:
        for rate in (41, 61, 93, 109):
            lay = ldpcsched.Layout(tables[rate], ldpcmodel.N // ldpcmodel.GROUP, s)
            g, cyc = ldpcsched.gaps(lay, WRITE_LATENCY, DECIDE)
            out = 0  # the output overlaps the next codeword's decoding
            its = []
            for c in clocks:
                budget = c * 1e6 * FRAME_S / CODEWORDS
                its.append(f"{(budget - out) / cyc - 1:.1f}")
            nz = [x for x in g if x]
            print(f"| {s} | {rate}/120 | {lay.degree[0]} | {cyc} | " + " | ".join(its) + " |"
                  + (f" gaps: {len(nz)} of {len(g)} steps, sum {sum(nz)}, max {max(nz or [0])}"
                     if "--gaps" in argv else ""), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
