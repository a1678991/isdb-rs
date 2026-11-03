# Timing constraints of the isdb-rs ISDB-S3 receive block (isdb_slots_rx:
# the front end, the chain and the LDPC decoder; fpga/isdb_fpga/chain/top.py
# SlotsRx) in Tezuka's PlutoSky R2 design. Read with the others
# (isdb_symbols.xdc's rules are repeated here for this cell), as an
# implementation-only, late-processed constraint file like
# isdb_symbols.xdc (the generated clocks of util_clkdiv must exist).
# The block design cell is assumed to be `isdb_slots` (the image piece names
# it; change the patterns with it).
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence).
#
# The block's three clocks:
#
#   clk      util_ad9361_divclk (= axi_ad9361 l_clk / 4: clk_div_sel_0_s, the
#            symbol clock, 40 MHz on the PlutoSky R2 at 40 Msps; ADI times it
#            at 62.5 MHz from the worst-case 250 MHz rx_clk);
#   fast_clk axi_ad9361 l_clk itself (rx_clk: 160.0 MHz measured, the 4x of
#            clk: util_clkdiv divides it, so the two are related with a fixed
#            phase and every edge of clk is an edge of fast_clk); the
#            time-multiplexed datapaths (the demapper's metrics, the
#            equaliser's FIR) and the clock-enable phasing between the two
#            (common.Phase4: the phase from a toggle of clk; data from clk
#            registers read on any of the four phases, data to clk registers
#            written by phase 2 and held through the next edge of clk);
#   ldpc     the decoder's 200 MHz, from the block's MMCME2_BASE on ldpc_ref
#            = clk_fpga_0 (100 MHz; Vivado derives the 5 ns clock itself):
#            asynchronous to the other two, reached only through the
#            bridge's AsyncFIFOs and its health handshake (ldpc_if.LdpcBridge).
#
# The chain is designed for 40 Msps (l_clk 160 MHz): at another sample rate
# the fast domain runs out of these constraints, and the host takes the
# chain only at 40 Msps (CTRL START restarts it).

set slots_cells [get_cells -hier -filter {NAME =~ */isdb_slots/inst/* && IS_SEQUENTIAL}]

# 1. Control in / out: as isdb_symbols.xdc's rules 1 and 2 (gpio_out into the
#    two-register synchronisers, the static readback into up_rdata).
set_false_path -from [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_rx/i_up_adc_common/up_adc_gpio_out_int_reg[*]/C}] \
               -to [get_cells -hier -filter {NAME =~ */isdb_slots/inst/* && NAME =~ *s1_reg*}]
set_max_delay -datapath_only 10.0 \
    -from $slots_cells \
    -to [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/i_rx/i_up_adc_common/up_rdata_int_reg[*]/D}]

# 2. The r1_mode clock (l_clk / 2, never on this board; the host refuses the
#    block's paths then): false, as for isdb_symbols.
set_false_path -from [get_clocks clk_div_sel_1_s] -to $slots_cells
set_false_path -from $slots_cells -to [get_clocks clk_div_sel_1_s]

# 3. The fast domain on l_clk. ADI constrains rx_clk at its worst case,
#    4 ns (250 MHz, 61.44 Msps); the chain runs only at 40 Msps, l_clk
#    6.25 ns. Every path that starts or ends at one of the block's l_clk
#    registers gets the real period as its requirement (set_max_delay
#    without -datapath_only keeps the clock skew and the hold checks).
#    Between clk and fast_clk Vivado sees related clocks (clk_div_sel_0_s
#    is generated from rx_clk / 4): the requirement it would take is one
#    fast period after the common edge, which is what the phasing needs
#    (6.25 ns is also never more than the time the logic has: a clk
#    register is read by fast registers one to four fast cycles after it
#    changes; a fast register read by clk is written by phase 2, one fast
#    cycle or more before the clk edge).
set slots_fast [filter [all_registers -clock [get_clocks -of_objects [get_pins -hier -filter {NAME =~ */axi_ad9361/inst/l_clk}]]] {NAME =~ */isdb_slots/inst/*}]
set slots_slow [filter [all_registers -clock [get_clocks clk_div_sel_0_s]] {NAME =~ */isdb_slots/inst/*}]
set_max_delay 6.250 -from $slots_fast -to $slots_fast
set_max_delay 6.250 -from $slots_slow -to $slots_fast
set_max_delay 6.250 -from $slots_fast -to $slots_slow

# 3b. The chain's own symbol-clock paths at the real 25 ns (40 Msps). ADI
#    times util_ad9361_divclk at 16 ns (l_clk / 4 of its 4 ns rx_clk); the
#    chain needs l_clk at 160 MHz (its fast domain, rule 3) and so runs only
#    at 40 Msps, where the symbol clock is 25 ns. Only the chain's and the
#    decoder bridge's registers: the front end, its control and the
#    register file and output selection of `SlotsRx` keep ADI's 16 ns (the
#    front end alone serves other rates).
set chain_slow [filter $slots_slow {NAME =~ */isdb_slots/inst/rx/chain/* || NAME =~ */isdb_slots/inst/rx/bridge/*}]
set_max_delay 25.000 -from $chain_slow -to $chain_slow

# 4. The decoder's clock against the symbol clock: only the bridge's
#    AsyncFIFOs cross (Gray-coded pointers through two-register
#    synchronisers, the FIFO memories' read data), and its health snapshot
#    (two toggles through two-register synchronisers, the snapshot register
#    into the chain's copy, stable for two symbol clocks before it is
#    sampled: ldpc_if.LdpcBridge "health"); bound them to the faster
#    period, datapath only (the clocks have no phase relation). Given by
#    clocks, not cells: the bridge's FIFO memories are block RAMs clocked
#    by both (written in one domain, read in the other), and a cell rule
#    would also put 5 ns on the same-clock paths into and out of them (the
#    fifth build: a symbol-clock FIFO read address at 5 ns, -1.55 ns at
#    placement). The decoder's clock exists only in this block.
set slots_ldpc_clk [get_clocks -of_objects [get_pins -hier -filter {NAME =~ */isdb_slots/inst/*mmcm*/CLKOUT0}]]
set_max_delay -datapath_only 5.0 -from [get_clocks clk_div_sel_0_s] -to $slots_ldpc_clk
set_max_delay -datapath_only 5.0 -from $slots_ldpc_clk -to [get_clocks clk_div_sel_0_s]

# 5. Asynchronous resets into the block's reset synchronizers (Amaranth's
#    ResetSynchronizer and the AsyncFIFOs' rst_cdc: a reset asserted
#    asynchronously on the stages' PRE pins and released through the two
#    stages in the destination clock). The assertion path from the symbol
#    clock's reset into the decoder's clock is not a timed transfer.
set_false_path -to [get_pins -hier -filter {NAME =~ */isdb_slots/inst/* && NAME =~ *_cdc/stage*_reg/PRE}]
set_false_path -to [get_pins -hier -filter {NAME =~ */isdb_slots/inst/* && NAME =~ *_rst*/stage*_reg/PRE}]
