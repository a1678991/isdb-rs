#!/bin/sh
# Sample a Pluto's interrupt counts, softirqs and the CPU time of a few
# processes at a short interval, on the board, to find what runs
# periodically while a stream is active (the ≈ 10 ms stall every 10.19 s
# of the 50.8 MB/s symbol stream, plan §0.18). Read-only: it only reads
# /proc. One awk process does all the reading (forking per sample takes
# ≈ 1 s on the Pluto's CPU, too slow to see a 10 ms event).
#
#   ssh PLUTO 'sh -s DUR "PIDS"' < tools/pluto-sample.sh > samples.txt
#
# DUR: seconds (default 25). PIDS: space-separated process ids to follow
# (default: iiod's). Output, one line per sample:
#   <uptime s> irq:<n>=<count> ... soft:<name>=<count> ... pid:<pid>=<ticks> ...
# Counts are cumulative (CPU0 + CPU1); diff consecutive lines.
DUR=${1:-25}
PIDS=${2:-$(pidof iiod)}
exec awk -v dur="$DUR" -v pids="$PIDS" '
function rd(f,   l) { l = ""; getline l < f; close(f); return l }
BEGIN {
    n = split(pids, P, " ")
    split(rd("/proc/uptime"), u, " "); t0 = u[1]
    while (1) {
        split(rd("/proc/uptime"), u, " ")
        if (u[1] - t0 > dur) break
        line = u[1]
        while ((getline l < "/proc/interrupts") > 0) {
            m = split(l, f, " ")
            if (f[1] ~ /^[0-9]+:$/) line = line " irq:" substr(f[1], 1, length(f[1]) - 1) "=" f[2] + f[3]
        }
        close("/proc/interrupts")
        while ((getline l < "/proc/softirqs") > 0) {
            m = split(l, f, " ")
            if (f[1] ~ /:$/) line = line " soft:" substr(f[1], 1, length(f[1]) - 1) "=" f[2] + f[3]
        }
        close("/proc/softirqs")
        for (i = 1; i <= n; i++) {
            s = rd("/proc/" P[i] "/stat")
            sub(/.*\) /, "", s); split(s, g, " ")
            line = line " pid:" P[i] "=" g[12] + g[13]
        }
        print line
        fflush()
    }
}'
