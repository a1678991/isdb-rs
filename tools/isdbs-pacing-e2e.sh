#!/usr/bin/env bash
# Offline end-to-end check of PCR-paced ISDB-S placement
# (docs/plans/isdbs-next/pcr-pacing.md §7.8, §8), no radio: the media-free
# bench service of isdbt-mux (as tools/isdbs-bench-tx.sh builds it, TS_ID
# 0x4010) multiplexed to a CBR TS of SECONDS → `isdb tx --system isdbs
# --iq` (cs8, BS layout, --feed file) with --placed-ts → `isdb rx --iq` →
# tools/isdbs-bench-check.sh on the received TS against the placed one
# (inspect, validate --expect-rate 52170000:PPM, byte-exact comparison).
#
# Usage: tools/isdbs-pacing-e2e.sh WORKDIR [SECONDS] [PPM]
#   WORKDIR  scratch directory (the IQ file is 80 MB per second of air)
#   SECONDS  length of the multiplexed TS (default 6)
#   PPM      --expect-rate tolerance (default 1: an offline file is exact)
# Environment: ISDB (default target/release/isdb of this checkout),
#   TS_PACING (default auto; `arrival` shows the old rule for comparison).
# Exit status: that of isdbs-bench-check.sh (0 PASS, 1 FAIL, 2 error).
set -euo pipefail
die() { echo "isdbs-pacing-e2e: $*" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '10,16s/^# \{0,1\}//p' "$0" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || die "$ISDB: build it with cargo build --release -p isdb-cli"
SECONDS_TS=${2:-6}
PPM=${3:-1}
PACING=${TS_PACING:-auto}
mkdir -p "$1"
cd "$1"

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
"$ISDB" mux -c bs-bench.toml --seconds "$SECONDS_TS" --out-ts bench.ts
"$ISDB" tx --system isdbs --ts "0x4010=$PWD/bench.ts" --ts-pacing "$PACING" \
    --iq tx.cs8 --iq-format cs8 --placed-ts 'placed-{ts_id}.ts' --status-secs 0
"$ISDB" rx --iq tx.cs8 --out 'rx-{ts_id}.ts' --json > rx.json
echo "== placed TS"
"$ISDB" ts inspect placed-4010.ts | grep -E "rate|CBR-fit|PCR" || true
echo "== received TS against the placed one"
"$HERE/tools/isdbs-bench-check.sh" --rate "52170000:$PPM" --fit-max-ns 100 \
    --placed placed-4010.ts rx-4010.ts
