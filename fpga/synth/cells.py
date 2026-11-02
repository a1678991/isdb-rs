"""Which cells Yosys's `synth_xilinx` made of a block, by name: where the
DSP48E1, block RAM or LUT RAM cells of an estimate come from (the names
carry the Amaranth signal the cell drives).

python synth/cells.py build/chain_demap.il chain_demap [TYPE ...]   # default DSP48E1 RAMB36E1 RAMB18E1

The .il files are the ones `chain_estimate.py` writes.
"""

import os
import subprocess
import sys


def main(argv):
    il, top = argv[1], argv[2]
    types = argv[3:] or ["DSP48E1", "RAMB36E1", "RAMB18E1"]
    sel = " ".join(f"t:{t}" for t in types)
    out = os.path.splitext(il)[0] + ".cells"
    script = (f"read_rtlil {il}; synth_xilinx -family xc7 -top {top} -flatten; "
              f"tee -q -o {out} select -list {sel}")
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    sys.stderr.write(res.stderr[-2000:])
    if res.returncode == 0:
        sys.stdout.write(open(out).read())
    return res.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv))
