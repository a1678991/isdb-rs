#!/usr/bin/env bash
# Self-test of tools/media-decode-check.py (no radio): 4 s of media made by
# ffmpeg (docs/test-data.md's recipe) multiplexed into the BS bench service
# (tools/bench-media.py, `isdb mux`), then checked
#   - as multiplexed, with and without --source: PASS;
#   - with the payload of a few video packets in the middle overwritten:
#     FAIL (decoded pictures not in the source, inside a continuous stretch);
#   - with about 1 s of packets cut out of the middle: FAIL (continuity
#     errors, and the media order jumps).
#
# Usage: tools/media-decode-check-test.sh WORKDIR   (needs ffmpeg, ffprobe
#   and target/release/isdb, or ISDB; exits 0 on PASS). WORKDIR takes about
#   30 MB; keep it on disk, not in a tmpfs /tmp.
set -euo pipefail
die() { echo "media-decode-check-test: $*" >&2; exit 2; }
[ $# -eq 1 ] || { sed -n '/^# Usage:/,/^#   30 MB/s/^# \{0,1\}//p' "$0" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || die "$ISDB: build it with cargo build --release -p isdb-cli"
command -v ffmpeg > /dev/null || die "needs ffmpeg"
mkdir -p "$1/media"
cd "$1"
ffmpeg -loglevel error -y -f lavfi -i testsrc2=size=1440x1080:rate=30000/1001 \
    -t 4 -vf setfield=tff -c:v mpeg2video -b:v 12M -maxrate 12M -bufsize 7M \
    -g 15 -bf 2 -flags +ilme+ildct -aspect 16:9 -f mpeg2video media/video.m2v
ffmpeg -loglevel error -y -f lavfi -i sine=frequency=1000:sample_rate=48000 -ac 2 \
    -t 4 -c:a aac -b:a 192k -f adts media/audio.m2a
read -r FRAMES _ < <(python3 -I "$HERE/tools/bench-media.py" \
    "$HERE/crates/isdbt-mux/examples/bs-bench.toml" bench.toml "$PWD/media")
# two media loops and a bit: the media's own loop point inside
"$ISDB" mux -c bench.toml --frames "$((FRAMES * 2 + 3))" --out-ts clean.ts --log-level error
python3 -I - clean.ts corrupt.ts cut.ts <<'PY'
import sys
src, corrupt, cut = sys.argv[1:4]
b = bytearray(open(src, "rb").read())
n = len(b) // 188
video = [i for i in range(n) if ((b[i * 188 + 1] & 0x1F) << 8 | b[i * 188 + 2]) == 0x0111]
# corrupt: 6 video packets 40 % in, payload only (header and PES starts kept)
k = 0
for i in video[len(video) * 2 // 5:]:
    p = i * 188
    if b[p + 1] & 0x40 or (b[p + 3] >> 4) & 3 != 1:
        continue
    b[p + 4:p + 188] = bytes([0xA5]) * 184
    k += 1
    if k == 6:
        break
open(corrupt, "wb").write(b)
# cut: about 1 s (20 000 packets) 60 % in, one video packet more if the
# video packets cut are a multiple of 16 (the counter would not see it)
c = bytes(open(src, "rb").read())
m = n * 3 // 5
e = m + 20000
while sum(1 for i in video if m <= i < e) % 16 == 0:
    e = next(i for i in video if i >= e) + 1
open(cut, "wb").write(c[:m * 188] + c[e * 188:])
PY

CHECK=$HERE/tools/media-decode-check.py
FAIL=0
check() {  # NAME EXPECTED_RC PATTERN ARGS…
    local name=$1 want=$2 pat=$3 rc=0
    shift 3
    python3 -I "$CHECK" --isdb "$ISDB" "$@" > out.txt 2>&1 || rc=$?
    if [ "$rc" = "$want" ] && grep -Eq -- "$pat" out.txt; then
        echo "ok   $name"
    else
        echo "FAIL $name (exit $rc, wanted $want and /$pat/)"
        sed 's/^/     /' out.txt
        FAIL=1
    fi
}
check "as multiplexed, against the source: PASS" 0 'source loop point' --source media clean.ts
check "as multiplexed, without the source: PASS" 0 '^PASS$' clean.ts
check "corrupted video payload: FAIL, a run inside" 1 'at inside: not allowed' --source media corrupt.ts
check "1 s cut: FAIL, continuity errors" 1 \
    'FAIL  video: continuity errors [1-9]' --source media cut.ts
if grep -Eq ' [1-9][0-9]* order jumps' out.txt; then
    echo "ok   1 s cut: the media order jumps"
else
    echo "FAIL 1 s cut: no order jump"
    FAIL=1
fi
if [ "$FAIL" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi
