"""Build time and simulation speed of a compiled backend (`chain.simx`)
on one of `sim_compare.py`'s cases: the library built in a fresh
directory (`--cold`: the build alone timed, elaboration apart) or taken
from fpga/build (warm: the whole engine setup timed), then the case run
and its harness cycles per second (the time in the engine's run / tick
calls only).

python synth/sim_speed.py CASE BACKEND [--cold] [--jobs N]

CASE: a sim_compare.py case (chain: ChainTest's frame 0; ldpc-34: every
step-34 B44 set on the all-rates S = 34 decoder). --jobs: Verilator's
make -j ($FPGA_SIM_JOBS). Pin it with taskset for per-core figures:
taskset -c 2 python synth/sim_speed.py chain verilator --cold --jobs 1
"""

import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import sim_compare  # noqa: E402
from isdb_fpga.chain import simv, simx  # noqa: E402


def main(argv):
    name, backend = argv[1], argv[2]
    if "--jobs" in argv:
        os.environ["FPGA_SIM_JOBS"] = argv[argv.index("--jobs") + 1]
    builds = []
    cls = simv.Verilator if backend == "verilator" else simx._Cxx
    real = cls._build

    def timed(il, so):
        t0 = time.perf_counter()
        real(il, so)
        builds.append((os.path.basename(so), time.perf_counter() - t0))

    cls._build = staticmethod(timed)
    tmp = None
    if "--cold" in argv:
        tmp = tempfile.TemporaryDirectory(prefix="sim_speed-", dir=os.path.join(
            os.path.dirname(__file__), "..", "build"))
        cls.build_dir = tmp.name
    r = sim_compare.run_case(sim_compare.cases()[name], backend)
    for so, dt in builds:
        print(f"{name} {backend}: built {so} in {dt:.1f} s "
              f"(jobs {simv.jobs() if backend == 'verilator' else 1})")
    print(f"{name} {backend}: setup {r['setup']:.1f} s, {r['cycles']} cycles in {r['sim']:.2f} s: "
          f"{r['cycles'] / max(r['sim'], 1e-9):,.0f} cycles/s; wall {r['wall']:.1f} s; "
          f"mismatches {sim_compare.mismatches(r['result'])}")
    if tmp:
        tmp.cleanup()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
