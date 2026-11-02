# The receive-only image's base, in place of maia-hdl's maia.tcl,
# maiafirtoiq.tcl and sweeper.tcl (fpga/tezuka/apply.sh --isdbs3-rx): the
# PlutoSky R2 design without the maia-sdr IP core (spectrometer, DDC,
# recorder) and its clock wizard, which the ISDB-S3 receive chain needs the
# room of (docs/plans/isdbs3-fpga-rx.md, "Budget for the whole chain").
#
# What those three files did that the rest of the design still needs, and
# nothing else:
#   - the DMAs' memory port: maia.tcl put the ADC and DAC DMAs (and its
#     recorder) on S_AXI_HP2 through ad_mem_hp2_interconnect; here the two
#     DMAs alone. S_AXI_HP1 (the spectrometer's) is switched off (ps7.tcl
#     enables it; an enabled HP port needs its clock connected).
#   - mux_decim_i / mux_decim_q (maiafirtoiq.tcl), which cs12_cs8.tcl and
#     the isdb blocks read: the same ad_bus_mux cells on the same nets, with
#     path 0 (the AD9361 samples) selected for good: path 1 was maia-sdr's
#     DDC output, absent here (up_adc_gpio_out bit 0, which selected it,
#     still drives the cs12 packers' zero_count).
# Left out: the sweeper (sweeper.tcl: maia-sdr's fast-lock profile on the
# PS GPIO; gpio_o stays on sys_ps7/GPIO_O as xilinx_ad9361.tcl connects
# it), maia-sdr's interrupt (ps-11 stays tied low).
#
# The kernel's device tree still has maia-sdr's UIO node at 0x7c460000:
# maia-httpd's first register read there gets a decode error from the AXI
# interconnect (a bus error in that process, as Tezuka's own scripts take
# on the registers of blocks a board lacks), and S45msd's devmem read of
# the version leaves the web page's FPGA line empty. The kernel itself
# does not read the node's registers (uio_pdrv_genirq maps them).
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence). It drives Tezuka/maia-hdl and ADI HDL Tcl procs
# (ad_ip_instance, ad_connect, ad_disconnect) but contains none of their code.

# the DMAs on HP2
ad_mem_hp2_interconnect sys_cpu_clk sys_ps7/S_AXI_HP2
ad_mem_hp2_interconnect sys_cpu_clk axi_ad9361_adc_dma/m_dest_axi
ad_mem_hp2_interconnect sys_cpu_clk axi_ad9361_dac_dma/m_src_axi
ad_ip_parameter sys_ps7 CONFIG.PCW_USE_S_AXI_HP1 0

# the ADC FIFO's channels 0 and 1 to the packer through the muxes
# (ad_disconnect removes the whole net of its first pin)
ad_disconnect util_ad9361_adc_fifo/dout_data_0 util_ad9361_adc_pack/fifo_wr_data_0
ad_disconnect util_ad9361_adc_fifo/dout_data_1 util_ad9361_adc_pack/fifo_wr_data_1
ad_disconnect util_ad9361_adc_pack/fifo_wr_en util_ad9361_adc_fifo/dout_valid_0
ad_disconnect util_ad9361_adc_fifo/dout_enable_0 util_ad9361_adc_pack/enable_0
ad_disconnect util_ad9361_adc_fifo/dout_enable_1 util_ad9361_adc_pack/enable_1

foreach {ch n} {i 0 q 1} {
    create_bd_cell -type module -reference ad_bus_mux mux_decim_$ch
    ad_connect mux_decim_$ch/select_path GND
    ad_connect util_ad9361_adc_fifo/dout_valid_$n mux_decim_$ch/valid_in_0
    ad_connect util_ad9361_adc_fifo/dout_enable_$n mux_decim_$ch/enable_in_0
    ad_connect util_ad9361_adc_fifo/dout_data_$n mux_decim_$ch/data_in_0
    ad_connect util_ad9361_adc_fifo/dout_enable_$n mux_decim_$ch/enable_in_1
    ad_connect mux_decim_$ch/valid_in_1 GND
    ad_connect mux_decim_$ch/data_in_1 GND
    ad_connect mux_decim_$ch/data_out util_ad9361_adc_pack/fifo_wr_data_$n
    ad_connect mux_decim_$ch/enable_out util_ad9361_adc_pack/enable_$n
}
ad_connect mux_decim_i/valid_out util_ad9361_adc_pack/fifo_wr_en
