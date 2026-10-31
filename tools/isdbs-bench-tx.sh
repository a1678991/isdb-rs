#!/usr/bin/env bash
# ISDB-S test broadcast into a conducted receiver (docs/plans/isdbs-fpga-tx.md
# §4 rung 4; VERIFICATION.md "ISDB-S transmitter in the FPGA: into the
# PIX-DT295"): the media-free BS bench service of isdbt-mux
# (crates/isdbt-mux/examples/bs-bench.toml, docs/plans/isdbs-next/bs-si.md:
# BS-shaped SI, NIT network_id 0x0004 with the satellite delivery system
# descriptor of BS1, TS_ID 0x4017, service 0x03E7, broadcaster 0xEE; its
# declared streams, or with --media real video and audio) multiplexed to
# a CBR TS and transmitted as the one TS
# of a BS layout (TC8PSK 2/3 × 48) through the PlutoSky R2's FPGA transmit
# block on the BS1 IF. The TMCC announces the TOML's transport_stream_id
# (--ts 0x4017=…). Receive it with
#   dt295 tune --isdbs bs1 --tsid 4017
#   tools/isdbs-bench-check.sh --record 20 WORKDIR/bs.ts   (BS SI, TS_ID 4017)
#   python3 -I tools/media-decode-check.py --source MEDIA_DIR WORKDIR/bs.ts  (--media)
#
# Usage: tools/isdbs-bench-tx.sh [OPTIONS] WORKDIR [ATT_DB] [PLUTO_URI]
#   WORKDIR   scratch directory for the TOML, the TS and the FIFO
#   ATT_DB    TX attenuation in dB (default 45: about -38 dBm at the port,
#             -68 dBm after a 30 dB pad, inside a tuner's -75..-20 dBm);
#             below 45 is refused without --margin
#   PLUTO_URI libiio URI (default: ISDB_PLUTO_URI or mDNS)
# Options:
#   --feed fifo  (default without --media) the TS looped by `cat` (with
#                --media: `isdb mux` writing it live), paced at its own
#                rate by `pv` into a FIFO, `isdb tx --feed live` on the FIFO
#   --feed loop  (default with --media) the TS given to `isdb tx --feed
#                loop` directly (no pv, no FIFO); the mode for PCR-paced
#                placement (docs/plans/isdbs-next/pcr-pacing.md §8, bench
#                acceptance)
#   --terrestrial-si  the bench service as sent before BS SI (bench-minimal
#                with declared streams: terrestrial NIT, TS_ID 0x4010 =
#                BS-1 TS 0 on air), for comparison; receive with --tsid 4010
#   --margin     allow ATT_DB below 45 (an explicit level-margin or overload
#                measurement only)
#   --dry-run    pass --dry-run instead of --rf-enable: print the radio
#                settings and exit, nothing radiates
#   --placed T   pass --placed-ts T to isdb tx: the TS as placed in the
#                slots ({ts_id} -> 4017), for tools/isdbs-bench-check.sh
#                --placed (a relative T is under WORKDIR)
#   --media[=DIR]  the bench service with real video and audio: DIR's
#                video.m2v (MPEG-2 video ES) and audio.m2a (AAC ADTS), else
#                those of $ISDB_RS_MEDIA_DIR, else of
#                crates/isdbt-mux/examples/media/ (git-ignored; made as in
#                docs/test-data.md "Making a media directory"); refused
#                when none has both. The default feed becomes loop. See
#                "Media" below.
#
# WORKDIR keeps the TOML sent (bs-bench.toml, or terrestrial-bench.toml)
# and 60 s of its TS (bs60.ts, or bench60.ts), made again when the TOML
# is newer.
#
# Media (--media; docs/plans/isdbs-next/media-and-margin.md §2): the two
# declared streams of bs-bench.toml become an `mpeg2-video` (0x0111) and
# an `aac-adts` (0x0112) stream of the media files (absolute paths, with
# adts_compliance = "warn": FFmpeg's ADTS has no crc_check and a variable
# buffer fullness, which the default "conform" refuses), written to
# WORKDIR/bs-bench-media.toml by tools/bench-media.py. In loop mode its
# TS, bs-media.ts, is one media loop long: the multiplexer loops the media
# at the longest stream's length (pictures x frame period, ADTS frames x
# 1024 / fs; 10.027 s for the 10 s media of docs/test-data.md, whose audio
# is the longer), and the TS takes the whole number of 231.336 ms OFDM
# frames nearest to it (43 frames, 9.947 s, for those media), so each pass
# of `isdb tx --feed loop` restarts the media near where the multiplexer
# would have. `isdb tx` joins the passes (PCR, PTS/DTS run on, counters
# renumbered: no CC error, no time stamp going back), but the media are
# not seamless there: the file ends with what the multiplexer had sent by
# then, and each stream's lead over the PCR at the file's start is larger
# than at its end, so every pass leaves a gap in the time stamps and skips
# the media the file had not sent (offline, those media: video 137.6 ms,
# pictures 295-299 skipped; audio 603 ms, frames 438-469 skipped; a
# receiver freezes the picture and mutes for that long once per pass). In
# fifo mode no TS file is made: `isdb mux --out-ts -` (24 h of stream, the
# media looped inside the multiplexer, so no file seam at all) is paced by
# `pv` into the FIFO, `isdb tx --feed live` placing it by its PCR with a
# deadline. Check a recording with tools/media-decode-check.py --source DIR.
#
# Conducted bench only: a fixed attenuator (30-60 dB) and a DC
# block (or the receiver's LNB supply off) between the TX port and the
# receiver, as docs/plans/isdb-s.md §2 requires; a receiver that has
# scanned real BS may replace its stored BS network table with this
# one-TS NIT (bs-si.md §7 item 3). Stop with Ctrl-C or SIGTERM: the signal
# is passed on to `isdb tx` (which stops the radio first), the FIFO's
# writer is killed on every exit path, and the script exits with isdb's
# status. In loop mode `isdb tx` places the TS by its PCR (--ts-pacing
# auto; each loop seam joined: PCR/PTS/DTS run on, counters renumbered). In
# fifo mode (--feed live) it places the TS by its PCR as well, waiting for
# the writer only up to a deadline (pcr-pacing.md §7.7: the first PCR on
# air 0.3 s after it arrived, a superframe sent at the latest 0.2 s after
# its schedule; `live-input lag` and `deadline-misses` in the status
# lines); the `cat` seams every 60 s are not joined there (the PCR steps
# back, counted as a pcr-discontinuity; only --feed loop tags the passes).
set -euo pipefail
die() { echo "isdbs-bench-tx: $*" >&2; exit 2; }
usage() { sed -n '/^# Usage:/,/^#$/s/^# \{0,1\}//p' "$0"; }

FEED=
MARGIN=0
GATE=--rf-enable
PLACED=()
SI=bs
MEDIA=0
MEDIA_DIR=
while [ $# -gt 0 ]; do
    case $1 in
        --feed) [ $# -ge 2 ] || die "--feed needs fifo or loop"; FEED=$2; shift 2 ;;
        --feed=*) FEED=${1#--feed=}; shift ;;
        --media) MEDIA=1; shift ;;
        --media=*) MEDIA=1; MEDIA_DIR=${1#--media=}; [ -n "$MEDIA_DIR" ] || die "--media=: empty DIR"; shift ;;
        --terrestrial-si) SI=terrestrial; shift ;;
        --margin) MARGIN=1; shift ;;
        --dry-run) GATE=--dry-run; shift ;;
        --placed) [ $# -ge 2 ] || die "--placed needs a path template"; PLACED=(--placed-ts "$2"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
done
if [ -z "$FEED" ]; then
    if [ "$MEDIA" = 1 ]; then FEED=loop; else FEED=fifo; fi
fi
case $FEED in fifo|loop) ;; *) die "--feed $FEED: expected fifo or loop" ;; esac
[ "$MEDIA" = 0 ] || [ "$SI" = bs ] || die "--media sends the BS bench service only, not with --terrestrial-si"
[ $# -ge 1 ] && [ $# -le 3 ] || { usage >&2; exit 2; }
WORK=$1
ATT=${2:-45}
URI=${3:-${ISDB_PLUTO_URI:-}}
[[ $ATT =~ ^[0-9]+(\.[0-9]+)?$ ]] || die "ATT_DB $ATT: expected a number of dB"
if awk -v a="$ATT" 'BEGIN { exit !(a < 45) }'; then
    [ "$MARGIN" = 1 ] || die "ATT_DB $ATT is below 45 dB; pass --margin for an explicit margin measurement"
    echo "isdbs-bench-tx: --margin: attenuation $ATT dB, below the 45 dB bench default" >&2
fi
if [ "$FEED" = fifo ]; then
    command -v pv >/dev/null || die "--feed fifo needs pv"
    command -v setsid >/dev/null || die "--feed fifo needs setsid (util-linux)"
fi
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
if [ "$MEDIA" = 1 ]; then
    # --media=DIR, else ISDB_RS_MEDIA_DIR, else the examples' media/ if there
    if [ -z "$MEDIA_DIR" ]; then
        MEDIA_DIR=${ISDB_RS_MEDIA_DIR:-}
        [ -n "$MEDIA_DIR" ] || [ ! -d "$HERE/crates/isdbt-mux/examples/media" ] ||
            MEDIA_DIR=$HERE/crates/isdbt-mux/examples/media
        [ -n "$MEDIA_DIR" ] || die "--media: no media directory: give --media=DIR, set ISDB_RS_MEDIA_DIR, or put video.m2v and audio.m2a into crates/isdbt-mux/examples/media/ (docs/test-data.md, \"Making a media directory\")"
    fi
    [ -d "$MEDIA_DIR" ] || die "--media: $MEDIA_DIR is not a directory"
    MEDIA_DIR=$(cd "$MEDIA_DIR" && pwd)
    for f in video.m2v audio.m2a; do
        [ -s "$MEDIA_DIR/$f" ] && [ -r "$MEDIA_DIR/$f" ] ||
            die "--media: $MEDIA_DIR/$f is missing or empty (the media directory needs video.m2v, an MPEG-2 video ES, and audio.m2a, AAC ADTS; docs/test-data.md)"
    done
fi
mkdir -p "$WORK"
cd "$WORK"
WORK=$(pwd)

PACER=
TX=
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
    if [ -n "$PACER" ]; then
        # The pacer runs in its own process group (setsid): the subshell,
        # every `cat` and `pv` go together.
        kill -TERM -- "-$PACER" 2>/dev/null || true
        wait "$PACER" 2>/dev/null || true
        PACER=
    fi
    rm -f "$WORK/ts.fifo"
}
# Pass a stop request on to isdb tx and keep waiting for it to stop the
# radio. TERM in both cases: a background child of a non-interactive shell
# may start with SIGINT ignored, and isdb handles TERM like INT.
# shellcheck disable=SC2329 # invoked by the signal traps
on_signal() {
    if [ -n "$TX" ]; then
        kill -TERM "$TX" 2>/dev/null || true
    else
        exit "$1"
    fi
}
trap cleanup EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM
trap 'on_signal 129' HUP

# The TOML sent, copied into WORKDIR (or, --terrestrial-si, written there),
# and its TS. The TS file is named after the SI, so that a TS of the other
# kind left in WORKDIR is never sent by mistake.
if [ "$SI" = bs ]; then
    TOML=bs-bench.toml
    TS=bs60.ts
    SRC=$HERE/crates/isdbt-mux/examples/bs-bench.toml
    cmp -s "$SRC" "$TOML" || cp "$SRC" "$TOML"
    # 0x43 names the channel; `isdb tx --channel` below must be the same.
    grep -Eq '^[[:space:]]*channel = "BS1"' "$TOML" ||
        die "$SRC: [network.satellite] channel is not \"BS1\", the channel transmitted"
    if [ "$MEDIA" = 1 ]; then
        # bs-bench.toml's two declared streams made media streams of the
        # media files (tools/bench-media.py, also used by
        # tools/isdbs-pacing-e2e.sh MEDIA=DIR), and the whole number of OFDM
        # frames nearest to the media loop: one media loop per pass of
        # --feed loop.
        TOML=bs-bench-media.toml
        TS=bs-media.ts
        LINE=$(python3 -I "$HERE/tools/bench-media.py" bs-bench.toml "$TOML.new" "$MEDIA_DIR") ||
            die "--media: $MEDIA_DIR: the media could not be read"
        read -r FRAMES MEDIA_NOTE <<< "$LINE"
        if cmp -s "$TOML.new" "$TOML"; then rm -f "$TOML.new"; else mv "$TOML.new" "$TOML"; fi
        echo "isdbs-bench-tx: media from $MEDIA_DIR: $MEDIA_NOTE" >&2
    fi
else
    TOML=terrestrial-bench.toml
    TS=bench60.ts
    # bench-minimal.toml without media (as isdb-cli's tests declare it),
    # TS_ID 0x4010.
    python3 -I - "$HERE/crates/isdbt-mux/examples/bench-minimal.toml" "$TOML.new" <<'PY'
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
    if cmp -s "$TOML.new" "$TOML"; then rm -f "$TOML.new"; else mv "$TOML.new" "$TOML"; fi
fi
# The TS_ID announced in the TMCC: the TS's own (its PAT's), from the TOML.
TSID=$(sed -n 's/^transport_stream_id = \(0x[0-9A-Fa-f]\{1,4\}\).*/\1/p' "$TOML" | head -1)
[ -n "$TSID" ] || die "$TOML: no transport_stream_id = 0x… line"
if [ "$MEDIA" = 1 ] && [ "$FEED" = fifo ]; then
    echo "isdbs-bench-tx: $SI SI with media, TS_ID $TSID <- isdb mux -c $WORK/$TOML (live)" >&2
else
    if [ "$MEDIA" = 1 ]; then
        if [ ! -s "$TS" ] || [ "$TOML" -nt "$TS" ] ||
            [ "$MEDIA_DIR/video.m2v" -nt "$TS" ] || [ "$MEDIA_DIR/audio.m2a" -nt "$TS" ]; then
            "$ISDB" mux -c "$TOML" --frames "$FRAMES" --out-ts "$TS"
        fi
    elif [ ! -s "$TS" ] || [ "$TOML" -nt "$TS" ]; then
        "$ISDB" mux -c "$TOML" --seconds 60 --out-ts "$TS"
    fi
    echo "isdbs-bench-tx: $SI SI${MEDIA_DIR:+ with media}, TS_ID $TSID <- $WORK/$TS" >&2
fi

if [ "$FEED" = fifo ]; then
    rm -f ts.fifo
    mkfifo ts.fifo
    if [ "$MEDIA" = 1 ]; then
        # 4608 packets per OFDM frame of 231.336 ms (mode 3, GI 1/8): the
        # byte rate of isdb mux's --out-ts. pv's back-pressure paces the
        # multiplexer; it ends with pv (EPIPE) or the group kill.
        RATE=$(python3 -c "print(int(4608 * 188 / 0.231336))")
        # shellcheck disable=SC2016 # $1..$3 are expanded by the inner bash
        setsid bash -c '"$2" mux -c "$3" --seconds 86400 --out-ts - | pv -q -B 7520 -L "$1" > ts.fifo' \
            pacer "$RATE" "$ISDB" "$TOML" &
    else
        # 260 OFDM frames of 231.336 ms (mode 3, GI 1/8): the TS's own byte rate.
        RATE=$(python3 -c "import os, sys; print(int(os.path.getsize(sys.argv[1]) / (260 * 0.231336)))" "$TS")
        # `while cat` ends once pv is gone (cat dies of SIGPIPE); `while :` would
        # restart cat for ever.
        # shellcheck disable=SC2016 # $1, $2 are expanded by the inner bash
        setsid bash -c 'while cat "$2"; do :; done | pv -q -B 7520 -L "$1" > ts.fifo' \
            pacer "$RATE" "$TS" &
    fi
    PACER=$!
    # Until setsid() has run the pacer is in this shell's process group and
    # the group kill in cleanup would miss it: wait for it to lead its own.
    leads() { [ "$(ps -o pgid= -p "$PACER" 2>/dev/null | tr -d ' ')" = "$PACER" ]; }
    for _ in $(seq 500); do
        leads && break
        kill -0 "$PACER" 2>/dev/null || die "the FIFO pacer did not start"
        sleep 0.01
    done
    leads || die "the FIFO pacer has no process group of its own"
    INPUT=(--ts "$TSID=$WORK/ts.fifo" --feed live)
else
    INPUT=(--ts "$TSID=$WORK/$TS" --feed loop)
fi

RADIO="pluto${URI:+:uri=$URI}"
"$ISDB" tx --system isdbs --radio "$RADIO" --channel BS1 --freq-unrestricted \
    --offload fpga "$GATE" --gain "att=$ATT" "${INPUT[@]}" "${PLACED[@]}" --status-secs 5 &
TX=$!
# `wait` returns early when a trapped signal arrives; wait again until
# isdb tx has really exited, then take its status (bash keeps it).
while kill -0 "$TX" 2>/dev/null; do
    wait "$TX" || true
done
STATUS=0
wait "$TX" || STATUS=$?
TX=
exit "$STATUS"
