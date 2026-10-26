# Timing constraints of the isdb-rs symbol front end (isdb_symbols_rx) in
# Tezuka's PlutoSky R2 design. Added to constrs_1 by isdb_symbols.tcl.
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence).

# 1. Control in: axi_ad9361 up_adc_gpio_out (clk_fpga_0, the AXI clock) ->
#    the block's two-register synchroniser (fpga/isdb_fpga/control.py:
#    every bit passes s1, s2; a command executes two cycles after its
#    toggle bit changes in s2, when all bits have settled). The host writes
#    the word milliseconds apart. Tezuka's own system_constr.xdc and ADI's
#    Pluto constraints declare the same register's other bits false paths
#    for the same reason (up_adc_gpio_out_int_reg[0..2]).
set_false_path -from [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_rx/i_up_adc_common/up_adc_gpio_out_int_reg[*]/C}] \
               -to [get_cells -hier -filter {NAME =~ */isdb_symbols/inst/control/s1_reg*}]

# 2. Control out: the block's gpio_in (dout_clk domain: the ID word, or a
#    register's readback after an OP_READ) -> axi_ad9361 up_rdata
#    (clk_fpga_0). The word changes only in response to a command, which
#    the host issues and then reads back over AXI at least one libiio
#    register round trip (hundreds of microseconds) later; it is static
#    whenever it is sampled. A datapath-only bound keeps the skew sane
#    without claiming a synchronous relation.
set_max_delay -datapath_only 10.0 \
    -from [get_cells -hier -filter {NAME =~ */isdb_symbols/inst/control/* && IS_SEQUENTIAL}] \
    -to [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_rx/i_up_adc_common/up_rdata_int_reg[*]/D}]

# 3. The block's clock: util_ad9361_divclk selects l_clk / 4 (clk_div_sel_0)
#    while the AD9361 core is not in r1_mode, and l_clk / 2 (clk_div_sel_1)
#    when both its ADC and DAC are (ADI util_clkdiv; maia-hdl
#    projects/common/xilinx_ad9361.tcl, LVDS: SEL_0_DIV 4, SEL_1_DIV 2).
#    The PlutoSky R2 runs 2R2T with r1_mode 0 (measured: register
#    0x80000044 bit 2 reads 0 on both cores), and the host refuses the
#    symbol path when r1_mode is set (isdb-radio pluto backend,
#    R1_MODE_REG). So the block is timed for the l_clk / 4 case only: the
#    paths of its registers in the l_clk / 2 clock are false. (ADI's
#    util_clkdiv_constr.xdc already makes the two cases logically
#    exclusive; this removes the unused one for this block alone, leaving
#    every other block's timing as Tezuka's.)
set_false_path -from [get_clocks clk_div_sel_1_s] \
    -to [get_cells -hier -filter {NAME =~ */isdb_symbols/inst/* && IS_SEQUENTIAL}]
set_false_path -from [get_cells -hier -filter {NAME =~ */isdb_symbols/inst/* && IS_SEQUENTIAL}] \
    -to [get_clocks clk_div_sel_1_s]
