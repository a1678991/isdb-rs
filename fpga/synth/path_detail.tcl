# The worst setup path into the cells matching PATTERN of a routed
# checkpoint, in detail (logic levels, fanouts, net delays): read-only.
#
#   vivado -mode batch -source path_detail.tcl -tclargs CHECKPOINT.dcp PATTERN [N]
set dcp [lindex $argv 0]
set pat [lindex $argv 1]
set n [expr {[llength $argv] > 2 ? [lindex $argv 2] : 1}]
open_checkpoint $dcp
report_timing -setup -to [get_cells -hier -filter "NAME =~ $pat"] -max_paths $n -nworst 1 -input_pins
