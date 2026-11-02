# isdb-rs ISDB-S3 receive block (SlotsRx: the symbol front end, the chain
# and the LDPC decoder; module isdb_slots_top around isdb_slots_rx) on the
# PlutoSky R2 RX path of Tezuka's maia-hdl block design, in place of the
# front end alone (isdb_symbols.tcl). Sourced by system_bd.tcl after
# cs12_cs8.tcl when the board sets `isdb_slots` (fpga/tezuka/apply.sh
# --isdbs3-rx).
#
# The same ADC-path connections as isdb_symbols.tcl (the block is a
# superset of SymbolsRx: with CTRL SLOTS clear it is the front end, with
# SYMBOLS clear too the stock path passes through), plus:
#   fast_clk  axi_ad9361/l_clk (the 4x of the symbol clock; 160 MHz at
#             40 Msps on the PlutoSky R2)
#   ldpc_ref  sys_cpu_clk = sys_ps7/FCLK_CLK0 (clk_fpga_0, 100 MHz) into
#             the block's MMCM, the LDPC decoder's 200 MHz
# and the timing constraints of fpga/tezuka/isdb_slots.xdc (implementation
# only, late, like isdb_symbols.xdc). The block-design cell is named
# `isdb_slots`; the XDC's patterns use that name.
#
# Clock and reset: the ADC packer's domain, util_ad9361_adc_fifo dout_clk /
# dout_rstn (active low; the block's rst is active high), as for
# isdb_symbols; the fast domain's reset is made from it in
# isdb_slots_top.v.
#
# Control: axi_ad9361 up_adc_gpio_out (bits 31..1; bit 0 stays Tezuka's)
# and up_adc_gpio_in (the ID word with the chain's Slots version in bits
# 11..8, or a register's readback).
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence). It drives Tezuka/maia-hdl and ADI HDL Tcl procs
# (ad_ip_instance, ad_connect, ad_disconnect) but contains none of their code.

foreach v {::isdb_slots_rx_v ::isdb_slots_top_v} {
    if {![info exists $v]} {
        error "isdb_slots.tcl: $v is not set"
    }
}
add_files -norecurse [list $::isdb_slots_rx_v $::isdb_slots_top_v]
update_compile_order -fileset sources_1
if {[info exists ::isdb_slots_xdc]} {
    add_files -norecurse -fileset constrs_1 $::isdb_slots_xdc
    set_property USED_IN_SYNTHESIS false [get_files $::isdb_slots_xdc]
    set_property PROCESSING_ORDER LATE [get_files $::isdb_slots_xdc]
}

create_bd_cell -type module -reference isdb_slots_top isdb_slots

# clocks and reset
ad_connect util_ad9361_adc_fifo/dout_clk isdb_slots/clk
ad_ip_instance util_vector_logic isdb_slots_rst [list C_OPERATION {not} C_SIZE 1]
ad_connect util_ad9361_adc_fifo/dout_rstn isdb_slots_rst/Op1
ad_connect isdb_slots_rst/Res isdb_slots/rst
ad_connect axi_ad9361/l_clk isdb_slots/fast_clk
ad_connect sys_cpu_clk isdb_slots/ldpc_ref

# front-end input: the samples cs12_8mux_chan0 reads
ad_connect mux_decim_i/data_out isdb_slots/adc_i
ad_connect mux_decim_q/data_out isdb_slots/adc_q
ad_connect mux_decim_i/valid_out isdb_slots/adc_valid

# pass-through: what cs12_cs8.tcl drives into the packer's channel 0
ad_disconnect cs12_sync_chan0/data_out0 util_ad9361_adc_pack/fifo_wr_data_0
ad_disconnect cs12_sync_chan0/data_out1 util_ad9361_adc_pack/fifo_wr_data_1
ad_disconnect channels_or/Res util_ad9361_adc_pack/fifo_wr_en
ad_connect cs12_sync_chan0/data_out0 isdb_slots/pass_data0
ad_connect cs12_sync_chan0/data_out1 isdb_slots/pass_data1
ad_connect channels_or/Res isdb_slots/pass_valid
ad_connect isdb_slots/pack_data0 util_ad9361_adc_pack/fifo_wr_data_0
ad_connect isdb_slots/pack_data1 util_ad9361_adc_pack/fifo_wr_data_1
ad_connect isdb_slots/pack_valid util_ad9361_adc_pack/fifo_wr_en

# control through the AD9361 core's GPIO registers
ad_connect axi_ad9361/up_adc_gpio_out isdb_slots/gpio_out
ad_connect isdb_slots/gpio_in axi_ad9361/up_adc_gpio_in
