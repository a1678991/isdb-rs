"""The nominal points of every mode in one ROM (`tables.nominal_compact`:
512 x 24 bits, one RAMB18), read by the equaliser (the pilots' training
references) and the demapper (its point sets before any are learned).

`NominalRom(ports)`: `ports` synchronous read ports, each a `NominalPort`
with `mod`, `rate` (TMCC codes) and `label` in a cycle and `re`, `im`
(Q.10, sign-extended) the cycle after, as the 4 096-entry table it
replaces (entry ((mod - 1) * 16 + rate - 1) * 32 + label) answered for
every mode in use; an unused mode, or a label past the mode's points,
reads another mode's point (no caller uses it). The address is
base[(mod, rate)] + label, the base from a small LUT table.

The equaliser and the demapper share one (`back.py`, two ports: a true
dual-port RAMB18); each makes its own when used alone (the stage tests).
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Signal, signed
from amaranth.lib.memory import Memory

from .tables import NOM_BITS, nominal_compact, nominal_word


class NominalPort:
    def __init__(self, name):
        self.mod = Signal(4, name=f"{name}_mod")
        self.rate = Signal(4, name=f"{name}_rate")
        self.label = Signal(5, name=f"{name}_label")
        self.re = Signal(signed(16), name=f"{name}_re")
        self.im = Signal(signed(16), name=f"{name}_im")


class NominalRom(Elaboratable):
    def __init__(self, ports=1, name="nom"):
        self.ports = [NominalPort(f"{name}{i}") for i in range(ports)]

    def elaborate(self, platform):
        m = Module()
        entries, base = nominal_compact()
        rom = Memory(shape=2 * NOM_BITS, depth=512,
                     init=[nominal_word(re, im) for re, im in entries] + [0] * (512 - len(entries)))
        m.submodules.rom = rom
        bt = Array(Const(base.get((md, rc), 0), 9) for md in range(8) for rc in range(16))
        for p in self.ports:
            rp = rom.read_port()
            m.d.comb += [
                rp.addr.eq((bt[Cat(p.rate, p.mod[:3])] + p.label)[:9]),
                p.re.eq(rp.data[:NOM_BITS].as_signed()),
                p.im.eq(rp.data[NOM_BITS:].as_signed()),
            ]
        return m
