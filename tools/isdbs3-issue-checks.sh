#!/usr/bin/env bash
# Board runs for the receive-only ISDB-S3 image's issues #27, #28 and #29
# (the decoder's banks across stop/START; the training-sum reads against
# other direct_reg_access clients on the board). Receive only.
#
#   tools/isdbs3-issue-checks.sh restarts OUT_DIR N SECONDS   # N runs back to back, no reboot
#   tools/isdbs3-issue-checks.sh run OUT_DIR NAME SECONDS     # one run
#
# Each run: `isdb rx --system isdbs3 --radio pluto:uri=$URI --channel $CHANNEL
# --freq-unrestricted --offload auto --seconds S --json --slots-out
# OUT/NAME.slots --out 'OUT/NAME-{tlv_id}.tlv'` under /usr/bin/time -v, its
# stderr stamped with the wall clock in OUT/NAME.log, its exit status in
# OUT/NAME.exit; then examples/slotsstatus's totals (not converged,
# dropped with 0 iterations, failures per minute) in OUT/NAME.status and
# tools/isdbs3-run-summary.py's lines in OUT/NAME.summary. URI (required,
# e.g. ip:<PLUTO_IP>, or PLUTO_URI), CHANNEL (BS7), ISDB (target/release/isdb), and
# RUST_LOG pass through.
set -uo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
isdb=${ISDB:-$here/target/release/isdb}
status_bin=${SLOTSSTATUS:-$here/target/release/examples/slotsstatus}
uri=${URI:-${PLUTO_URI:?set URI or PLUTO_URI, e.g. ip:<PLUTO_IP>}}
chan=${CHANNEL:-BS7}

stamp() { perl -MTime::HiRes=time -ne '$|=1; printf "%.3f %s", time, $_'; }

one() {
    local out=$1 name=$2 secs=$3
    mkdir -p "$out"
    echo "== $name: $secs s ($(date '+%H:%M:%S'))" >&2
    /usr/bin/time -v "$isdb" rx --system isdbs3 --radio "pluto:uri=$uri" --channel "$chan" \
        --freq-unrestricted --offload auto --seconds "$secs" --json --status-secs 10 \
        --slots-out "$out/$name.slots" --out "$out/$name-{tlv_id}.tlv" \
        >"$out/$name.json" 2> >(stamp >"$out/$name.log")
    echo $? >"$out/$name.exit"
    sleep 1
    echo "exit $(cat "$out/$name.exit")" >&2
    if [ -s "$out/$name.slots" ]; then
        "$status_bin" "$out/$name.slots" 1 >"$out/$name.frames" 2>&1
        grep SUMMARY "$out/$name.frames" >"$out/$name.status"
        cat "$out/$name.status" >&2
    fi
    if [ -s "$out/$name.json" ]; then
        python3 -I "$here/tools/isdbs3-run-summary.py" "$out/$name.json" "$out/$name.log" \
            >"$out/$name.summary" 2>&1
        grep -E '^(open|slow loop|status):' "$out/$name.summary" | cut -c1-400 >&2
    fi
    grep -E 'Maximum resident|User time|System time|Elapsed' "$out/$name.log" | sed 's/^[0-9.]* //' >&2
}

case "${1:-}" in
restarts)
    out=${2:?restarts OUT_DIR N SECONDS}
    n=${3:?}
    secs=${4:?}
    for k in $(seq 1 "$n"); do one "$out" "run$k" "$secs"; done
    ;;
run)
    one "${2:?run OUT_DIR NAME SECONDS}" "${3:?}" "${4:?}"
    ;;
*)
    sed -n '2,20p' "$0"
    exit 2
    ;;
esac
