# Whole-design utilisation of a routed Vivado checkpoint, by hierarchy:
# what every block of the image costs (ADI's AD9361 core, the DMAs and
# interconnects, maia-sdr's spectrometer / DDC / recorder, Tezuka's cs12
# packers, the isdb blocks), for the budget of a new block
# (docs/plans/isdbs3-fpga-rx.md "Capacity"). timing.tcl reports the isdb
# blocks alone.
#
#   vivado -mode batch -source fpga/tezuka/utilization.tcl -tclargs CHECKPOINT.dcp [OUT.rpt] [DEPTH]
set dcp [lindex $argv 0]
set out [expr {[llength $argv] > 1 ? [lindex $argv 1] : "[file rootname $dcp]_util.rpt"}]
set depth [expr {[llength $argv] > 2 ? [lindex $argv 2] : 6}]
open_checkpoint $dcp
report_utilization -file $out
report_utilization -hierarchical -hierarchical_depth $depth -file "[file rootname $out]_hier.rpt"
puts "UTIL $dcp: $out, [file rootname $out]_hier.rpt"
