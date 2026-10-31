#!/usr/bin/env bash
# The checks are strings eval'd by check(), RC included.
# shellcheck disable=SC2016,SC2034
# Self-test of tools/isdbs-bench-tx.sh with a dummy `isdb` in place of the
# real one (ISDB=…): no radio is opened, nothing is built or transmitted.
# Checks the argument gate (attenuation ≥ 45 unless --margin), the TS sent
# (bs-bench.toml copied into WORKDIR, TS_ID 0x4017 from it, `isdb mux` only
# when its TS is missing or older; --terrestrial-si: TS_ID 0x4010 from
# bench-minimal.toml; a TS of the other SI never reused), the command
# line of each feed mode, that isdb's exit status is propagated, that
# INT/TERM reach isdb, and that no `cat`/`pv` of the FIFO pacer survives
# any exit path (the `while :; do cat …` loop and `exec` that dropped the
# EXIT trap, docs/plans/isdbs-next/media-and-margin.md §1). --media runs on
# stub media files (a sequence header and 30 picture start codes, 47 empty
# ADTS frames; no real media needed): the media directory's resolution and
# refusals, the media TOML, the TS length from the media length, the TS
# made again when a media file is newer, and fifo mode's live `isdb mux`
# writer, which no exit path may leave behind either.
#
# Usage: tools/isdbs-bench-tx-test.sh     (needs pv and setsid; exits 0 on PASS)
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT=${BENCH_TX:-$HERE/tools/isdbs-bench-tx.sh}  # BENCH_TX: test another copy
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
WORK=$T/work
mkdir -p "$WORK"
# The TOML as the script copies it, and a small stand-in for its TS,
# newer (the script then skips `isdb mux`).
BS_TOML=$HERE/crates/isdbt-mux/examples/bs-bench.toml
cp "$BS_TOML" "$WORK/bs-bench.toml"
touch -d '1 minute ago' "$WORK/bs-bench.toml"
head -c $((188 * 4000)) /dev/zero > "$WORK/bs60.ts"

# The dummy: logs its arguments; `isdb mux` writes a small --out-ts and
# exits 0 (--out-ts -: zeros to stdout until the reader is gone); otherwise DUMMY=exit:N exits N at once, DUMMY=read:N reads
# 4 KiB of its --ts input then exits N, DUMMY=wait runs until a TERM/INT
# and exits 7.
cat > "$T/isdb" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DUMMY_LOG"
if [ "$1" = mux ]; then
    while [ $# -gt 0 ]; do
        if [ "$1" = --out-ts ]; then
            [ "$2" != - ] || exec cat /dev/zero
            head -c $((188 * 4000)) /dev/zero > "$2"
        fi
        shift
    done
    exit 0
fi
input=
for a in "$@"; do case $a in 0x*=*) input=${a#*=} ;; esac; done
case $DUMMY in
    exit:*) exit "${DUMMY#exit:}" ;;
    read:*) head -c 4096 "$input" > /dev/null; exit "${DUMMY#read:}" ;;
    wait)
        trap 'echo "got TERM" >> "$DUMMY_LOG"; exit 7' TERM
        trap 'echo "got INT" >> "$DUMMY_LOG"; exit 7' INT
        cat "$input" > /dev/null &
        while :; do sleep 0.05; done ;;
esac
SH
chmod +x "$T/isdb"
export ISDB=$T/isdb DUMMY_LOG=$T/log

FAIL=0
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; FAIL=1; fi; }
# Processes still running in WORK, or the directory given (the pacer's
# cat/pv/bash, the dummy and its live mux).
leftovers() {
    local n=0 p d=${1:-$WORK}
    for p in /proc/[0-9]*; do
        [ "$(readlink "$p/cwd" 2>/dev/null)" = "$d" ] && n=$((n + 1))
    done
    echo "$n"
}
run() { : > "$DUMMY_LOG"; set +e; timeout 20 "$SCRIPT" "$@" > "$T/out" 2>&1; RC=$?; set -e; }

DUMMY=exit:0 run "$WORK" 44
check "ATT 44 refused without --margin" '[ $RC = 2 ] && grep -q -- --margin "$T/out" && [ ! -s "$DUMMY_LOG" ]'
DUMMY=exit:0 run "$WORK" 4x
check "non-numeric ATT refused" '[ $RC = 2 ]'
DUMMY=exit:0 run --feed bogus "$WORK"
check "unknown --feed refused" '[ $RC = 2 ]'
DUMMY=exit:0 run --margin --feed loop "$WORK" 40
check "--margin allows ATT 40" '[ $RC = 0 ] && grep -q "att=40" "$DUMMY_LOG"'

DUMMY=exit:3 run --feed loop "$WORK" 45 ip:192.0.2.1
check "loop: isdb's status 3 propagated" '[ $RC = 3 ]'
check "loop: --feed loop on bs60.ts as TS_ID 0x4017, --rf-enable, att, uri" \
    'grep -q -- "--rf-enable --gain att=45 --ts 0x4017=$WORK/bs60.ts --feed loop" "$DUMMY_LOG" && grep -q "pluto:uri=ip:192.0.2.1" "$DUMMY_LOG"'
check "loop: BS1 with the FPGA block" 'grep -q -- "tx --system isdbs --radio pluto:uri=ip:192.0.2.1 --channel BS1 --freq-unrestricted --offload fpga" "$DUMMY_LOG"'
check "loop: no FIFO" '[ ! -e "$WORK/ts.fifo" ]'
check "an up-to-date TS is not multiplexed again" '! grep -q "^mux " "$DUMMY_LOG"'

# A fresh WORKDIR holding a TS of the other SI (the terrestrial bench60.ts
# of the runs before BS SI): the BS TS is made, the old one left alone.
W2=$T/work2
mkdir -p "$W2"
head -c 188 /dev/zero > "$W2/bench60.ts"
DUMMY=exit:0 run --feed loop "$W2"
check "fresh: bs-bench.toml copied, muxed for 60 s into bs60.ts" \
    '[ $RC = 0 ] && cmp -s "$BS_TOML" "$W2/bs-bench.toml" && grep -qx "mux -c bs-bench.toml --seconds 60 --out-ts bs60.ts" "$DUMMY_LOG"'
check "fresh: TS_ID 0x4017 on bs60.ts, not the old bench60.ts" \
    'grep -q -- "--ts 0x4017=$W2/bs60.ts --feed loop" "$DUMMY_LOG" && ! grep -q "bench60" "$DUMMY_LOG"'
check "fresh: the run names its SI and TS_ID" 'grep -q "bs SI, TS_ID 0x4017" "$T/out"'
touch -d '2 minutes ago' "$W2/bs60.ts"
touch "$W2/bs-bench.toml"
DUMMY=exit:0 run --feed loop "$W2"
check "a TOML newer than its TS is multiplexed again" 'grep -q "^mux -c bs-bench.toml" "$DUMMY_LOG"'

DUMMY=exit:0 run --terrestrial-si --feed loop "$W2"
check "--terrestrial-si: bench-minimal without media, TS_ID 0x4010" \
    '[ $RC = 0 ] && grep -q "^transport_stream_id = 0x4010" "$W2/terrestrial-bench.toml" && ! grep -q "^ *source =" "$W2/terrestrial-bench.toml" && grep -q "kind = \"declared\"" "$W2/terrestrial-bench.toml" && ! grep -q "kind = \"bs\"" "$W2/terrestrial-bench.toml"'
check "--terrestrial-si: sends bench60.ts as TS_ID 0x4010 (made again: older than its TOML)" \
    'grep -qx "mux -c terrestrial-bench.toml --seconds 60 --out-ts bench60.ts" "$DUMMY_LOG" && grep -q -- "--ts 0x4010=$W2/bench60.ts --feed loop" "$DUMMY_LOG"'
DUMMY=exit:0 run --terrestrial-si --feed loop "$W2"
check "--terrestrial-si: an unchanged TOML is not rewritten, its TS kept" '! grep -q "^mux " "$DUMMY_LOG"'
DUMMY=exit:0 run --help
check "--help prints the usage" '[ $RC = 0 ] && grep -q "^Usage: tools/isdbs-bench-tx.sh" "$T/out" && grep -q -- "--terrestrial-si" "$T/out" && ! grep -q "^set -euo" "$T/out"'

DUMMY=exit:0 run --dry-run "$WORK"
check "--dry-run replaces --rf-enable" 'grep -q -- "--dry-run" "$DUMMY_LOG" && ! grep -q -- "--rf-enable" "$DUMMY_LOG"'
check "no --placed-ts unless asked" '! grep -q -- "--placed-ts" "$DUMMY_LOG"'
DUMMY=exit:0 run --feed loop --placed 'placed-{ts_id}.ts' "$WORK"
check "--placed passes --placed-ts" 'grep -q -- "--feed loop --placed-ts placed-{ts_id}.ts --status-secs" "$DUMMY_LOG"'

# --media on stub media: 30 pictures at 29.97 Hz (1.001 s) and 47 ADTS
# frames of 1024 samples at 48 kHz (1.003 s, the longer): the TS is the
# 4 OFDM frames (0.925 s) nearest to the media loop.
M=$T/media
mkdir -p "$M"
python3 -I - "$M" <<'PY'
import sys
d = sys.argv[1]
with open(f"{d}/video.m2v", "wb") as f:
    f.write(b"\x00\x00\x01\xb3\x5a\x04\x38\x34\xff\xff\xe0\x18")  # 1440x1080, 29.97 Hz
    f.write(b"\x00\x00\x01\x00\x00\x0f" * 30)
with open(f"{d}/audio.m2a", "wb") as f:
    f.write(bytes.fromhex("fff14c8000fffc") * 47)  # ADTS LC 48 kHz 2 ch, 7 bytes each
PY
W3=$T/work3
mkdir -p "$W3"
DUMMY=exit:0 run --media="$M" "$W3"
check "--media=DIR: bs-bench-media.toml with the media streams, warn, TS_ID 0x4017" \
    '[ $RC = 0 ] && grep -q "^transport_stream_id = 0x4017" "$W3/bs-bench-media.toml" && grep -qx "  source = \"$M/video.m2v\"" "$W3/bs-bench-media.toml" && grep -qx "  source = \"$M/audio.m2a\"" "$W3/bs-bench-media.toml" && grep -q "kind = \"mpeg2-video\"" "$W3/bs-bench-media.toml" && grep -q "kind = \"aac-adts\"" "$W3/bs-bench-media.toml" && grep -qx "  adts_compliance = \"warn\"" "$W3/bs-bench-media.toml" && ! grep -q "kind = \"declared\"" "$W3/bs-bench-media.toml"'
check "--media: the TS is one media loop, 4 OFDM frames, into bs-media.ts" \
    'grep -qx "mux -c bs-bench-media.toml --frames 4 --out-ts bs-media.ts" "$DUMMY_LOG" && grep -q "media loop 1.003 s -> TS of 4 OFDM frames = 0.925 s" "$T/out"'
check "--media: --feed loop by default, bs-media.ts as TS_ID 0x4017, --rf-enable" \
    'grep -q -- "--rf-enable --gain att=45 --ts 0x4017=$W3/bs-media.ts --feed loop" "$DUMMY_LOG" && [ ! -e "$W3/ts.fifo" ] && grep -q "bs SI with media, TS_ID 0x4017" "$T/out"'
DUMMY=exit:0 ISDB_RS_MEDIA_DIR=$M run --media "$W3"
check "--media: ISDB_RS_MEDIA_DIR, an up-to-date TS not multiplexed again" \
    '[ $RC = 0 ] && grep -q "media from $M:" "$T/out" && ! grep -q "^mux " "$DUMMY_LOG"'
touch -d '1 minute ago' "$W3/bs-media.ts"
touch "$M/audio.m2a"
DUMMY=exit:0 run --media="$M" "$W3"
check "--media: a media file newer than the TS is multiplexed again" 'grep -q "^mux -c bs-bench-media.toml" "$DUMMY_LOG"'
DUMMY=exit:0 run --media="$M" --dry-run "$W3"
check "--media --dry-run replaces --rf-enable" '[ $RC = 0 ] && grep -q -- "--dry-run" "$DUMMY_LOG" && ! grep -q -- "--rf-enable" "$DUMMY_LOG"'
DUMMY=exit:0 run --media="$M" "$W3" 44
check "--media: ATT 44 refused without --margin" '[ $RC = 2 ] && [ ! -s "$DUMMY_LOG" ]'

mkdir -p "$T/noaudio"
cp "$M/video.m2v" "$T/noaudio/"
DUMMY=exit:0 run --media="$T/noaudio" "$W3"
check "--media: a directory without audio.m2a refused" '[ $RC = 2 ] && grep -q "noaudio/audio.m2a is missing" "$T/out" && [ ! -s "$DUMMY_LOG" ]'
DUMMY=exit:0 run --media="$T/absent" "$W3"
check "--media: a missing directory refused" '[ $RC = 2 ] && grep -q "is not a directory" "$T/out" && [ ! -s "$DUMMY_LOG" ]'
mkdir -p "$T/badaudio"
cp "$M/video.m2v" "$T/badaudio/"
head -c 100 /dev/zero > "$T/badaudio/audio.m2a"
DUMMY=exit:0 run --media="$T/badaudio" "$W3"
check "--media: audio without ADTS sync refused" '[ $RC = 2 ] && grep -q "no ADTS sync" "$T/out" && [ ! -s "$DUMMY_LOG" ]'
DUMMY=exit:0 run --media="$M" --terrestrial-si "$W3"
check "--media with --terrestrial-si refused" '[ $RC = 2 ] && [ ! -s "$DUMMY_LOG" ]'
if [ -d "$HERE/crates/isdbt-mux/examples/media" ]; then
    echo "skip --media without any media directory (crates/isdbt-mux/examples/media exists)"
else
    DUMMY=exit:0 ISDB_RS_MEDIA_DIR='' run --media "$W3"
    check "--media without any media directory refused" '[ $RC = 2 ] && grep -q "no media directory" "$T/out" && [ ! -s "$DUMMY_LOG" ]'
fi

# fifo mode with media: a live `isdb mux --out-ts -` (the dummy writes
# zeros until its reader is gone) paced by pv; no TS file.
rm -f "$W3/bs-media.ts"
DUMMY=read:0 run --media="$M" --feed fifo "$W3"
check "--media --feed fifo: live isdb mux into the FIFO, --feed live, no TS file" \
    '[ $RC = 0 ] && grep -qx "mux -c bs-bench-media.toml --seconds 86400 --out-ts -" "$DUMMY_LOG" && grep -q -- "--ts 0x4017=$W3/ts.fifo --feed live" "$DUMMY_LOG" && [ ! -e "$W3/bs-media.ts" ]'
sleep 0.3
check "--media --feed fifo: no writer left after a normal exit" '[ "$(leftovers "$W3")" = 0 ]'
DUMMY=exit:5 run --media="$M" --feed fifo "$W3"
check "--media --feed fifo: isdb's status 5 propagated" '[ $RC = 5 ]'
sleep 0.3
check "--media --feed fifo: no writer left when isdb never opened the FIFO" '[ "$(leftovers "$W3")" = 0 ]'

DUMMY=read:0 run "$WORK"
check "fifo: isdb read the FIFO and exited 0" '[ $RC = 0 ] && grep -q -- "--ts 0x4017=$WORK/ts.fifo --feed live" "$DUMMY_LOG"'
sleep 0.3
check "fifo: no pacer left after a normal exit" '[ "$(leftovers)" = 0 ]'

# isdb exits without ever opening the FIFO: pv blocks on its open() and
# cat on a full pipe; only the process-group kill ends them.
DUMMY=exit:5 run "$WORK"
check "fifo: isdb's status 5 propagated" '[ $RC = 5 ]'
sleep 0.3
check "fifo: no pacer left when isdb never opened the FIFO" '[ "$(leftovers)" = 0 ]'

# A background job of this non-interactive shell would start with SIGINT
# ignored, which no trap can undo; at a terminal the script is in the
# foreground and gets it. Start it with SIGINT at its default.
sigdfl() {
    # exec: the background job's pid ($!) is then the script's own.
    exec python3 -I -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])' "$@"
}
# The last case: --media --feed fifo, whose writer is a live isdb mux.
for case in TERM INT "TERM --media"; do
    sig=${case%% *}
    D=$WORK
    ARGS=()
    if [ "$case" != "$sig" ]; then D=$W3; ARGS=(--media="$M" --feed fifo); fi
    : > "$DUMMY_LOG"
    DUMMY="wait" sigdfl "$SCRIPT" "${ARGS[@]}" "$D" > "$T/out" 2>&1 &
    S=$!
    for _ in $(seq 100); do grep -q '^tx ' "$DUMMY_LOG" 2>/dev/null && break; sleep 0.05; done
    sleep 0.3
    kill -s "$sig" "$S"
    for _ in $(seq 200); do kill -0 "$S" 2>/dev/null || break; sleep 0.05; done
    if kill -0 "$S" 2>/dev/null; then
        echo "FAIL fifo: the script did not end within 10 s of $case"
        kill -KILL "$S"
        FAIL=1
    fi
    set +e; wait "$S"; RC=$?; set -e
    check "fifo: $case reaches isdb, its status 7 propagated" '[ $RC = 7 ] && grep -q "got TERM" "$DUMMY_LOG"'
    sleep 0.3
    check "fifo: no pacer left after $case" '[ "$(leftovers "$D")" = 0 ]'
done

if [ "$FAIL" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi
