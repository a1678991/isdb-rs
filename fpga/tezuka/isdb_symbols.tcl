# isdb-rs symbol front end (SymbolsRx, `isdb_symbols_rx`) on the PlutoSky R2
# RX path of Tezuka's maia-hdl block design. Sourced by system_bd.tcl after
# cs12_cs8.tcl when the board sets `isdb_symbols` (fpga/tezuka/apply.sh).
#
# cs12_cs8.tcl leaves this at the ADC packer's channel 0:
#   cs12_sync_chan0/data_out0, data_out1 --> util_ad9361_adc_pack/fifo_wr_data_0, _1
#   channels_or/Res                      --> util_ad9361_adc_pack/fifo_wr_en
# This block takes those three nets as its pass-through and drives the
# packer instead, so with symbols off (CTRL bit 2 clear, the reset state)
# the stock path is unchanged; with symbols on, channel 0 carries the
# Symbols words. Its front end reads the same samples cs12_8mux_chan0 does,
# mux_decim_i/q data_out with mux_decim_i valid_out (full rate, or the DDC
# output when Tezuka's decimation is selected). Clock and reset: the ADC
# packer's domain, util_ad9361_adc_fifo dout_clk / dout_rstn (active low;
# the block's rst is active high).
#
# Control: axi_ad9361 up_adc_gpio_out (bits 31..1; bit 0 stays Tezuka's:
# it selects the DDC path in maiafirtoiq.tcl and drives cs12 zero_count in
# cs12_cs8.tcl) and up_adc_gpio_in (the ID word / readback; unused by
# Tezuka at this revision). No new AXI slave, no device-tree change.
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence). It drives Tezuka/maia-hdl and ADI HDL Tcl procs
# (ad_ip_instance, ad_connect, ad_disconnect) but contains none of their code.

if {![info exists ::isdb_symbols_rx_v]} {
    error "isdb_symbols.tcl: ::isdb_symbols_rx_v (path of isdb_symbols_rx.v) is not set"
}
add_files -norecurse $::isdb_symbols_rx_v
update_compile_order -fileset sources_1

create_bd_cell -type module -reference isdb_symbols_rx isdb_symbols

# clock and reset of the packer's domain
ad_connect util_ad9361_adc_fifo/dout_clk isdb_symbols/clk
ad_ip_instance util_vector_logic isdb_symbols_rst [list C_OPERATION {not} C_SIZE 1]
ad_connect util_ad9361_adc_fifo/dout_rstn isdb_symbols_rst/Op1
ad_connect isdb_symbols_rst/Res isdb_symbols/rst

# front-end input: the samples cs12_8mux_chan0 reads
ad_connect mux_decim_i/data_out isdb_symbols/adc_i
ad_connect mux_decim_q/data_out isdb_symbols/adc_q
ad_connect mux_decim_i/valid_out isdb_symbols/adc_valid

# pass-through: what cs12_cs8.tcl drives into the packer's channel 0
ad_disconnect cs12_sync_chan0/data_out0 util_ad9361_adc_pack/fifo_wr_data_0
ad_disconnect cs12_sync_chan0/data_out1 util_ad9361_adc_pack/fifo_wr_data_1
ad_disconnect channels_or/Res util_ad9361_adc_pack/fifo_wr_en
ad_connect cs12_sync_chan0/data_out0 isdb_symbols/pass_data0
ad_connect cs12_sync_chan0/data_out1 isdb_symbols/pass_data1
ad_connect channels_or/Res isdb_symbols/pass_valid
ad_connect isdb_symbols/pack_data0 util_ad9361_adc_pack/fifo_wr_data_0
ad_connect isdb_symbols/pack_data1 util_ad9361_adc_pack/fifo_wr_data_1
ad_connect isdb_symbols/pack_valid util_ad9361_adc_pack/fifo_wr_en

# control through the AD9361 core's GPIO registers
ad_connect axi_ad9361/up_adc_gpio_out isdb_symbols/gpio_out
ad_connect isdb_symbols/gpio_in axi_ad9361/up_adc_gpio_in
