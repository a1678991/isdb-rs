# Timing of the symbol-clock <-> fast-clock crossings of isdb_slots on a
# placed or routed checkpoint of the third isdbs3-rx build, whose wrapper
# put fast_clk through a BUFR (BYPASS) and a BUFGMUX_CTRL with its select
# tied: Vivado propagated rx_clk through the BUFR but not the BUFGCTRL, so
# the block's fast registers were unclocked. This defines the clock on the
# global buffer's output (divide by 1, from its I0), re-applies
# isdb_slots.xdc's fast-domain rules to it, and reports the clock-pair
# slacks and the worst crossings with their skew. (isdb_slots_top.v now
# uses a BUFG, through which the clock propagates: timing.tcl suffices.)
#
#   fpga/tezuka/vivado-batch.sh skew_check.tcl CHECKPOINT.dcp OUT.rpt
set dcp [lindex $argv 0]
set out [lindex $argv 1]
open_checkpoint $dcp
set bufr [get_cells -hier -filter {NAME =~ */isdb_slots/inst/fast_bufr_i}]
foreach pin [list $bufr/I $bufr/O] {
    puts "SKEW clocks at $pin before: [get_clocks -quiet -of_objects [get_pins $pin]]"
}
set gbuf [get_cells -hier -filter {NAME =~ */isdb_slots/inst/fast_gbuf_i}]
puts "SKEW $gbuf: [get_property REF_NAME $gbuf], O clocks [get_clocks -quiet -of_objects [get_pins $gbuf/O]]"
puts "SKEW BUFR driver: [get_cells -quiet -of_objects [get_pins -leaf -filter {DIRECTION == OUT} -of_objects [get_nets -of_objects [get_pins $bufr/I]]]]"
# rx_clk reaches the BUFR's output but not the BUFGMUX_CTRL's (mapped to a
# BUFGCTRL with S tied: Vivado stops the clock there), so the clock is
# defined on the global buffer's output, from its I0
create_generated_clock -name slots_fast_clk -divide_by 1 \
    -source [get_pins $gbuf/I0] [get_pins $gbuf/O]
set slots_fast [filter [all_registers -clock [get_clocks slots_fast_clk]] {NAME =~ */isdb_slots/inst/*}]
set slots_slow [filter [all_registers -clock [get_clocks clk_div_sel_0_s]] {NAME =~ */isdb_slots/inst/*}]
puts "SKEW fast registers [llength $slots_fast], slow registers [llength $slots_slow]"
set_false_path -from [get_clocks clk_div_sel_1_s] -to $slots_fast
set_false_path -from $slots_fast -to [get_clocks clk_div_sel_1_s]
set_max_delay 6.250 -from $slots_fast -to $slots_fast
set_max_delay 6.250 -from $slots_slow -to $slots_fast
set_max_delay 6.250 -from $slots_fast -to $slots_slow
report_clock_interaction -delay_type min_max -file $out
report_timing_summary -delay_type min_max -max_paths 5 -nworst 1 -append -file $out
foreach {a b} {clk_div_sel_0_s slots_fast_clk slots_fast_clk clk_div_sel_0_s slots_fast_clk slots_fast_clk} {
    set p [get_timing_paths -quiet -setup -from [get_clocks $a] -to [get_clocks $b] -max_paths 1]
    set h [get_timing_paths -quiet -hold -from [get_clocks $a] -to [get_clocks $b] -max_paths 1]
    if {[llength $p]} {
        puts "SKEW $a -> $b: setup [get_property SLACK $p] (skew [get_property SKEW $p]) hold [expr {[llength $h] ? [get_property SLACK $h] : "-"}]: [get_property STARTPOINT_PIN $p] -> [get_property ENDPOINT_PIN $p]"
    }
}
