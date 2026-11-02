"""That Verilator's random X values are in effect (`sim_compare.py
--x-unique`): tests/test_simx.py's bench built with `--x-initial unique
--x-assign unique`, its memory read port's data (no init: X) read before
the first clock under several seeds ($FPGA_VERILATOR_SEED); random values
when they are, 0 with the default build.

python synth/sim_xcheck.py
"""

import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CHILD = """
import sys
sys.path.insert(0, "tests")
import test_simx
from isdb_fpga.chain import simx
b = test_simx.Bench()
e = simx.engine(b, b.ports(), "verilator", tag="simx-bench")
print(e.get("rdata"))
"""


def main():
    for flags, seeds in (("", [None]), ("--x-initial unique --x-assign unique", [1, 2, 3])):
        for seed in seeds:
            env = dict(os.environ, FPGA_VERILATOR_FLAGS=flags)
            env.pop("FPGA_VERILATOR_SEED", None)
            if seed is not None:
                env["FPGA_VERILATOR_SEED"] = str(seed)
            out = subprocess.run([sys.executable, "-c", CHILD], cwd=os.path.join(HERE, ".."),
                                 env=env, capture_output=True, text=True, check=True).stdout
            print(f"flags {flags or '(default)'!r}, seed {seed}: rdata before the first clock "
                  f"{int(out):#010x}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
