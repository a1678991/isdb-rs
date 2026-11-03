#!/usr/bin/env bash
# Board checks of the receive-only ISDB-S3 image (isdbs3_rx_top.bin:
# SlotsRx, docs/plans/isdbs3-fpga-rx.md "Verification ladder" step 4).
# Receive only: nothing here transmits.
#
#   tools/isdbs3-board-check.sh OUT_DIR [URI] [CHANNEL] [SECONDS]
#
#   URI      defaults to ISDB_PLUTO_URI, then PLUTO_URI (required if omitted)
#   CHANNEL  default BS7; SECONDS default 30
#
# Runs a release `isdb` (target/release/isdb, or $ISDB):
#   0  isdb symbols --probe                       (the ID word: front end and
#                                                  the chain's Slots version)
#   1  isdb symbols … --out OUT/before.sym         (the front end's Symbols,
#                                                  just before)
#   2  isdb rx --offload auto --slots-out …        (the FPGA chain: Slots, the
#                                                  host assembling TLV only),
#      under /usr/bin/time -v, TLV to OUT/fpga/fpga-{tlv_id}.tlv
#   2b isdb rx --slots OUT/fpga.slots             (the same words replayed:
#      the host's CPU and memory without the radio)
#   3  isdb symbols … --out OUT/after.sym          (just after)
#   4  isdb rx --symbols OUT/before.sym            (the host receiver on 1),
#      TLV to OUT/host/host-{tlv_id}.tlv
#   5  tools/tlv-packet-diff.py OUT/host OUT/fpga  (different times of the
#      same transponder: what matches is the repeated SI and whatever else
#      repeats)
#   6  isdb tlv inspect on each FPGA stream
# Then, separately (slow: ~5 s per second of signal):
#   tools/slots-vs-symbols.sh OUT/before.sym OUT/model
# Each step's stdout (JSON) and stderr are kept in OUT_DIR.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?usage: $0 OUT_DIR [URI] [CHANNEL] [SECONDS]}
uri=${2:-${ISDB_PLUTO_URI:-${PLUTO_URI:-}}}
: "${uri:?give URI or set ISDB_PLUTO_URI (e.g. ip:<DEVICE_IP>)}"
chan=${3:-BS7}
secs=${4:-30}
isdb=${ISDB:-$here/target/release/isdb}
radio="pluto:uri=$uri"
mkdir -p "$out"/{fpga,host}
rm -f "$out"/fpga/*.tlv "$out"/host/*.tlv

step() { echo "== $*" >&2; }
# wall-clock seconds before each line of the radio run's log (the slow
# loop's first tap write against the lock)
stamp() { perl -MTime::HiRes=time -ne '$|=1; printf "%.3f %s", time, $_'; }

step "0: probe"
"$isdb" symbols --radio "$radio" --freq "$chan" --probe >"$out/probe.txt" 2>&1 || true
cat "$out/probe.txt" >&2

step "1: Symbols capture before ($secs s)"
"$isdb" symbols --radio "$radio" --freq "$chan" --system isdb-s3 --offload auto \
    --seconds "$secs" --out "$out/before.sym" >"$out/before.json" 2>"$out/before.log" || {
    tail "$out/before.log" >&2
    exit 1
}

step "2: the FPGA chain (--offload auto, $secs s)"
/usr/bin/time -v "$isdb" rx --system isdbs3 --radio "$radio" --channel "$chan" --freq-unrestricted --offload auto \
    --seconds "$secs" --json --status-secs 5 --slots-out "$out/fpga.slots" \
    --out "$out/fpga/fpga-{tlv_id}.tlv" >"$out/fpga.json" 2> >(stamp >"$out/fpga.log") || {
    sleep 1
    tail -30 "$out/fpga.log" >&2
    exit 1
}
sleep 1  # the stamping process flushes
grep -i -E 'offload|decoded by|slots|chain' "$out/fpga.log" | head -20 >&2 || true

step "2b: the recorded Slots words again (--slots), the host's cost without the radio"
/usr/bin/time -v "$isdb" rx --slots "$out/fpga.slots" --json --status-secs 0 \
    >"$out/replay.json" 2>"$out/replay.log" || true
grep -E 'done:|Maximum resident|User time|Elapsed' "$out/replay.log" >&2 || true

step "3: Symbols capture after ($secs s)"
"$isdb" symbols --radio "$radio" --freq "$chan" --system isdb-s3 --offload auto \
    --seconds "$secs" --out "$out/after.sym" >"$out/after.json" 2>"$out/after.log" || {
    tail "$out/after.log" >&2
    exit 1
}

step "4: host receiver on the 'before' capture"
/usr/bin/time -v "$isdb" rx --symbols "$out/before.sym" --system isdbs3 --json --status-secs 0 \
    --out "$out/host/host-{tlv_id}.tlv" >"$out/host.json" 2>"$out/host.log" || {
    tail "$out/host.log" >&2
    exit 1
}

step "5: TLV packets, host (before) against FPGA"
python3 -I "$here/tools/tlv-packet-diff.py" "$out/host" "$out/fpga" --json "$out/diff.json" \
    | tee "$out/diff.txt" >&2

step "6: isdb tlv inspect on the FPGA streams"
for f in "$out"/fpga/*.tlv; do
    "$isdb" tlv inspect "$f" >"${f%.tlv}.inspect.txt" 2>&1 || true
    echo "-- $f" >&2
    head -15 "${f%.tlv}.inspect.txt" >&2
done
