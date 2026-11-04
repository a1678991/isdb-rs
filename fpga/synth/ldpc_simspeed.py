"""Simulation speed of the LDPC decoder on both backends (`simldpc`):
the compiled CXXRTL library's build time (write_cxxrtl + c++, once per
design: code, S, K and rate set; cached in fpga/build/cxxrtl) and, per
backend, the wall time of a vectors file's first codewords and the
decoder cycles per second (the engine's cycles of every codeword plus its
load, one LLR per cycle, over the wall time after elaboration).

python synth/ldpc_simspeed.py VECTORS.json [--max N] [--all-rates] [--backends pysim,cxxrtl]
"""

import glob
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga import simldpc  # noqa: E402
from isdb_fpga.chain import simx  # noqa: E402


def main(argv):
    path = argv[1]
    limit = int(argv[argv.index("--max") + 1]) if "--max" in argv else 1
    backends = (argv[argv.index("--backends") + 1].split(",") if "--backends" in argv
                else ["cxxrtl", "pysim"])
    all_rates = "--all-rates" in argv
    doc = simldpc.load(path)
    if "cxxrtl" in backends and simx.available():
        before = set(glob.glob(os.path.join(simx.BUILD, "ldpc-*.so")))
        t0 = time.time()
        simldpc.simulate(doc, 1, all_rates, backend="cxxrtl", plan=[])
        built = set(glob.glob(os.path.join(simx.BUILD, "ldpc-*.so"))) - before
        print(f"cxxrtl library: {time.time() - t0:.1f} s "
              + ("(built: elaboration, write_cxxrtl, c++ -O2)" if built else "(cached)"))
    for be in backends:
        if be == "cxxrtl" and not simx.available():
            print("cxxrtl: not available here")
            continue
        t0 = time.time()
        res = simldpc.simulate(doc, limit, all_rates, backend=be)
        dt = time.time() - t0
        cyc = sum(r["cycles"] + doc["n"] for r in res)
        bad = simldpc.compare(doc, res)
        print(f"{be}: {len(res)} codewords in {dt:.1f} s, {cyc} cycles, {cyc / dt:,.0f} cycles/s; "
              + ("bit-exact" if not bad else f"{len(bad)} mismatches"), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
