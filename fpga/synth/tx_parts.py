"""Where the transmit block's LUTs go: Yosys `synth_xilinx` estimates of
`SymbolsTx`, of the bare `SymbolTx` core and of the core's datapath alone
(no unpacker, counters or control: the shape of `tx_sketch.py`), each
with the per-cell-type counts, for the budget in fpga/README.md.

python synth/tx_parts.py TXVECTORS.json
"""

import json
import os
import subprocess
import sys

from amaranth.back import rtlil
from amaranth.hdl import Elaboratable, Module

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga.top import SymbolsTx  # noqa: E402
from isdb_fpga.tx import SymbolTx  # noqa: E402


def core_ports(c):
    return [c.start, c.run, c.selected, c.req, c.rd_en, c.rd_data, c.rd_valid, c.rd_underflow,
            c.o_i, c.o_q, c.c_we, c.c_bank, c.c_q, c.c_t, c.c_data, c.consumed, c.missed,
            c.first_miss, c.samples, c.underrun, c.regs.step, c.regs.shift]


def estimate(dut, ports, tag):
    out = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out, exist_ok=True)
    il = os.path.join(out, f"{tag}.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name=tag, ports=ports))
    stat = os.path.join(out, f"{tag}.stat")
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top {tag}; tee -q -o {stat} stat"
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode:
        print(res.stdout[-3000:] + res.stderr[-3000:])
        return
    counts = {}
    for line in open(stat).read().split("design hierarchy")[-1].splitlines():
        p = line.split()
        if len(p) == 2 and p[0].isdigit():
            counts[p[1]] = int(p[0])
    luts = sum(v for k, v in counts.items() if k.startswith("LUT"))
    ffs = sum(v for k, v in counts.items() if k.startswith("FD"))
    cells = ", ".join(f"{k} {v}" for k, v in sorted(counts.items()))
    print(f"{tag}: {luts} LUT, {ffs} FF ({cells})")


def main(argv):
    p = json.load(open(argv[1]))["params"]
    top = SymbolsTx(p["phases"], p["span"], p["a"], p["b"])
    estimate(top, top.ports(), "tx_top")
    core = SymbolTx(p["phases"], p["span"], p["a"], p["b"])
    estimate(core, core_ports(core), "tx_core")
    # the core with SHIFT a constant (what the run-time shift costs)
    core = SymbolTx(p["phases"], p["span"], p["a"], p["b"])
    fixed = _Fixed(core, p["shift"])
    estimate(fixed, [x for x in core_ports(core) if x is not core.regs.shift], "tx_core_fixed_shift")
    return 0


class _Fixed(Elaboratable):
    def __init__(self, core, shift):
        self.core, self.shift = core, shift

    def elaborate(self, platform):
        m = Module()
        m.submodules.core = self.core
        m.d.comb += self.core.regs.shift.eq(self.shift)
        return m


if __name__ == "__main__":
    sys.exit(main(sys.argv))
