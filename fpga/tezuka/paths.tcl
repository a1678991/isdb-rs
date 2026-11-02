# The worst setup paths of a checkpoint, per capture clock: a summary of
# the N worst endpoints and the full path of the first few, for finding
# which block and which structure misses timing.
#
#   fpga/tezuka/vivado-batch.sh paths.tcl CHECKPOINT.dcp OUT.rpt N CLOCK...
#
# Each CLOCK is a clock name (get_clocks), e.g. rx_clk, c0, clk_div_sel_0_s.
# Also writes the timing exceptions (report_exceptions) the checkpoint
# carries, to check that a block's XDC rules found their objects.
set dcp [lindex $argv 0]
set out [lindex $argv 1]
set n [lindex $argv 2]
set clocks [lrange $argv 3 end]
open_checkpoint $dcp
set f [open $out w]
close $f
foreach c $clocks {
    set ck [get_clocks $c]
    report_timing -to $ck -setup -max_paths $n -nworst 1 -sort_by slack -path_type summary -append -file $out
    report_timing -to $ck -setup -max_paths 3 -nworst 1 -sort_by slack -path_type full -input_pins -append -file $out
    report_timing -from $ck -setup -max_paths $n -nworst 1 -sort_by slack -path_type summary -append -file $out
}
report_exceptions -ignored -file "[file rootname $out]_exceptions.rpt"
report_exceptions -append -file "[file rootname $out]_exceptions.rpt"
puts "PATHS $dcp: $out"
