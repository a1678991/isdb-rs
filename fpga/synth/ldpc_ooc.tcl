# Out-of-context Vivado timing of the LDPC decoder alone (synth/ldpc_vivado.py
# writes the Verilog and runs this): synth_design -mode out_of_context, the
# clock at PERIOD ns, opt, place, route; then the setup WNS / TNS, and the
# endpoints with less than MARGIN ns of slack grouped by register (the
# bit indices removed), worst first.
#
#   vivado -mode batch -source ldpc_ooc.tcl -tclargs VERILOG TOP PERIOD MARGIN OUTDIR
set v [lindex $argv 0]
set top [lindex $argv 1]
set period [lindex $argv 2]
set margin [lindex $argv 3]
set out [lindex $argv 4]
# optional: a second clock port and its period (the bridge's chain side),
# asynchronous to the first (the FIFOs' crossings are not timed)
set clk2 [expr {[llength $argv] > 5 ? [lindex $argv 5] : ""}]
set period2 [expr {[llength $argv] > 6 ? [lindex $argv 6] : 25.0}]
file mkdir $out
read_verilog $v
synth_design -top $top -part xc7z020clg484-2 -mode out_of_context
if {$clk2 ne ""} {
    create_clock -name clk -period $period [get_ports ldpc_clk]
    create_clock -name clk2 -period $period2 [get_ports $clk2]
    set_clock_groups -asynchronous -group clk -group clk2
} else {
    create_clock -name clk -period $period [get_ports clk]
}
opt_design
place_design
phys_opt_design
route_design
write_checkpoint -force $out/routed.dcp
report_timing_summary -max_paths 20 -nworst 1 -file $out/timing.rpt
report_utilization -file $out/util.rpt
set wns [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -setup]]
puts "OOC $top at $period ns: WNS $wns"
set paths [get_timing_paths -setup -max_paths 100000 -nworst 1 -slack_lesser_than $margin]
array set worst {}
array set count {}
array set from {}
foreach p $paths {
    set ep [get_property NAME [get_property ENDPOINT_PIN $p]]
    set key [regsub -all {\[\d+\]} [regsub -all {_\d+_reg} $ep {_#_reg}] {[*]}]
    set s [get_property SLACK $p]
    if {![info exists worst($key)] || $s < $worst($key)} {
        set worst($key) $s
        set from($key) [regsub -all {\[\d+\]} [get_property NAME [get_property STARTPOINT_PIN $p]] {[*]}]
    }
    incr count($key)
}
set rows {}
foreach k [array names worst] { lappend rows [list $worst($k) $count($k) $k $from($k)] }
foreach r [lsort -real -index 0 $rows] {
    puts [format "NEAR %8.3f ns %5d  %s  <- %s" {*}$r]
}
puts "OOC $top: [llength $paths] endpoints under $margin ns"
