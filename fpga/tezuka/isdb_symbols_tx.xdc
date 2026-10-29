# Timing constraints of the isdb-rs ISDB-S transmit block (isdb_symbols_tx)
# in Tezuka's PlutoSky R2 design. Added to constrs_1 by isdb_symbols_tx.tcl
# (implementation only, processed late: util_clkdiv's generated clocks and
# the block's cells exist only then). The same three as isdb_symbols.xdc,
# on the DAC core's GPIO pair.
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence).

# 1. Control in: axi_ad9361 up_dac_gpio_out (clk_fpga_0, the AXI clock) ->
#    the block's two-register synchroniser (fpga/isdb_fpga/tx.py
#    TxControl: every bit passes s1, s2; a command executes two cycles
#    after its toggle bit changes in s2, when all bits have settled). The
#    host writes the word milliseconds apart. Tezuka's own
#    system_constr.xdc declares the same register's bits 0..2 false paths
#    for the same reason (up_dac_gpio_out_int_reg[0..2]).
set_false_path -from [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_tx/i_up_dac_common/up_dac_gpio_out_int_reg[*]/C}] \
               -to [get_cells -hier -filter {NAME =~ */isdb_symbols_tx/inst/control/s1_reg*}]

# 2. Control out: the block's gpio_in (divclk domain: the ID word, or a
#    register's readback after an OP_READ) -> axi_ad9361 up_rdata
#    (clk_fpga_0, the DAC common's word 0x2E). The word changes only in
#    response to a command, which the host issues and then reads back over
#    AXI at least one libiio register round trip (hundreds of
#    microseconds) later; it is static whenever it is sampled. A
#    datapath-only bound keeps the skew sane without claiming a
#    synchronous relation.
set_max_delay -datapath_only 10.0 \
    -from [get_cells -hier -filter {NAME =~ */isdb_symbols_tx/inst/control/* && IS_SEQUENTIAL}] \
    -to [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_tx/i_up_dac_common/up_rdata_int_reg[*]/D}]

# 3. The block's clock: util_ad9361_divclk selects l_clk / 4 (clk_div_sel_0)
#    while the AD9361 core is not in r1_mode, and l_clk / 2 (clk_div_sel_1)
#    when both its ADC and DAC are (ADI util_clkdiv; maia-hdl
#    projects/common/xilinx_ad9361.tcl, LVDS: SEL_0_DIV 4, SEL_1_DIV 2).
#    The PlutoSky R2 runs 2R2T with r1_mode 0 on both cores (measured:
#    ADC 0x80000044 bit 2 and DAC 0x80000048 bit 5 read 0, VERIFICATION.md
#    "ISDB-S transmitter in the FPGA: board checks"), and the host refuses
#    the offloaded paths when r1_mode is set (isdb-radio pluto backend).
#    So the block is timed for the l_clk / 4 case only: the paths of its
#    registers in the l_clk / 2 clock are false, leaving every other
#    block's timing as Tezuka's.
set_false_path -from [get_clocks clk_div_sel_1_s] \
    -to [get_cells -hier -filter {NAME =~ */isdb_symbols_tx/inst/* && IS_SEQUENTIAL}]
set_false_path -from [get_cells -hier -filter {NAME =~ */isdb_symbols_tx/inst/* && IS_SEQUENTIAL}] \
    -to [get_clocks clk_div_sel_1_s]
