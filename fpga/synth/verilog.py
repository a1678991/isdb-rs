"""Verilog of a block for a vendor flow (Vivado IP packaging), through the
venv's YoWASP Yosys (Amaranth's own Verilog backend looks for a system
Yosys).

python synth/verilog.py VECTORS.json TAPS OUT.v   # SymbolsRx, module isdb_symbols_rx
python synth/verilog.py TXVECTORS.json OUT.v [--any-design]  # SymbolsTx, module isdb_symbols_tx

The coefficient memories start with the vectors' banks. The receive host
rewrites them through the command register anyway; the transmit block's
banks are its default configuration (the host loads others only for
another rate or backoff), so its vectors must be the model's default
design (ISDB-S, 40 Msps, 10 dB backoff).
"""

import json
import os
import subprocess
import sys

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga.sim import is_tx  # noqa: E402
from isdb_fpga.top import SymbolsRx, SymbolsTx  # noqa: E402


def main(argv):
    doc = json.load(open(argv[1]))
    out = [a for a in argv if a != "--any-design"][-1]
    p = doc["params"]
    if is_tx(doc):
        d = doc["design"]
        default = {"sample_rate": 40e6, "symbol_rate": 28.86e6, "roll_off": 0.35,
                   "backoff_db": 10.0, "phases": 512, "span": 12}
        other = {k: d.get(k) for k in default if d.get(k) != default[k]}
        if other and "--any-design" not in argv:
            print(f"{argv[1]}: not the default design ({other}); the bitstream's bank is the "
                  "host's default configuration (--any-design to build it anyway)")
            return 2
        dut = SymbolsTx(p["phases"], p["span"], p["a"], p["b"])
        name = "isdb_symbols_tx"
    else:
        taps = int(argv[2])
        h = [(row + [0] * taps)[:taps] for row in p["h"]]
        g = [(row + [0] * taps)[:taps] for row in p["g"]]
        k = [(row + [0] * taps)[:taps] for row in p.get("k", p["h"])]
        dut = SymbolsRx(taps, doc["cos_table"], h, g, k)
        name = "isdb_symbols_rx"
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    il = out + ".il"
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name=name, ports=dut.ports()))
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
