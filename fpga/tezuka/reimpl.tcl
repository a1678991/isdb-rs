# Place and route an optimised checkpoint again with other directives, for
# a timing experiment without a whole build (the project flow's impl_1 runs
# opt, place, phys_opt, route, upstream's hold_fix.tcl re-route and a
# post-route phys_opt, all Explore; build.sh keeps system_top_opt.dcp only
# while the project lives, so copy it out first).
#
#   fpga/tezuka/vivado-batch.sh reimpl.tcl OPT.dcp OUT.dcp PLACE PHYS ROUTE POSTPHYS [XDC...]
#
# PLACE, PHYS, ROUTE, POSTPHYS: the -directive of place_design,
# phys_opt_design (after placement), route_design and the post-route
# phys_opt_design; `-` skips a phys_opt step. Each XDC is read before
# placement (e.g. a changed fpga/tezuka/isdb_slots.xdc: its rules are
# applied on top of the checkpoint's). Prints the WNS / WHS per clock as
# timing.tcl does.
set in [lindex $argv 0]
set out [lindex $argv 1]
lassign [lrange $argv 2 5] pdir phdir rdir pphdir
set xdcs [lrange $argv 6 end]
open_checkpoint $in
foreach x $xdcs { read_xdc $x }
place_design -directive $pdir
if {$phdir ne "-"} { phys_opt_design -directive $phdir }
route_design -directive $rdir
# upstream's hold_fix.tcl: the Vivado 2025+ router leaves hold violations
# that a second route_design fixes
route_design
if {$pphdir ne "-"} { phys_opt_design -directive $pphdir }
write_checkpoint -force $out
report_timing_summary -delay_type min_max -max_paths 10 -nworst 1 -file "[file rootname $out]_timing.rpt"
set wns [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -setup]]
set whs [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -hold]]
puts "TIMING $out: WNS $wns WHS $whs"
foreach clk [get_clocks] {
    set s [get_timing_paths -quiet -max_paths 1 -nworst 1 -setup -to $clk]
    set h [get_timing_paths -quiet -max_paths 1 -nworst 1 -hold -to $clk]
    if {[llength $s] == 0 && [llength $h] == 0} { continue }
    set ws [expr {[llength $s] ? [get_property SLACK $s] : "-"}]
    set wh [expr {[llength $h] ? [get_property SLACK $h] : "-"}]
    puts "CLOCK $clk: WNS $ws WHS $wh"
}
