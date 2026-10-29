#!/usr/bin/env bash
# ISDB-S test broadcast into a conducted receiver (docs/plans/isdbs-fpga-tx.md
# §4 rung 4; VERIFICATION.md "ISDB-S transmitter in the FPGA: into the
# PIX-DT295"): a media-free bench service of isdbt-mux (its declared
# streams, TS_ID 0x4010) multiplexed to a CBR TS, paced at its own rate
# into a FIFO, and transmitted as the one TS of a BS layout (TC8PSK 2/3 ×
# 48) through the PlutoSky R2's FPGA transmit block on the BS1 IF.
#
# Usage: tools/isdbs-bench-tx.sh WORKDIR [ATT_DB] [PLUTO_URI]
#   WORKDIR   scratch directory for the TOML, the TS and the FIFO
#   ATT_DB    TX attenuation (default 45: about -38 dBm at the port, -68 dBm
#             after a 30 dB pad, inside a tuner's -75..-20 dBm)
#   PLUTO_URI libiio URI (default: ISDB_PLUTO_URI or mDNS)
#
# Conducted bench only: a fixed attenuator (30-60 dB) and a DC block (or the
# receiver's LNB supply off) between the TX port and the receiver, as
# docs/plans/isdb-s.md §2 requires. Stop with Ctrl-C (the FIFO's writer is
# killed too). The TS is re-timed by arrival order on the ISDB-S side
# (12 ms rms PCR jitter, PAT/PMT intervals up to 0.17 s): enough to lock
# and read the SI; PCR-paced placement is the follow-up.
set -euo pipefail
WORK=${1:?usage: tools/isdbs-bench-tx.sh WORKDIR [ATT_DB] [PLUTO_URI]}
ATT=${2:-45}
URI=${3:-${ISDB_PLUTO_URI:-}}
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
mkdir -p "$WORK"
cd "$WORK"

# The bench service without media (as isdb-cli's tests declare it), TS_ID
# 0x4010: the TS_ID the transmitter announces in the TMCC.
python3 -I - "$HERE/crates/isdbt-mux/examples/bench-minimal.toml" bs-bench.toml <<'PY'
import sys
src, dst = sys.argv[1:3]
out = []
for l in open(src).read().splitlines():
    t = l.strip()
    if t.startswith("source =") or t.startswith("frames_per_pes"):
        continue
    l = l.replace('kind = "mpeg2-video"', 'kind = "declared"\n  stream_type = 0x02')
    l = l.replace('kind = "aac-adts"', 'kind = "declared"\n  stream_type = 0x0F')
    out.append(l)
s = "\n".join(out) + "\n"
s = s.replace("transport_stream_id = 0x79EE", "transport_stream_id = 0x4010", 1)
open(dst, "w").write(s)
PY
if [ ! -s bench60.ts ]; then
    "$ISDB" mux -c bs-bench.toml --seconds 60 --out-ts bench60.ts
fi
# 260 OFDM frames of 231.336 ms (mode 3, GI 1/8): the TS's own byte rate.
RATE=$(python3 -c "import os; print(int(os.path.getsize('bench60.ts') / (260 * 0.231336)))")
rm -f ts.fifo; mkfifo ts.fifo
( while :; do cat bench60.ts; done | pv -q -B 7520 -L "$RATE" > ts.fifo ) &
PACER=$!
trap 'kill $PACER 2>/dev/null || true' EXIT
RADIO="pluto${URI:+:uri=$URI}"
exec "$ISDB" tx --system isdbs --radio "$RADIO" --channel BS1 --freq-unrestricted \
    --offload fpga --rf-enable --gain "att=$ATT" --ts "0x4010=$WORK/ts.fifo" --feed live \
    --status-secs 5
