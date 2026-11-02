"""How Yosys `synth_xilinx` maps the LDPC decoder's memories, one shape
at a time (a memory with one read and one write port, as the decoder
uses them): block RAMs per shape, for the BRAM budget in fpga/README.md
("The LDPC decoder"). Shapes: (name, width, depth, ranks), ranks > 1 =
the depth split into that many memories by the top address bits.

python synth/ldpc_memories.py
"""

import os
import subprocess
import sys

from amaranth.back import rtlil
from amaranth.hdl import Elaboratable, Module, Mux, Signal
from amaranth.lib.memory import Memory

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

SHAPES = [
    # S = 34 (W = 11, 120 groups): an L lane of one bank, the record (19 bits x S per
    # step, 869 steps at 41/120), the sign memory (S bits per edge slot, 4 785)
    ("L lane, S 34", 9, 1920, 1),
    ("record, S 34", 19 * 34, 869, 1),
    ("signs, S 34", 34, 4785, 1),
    ("signs, S 34, 1024-deep ranks", 34, 4785, 5),
    # S = 17 (W = 22)
    ("L lane, S 17", 9, 3840, 1),
    ("record, S 17", 19 * 17, 1738, 1),
    ("signs, S 17", 17, 9570, 1),
    ("signs, S 17, 2048-deep ranks", 17, 9570, 5),
]


class _Mem(Elaboratable):
    def __init__(self, width, depth, ranks):
        self.width, self.depth, self.ranks = width, depth, ranks
        self.ra = Signal(range(depth))
        self.wa = Signal(range(depth))
        self.wd = Signal(width)
        self.we = Signal()
        self.rd = Signal(width)

    def elaborate(self, platform):
        m = Module()
        per = -(-self.depth // self.ranks)
        datas = []
        for i in range(self.ranks):
            mem = Memory(shape=self.width, depth=per, init=[])
            m.submodules[f"m{i}"] = mem
            rp, wp = mem.read_port(), mem.write_port()
            m.d.comb += [rp.addr.eq(self.ra % per if self.ranks > 1 else self.ra),
                         wp.addr.eq(self.wa % per if self.ranks > 1 else self.wa),
                         wp.data.eq(self.wd),
                         wp.en.eq(self.we & ((self.wa // per) == i if self.ranks > 1 else 1))]
            datas.append(rp.data)
        sel = Signal(range(self.ranks + 1))
        m.d.sync += sel.eq(self.ra // per if self.ranks > 1 else 0)
        out = datas[0]
        for i in range(1, self.ranks):
            out = Mux(sel == i, datas[i], out)
        m.d.comb += self.rd.eq(out)
        return m


def bram(width, depth, ranks):
    dut = _Mem(width, depth, ranks)
    out = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out, exist_ok=True)
    il = os.path.join(out, "ldpc_mem.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name="mem", ports=[dut.ra, dut.wa, dut.wd, dut.we, dut.rd]))
    stat = os.path.join(out, "ldpc_mem.stat")
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    subprocess.run([yosys, "-q", "-p",
                    f"read_rtlil {il}; synth_xilinx -family xc7 -top mem; tee -q -o {stat} stat"],
                   check=True, capture_output=True)
    counts = {}
    for line in open(stat).read().split("design hierarchy")[-1].splitlines():
        p = line.split()
        if len(p) == 2 and p[0].isdigit():
            counts[p[1]] = int(p[0])
    return counts.get("RAMB36E1", 0), counts.get("RAMB18E1", 0)


def main(argv):
    for name, width, depth, ranks in SHAPES:
        r36, r18 = bram(width, depth, ranks)
        print(f"{name}: {width} x {depth}: {r36} RAMB36E1 + {r18} RAMB18E1 = {r36 + r18 / 2:g} "
              f"BRAM36 ({width * depth / 36864:.1f} of data)", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
