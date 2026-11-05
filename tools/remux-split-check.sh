#!/usr/bin/env bash
# Check `isdb-remux --mkv --split-events` on a clear TLV recording: the files' names
# against their first video frame, their streams, every audio frame decoding, the
# head and tail of the video decoding, and pictures and audio frames conserved
# against the run's own report.
#
#   tools/remux-split-check.sh /path/to/dec.tlv OUTDIR
#
# Writes to OUTDIR (about the recording's size, twice over while running) and leaves
# the files there; with OUTDIR/report.txt already there it only checks them again.
# Needs ffmpeg, ffprobe and a release build of isdb-remux. A loss in the recording
# shows up as decode errors of its own.
set -u
tlv=$1
out=$2
remux=${ISDB_REMUX:-target/release/isdb-remux}
mkdir -p "$out"
if [ ! -e "$out/report.txt" ]; then
  "$remux" -i "$tlv" --mkv --split-events -o "$out" --no-progress >"$out/report.txt" 2>&1
fi
grep -a '^mkv:' "$out/report.txt" | cut -c1-140
total_pictures=$(sed -n 's/^mkv: .*; \([0-9]*\) picture(s), \([0-9]*\) audio.*/\1/p' "$out/report.txt")
total_audio=$(sed -n 's/^mkv: .*; \([0-9]*\) picture(s), \([0-9]*\) audio.*/\2/p' "$out/report.txt")
noise='attachment|Invalid length|Could not find codec|analyzeduration'
decode_errors() { ffmpeg -nostdin -v error "$@" -f null - 2>&1 | grep -avE "$noise" | wc -l; }
sum_v=0
sum_a=0
errors=0
n=0
for f in "$out"/*.mkv; do
  n=$((n + 1))
  echo "=== $(basename "$f")"
  ffprobe -v quiet -show_entries format=duration:format_tags=title:stream=index,codec_name,channels,color_transfer,codec_type \
    -of compact=p=0 "$f" </dev/null 2>/dev/null | grep -av attachment | cut -c1-130
  v=$(ffprobe -v quiet -select_streams v:0 -count_packets -show_entries stream=nb_read_packets -of csv=p=0 "$f" </dev/null 2>/dev/null | tr -d ',')
  a=$(ffprobe -v quiet -select_streams a -count_packets -show_entries stream=nb_read_packets -of csv=p=0 "$f" </dev/null 2>/dev/null | tr -d ',' | awk '{s+=$1} END {print s+0}')
  sum_v=$((sum_v + ${v:-0}))
  sum_a=$((sum_a + ${a:-0}))
  first=$(ffprobe -v quiet -select_streams v:0 -show_entries packet=pts_time -read_intervals '%+#40' -of csv=p=0 "$f" </dev/null 2>/dev/null | tr -d ',' | sort -n | head -1)
  echo "first video PTS in the file: $first; pictures ${v:-?}; audio frames $a"
  a_err=$(decode_errors -i "$f" -map 0:a)
  h_err=$(decode_errors -t 4 -i "$f" -map 0:v:0)
  t_err=$(decode_errors -sseof -4 -i "$f" -map 0:v:0)
  echo "decode error lines: audio, whole file $a_err; video, first 4 s $h_err, last 4 s $t_err"
  errors=$((errors + a_err + h_err + t_err))
done
echo "--- files $n; pictures $sum_v of $total_pictures (left out $((total_pictures - sum_v))); audio frames $sum_a of $total_audio; decode error lines $errors"
