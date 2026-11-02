# Repair the hold violations a routed checkpoint has left (the Vivado
# 2025+ router can leave a few, upstream's hold_fix.tcl reroutes once),
# and, when the result meets every setup and hold constraint, write the
# bitstream and an XSA with it (what build.sh copies from the project
# flow), for fpga/tezuka/sdbin.sh.
#
#   fpga/tezuka/vivado-batch.sh holdfix.tcl FINAL.dcp OUTDIR
#
# Steps, while hold still fails (up to four rounds): phys_opt_design
# -directive ExploreWithHoldFix; the failing endpoints' nets unrouted and
# routed again (timing-driven, so the router adds the detour hold needs);
# then a post-route phys_opt for setup if setup fails. Writes OUTDIR/system_top_holdfix.dcp,
# OUTDIR/holdfix_timing.rpt and, if met, OUTDIR/system_top.bit and
# OUTDIR/plutoskyr2.xsa.
set dcp [lindex $argv 0]
set outdir [lindex $argv 1]
open_checkpoint $dcp
proc slacks {} {
    set s [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -setup]]
    set h [get_property SLACK [get_timing_paths -max_paths 1 -nworst 1 -hold]]
    return [list $s $h]
}
lassign [slacks] wns whs
puts "HOLDFIX start: WNS $wns WHS $whs"
# up to four rounds: the hold-fixing phys_opt, then the remaining failing
# endpoints' nets rerouted (a reroute can move a violation to a
# neighbouring net, hence the rounds)
for {set round 1} {$round <= 4 && $whs < 0} {incr round} {
    phys_opt_design -directive ExploreWithHoldFix
    lassign [slacks] wns whs
    puts "HOLDFIX round $round after phys_opt ExploreWithHoldFix: WNS $wns WHS $whs"
    if {$whs >= 0} { break }
    set nets {}
    foreach p [get_timing_paths -hold -max_paths 100 -nworst 1 -slack_lesser_than 0] {
        foreach n [get_nets -quiet -of_objects [get_property ENDPOINT_PIN $p]] { lappend nets $n }
    }
    set nets [lsort -unique $nets]
    puts "HOLDFIX round $round rerouting [llength $nets] nets: $nets"
    route_design -unroute -nets $nets
    route_design -nets $nets
    route_design
    lassign [slacks] wns whs
    puts "HOLDFIX round $round after reroute: WNS $wns WHS $whs"
}
if {$wns < 0} {
    phys_opt_design -directive Explore
    lassign [slacks] wns whs
    puts "HOLDFIX after setup phys_opt: WNS $wns WHS $whs"
}
write_checkpoint -force "$outdir/system_top_holdfix.dcp"
report_timing_summary -delay_type min_max -max_paths 10 -nworst 1 -report_unconstrained -file "$outdir/holdfix_timing.rpt"
if {$wns >= 0 && $whs >= 0} {
    write_bitstream -force "$outdir/system_top.bit"
    write_hw_platform -fixed -force -include_bit -file "$outdir/plutoskyr2.xsa"
    puts "HOLDFIX met: WNS $wns WHS $whs; $outdir/system_top.bit, $outdir/plutoskyr2.xsa"
} else {
    puts "HOLDFIX not met: WNS $wns WHS $whs; no bitstream"
}
