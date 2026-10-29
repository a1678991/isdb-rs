# Timing of a routed Vivado checkpoint, for the image builds of build.sh:
# the design-wide setup and hold slacks, per clock, and the worst hold
# paths (which the Vivado 2025/2026 router leaves unfixed: upstream's
# hold_fix.tcl). The project flow's *_timing_summary_routed.rpt is taken
# after route_design, before the post-route physical optimisation the
# bitstream is written from, so read the final checkpoint instead.
#
#   vivado -mode batch -source fpga/tezuka/timing.tcl -tclargs CHECKPOINT.dcp [OUT.rpt]
set dcp [lindex $argv 0]
set out [expr {[llength $argv] > 1 ? [lindex $argv 1] : "[file rootname $dcp]_timing.rpt"}]
open_checkpoint $dcp
report_timing_summary -delay_type min_max -max_paths 10 -nworst 1 -report_unconstrained -file $out
set wns [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -setup]]
set whs [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -hold]]
puts "TIMING $dcp: WNS $wns WHS $whs (report $out)"
# The isdb blocks' own placed resources (LUT, FF, BRAM, DSP), by hierarchy.
set util "[file rootname $out]_isdb_util.rpt"
report_utilization -hierarchical -hierarchical_depth 3 -cells [get_cells -hier -filter {NAME =~ *isdb_symbols*/inst}] -file $util
foreach c [get_cells -hier -filter {NAME =~ *isdb_symbols*/inst}] {
    puts "UTIL $c: LUT [llength [get_cells -hier -filter "PRIMITIVE_GROUP == LUT && NAME =~ $c/*"]] FF [llength [get_cells -hier -filter "PRIMITIVE_GROUP == FLOP_LATCH && NAME =~ $c/*"]] BRAM [llength [get_cells -hier -filter "PRIMITIVE_GROUP == BLOCKRAM && NAME =~ $c/*"]] DSP [llength [get_cells -hier -filter "PRIMITIVE_GROUP == DSP && NAME =~ $c/*"]]"
}
