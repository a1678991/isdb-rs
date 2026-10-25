"""Verilog of `SymbolsRx` for a vendor flow (Vivado IP packaging), through
the venv's YoWASP Yosys (Amaranth's own Verilog backend looks for a system
Yosys).

python synth/verilog.py VECTORS.json TAPS OUT.v

The coefficient memories start with the vectors' banks; the host rewrites
them through the command register anyway.
"""

import json
import os
import subprocess
import sys

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga.top import SymbolsRx  # noqa: E402


def main(argv):
    doc = json.load(open(argv[1]))
    taps = int(argv[2])
    out = argv[3]
    p = doc["params"]
    h = [(row + [0] * taps)[:taps] for row in p["h"]]
    g = [(row + [0] * taps)[:taps] for row in p["g"]]
    k = [(row + [0] * taps)[:taps] for row in p.get("k", p["h"])]
    dut = SymbolsRx(taps, doc["cos_table"], h, g, k)
    il = out + ".il"
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name="isdb_symbols_rx", ports=dut.ports()))
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    script = f"read_rtlil {il}; proc; opt_clean; write_verilog -norename {out}"
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode:
        print(res.stdout + res.stderr)
        return res.returncode
    os.remove(il)
    print(f"{out}: {sum(1 for _ in open(out))} lines")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
