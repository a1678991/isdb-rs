"""Resource estimate of the front end block (`SymbolsRx`: core, control
and output selection) for a Xilinx 7-series part (the
PlutoSky R2's XC7Z020) with Yosys `synth_xilinx` (YoWASP build, user space).
Vivado's numbers after place and route will differ; this is for budgeting.

python synth/estimate.py VECTORS.json [TAPS]
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
    p = doc["params"]
    taps = int(argv[2]) if len(argv) > 2 else p["taps"]
    h = [(row + [0] * taps)[:taps] for row in p["h"]]
    g = [(row + [0] * taps)[:taps] for row in p["g"]]
    dut = SymbolsRx(taps, doc["cos_table"], h, g)
    ports = dut.ports()
    out_dir = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out_dir, exist_ok=True)
    il = os.path.join(out_dir, f"frontend_t{taps}.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name="isdb_symbols_rx", ports=ports))
    stat = os.path.join(out_dir, f"frontend_t{taps}.stat")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top isdb_symbols_rx; tee -q -o {stat} stat"
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode != 0:
        print(res.stdout + res.stderr)
    with open(stat) as f:
        log = f.read()
    # The last section of `stat` is the whole hierarchy.
    total = log.split("design hierarchy")[-1]
    counts = {}
    for line in total.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0].isdigit():
            counts[parts[1]] = int(parts[0])
    luts = sum(v for k, v in counts.items() if k.startswith("LUT"))
    print(f"{taps} taps: {counts.get('DSP48E1', 0)} DSP48E1, {luts} LUT, "
          f"{counts.get('FDRE', 0) + counts.get('FDSE', 0)} FF, {counts.get('RAMB18E1', 0)} RAMB18E1, "
          f"{counts.get('RAMB36E1', 0)} RAMB36E1, {counts.get('CARRY4', 0)} CARRY4")
    return res.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv))
