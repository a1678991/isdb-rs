"""The block that goes into the PlutoSky R2 bitstream: the front end, its
control and the output selection, between the existing sample path and
the ADC packer that feeds the RX DMA (Tezuka's maia-hdl block design,
`projects/common/cs12_cs8.tcl`):

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
is transparent. Everything runs in the ADC clock domain (dout_clk of
util_ad9361_adc_fifo, 4x the sample rate).
"""

from amaranth.hdl import Elaboratable, Module, Mux, Signal, signed

from .control import Control
from .frontend import SymbolFrontend


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
