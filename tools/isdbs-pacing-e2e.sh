#!/usr/bin/env bash
# Offline end-to-end check of PCR-paced ISDB-S placement
# (docs/plans/isdbs-next/pcr-pacing.md §7.6, §7.8, §8), no radio: the
# media-free BS bench service of isdbt-mux (as tools/isdbs-bench-tx.sh
# sends it: crates/isdbt-mux/examples/bs-bench.toml, BS-shaped SI with
# network_id 0x0004 and the 0x43 of BS1, TS_ID 0x4017;
# docs/plans/isdbs-next/bs-si.md) multiplexed to a CBR TS of SECONDS →
# `isdb tx --system isdbs --channel BS1 --iq` (cs8, BS layout) with
# --placed-ts → `isdb rx --iq` → tools/isdbs-bench-check.sh on the
# received TS against the placed one (inspect, validate --expect-rate
# 52170000:PPM with the BS SI expectations, byte-exact comparison).
#
# Usage: tools/isdbs-pacing-e2e.sh WORKDIR [SECONDS] [PPM]
#   WORKDIR  scratch directory (the IQ file is 80 MB per second of air)
#   SECONDS  length of the multiplexed TS (default 6)
#   PPM      --expect-rate tolerance (default 1: an offline file is exact)
# Environment: ISDB (default target/release/isdb of this checkout),
#   TS_PACING (default auto; `arrival` shows the old rule for comparison),
#   FEED (default file; `loop` plays the TS PASSES times, default 3, with
#   --feed loop and checks the seams with tools/ts-loop-seam.py; `live`
#   writes the TS into a FIFO at its own rate with `pv -L`, as
#   isdbs-bench-tx.sh's fifo mode does, and `isdb tx --feed live` paces it
#   by its PCR against the wall clock, the producer paced to real time
#   as for a radio (the default with --feed live and --iq); its live
#   status — input lag, deadline misses — is printed; ends when pv
#   closes the FIFO),
#   STALL (FEED=live: stop the writer 4 s in for STALL seconds, e.g. 1:
#   the fills go out at their deadlines, the late packets follow with
#   lag, restamped; the run then expects deadline misses; default 0),
#   MEDIA (1: give the service video and audio made by ffmpeg, so that
#   the TS carries PES with PTS/DTS; default 0, declared streams only).
#   The bench's own TOML is copied to WORKDIR/bs-bench.toml (MEDIA=1: its
#   two declared streams made media streams of these files).
# Exit status: 0 PASS, 1 FAIL (isdbs-bench-check.sh, a seam check, or
#   FEED=live with a deadline miss, STALL without one), 2 error.
set -euo pipefail
die() { echo "isdbs-pacing-e2e: $*" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '/^# Usage:/,/^# Exit status:/s/^# \{0,1\}//p' "$0" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || die "$ISDB: build it with cargo build --release -p isdb-cli"
SECONDS_TS=${2:-6}
PPM=${3:-1}
PACING=${TS_PACING:-auto}
FEED=${FEED:-file}
PASSES=${PASSES:-3}
MEDIA=${MEDIA:-0}
STALL=${STALL:-0}
[[ $SECONDS_TS =~ ^[1-9][0-9]*$ ]] || die "SECONDS $SECONDS_TS: expected whole seconds"
case $FEED in file|loop|live) ;; *) die "FEED=$FEED: file, loop or live" ;; esac
[[ $PASSES =~ ^[1-9][0-9]*$ ]] || die "PASSES=$PASSES: expected a count"
[[ $STALL =~ ^[0-9]+(\.[0-9]+)?$ ]] || die "STALL=$STALL: expected seconds"
if [ "$FEED" = live ]; then command -v pv > /dev/null || die "FEED=live needs pv"; fi
mkdir -p "$1"
cd "$1"
# FEED=live: the FIFO's writer and the STALL timer, killed by their pids
# (kept in writer.pid, stall.pid) on every exit path; the writer is
# continued first, a stopped process does not act on TERM
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
    local f
    for f in stall.pid writer.pid; do
        if [ -s "$f" ]; then
            kill -CONT "$(cat "$f")" 2> /dev/null || true
            kill "$(cat "$f")" 2> /dev/null || true
            rm -f "$f"
        fi
    done
    rm -f ts.fifo
}
trap cleanup EXIT

if [ "$MEDIA" = 1 ]; then
    command -v ffmpeg > /dev/null || die "MEDIA=1 needs ffmpeg"
    # docs/test-data.md's recipe, SECONDS long
    ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1440x1080:rate=30000/1001 \
        -t "$SECONDS_TS" -vf setfield=tff -c:v mpeg2video -b:v 12M -maxrate 12M -bufsize 7M \
        -g 15 -bf 2 -flags +ilme+ildct -aspect 16:9 -f mpeg2video video.m2v
    ffmpeg -loglevel error -y -f lavfi -i sine=frequency=1000:sample_rate=48000 -ac 2 \
        -t "$SECONDS_TS" -c:a aac -b:a 192k -f adts audio.m2a
fi
# The TS_ID of bs-bench.toml (= the TMCC's) and the channel of its 0x43.
TSID=4017
python3 -I - "$HERE/crates/isdbt-mux/examples/bs-bench.toml" bs-bench.toml "$MEDIA" "$PWD" <<'PY'
import re, sys
src, dst, media, here = sys.argv[1:5]
s = open(src).read()
assert re.search(r'^transport_stream_id = 0x4017\b', s, re.M), "TS_ID of bs-bench.toml"
assert re.search(r'^\s*channel = "BS1"', s, re.M), "channel of bs-bench.toml"
if media == "1":
    for pid, st, kind, extra in (
        ("0x0111", "0x02", "mpeg2-video", f'source = "{here}/video.m2v"'),
        # ffmpeg's ADTS has no crc_check: accepted with a warning
        ("0x0112", "0x0F", "aac-adts", f'source = "{here}/audio.m2a"\n  adts_compliance = "warn"'),
    ):
        s, n = re.subn(
            rf'kind = "declared"\n(\s*pid = {pid}\n)\s*stream_type = {st}\b[^\n]*\n',
            rf'kind = "{kind}"\n\g<1>  {extra}\n', s)
        assert n == 1, f"declared stream {pid} of bs-bench.toml"
open(dst, "w").write(s)
PY
"$ISDB" mux -c bs-bench.toml --seconds "$SECONDS_TS" --out-ts bench.ts
INPUT=(--ts "0x$TSID=$PWD/bench.ts")
STATUS=(--status-secs 0)
if [ "$FEED" = loop ]; then
    # air time for PASSES passes and a bit of the next (lead-in included)
    INPUT+=(--feed loop --seconds "$((SECONDS_TS * PASSES + 1))")
elif [ "$FEED" = live ]; then
    # the TS's own byte rate, from its PCRs
    RATE=$("$ISDB" ts inspect bench.ts |
        sed -n 's/.*packet rate \([0-9.]*\) Mbit\/s.*/\1/p' | head -1)
    [ -n "$RATE" ] || die "no packet rate in isdb ts inspect bench.ts"
    BPS=$(awk -v r="$RATE" 'BEGIN { printf "%d", r * 1e6 / 8 + 0.5 }')
    echo "== live: bench.ts written into ts.fifo at $RATE Mbit/s ($BPS B/s) by pv"
    rm -f ts.fifo
    mkfifo ts.fifo
    pv -q -B 7520 -L "$BPS" bench.ts > ts.fifo &
    echo $! > writer.pid
    if [ "$STALL" != 0 ]; then
        echo "== live: the writer stalls for $STALL s, 4 s in"
        W=$(cat writer.pid)
        (sleep 4; kill -STOP "$W"; sleep "$STALL"; kill -CONT "$W") 2> /dev/null &
        echo $! > stall.pid
    fi
    INPUT=(--ts "0x$TSID=$PWD/ts.fifo" --feed live)
    STATUS=(--status-secs 2)
fi
"$ISDB" tx --system isdbs --channel BS1 "${INPUT[@]}" --ts-pacing "$PACING" \
    --iq tx.cs8 --iq-format cs8 --placed-ts 'placed-{ts_id}.ts' "${STATUS[@]}" 2>&1 | tee tx.log
LIVE_MISSES=0
if [ "$FEED" = live ]; then
    wait "$(cat writer.pid)" || true
    if [ -s stall.pid ]; then wait "$(cat stall.pid)" || true; rm -f stall.pid; fi
    rm -f writer.pid
    echo "== live: the transmitter's input at the end"
    grep -E 'input at the end|paced by PCR|done' tx.log || true
    LIVE_MISSES=$(sed -n 's/.*input at the end:.* deadline-misses \([0-9]*\).*/\1/p' tx.log | tail -1)
    [ -n "$LIVE_MISSES" ] || die "no live status in the transmitter's log (not paced live?)"
fi
"$ISDB" rx --iq tx.cs8 --out 'rx-{ts_id}.ts' --json > rx.json
echo "== placed TS"
"$ISDB" ts inspect "placed-$TSID.ts" | grep -E "packet rate|CBR-fit|PCR" || true
RX=rx-$TSID.ts
if [ "$FEED" = live ]; then
    # Live pacing starts the TS L (0.3 s) of nulls after the lead-in: a
    # receiver tuned then sees no PAT for 0.3 s, which validate counts as
    # a capture-edge gap. Check the recording from its first TS packet, as
    # a receiver tuned once the TS is on air would have it.
    RX=rx-$TSID.live.ts
    python3 -I - "rx-$TSID.ts" "$RX" <<'PY'
import sys
src, dst = sys.argv[1:3]
b = open(src, "rb").read()
i = 0
while i + 188 <= len(b) and ((b[i + 1] & 0x1F) << 8 | b[i + 2]) == 0x1FFF:
    i += 188
open(dst, "wb").write(b[i:])
print(f"== live: {i // 188} leading null packets ({i // 188 / 384 * 0.0110703:.3f} s) left out of {dst}")
PY
fi
echo "== received TS against the placed one"
VERDICT=0
"$HERE/tools/isdbs-bench-check.sh" --rate "52170000:$PPM" --fit-max-ns 100 \
    --placed "placed-$TSID.ts" "$RX" | tee check.log || VERDICT=$?
[ "$VERDICT" -le 1 ] || exit "$VERDICT"
if [ "$STALL" != 0 ] && [ "$FEED" = live ]; then
    # The stall is a hole in the TS (the writer sent nothing): PCR
    # intervals and PAT/PMT gaps beyond 0.1 s there are expected. What
    # must hold: PCRs on the slot grid, the rate, the placed TS on air,
    # and the lag back to zero once the backlog is sent.
    echo "== live: the stall's gap in the TS is expected (pcr.interval, tables.frequency)"
    VERDICT=0
    grep -q '^PASS  CBR-fit max' check.log || VERDICT=1
    grep -q '^PASS  PCR-implied rate' check.log || VERDICT=1
    grep -q '^PASS: .* packets identical' check.log || VERDICT=1
    if grep -q 'input at the end: .*PCR lag 0.00 s' tx.log; then
        echo "PASS  live: the lag recovered after the stall"
    else
        echo "FAIL  live: the lag did not recover"
        VERDICT=1
    fi
fi
if [ "$FEED" = loop ]; then
    echo "== loop seams of the received TS (tools/ts-loop-seam.py)"
    python3 -I "$HERE/tools/ts-loop-seam.py" bench.ts "rx-$TSID.ts" || VERDICT=1
fi
if [ "$FEED" = live ] && [ "$STALL" = 0 ]; then
    if [ "$LIVE_MISSES" = 0 ]; then
        echo "PASS  live: no deadline miss"
    else
        # a writer or a producer slower than real time (a loaded machine)
        echo "FAIL  live: $LIVE_MISSES deadline misses"
        VERDICT=1
    fi
elif [ "$FEED" = live ]; then
    if [ "$LIVE_MISSES" != 0 ]; then
        echo "PASS  live: the $STALL s stall gave $LIVE_MISSES deadline misses"
    else
        echo "FAIL  live: no deadline miss for a $STALL s stall"
        VERDICT=1
    fi
fi
if [ "$VERDICT" = 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$VERDICT"
