"""Host control of the front end through the AD9361 core's general purpose
registers (`isdb_frontend::control`, Rust: the register map, the command
words and the ID word are defined there; this is their implementation).

`gpio_out` arrives from the AXI clock domain and changes only when the host
writes it (milliseconds apart); all 32 bits pass a two-register
synchroniser and a command executes two cycles after its toggle bit
(bit 31) changes there, when every bit of the word has settled. `gpio_in`
changes only in response to a command, long before the host reads it.
"""

from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal

from .frontend import FORMAT_VERSION

ID_MAGIC = 0x15DB

# Register addresses (isdb_frontend::control::reg) and the Registers field
# each one sets.
REGS = {
    0x00: "nco_step",
    0x01: "period_nominal",
    0x02: "period_min",
    0x03: "period_max",
    0x04: "ted_shift",
    0x05: "kp",
    0x06: "kp_shift",
    0x07: "ki",
    0x08: "ki_shift",
    0x09: "integrator_max",
    0x0A: "agc_init",
    0x0B: "agc_target",
    0x0C: "agc_shift",
    0x0D: "bits",
    0x0E: "q_shift",
    0x0F: "block_log2",
}
STATUS = 0x10
SYMBOLS = 0x11
H_BASE = 0x1000
G_BASE = 0x2000

OP_NOP, OP_ADDR, OP_HI, OP_WRITE, OP_READ, OP_CTRL, OP_WRITE_S = range(7)
CTRL_RUN, CTRL_START, CTRL_SYMBOLS = 1, 2, 4


class Control(Elaboratable):
    """Command decoder and register file for one `SymbolFrontend`."""

    def __init__(self, core):
        self.core = core
        self.gpio_out = Signal(32)
        self.gpio_in = Signal(32)
        self.run = Signal()
        self.symbols = Signal()

    def elaborate(self, platform):
        m = Module()
        core = self.core
        regs = core.regs.fields()

        s1 = Signal(32)
        s2 = Signal(32)
        m.d.sync += [s1.eq(self.gpio_out), s2.eq(s1)]
        prev = Signal()
        m.d.sync += prev.eq(s2[31])
        edge1 = Signal()
        go = Signal()
        m.d.sync += [edge1.eq(s2[31] != prev), go.eq(edge1)]

        op = s2[28:31]
        arg = s2[1:28]
        addr = Signal(16)
        staged = Signal(16)
        show_read = Signal()
        readback = Signal(32)
        nsym = Signal(32)
        with m.If(core.s_valid):
            m.d.sync += nsym.eq(nsym + 1)

        value = Signal(32)
        m.d.comb += value.eq(Mux(op == OP_WRITE_S, arg.as_signed(), Cat(arg[:16], staged)))

        m.d.comb += [
            core.c_phase.eq(addr[6:12]),
            core.c_tap.eq(addr[:6]),
            core.c_h.eq(value[:18]),
            core.c_g.eq(value[:18]),
        ]
        m.d.sync += core.start.eq(0)
        with m.If(go):
            with m.Switch(op):
                with m.Case(OP_NOP):
                    m.d.sync += show_read.eq(0)
                with m.Case(OP_ADDR):
                    m.d.sync += [addr.eq(arg[:16]), show_read.eq(0)]
                with m.Case(OP_HI):
                    m.d.sync += [staged.eq(arg[:16]), show_read.eq(0)]
                with m.Case(OP_WRITE, OP_WRITE_S):
                    m.d.sync += [addr.eq(addr + 1), show_read.eq(0)]
                    for a, name in REGS.items():
                        with m.If(addr == a):
                            sig = regs[name]
                            m.d.sync += sig.eq(value[: len(sig)])
                    m.d.comb += [
                        core.c_we_h.eq(addr[12:] == H_BASE >> 12),
                        core.c_we_g.eq(addr[12:] == G_BASE >> 12),
                    ]
                with m.Case(OP_READ):
                    m.d.sync += show_read.eq(1)
                    with m.Switch(addr):
                        for a, name in REGS.items():
                            with m.Case(a):
                                sig = regs[name]
                                m.d.sync += readback.eq(
                                    sig.as_signed() if sig.shape().signed else sig
                                )
                        with m.Case(STATUS):
                            m.d.sync += readback.eq(Cat(self.run, self.symbols))
                        with m.Case(SYMBOLS):
                            m.d.sync += readback.eq(nsym)
                        with m.Default():
                            m.d.sync += readback.eq(0)
                with m.Case(OP_CTRL):
                    m.d.sync += [
                        show_read.eq(0),
                        self.run.eq(arg[0]),
                        self.symbols.eq(arg[2]),
                        core.start.eq(arg[1]),
                    ]
                    with m.If(arg[1]):
                        m.d.sync += nsym.eq(0)
        ident = Const(ID_MAGIC << 16 | FORMAT_VERSION << 12 | (core.taps & 0xFF), 32)
        m.d.comb += self.gpio_in.eq(Mux(show_read, readback, ident))
        return m
