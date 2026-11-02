"""Several processes wanting the same compiled library at once (pytest-
xdist workers): N processes make the same engine in an empty build
directory at the same moment; says how many built it (one should: the
others wait on its lock and load its library), whether they all got the
same results, and what files are left in the directory.

python synth/sim_parallel_build.py BACKEND [N]
"""

import hashlib
import multiprocessing
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "tests"))
from isdb_fpga.chain import simv, simx  # noqa: E402


def worker(args):
    backend, build_dir, start = args
    cls = simv.Verilator if backend == "verilator" else simx._Cxx
    cls.build_dir = build_dir
    real = cls._build
    built = []

    def counted(il, so):
        built.append(so)
        real(il, so)

    cls._build = staticmethod(counted)
    import test_simx

    while time.time() < start:
        time.sleep(0.001)
    t0 = time.time()
    out = test_simx.run_bench(backend, n=500)
    return os.getpid(), len(built), time.time() - t0, hashlib.sha256(repr(out).encode()).hexdigest()[:8]


def main(argv):
    backend = argv[1]
    n = int(argv[2]) if len(argv) > 2 else 4
    with tempfile.TemporaryDirectory(prefix="sim_parallel-", dir=os.path.join(
            os.path.dirname(__file__), "..", "build")) as d:
        start = time.time() + 2
        with multiprocessing.get_context("spawn").Pool(n) as pool:
            res = pool.map(worker, [(backend, d, start)] * n)
        for pid, built, dt, h in res:
            print(f"pid {pid}: built {built}, {dt:.1f} s, result {h}")
        print(f"builds: {sum(r[1] for r in res)}; results identical: {len({r[3] for r in res}) == 1}")
        print("left:", sorted(os.listdir(d)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
