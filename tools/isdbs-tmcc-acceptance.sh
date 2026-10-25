#!/bin/sh
# Phase 2 acceptance: decode the TMCC of every ISDB-S transponder recording
# in a capture directory with `isdb iq tmcc` and print one line each.
# Usage: tools/isdbs-tmcc-acceptance.sh DIR
set -eu
DIR=${1:?usage: DIR, the directory of the recordings}
ISDB=${ISDB:-target/release/isdb}
cargo build -q --release -p isdb-cli
for c in bs1-40-bw56-iio-cs8 bs3 bs5 bs9 bs13 bs15 bs19 bs21 bs23 \
         nd2-61 nd4-61 nd6-61 nd8-61 nd10-61 nd12-61 nd14-61 nd16-61 nd18-61 nd20-61 nd22-61 nd24-61; do
    m="$DIR/$c.sigmf-meta"
    [ -f "$m" ] || { echo "$c: missing"; continue; }
    printf '%s ' "$c"
    "$ISDB" iq tmcc "$m" --summary
done
