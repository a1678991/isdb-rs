"""Resource estimate of a block for a Xilinx 7-series part (the PlutoSky
R2's XC7Z020) with Yosys `synth_xilinx` (YoWASP build, user space).
Vivado's numbers after place and route will differ; this is for budgeting.

python synth/estimate.py VECTORS.json [TAPS]   # SymbolsRx (front end, control, output selection)
python synth/estimate.py TXVECTORS.json        # SymbolsTx (a transmit vectors file: its banks
                                               # are the memories' initial contents)
"""

import json
import os
import subprocess
import sys

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga.sim import is_tx  # noqa: E402
from isdb_fpga.top import SymbolsRx, SymbolsTx  # noqa: E402


def rx_block(doc, argv):
    p = doc["params"]
    taps = int(argv[2]) if len(argv) > 2 else p["taps"]
    h = [(row + [0] * taps)[:taps] for row in p["h"]]
    g = [(row + [0] * taps)[:taps] for row in p["g"]]
    k = [(row + [0] * taps)[:taps] for row in p.get("k", p["h"])]
    return SymbolsRx(taps, doc["cos_table"], h, g, k), "isdb_symbols_rx", f"frontend_t{taps}", f"{taps} taps"


def tx_block(doc):
    p = doc["params"]
    dut = SymbolsTx(p["phases"], p["span"], p["a"], p["b"])
    tag = f"tx_p{p['phases']}_s{p['span']}"
    return dut, "isdb_symbols_tx", tag, f"TX P {p['phases']} span {p['span']}"


def main(argv):
    doc = json.load(open(argv[1]))
    dut, top, tag, what = tx_block(doc) if is_tx(doc) else rx_block(doc, argv)
    ports = dut.ports()
    out_dir = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out_dir, exist_ok=True)
    il = os.path.join(out_dir, f"{tag}.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name=top, ports=ports))
    stat = os.path.join(out_dir, f"{tag}.stat")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top {top}; tee -q -o {stat} stat"
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
    lram = {k: v for k, v in counts.items() if k.startswith("RAM") and not k.startswith("RAMB")}
    print(f"{what}: {counts.get('DSP48E1', 0)} DSP48E1, {luts} LUT, "
          f"{counts.get('FDRE', 0) + counts.get('FDSE', 0)} FF, {counts.get('RAMB18E1', 0)} RAMB18E1, "
          f"{counts.get('RAMB36E1', 0)} RAMB36E1, {counts.get('CARRY4', 0)} CARRY4"
          + (f", LUTRAM {lram}" if lram else ""))
    return res.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv))
