# Failing setup endpoints of a routed checkpoint, grouped by (launch clock,
# capture clock, the destination's block): how many, the worst slack, and
# the worst path's source and destination. For a timing report by block
# (fpga/README.md, the receive-only image).
#
#   fpga/tezuka/vivado-batch.sh failing.tcl CHECKPOINT.dcp [DEPTH] [MAX_PATHS]
#
# DEPTH: hierarchy levels of the destination kept below
# i_system_wrapper/system_i (default 5: isdb_slots/inst/rx/chain/back).
set dcp [lindex $argv 0]
set depth [expr {[llength $argv] > 1 ? [lindex $argv 1] : 5}]
set maxp [expr {[llength $argv] > 2 ? [lindex $argv 2] : 50000}]
open_checkpoint $dcp
set paths [get_timing_paths -setup -max_paths $maxp -nworst 1 -slack_lesser_than 0]
array set n {}
array set worst {}
array set ex {}
foreach p $paths {
    set sc [get_property STARTPOINT_CLOCK $p]
    set ec [get_property ENDPOINT_CLOCK $p]
    set ep [get_property ENDPOINT_PIN $p]
    set h [split [regsub {^i_system_wrapper/system_i/} $ep {}] /]
    set blk [join [lrange $h 0 [expr {$depth - 1}]] /]
    set key "$sc -> $ec : $blk"
    set s [get_property SLACK $p]
    if {![info exists n($key)]} {
        set n($key) 0
        set worst($key) $s
        set ex($key) "[get_property STARTPOINT_PIN $p] -> $ep"
    }
    incr n($key)
    if {$s < $worst($key)} {
        set worst($key) $s
        set ex($key) "[get_property STARTPOINT_PIN $p] -> $ep"
    }
}
puts "FAILING $dcp: [llength $paths] endpoints"
foreach key [lsort [array names n]] {
    puts [format "FAIL %-90s %6d endpoints, worst %7.3f ns  (%s)" $key $n($key) $worst($key) $ex($key)]
}
