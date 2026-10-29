# isdb-rs ISDB-S transmit block (SymbolsTx, `isdb_symbols_tx`) on the
# PlutoSky R2 DAC path of Tezuka's maia-hdl block design, in the place of
# Tezuka's x8 TX FIR interpolator (projects/common/txfir.tcl, which this
# image does not source: its 20 DSP48E1 do nothing at 40 Msps). Sourced by
# system_bd.tcl where txfir.tcl would be, when the board sets
# `isdb_symbols_tx` (fpga/tezuka/apply.sh).
#
# xilinx_ad9361.tcl leaves this between the DAC unpacker and the rfifo
# (both on util_ad9361_divclk):
#   axi_ad9361_dac_fifo/din_valid_0          --> util_ad9361_dac_upack/fifo_rd_en
#   util_ad9361_dac_upack/fifo_rd_data_0..3  --> axi_ad9361_dac_fifo/din_data_0..3
#   util_ad9361_dac_upack/fifo_rd_valid      --> axi_ad9361_dac_fifo/din_valid_in_0..3
#   util_ad9361_dac_upack/fifo_rd_underflow  --> axi_ad9361_dac_fifo/din_unf
# This block takes over, as txfir.tcl does, the read request and channels
# 0 and 1 (TX1 I and Q): the rfifo's request goes to the block's `req`,
# the block drives `fifo_rd_en`, reads the unpacker's words and drives
# din_data_0/1 and din_valid_in_0/1. Not selected (CTRL bit 2 clear, the
# reset state) it passes today's nets through (req -> fifo_rd_en, the
# unpacker's data and valid -> the rfifo), so the stock path is
# unchanged. Selected, it pulls one DMA word (8 symbol codes) per
# fifo_rd_en pulse when its unpack register needs one, makes one 12-bit
# I/Q sample per request strobe (dac_data[15:4]) and holds
# din_valid_in_0/1 high, as txfir.tcl's dac_fifo_valid_or does in FIR
# mode (the rfifo then writes on every clock, which is the DAC sample
# rate here). Channels 2 and 3 (TX2), din_unf (the DAC core's underflow
# flag: it reports the block's missed words too) and the channel enables
# stay as xilinx_ad9361.tcl connects them.
#
# Clock and reset: the unpacker's, util_ad9361_divclk/clk_out and
# util_ad9361_divclk_reset/peripheral_reset (active high, as the block's
# rst). That clock is l_clk / 4 = the sample rate on the PlutoSky R2 (2R2T
# LVDS, r1_mode 0; 40 MHz at 40 Msps), the RX block's clock too.
#
# Control: axi_ad9361 up_dac_gpio_out (bits 31..1; bit 0 is txfir's
# `active` in the stock image and also written by the kernel's
# interpolation attribute, the block ignores it) and up_dac_gpio_in (the
# ID word / readback; unconnected in Tezuka v0.3.24). No new AXI slave, no
# device-tree change: cf-ad9361-dds-core-lpc debug addresses 0x800000BC and
# 0x800000B8.
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence). It drives Tezuka/maia-hdl and ADI HDL Tcl procs
# (ad_ip_instance, ad_connect, ad_disconnect) but contains none of their
# code.

if {![info exists ::isdb_symbols_tx_v]} {
    error "isdb_symbols_tx.tcl: ::isdb_symbols_tx_v (path of isdb_symbols_tx.v) is not set"
}
add_files -norecurse $::isdb_symbols_tx_v
update_compile_order -fileset sources_1
# its timing constraints (CDC of the control word, the clock case it runs
# in), implementation only and processed late, as isdb_symbols.xdc
if {[info exists ::isdb_symbols_tx_xdc]} {
    add_files -norecurse -fileset constrs_1 $::isdb_symbols_tx_xdc
    set_property USED_IN_SYNTHESIS false [get_files $::isdb_symbols_tx_xdc]
    set_property PROCESSING_ORDER LATE [get_files $::isdb_symbols_tx_xdc]
}

create_bd_cell -type module -reference isdb_symbols_tx isdb_symbols_tx

# clock and reset of the unpacker's domain
ad_connect util_ad9361_divclk/clk_out isdb_symbols_tx/clk
ad_connect util_ad9361_divclk_reset/peripheral_reset isdb_symbols_tx/rst

# the nets txfir.tcl takes over
ad_disconnect axi_ad9361_dac_fifo/din_valid_0 util_ad9361_dac_upack/fifo_rd_en
ad_disconnect util_ad9361_dac_upack/fifo_rd_data_0 axi_ad9361_dac_fifo/din_data_0
ad_disconnect util_ad9361_dac_upack/fifo_rd_data_1 axi_ad9361_dac_fifo/din_data_1
ad_disconnect util_ad9361_dac_upack/fifo_rd_valid axi_ad9361_dac_fifo/din_valid_in_0
ad_disconnect util_ad9361_dac_upack/fifo_rd_valid axi_ad9361_dac_fifo/din_valid_in_1

# the rfifo's request in, the unpacker's read out
ad_connect axi_ad9361_dac_fifo/din_valid_0 isdb_symbols_tx/req
ad_connect isdb_symbols_tx/rd_en util_ad9361_dac_upack/fifo_rd_en

# the unpacker's words: the block's codes, and the pass-through
ad_connect util_ad9361_dac_upack/fifo_rd_data_0 isdb_symbols_tx/rd_data0
ad_connect util_ad9361_dac_upack/fifo_rd_data_1 isdb_symbols_tx/rd_data1
ad_connect util_ad9361_dac_upack/fifo_rd_valid isdb_symbols_tx/rd_valid
ad_connect util_ad9361_dac_upack/fifo_rd_underflow isdb_symbols_tx/rd_underflow
ad_connect util_ad9361_dac_upack/fifo_rd_data_0 isdb_symbols_tx/pass_data0
ad_connect util_ad9361_dac_upack/fifo_rd_data_1 isdb_symbols_tx/pass_data1
ad_connect util_ad9361_dac_upack/fifo_rd_valid isdb_symbols_tx/pass_valid

# samples (or the pass-through) into the rfifo's channels 0 and 1
ad_connect isdb_symbols_tx/out_data0 axi_ad9361_dac_fifo/din_data_0
ad_connect isdb_symbols_tx/out_data1 axi_ad9361_dac_fifo/din_data_1
ad_connect isdb_symbols_tx/out_valid axi_ad9361_dac_fifo/din_valid_in_0
ad_connect isdb_symbols_tx/out_valid axi_ad9361_dac_fifo/din_valid_in_1

# control through the DAC core's GPIO registers
ad_connect axi_ad9361/up_dac_gpio_out isdb_symbols_tx/gpio_out
ad_connect isdb_symbols_tx/gpio_in axi_ad9361/up_dac_gpio_in
