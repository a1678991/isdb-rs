#!/bin/sh
# Re-capture the satellite IF recordings the ISDB-S acceptance scripts read,
# with the settings of the 2026-10-20 set, so a change to the feed (cable,
# DC block, splitter, LNB) can be compared recording for recording:
# BS<n> as `bs<n>`, ND<n> as `nd<n>-61`, maia-sdr recorder, 61.44 Msps,
# 0.8 × rate analog bandwidth, slow-attack AGC. Receive only.
# Usage: tools/isdbs-recapture.sh OUTDIR [DESCRIPTION]
#   PLUTO=ip:192.0.2.30 selects the radio; CHANNELS="ND20 ND22" restricts
#   the list (channel names; default every BS ISDB-S and every ND channel).
# Then: tools/isdbs-tmcc-acceptance.sh OUTDIR / tools/isdbs-ts-acceptance.sh OUTDIR
set -eu
OUT=${1:?usage: tools/isdbs-recapture.sh OUTDIR [DESCRIPTION]}
DESC=${2:-satellite IF re-capture}
PLUTO=${PLUTO:-ip:192.0.2.30}
ISDB=${ISDB:-target/release/isdb}
cargo build -q --release -p isdb-cli
mkdir -p "$OUT"
for ch in ${CHANNELS:-BS1 BS3 BS5 BS9 BS13 BS15 BS19 BS21 BS23 \
          ND2 ND4 ND6 ND8 ND10 ND12 ND14 ND16 ND18 ND20 ND22 ND24}; do
    n=$(echo "$ch" | tr 'A-Z' 'a-z')
    case $n in nd*) n=$n-61 ;; esac
    "$ISDB" capture --radio "pluto:uri=$PLUTO,recorder=maia" --freq "$ch" \
        --rate 61.44M --gain agc=slow_attack --description "$DESC" \
        -o "$OUT/$n.sigmf-data" 2>&1 | grep -E 'gain|wrote|Error' | sed "s/^/$ch: /" || true
done
