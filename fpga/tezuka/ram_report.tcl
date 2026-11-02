# Memory by memory: what synthesis mapped each RTL memory of a checkpoint
# to (block RAM, LUT RAM), its shape and its primitives. For the block RAM
# budget of an image (fpga/README.md "Building the PlutoSky R2 bitstream").
#
#   fpga/tezuka/vivado-batch.sh ram_report.tcl CHECKPOINT.dcp [OUT.rpt]
set dcp [lindex $argv 0]
set out [expr {[llength $argv] > 1 ? [lindex $argv 1] : "[file rootname $dcp]_ram.rpt"}]
open_checkpoint $dcp
report_ram_utilization -detail -file $out
puts "RAM $dcp: $out"
