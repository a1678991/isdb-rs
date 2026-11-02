"""Yosys estimates of the BCH unit's parts (finish.py), to see where its
LUTs go and what a narrower datapath would save:

python synth/bch_parts.py

- `syn16`, `syn4`, `syn1`: the 12 odd syndromes' update networks at 16, 4
  and 1 bit(s) per clock (S_i <- S_i alpha^(w i) + the beat's terms);
- `chien`: the 12 constant multipliers R_x <- R_x alpha^x and the sum;
- `gfmul`: one general GF(2^16) multiplier;
- `unit`: the whole BchUnit.
"""

import os
import sys

from amaranth.hdl import Const, Elaboratable, Module, Mux, Signal

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
sys.path.insert(0, os.path.dirname(__file__))

from chain_estimate import stat  # noqa: E402
from isdb_fpga.chain import gf  # noqa: E402
from isdb_fpga.chain.finish import BCH_LENGTHS, BchUnit, gf_const_mul, gf_mul  # noqa: E402


class Syn(Elaboratable):
    def __init__(self, w):
        self.w = w
        self.bits = Signal(w)
        self.en = Signal()
        self.s = [Signal(16, name=f"s{i}") for i in range(12)]

    def elaborate(self, platform):
        m = Module()
        for k, i in enumerate(range(1, 24, 2)):
            acc = gf_const_mul(self.s[k], gf.alpha(self.w * i))
            for b in range(self.w):
                acc = acc ^ Mux(self.bits[b], Const(gf.alpha((self.w - 1 - b) * i), 16), 0)
            with m.If(self.en):
                m.d.sync += self.s[k].eq(acc)
        return m


class Chien(Elaboratable):
    def __init__(self):
        self.r = [Signal(16, name=f"r{i}") for i in range(12)]
        self.load = Signal()
        self.root = Signal()

    def elaborate(self, platform):
        m = Module()
        v = Const(1, 16)
        for x in range(12):
            v = v ^ self.r[x]
            with m.If(~self.load):
                m.d.sync += self.r[x].eq(gf_const_mul(self.r[x], gf.alpha(x + 1)))
        m.d.sync += self.root.eq(v == 0)
        return m


class Mul(Elaboratable):
    def __init__(self):
        self.a = Signal(16)
        self.b = Signal(16)
        self.p = Signal(16)

    def elaborate(self, platform):
        m = Module()
        m.d.sync += self.p.eq(gf_mul(self.a, self.b))
        return m


def main():
    parts = {
        "syn16": lambda: (lambda d: (d, [d.bits, d.en, *d.s]))(Syn(16)),
        "syn4": lambda: (lambda d: (d, [d.bits, d.en, *d.s]))(Syn(4)),
        "syn1": lambda: (lambda d: (d, [d.bits, d.en, *d.s]))(Syn(1)),
        "chien": lambda: (lambda d: (d, [d.load, d.root, *d.r]))(Chien()),
        "gfmul": lambda: (lambda d: (d, [d.a, d.b, d.p]))(Mul()),
        "unit": lambda: (lambda d: (d, [d.start, d.n_sel, d.i_valid, d.i_bits, d.i_last, d.done, d.ok,
                                        d.corrected, d.nroots, *d.roots, d.q_beat, d.o_mask]))(
            BchUnit(BCH_LENGTHS)),
    }
    for name in sys.argv[1:] or parts:
        dut, ports = parts[name]()
        r = stat(dut, ports, f"bch_{name}")
        print(f"{name}: {r['LUT_ALL']} LUT, {r['FF']} FF", flush=True)


if __name__ == "__main__":
    main()
