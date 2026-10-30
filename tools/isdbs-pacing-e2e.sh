#!/usr/bin/env bash
# Offline end-to-end check of PCR-paced ISDB-S placement
# (docs/plans/isdbs-next/pcr-pacing.md §7.6, §7.8, §8), no radio: the
# media-free bench service of isdbt-mux (as tools/isdbs-bench-tx.sh builds
# it, TS_ID 0x4010) multiplexed to a CBR TS of SECONDS → `isdb tx --system
# isdbs --iq` (cs8, BS layout) with --placed-ts → `isdb rx --iq` →
# tools/isdbs-bench-check.sh on the received TS against the placed one
# (inspect, validate --expect-rate 52170000:PPM, byte-exact comparison).
#
# Usage: tools/isdbs-pacing-e2e.sh WORKDIR [SECONDS] [PPM]
#   WORKDIR  scratch directory (the IQ file is 80 MB per second of air)
#   SECONDS  length of the multiplexed TS (default 6)
#   PPM      --expect-rate tolerance (default 1: an offline file is exact)
# Environment: ISDB (default target/release/isdb of this checkout),
#   TS_PACING (default auto; `arrival` shows the old rule for comparison),
#   FEED (default file; `loop` plays the TS PASSES times, default 3, with
#   --feed loop and checks the seams with tools/ts-loop-seam.py),
#   MEDIA (1: give the service video and audio made by ffmpeg, so that
#   the TS carries PES with PTS/DTS; default 0, declared streams only).
# Exit status: 0 PASS, 1 FAIL (isdbs-bench-check.sh, or a seam check),
#   2 error.
set -euo pipefail
die() { echo "isdbs-pacing-e2e: $*" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '10,21s/^# \{0,1\}//p' "$0" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || die "$ISDB: build it with cargo build --release -p isdb-cli"
SECONDS_TS=${2:-6}
PPM=${3:-1}
PACING=${TS_PACING:-auto}
FEED=${FEED:-file}
PASSES=${PASSES:-3}
MEDIA=${MEDIA:-0}
[[ $SECONDS_TS =~ ^[1-9][0-9]*$ ]] || die "SECONDS $SECONDS_TS: expected whole seconds"
case $FEED in file|loop) ;; *) die "FEED=$FEED: file or loop" ;; esac
[[ $PASSES =~ ^[1-9][0-9]*$ ]] || die "PASSES=$PASSES: expected a count"
mkdir -p "$1"
cd "$1"

if [ "$MEDIA" = 1 ]; then
    command -v ffmpeg > /dev/null || die "MEDIA=1 needs ffmpeg"
    # docs/test-data.md's recipe, SECONDS long
    ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1440x1080:rate=30000/1001 \
        -t "$SECONDS_TS" -vf setfield=tff -c:v mpeg2video -b:v 12M -maxrate 12M -bufsize 7M \
        -g 15 -bf 2 -flags +ilme+ildct -aspect 16:9 -f mpeg2video video.m2v
    ffmpeg -loglevel error -y -f lavfi -i sine=frequency=1000:sample_rate=48000 -ac 2 \
        -t "$SECONDS_TS" -c:a aac -b:a 192k -f adts audio.m2a
fi
python3 -I - "$HERE/crates/isdbt-mux/examples/bench-minimal.toml" bs-bench.toml "$MEDIA" "$PWD" <<'PY'
import sys
src, dst, media, here = sys.argv[1:5]
out = []
for l in open(src).read().splitlines():
    t = l.strip()
    if media == "1":
        # ffmpeg's ADTS has no crc_check: accepted with a warning
        l = l.replace('source = "media/audio.m2a"', f'source = "{here}/audio.m2a"\n  adts_compliance = "warn"')
        l = l.replace('source = "media/video.m2v"', f'source = "{here}/video.m2v"')
    else:
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
LOOP=()
if [ "$FEED" = loop ]; then
    # air time for PASSES passes and a bit of the next (lead-in included)
    LOOP=(--feed loop --seconds "$((SECONDS_TS * PASSES + 1))")
fi
"$ISDB" tx --system isdbs --ts "0x4010=$PWD/bench.ts" --ts-pacing "$PACING" "${LOOP[@]}" \
    --iq tx.cs8 --iq-format cs8 --placed-ts 'placed-{ts_id}.ts' --status-secs 0
"$ISDB" rx --iq tx.cs8 --out 'rx-{ts_id}.ts' --json > rx.json
echo "== placed TS"
"$ISDB" ts inspect placed-4010.ts | grep -E "rate|CBR-fit|PCR" || true
echo "== received TS against the placed one"
VERDICT=0
"$HERE/tools/isdbs-bench-check.sh" --rate "52170000:$PPM" --fit-max-ns 100 \
    --placed placed-4010.ts rx-4010.ts || VERDICT=$?
[ "$VERDICT" -le 1 ] || exit "$VERDICT"
if [ "$FEED" = loop ]; then
    echo "== loop seams of the received TS (tools/ts-loop-seam.py)"
    python3 -I "$HERE/tools/ts-loop-seam.py" bench.ts rx-4010.ts || VERDICT=1
fi
if [ "$VERDICT" = 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$VERDICT"
