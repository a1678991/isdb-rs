"""The blocks that go into the PlutoSky R2 bitstream. `SymbolsRx` (below)
and `SymbolsTx` (at the end: the transmit block, between the DAC unpacker
and the rfifo).

`SymbolsRx`: the front end, its control and the output selection,
between the existing sample path and the ADC packer that feeds the RX DMA
(Tezuka's maia-hdl block design, `projects/common/cs12_cs8.tcl`):

    mux_decim_i/q data_out, valid_out --> adc_i, adc_q, adc_valid
    cs12_sync_chan0 data_out0/1, cs12_8mux_chan0 valid_out
                                       --> pass_data0/1, pass_valid
    pack_valid, pack_data0/1           --> util_ad9361_adc_pack fifo_wr_en,
                                           fifo_wr_data_0/1
    axi_ad9361 up_adc_gpio_out[31:0]   --> gpio_out
    gpio_in                            --> axi_ad9361 up_adc_gpio_in[31:0]

With symbols selected (CTRL bit 2) the packer's channel 0 carries the
`Symbols` words instead of samples, one per strobe; the host enables only
`voltage0` of `cf-ad9361-lpc` and reads 16-bit words. Otherwise the block
is transparent. Everything runs in the ADC packer's clock domain (dout_clk
of util_ad9361_adc_fifo = util_ad9361_divclk = l_clk / 4: the sample rate,
40 MHz at 40 Msps on the PlutoSky R2; one sample per clock).
"""

from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal, signed

from .control import Control
from .frontend import SymbolFrontend
from .tx import SymbolTx, TxControl


class SymbolsRx(Elaboratable):
    def __init__(self, taps, cos_table, h=None, g=None, k=None):
        self.core = SymbolFrontend(taps, cos_table, h, g, k)
        self.control = Control(self.core)
        self.adc_valid = Signal()
        self.adc_i = Signal(signed(16))
        self.adc_q = Signal(signed(16))
        self.pass_valid = Signal()
        self.pass_data0 = Signal(16)
        self.pass_data1 = Signal(16)
        self.pack_valid = Signal()
        self.pack_data0 = Signal(16)
        self.pack_data1 = Signal(16)
        self.gpio_out = Signal(32)
        self.gpio_in = Signal(32)

    def ports(self):
        return [
            self.adc_valid, self.adc_i, self.adc_q,
            self.pass_valid, self.pass_data0, self.pass_data1,
            self.pack_valid, self.pack_data0, self.pack_data1,
            self.gpio_out, self.gpio_in,
        ]

    def elaborate(self, platform):
        m = Module()
        m.submodules.core = core = self.core
        m.submodules.control = ctl = self.control
        m.d.comb += [
            ctl.gpio_out.eq(self.gpio_out),
            self.gpio_in.eq(ctl.gpio_in),
            core.i_valid.eq(self.adc_valid & ctl.run),
            core.i_i.eq(self.adc_i),
            core.i_q.eq(self.adc_q),
            self.pack_valid.eq(Mux(ctl.symbols, core.o_valid, self.pass_valid)),
            self.pack_data0.eq(Mux(ctl.symbols, core.o_word, self.pass_data0)),
            self.pack_data1.eq(Mux(ctl.symbols, 0, self.pass_data1)),
        ]
        return m


class SymbolsTx(Elaboratable):
    """The transmit block for the bitstream: `SymbolTx` and its control,
    between the DAC unpacker and the rfifo in the divclk domain, where
    Tezuka's TX FIR interpolator sits (maia-hdl `projects/common/
    txfir.tcl`, whose place it takes; fpga/tezuka/isdb_symbols_tx.tcl):

        axi_ad9361_dac_fifo din_valid_0     --> req
        rd_en                               --> util_ad9361_dac_upack fifo_rd_en
        util_ad9361_dac_upack fifo_rd_data_0/1, fifo_rd_valid, fifo_rd_underflow
                                            --> rd_data0/1, rd_valid, rd_underflow
        util_ad9361_dac_upack fifo_rd_data_0/1, fifo_rd_valid
                                            --> pass_data0/1, pass_valid
        out_data0/1, out_valid              --> axi_ad9361_dac_fifo din_data_0/1,
                                                din_valid_in_0/1
        axi_ad9361 up_dac_gpio_out[31:0]    --> gpio_out
        gpio_in                             --> axi_ad9361 up_dac_gpio_in[31:0]

    Not selected (CTRL bit 2 clear, the reset state) it is transparent: the
    rfifo's request reads the upack and the upack's words go to the rfifo,
    as without the block. Selected, the block owns `fifo_rd_en` (one pulse
    per DMA word of 8 codes) and drives the rfifo's channels 0 and 1 (TX1 I
    and Q, the 12-bit sample MSB-aligned: dac_data[15:4]) with their valid
    held high, as txfir's `dac_fifo_valid_or` does in FIR mode: the rfifo
    then writes on every clock, which is the sample rate on the PlutoSky
    R2, and its request strobe is high on nearly every clock. The host
    enables `voltage0` and `voltage1` of `cf-ad9361-dds-core-lpc` (both
    16-bit words of a DMA sample are codes).
    """

    def __init__(self, phases, span, a=None, b=None):
        self.core = SymbolTx(phases, span, a, b)
        self.control = TxControl(self.core)
        self.req = Signal()
        self.rd_en = Signal()
        self.rd_data0 = Signal(16)
        self.rd_data1 = Signal(16)
        self.rd_valid = Signal()
        self.rd_underflow = Signal()
        self.pass_data0 = Signal(16)
        self.pass_data1 = Signal(16)
        self.pass_valid = Signal()
        self.out_data0 = Signal(16)
        self.out_data1 = Signal(16)
        self.out_valid = Signal()
        self.gpio_out = Signal(32)
        self.gpio_in = Signal(32)

    def ports(self):
        return [
            self.req, self.rd_en, self.rd_data0, self.rd_data1, self.rd_valid,
            self.rd_underflow, self.pass_data0, self.pass_data1, self.pass_valid,
            self.out_data0, self.out_data1, self.out_valid, self.gpio_out, self.gpio_in,
        ]

    def elaborate(self, platform):
        m = Module()
        m.submodules.core = core = self.core
        m.submodules.control = ctl = self.control
        sel = ctl.selected
        m.d.comb += [
            ctl.gpio_out.eq(self.gpio_out),
            self.gpio_in.eq(ctl.gpio_in),
            core.req.eq(self.req),
            core.rd_data.eq(Cat(self.rd_data0, self.rd_data1)),
            core.rd_valid.eq(self.rd_valid),
            core.rd_underflow.eq(self.rd_underflow),
            self.rd_en.eq(Mux(sel, core.rd_en, self.req)),
            self.out_data0.eq(Mux(sel, Cat(Const(0, 4), core.o_i), self.pass_data0)),
            self.out_data1.eq(Mux(sel, Cat(Const(0, 4), core.o_q), self.pass_data1)),
            self.out_valid.eq(sel | self.pass_valid),
        ]
        return m
