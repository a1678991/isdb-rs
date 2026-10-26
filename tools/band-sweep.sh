#!/bin/sh
# Receive-only spectrum sweep of a band with a Pluto: one short capture per
# step at a fixed manual gain (so levels compare across the band), each
# analysed by the `sweepspec` example (Welch spectrum, noise floor, spans
# above it) and deleted at once. Writes one JSON line per step to OUT.
#
# Usage: tools/band-sweep.sh START_HZ STOP_HZ OUT.jsonl
#   PLUTO=ip:192.0.2.30  RATE=61.44M  BW=56M  STEP=48000000  GAIN=30
#   SAMPLES=1Mi  KEEP=24e6 (centre ±Hz used, inside the analog filter)
#   DIR=data/sweep (where each capture lives until it is analysed)
# The captures go to DIR on disk (never tmpfs), one at a time; each run is
# capped at 4 GB of memory.
set -eu
START=${1:?usage: tools/band-sweep.sh START_HZ STOP_HZ OUT.jsonl}
STOP=${2:?usage: tools/band-sweep.sh START_HZ STOP_HZ OUT.jsonl}
OUT=${3:?usage: tools/band-sweep.sh START_HZ STOP_HZ OUT.jsonl}
PLUTO=${PLUTO:-ip:192.0.2.30}
RATE=${RATE:-61.44M}
BW=${BW:-56M}
STEP=${STEP:-48000000}
GAIN=${GAIN:-30}
SAMPLES=${SAMPLES:-1Mi}
KEEP=${KEEP:-24e6}
DIR=${DIR:-data/sweep}
ISDB=${ISDB:-target/release/isdb}
SPEC=${SPEC:-target/release/examples/sweepspec}
cargo build -q --release -p isdb-cli
cargo build -q --release -p isdb-iq --example sweepspec
mkdir -p "$DIR"
: > "$OUT"
cap() { systemd-run --user --scope -q -p MemoryMax=4G -p MemorySwapMax=0 "$@"; }
# first centre half a step above START, so the kept ±STEP/2 spans tile the band
f=$((START + STEP / 2))
while [ "$f" -le $((STOP - STEP / 2 + STEP - 1)) ]; do
    r="$DIR/step-$f"
    if cap "$ISDB" capture --radio "pluto:uri=$PLUTO" --freq "$f" --rate "$RATE" \
            --bw "$BW" --gain "gain=$GAIN" --samples "$SAMPLES" \
            --description "band sweep" -o "$r.sigmf-data" --overwrite >/dev/null 2>"$r.log"; then
        cap "$SPEC" "$r.sigmf-meta" --keep "$KEEP" >> "$OUT"
        printf '%s MHz done\n' "$((f / 1000000))" >&2
    else
        printf '%s MHz: capture failed (%s)\n' "$((f / 1000000))" "$(tail -1 "$r.log")" >&2
    fi
    rm -f "$r.sigmf-data" "$r.sigmf-meta" "$r.log"
    f=$((f + STEP))
done
rmdir "$DIR" 2>/dev/null || true
