#!/usr/bin/env bash
# ISDB-S test broadcast into a conducted receiver (docs/plans/isdbs-fpga-tx.md
# §4 rung 4; VERIFICATION.md "ISDB-S transmitter in the FPGA: into the
# PIX-DT295"): a media-free bench service of isdbt-mux (its declared
# streams, TS_ID 0x4010) multiplexed to a CBR TS and transmitted as the one
# TS of a BS layout (TC8PSK 2/3 × 48) through the PlutoSky R2's FPGA
# transmit block on the BS1 IF.
#
# Usage: tools/isdbs-bench-tx.sh [OPTIONS] WORKDIR [ATT_DB] [PLUTO_URI]
#   WORKDIR   scratch directory for the TOML, the TS and the FIFO
#   ATT_DB    TX attenuation in dB (default 45: about -38 dBm at the port,
#             -68 dBm after a 30 dB pad, inside a tuner's -75..-20 dBm);
#             below 45 is refused without --margin
#   PLUTO_URI libiio URI (default: ISDB_PLUTO_URI or mDNS)
# Options:
#   --feed fifo  (default) bench60.ts looped by `cat`, paced at its own rate
#                by `pv` into a FIFO, `isdb tx --feed live` on the FIFO
#   --feed loop  bench60.ts given to `isdb tx --feed loop` directly (no pv,
#                no FIFO); the mode for PCR-paced placement
#                (docs/plans/isdbs-next/pcr-pacing.md §8, bench acceptance)
#   --margin     allow ATT_DB below 45 (an explicit level-margin or overload
#                measurement only)
#   --dry-run    pass --dry-run instead of --rf-enable: print the radio
#                settings and exit, nothing radiates
#   --placed T   pass --placed-ts T to isdb tx: the TS as placed in the
#                slots ({ts_id} -> 4010), for tools/isdbs-bench-check.sh
#                --placed (a relative T is under WORKDIR)
#
# Conducted bench only: a fixed attenuator (30-60 dB) and a DC block (or the
# receiver's LNB supply off) between the TX port and the receiver, as
# docs/plans/isdb-s.md §2 requires. Stop with Ctrl-C or SIGTERM: the signal
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
usage() { sed -n '9,27s/^# \{0,1\}//p' "$0"; }

FEED=fifo
MARGIN=0
GATE=--rf-enable
PLACED=()
while [ $# -gt 0 ]; do
    case $1 in
        --feed) [ $# -ge 2 ] || die "--feed needs fifo or loop"; FEED=$2; shift 2 ;;
        --feed=*) FEED=${1#--feed=}; shift ;;
        --margin) MARGIN=1; shift ;;
        --dry-run) GATE=--dry-run; shift ;;
        --placed) [ $# -ge 2 ] || die "--placed needs a path template"; PLACED=(--placed-ts "$2"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
done
case $FEED in fifo|loop) ;; *) die "--feed $FEED: expected fifo or loop" ;; esac
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

if [ "$FEED" = fifo ]; then
    # 260 OFDM frames of 231.336 ms (mode 3, GI 1/8): the TS's own byte rate.
    RATE=$(python3 -c "import os; print(int(os.path.getsize('bench60.ts') / (260 * 0.231336)))")
    rm -f ts.fifo
    mkfifo ts.fifo
    # `while cat` ends once pv is gone (cat dies of SIGPIPE); `while :` would
    # restart cat for ever.
    # shellcheck disable=SC2016 # $1 is expanded by the inner bash
    setsid bash -c 'while cat bench60.ts; do :; done | pv -q -B 7520 -L "$1" > ts.fifo' \
        pacer "$RATE" &
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
    INPUT=(--ts "0x4010=$WORK/ts.fifo" --feed live)
else
    INPUT=(--ts "0x4010=$WORK/bench60.ts" --feed loop)
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
