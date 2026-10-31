#!/usr/bin/env bash
# ISDB-S level margin into the PIX-DT295 (docs/plans/isdbs-next/
# media-and-margin.md §6, item 4 of docs/plans/isdbs-next.md): a hold sweep
# of the PlutoSky R2's TX attenuation with the bench broadcast of
# tools/isdbs-bench-tx.sh on air, conducted into the tuner.
#
# From --from (45) upward, never below 45 without --margin, never above
# 89.75. At each step: the attenuation is written live with iio_attr
# (ad9361-phy, output channel voltage0 = TX1, hardwaregain = -att) and read
# back; a read-back off by more than 0.125 dB means the live write did not
# take effect: the sweep stops, or with --restart (needs --start-tx) goes
# on restarting the broadcast at each level. Then it settles, polls
# `dt295 status --isdbs --watch DWELL --json --read-reg bc:2` with the tuner
# never retuned (the hold: lock state, raw C/N 0xBC, error counters, BER),
# makes a fresh `dt295 tune` (the acquisition, time to lock) and records
# `dt295 stream --raw-config` for --stream-secs, checked with
# `isdb ts inspect` (sync losses, TEI, CC). The step grows --coarse (5 dB)
# up to --fine-from (70), or until the raw C/N leaves its floor (> 3000) or
# an error counter moves, then --fine (1 dB). At the first lost hold (at
# least --loss-rounds consecutive rounds with 0xC3 != 00) it goes back to
# the last held level and steps --finest (0.25 dB); the next loss ends the
# sweep. Hysteresis: from that loss point the attenuation is lowered by
# --finest steps, down to --hyst-span dB below it, with `status --watch
# --hyst-dwell` and no retune, until every round is locked again (whether
# the TC90532 re-acquires by itself). After a failed fresh tune the tuner is
# re-locked at the last level where a tune locked before the next step.
# Unverified on the bench (media-and-margin.md §5.2 [confirm]): that an
# (libiio 0.26: no -q, it silences the value). iio_attr write of hardwaregain while `isdb tx` streams takes effect without
# an underrun or MISSED, and the exact iio_attr argument order; the read-back
# after every write and the TX block MISSED column are the check.
#
# Usage: tools/isdbs-level-sweep.sh --conducted (--tx-pid FILE | --start-tx WORKDIR) [OPTIONS] OUTDIR
#   --conducted       required: acknowledges the conducted chain TX1 ->
#                     >= 30 dB attenuator -> DC block -> PIX-DT295, nothing
#                     else on TX1, no antenna (CONTRIBUTING.md "RF safety")
#   --tx-pid FILE     the broadcast was started by the operator
#                     (tools/isdbs-bench-tx.sh, in its own terminal); FILE
#                     holds the pid of that script or of its `isdb tx`. The
#                     sweep checks it is alive and is one of those before
#                     every write, and leaves it running at --exit-att
#   --tx-log FILE     with --tx-pid: the broadcast's log, for the TX block
#                     MISSED / underrun figures per step
#   --start-tx WORK   the sweep starts tools/isdbs-bench-tx.sh WORK ATT URI
#                     itself (its own --rf-enable gate; this script never
#                     passes --rf-enable), log OUTDIR/tx.log, pid
#                     OUTDIR/tx.pid, and stops it on every exit path
#   --tx-opt OPT      extra option for tools/isdbs-bench-tx.sh (repeatable,
#                     e.g. --tx-opt --feed --tx-opt loop)
#   --restart         when a live write does not read back, restart the
#                     broadcast at each level instead (needs --start-tx;
#                     those steps measure acquisition and stability after a
#                     restart, not the hold)
#   --uri URI         libiio URI of the Pluto (default ISDB_PLUTO_URI;
#                     required: iio_attr has no mDNS default here)
#   --from DB --to DB first and last attenuation (45, 89.75)
#   --coarse DB --fine DB --finest DB   step sizes (5, 1, 0.25)
#   --fine-from DB    use --fine from this attenuation on (70)
#   --dwell S         status watch per step, whole seconds (10)
#   --settle S        wait after each write (1)
#   --loss-rounds N   consecutive unlocked rounds that count as a loss (1)
#   --hyst-span DB    how far below the loss the hysteresis walk goes (3)
#   --hyst-dwell S    status watch per hysteresis step (5)
#   --tune-secs S     fresh tune lock limit (10)
#   --stream-secs S   recording per step, 0 = none (5)
#   --keep-ts         keep every recording (default: only those with errors)
#   --exit-att DB     attenuation written on exit while the broadcast runs
#                     (89.75)
#   --pad DB          attenuation added to the 30 dB chain for this pass
#                     (0; 20 for the second pass of §6.1 step 9), recorded
#   --transponder TP --tsid HEX   what the broadcast is (bs1, 4017)
#   --tx2             TX2 (voltage1) instead of TX1
#   --run LABEL       run label prefix in the table (s1)
#   --margin          allow attenuations below 45 dB (an explicit overload
#                     measurement only)
# Environment: DT295 (default ~/dev/PIX-DT295/target/release/dt295, needs
# `status --json`), ISDB (target/release/isdb of this checkout), IIO_ATTR
# (iio_attr), BENCH_TX (tools/isdbs-bench-tx.sh).
# Output: OUTDIR/sweep.csv (one row per step), OUTDIR/sweep.md (the
# VERIFICATION.md table and summary, also printed), per step
# step-<att>-<phase>.status.jsonl / .tune.txt / .stream.txt / .inspect.json
# (/ .ts when not clean), dt295's transfer logs under OUTDIR/captures.
# OUTDIR is outside git (logs); it must not hold a sweep.csv already.
# Exit status: 0 done, 1 stopped by a failure after the start (the exit
# attenuation and the broadcast handled as on success), 2 usage or a
# refused start, 128+N on signal N.
set -euo pipefail
die() { echo "isdbs-level-sweep: $*" >&2; exit 2; }
fail() { echo "isdbs-level-sweep: $*" >&2; exit 1; }
log() { echo "isdbs-level-sweep: $*" >&2; }
usage() { sed -n '/^# Usage:/,/^# Exit status:/s/^# \{0,1\}//p' "$0"; }

HERE=$(cd "$(dirname "$0")/.." && pwd)
CONDUCTED=0
MARGIN=0
URI=${ISDB_PLUTO_URI:-}
TX_PID_FILE=
TX_LOG=
START_TX=
TX_OPTS=()
RESTART=0
FROM=45
TO=89.75
COARSE=5
FINE=1
FINEST=0.25
FINE_FROM=70
DWELL=10
SETTLE=1
LOSS_ROUNDS=1
HYST_SPAN=3
HYST_DWELL=5
TUNE_SECS=10
STREAM_SECS=5
KEEP_TS=0
EXIT_ATT=89.75
PAD=0
TP=bs1
TSID=4017
CHAN=voltage0
RUN=s1
DT295=${DT295:-$HOME/dev/PIX-DT295/target/release/dt295}
ISDB=${ISDB:-$HERE/target/release/isdb}
IIO_ATTR=${IIO_ATTR:-iio_attr}
BENCH_TX=${BENCH_TX:-$HERE/tools/isdbs-bench-tx.sh}
PY=$HERE/tools/isdbs-level-sweep.py

need() { [ $# -ge 2 ] || die "$1 needs a value"; }
while [ $# -gt 0 ]; do
    case $1 in
        --conducted) CONDUCTED=1; shift ;;
        --margin) MARGIN=1; shift ;;
        --uri) need "$@"; URI=$2; shift 2 ;;
        --tx-pid) need "$@"; TX_PID_FILE=$2; shift 2 ;;
        --tx-log) need "$@"; TX_LOG=$2; shift 2 ;;
        --start-tx) need "$@"; START_TX=$2; shift 2 ;;
        --tx-opt) need "$@"; TX_OPTS+=("$2"); shift 2 ;;
        --restart) RESTART=1; shift ;;
        --from) need "$@"; FROM=$2; shift 2 ;;
        --to) need "$@"; TO=$2; shift 2 ;;
        --coarse) need "$@"; COARSE=$2; shift 2 ;;
        --fine) need "$@"; FINE=$2; shift 2 ;;
        --finest) need "$@"; FINEST=$2; shift 2 ;;
        --fine-from) need "$@"; FINE_FROM=$2; shift 2 ;;
        --dwell) need "$@"; DWELL=$2; shift 2 ;;
        --settle) need "$@"; SETTLE=$2; shift 2 ;;
        --loss-rounds) need "$@"; LOSS_ROUNDS=$2; shift 2 ;;
        --hyst-span) need "$@"; HYST_SPAN=$2; shift 2 ;;
        --hyst-dwell) need "$@"; HYST_DWELL=$2; shift 2 ;;
        --tune-secs) need "$@"; TUNE_SECS=$2; shift 2 ;;
        --stream-secs) need "$@"; STREAM_SECS=$2; shift 2 ;;
        --keep-ts) KEEP_TS=1; shift ;;
        --exit-att) need "$@"; EXIT_ATT=$2; shift 2 ;;
        --pad) need "$@"; PAD=$2; shift 2 ;;
        --transponder) need "$@"; TP=$2; shift 2 ;;
        --tsid) need "$@"; TSID=${2#0x}; shift 2 ;;
        --tx2) CHAN=voltage1; shift ;;
        --run) need "$@"; RUN=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
done
[ $# -eq 1 ] || { usage >&2; exit 2; }
OUT=$1

# --- argument checks (nothing is touched before they pass) ---------------
[ "$CONDUCTED" = 1 ] || die "refusing to run without --conducted: confirm the chain TX1 -> >= 30 dB attenuator -> DC block -> PIX-DT295, nothing else on TX1, no antenna"
isnum() { [[ $1 =~ ^[0-9]+(\.[0-9]+)?$ ]]; }
isint() { [[ $1 =~ ^[0-9]+$ ]]; }
for v in FROM TO COARSE FINE FINEST FINE_FROM SETTLE HYST_SPAN EXIT_ATT PAD; do
    isnum "${!v}" || die "--$(tr '[:upper:]_' '[:lower:]-' <<< "$v") ${!v}: expected a number"
done
for v in DWELL LOSS_ROUNDS HYST_DWELL TUNE_SECS STREAM_SECS; do
    isint "${!v}" || die "--$(tr '[:upper:]_' '[:lower:]-' <<< "$v") ${!v}: expected whole seconds / a count"
done
[ "$DWELL" -ge 1 ] && [ "$HYST_DWELL" -ge 1 ] && [ "$TUNE_SECS" -ge 1 ] && [ "$LOSS_ROUNDS" -ge 1 ] ||
    die "--dwell, --hyst-dwell, --tune-secs and --loss-rounds must be at least 1"
[[ $TSID =~ ^[0-9a-fA-F]{1,4}$ ]] || die "--tsid $TSID: expected hex, e.g. 4017"
# Floating point through awk: f 'expression' prints, t 'condition' tests.
f() { awk "BEGIN { printf \"%.2f\", $1 }"; }
t() { awk "BEGIN { exit !($1) }"; }
g() { awk "BEGIN { printf \"%g\", $1 }"; }
# An attenuation the sweep may write: 0.25 dB steps, <= 89.75, >= 45
# unless --margin.
att_ok() {
    t "$1 <= 89.75 && $1 * 4 == int($1 * 4)" || return 1
    [ "$MARGIN" = 1 ] || t "$1 >= 45"
}
for v in FROM TO EXIT_ATT; do
    att_ok "${!v}" || die "--$(tr '[:upper:]_' '[:lower:]-' <<< "$v") ${!v}: an attenuation from 45 (lower only with --margin) to 89.75 dB in 0.25 dB steps"
done
for v in COARSE FINE FINEST; do
    t "${!v} > 0 && ${!v} * 4 == int(${!v} * 4)" || die "--${v,,} ${!v}: a positive multiple of 0.25 dB"
done
t "$FROM <= $TO" || die "--from $FROM is above --to $TO"
t "$FINEST <= $FINE && $FINE <= $COARSE" || die "expected --finest <= --fine <= --coarse"
if [ -n "$TX_PID_FILE" ] && [ -n "$START_TX" ]; then die "--tx-pid and --start-tx exclude each other"; fi
[ -n "$TX_PID_FILE" ] || [ -n "$START_TX" ] || die "give --tx-pid FILE (a broadcast already on air) or --start-tx WORKDIR"
[ "$RESTART" = 0 ] || [ -n "$START_TX" ] || die "--restart needs --start-tx (the sweep must own the broadcast to restart it)"
[ -z "$TX_LOG" ] || [ -n "$TX_PID_FILE" ] || die "--tx-log goes with --tx-pid (with --start-tx the log is OUTDIR/tx.log)"
[ -n "$URI" ] || die "no Pluto URI: pass --uri ip:… or set ISDB_PLUTO_URI (iio_attr needs one)"
for o in "${TX_OPTS[@]}"; do
    case $o in --rf-enable|--dry-run|--margin) die "--tx-opt $o is not allowed (the bench script owns the gate; --margin comes from this script's --margin)" ;; esac
done
[ -x "$DT295" ] || die "dt295 not found at $DT295 (set DT295)"
"$DT295" status --help 2>/dev/null | grep -q -- '--json' ||
    die "$DT295 has no 'status --json' (rebuild ~/dev/PIX-DT295 at d4ad6dc or later)"
command -v "$IIO_ATTR" >/dev/null || die "iio_attr not found (libiio tools; or set IIO_ATTR)"
[ "$STREAM_SECS" = 0 ] || [ -x "$ISDB" ] || die "$ISDB not found (cargo build --release -p isdb-cli, or set ISDB; or --stream-secs 0)"
[ -z "$START_TX" ] || [ -x "$BENCH_TX" ] || die "bench script $BENCH_TX not executable"
[ -r "$PY" ] || die "$PY missing"
if [ -e "$OUT/sweep.csv" ]; then die "$OUT/sweep.csv exists: use a new OUTDIR"; fi
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
CSV=$OUT/sweep.csv

# --- the broadcast --------------------------------------------------------
TX_PID=
TX_OURS=0
CHILD=
WROTE=0
STOPPING=0

# Whether pid $1 is the bench script or an `isdb tx` (its command line).
is_transmitter() {
    local c
    c=$(tr '\0' ' ' < "/proc/$1/cmdline" 2>/dev/null) || return 1
    case $c in
        *isdbs-bench-tx*) return 0 ;;
        *isdb*" tx "*) return 0 ;;
    esac
    return 1
}
tx_alive() { [ -n "$TX_PID" ] && kill -0 "$TX_PID" 2>/dev/null; }
log_size() { if [ -f "$1" ]; then stat -c %s "$1"; else echo 0; fi; }

start_tx() {
    local att m=() off
    att=$(g "$1")
    t "$att < 45" && m=(--margin)
    off=$(log_size "$OUT/tx.log")
    log "starting the broadcast: $BENCH_TX ${TX_OPTS[*]} ${m[*]} $START_TX $att $URI (log $OUT/tx.log)"
    "$BENCH_TX" "${TX_OPTS[@]}" "${m[@]}" "$START_TX" "$att" "$URI" >> "$OUT/tx.log" 2>&1 < /dev/null &
    TX_PID=$!
    TX_OURS=1
    echo "$TX_PID" > "$OUT/tx.pid"
    local i
    for i in $(seq 1200); do
        if tail -c +"$((off + 1))" "$OUT/tx.log" | grep -q "FPGA transmit block running"; then
            log "broadcast running (pid $TX_PID)"
            return 0
        fi
        tx_alive || fail "the broadcast exited before running (see $OUT/tx.log)"
        [ "$STOPPING" = 0 ] || return 1
        [ "$i" -lt 1200 ] && sleep 0.1
    done
    fail "no 'FPGA transmit block running' within 120 s (see $OUT/tx.log)"
}

stop_tx() {
    [ "$TX_OURS" = 1 ] && [ -n "$TX_PID" ] || return 0
    if tx_alive; then
        log "stopping the broadcast (pid $TX_PID)"
        kill -TERM "$TX_PID" 2>/dev/null || true
        local i
        for i in $(seq 300); do
            tx_alive || break
            sleep 0.1
        done
        if tx_alive; then
            log "WARNING: the broadcast (pid $TX_PID, $OUT/tx.pid) is still running 30 s after SIGTERM; stop it by hand"
            return 1
        fi
    fi
    wait "$TX_PID" 2>/dev/null || true
    TX_PID=
    rm -f "$OUT/tx.pid"
}

# --- attenuation through iio_attr ----------------------------------------
# The attenuation now (hardwaregain is its negative), 2 decimals.
att_read() {
    local v
    v=$("$IIO_ATTR" -u "$URI" -o -c ad9361-phy "$CHAN" hardwaregain 2>/dev/null) || return 1
    awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^-?[0-9]+(\.[0-9]+)?$/) { printf "%.2f", -$i; found = 1; exit } }
         END { exit !found }' <<< "$v"
}
att_write() {
    att_ok "$1" || fail "internal: refusing to write attenuation $1"
    if [ -n "$TX_PID" ]; then
        tx_alive || fail "the broadcast (pid $TX_PID) is gone"
        is_transmitter "$TX_PID" || fail "pid $TX_PID is no longer the broadcast"
    fi
    WROTE=1
    att_write_raw "$1"
}
# The write itself, no checks (also used by the exit handler, which must
# not stop half-way).
att_write_raw() {
    # `--` ends the options, so getopt does not read -55 as one.
    "$IIO_ATTR" -u "$URI" -o -c -- ad9361-phy "$CHAN" hardwaregain "-$1" > /dev/null 2>&1 || return 1
}
# Write and read back; ATT_READ is what the radio reports.
ATT_READ=
set_att_live() {
    ATT_READ=
    att_write "$1" || { log "iio_attr write of $1 dB failed"; return 1; }
    ATT_READ=$(att_read) || { log "iio_attr read-back failed"; return 1; }
    if t "($ATT_READ - $1) > 0.125 || ($1 - $ATT_READ) > 0.125"; then
        log "wrote $1 dB, the radio reports $ATT_READ dB: the live write did not take effect"
        return 1
    fi
}

# --- exit -----------------------------------------------------------------
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
    local rc=$?
    set +e
    STOPPING=1
    trap - INT TERM HUP
    if [ -n "$CHILD" ]; then
        kill -TERM "$CHILD" 2>/dev/null
        local i
        for i in $(seq 150); do kill -0 "$CHILD" 2>/dev/null || break; sleep 0.1; done
        kill -0 "$CHILD" 2>/dev/null && kill -KILL "$CHILD" 2>/dev/null
        wait "$CHILD" 2>/dev/null
        CHILD=
        rm -f "$OUT/child.pid"
    fi
    if [ "$WROTE" = 1 ] && tx_alive; then
        local now
        if att_write_raw "$EXIT_ATT" && now=$(att_read) && t "($now - $EXIT_ATT)^2 <= 0.125^2"; then
            log "exit: attenuation $EXIT_ATT dB written, the radio reports $now dB"
        elif [ "$TX_OURS" = 1 ]; then
            log "exit: the attenuation write of $EXIT_ATT dB did not take effect (${now:-no read-back}); stopping the broadcast"
        else
            log "WARNING: exit: the attenuation write of $EXIT_ATT dB did not take effect (${now:-no read-back}); the broadcast (pid $TX_PID) is still on air: stop it"
        fi
    fi
    stop_tx
    if [ -s "$CSV" ]; then
        python3 -I "$PY" table "$CSV" --md "$OUT/sweep.md" --stream-secs "$STREAM_SECS" ||
            log "table failed"
        log "results: $CSV, $OUT/sweep.md"
    fi
    exit "$rc"
}
# shellcheck disable=SC2329 # invoked by the signal traps
on_signal() {
    log "signal: stopping"
    exit "$1"
}
trap cleanup EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM
trap 'on_signal 129' HUP

# A tool run in the background so a signal is handled at once (the trap
# stops it); its pid is in OUTDIR/child.pid while it runs.
run_child() {
    "$@" &
    CHILD=$!
    echo "$CHILD" > "$OUT/child.pid"
    local rc=0
    wait "$CHILD" || rc=$?
    CHILD=
    rm -f "$OUT/child.pid"
    return "$rc"
}
DT_LOG=(--log-dir "$OUT/captures")
dt_status() { run_child "$DT295" status --isdbs --watch "$2" --json --read-reg bc:2 "${DT_LOG[@]}" > "$1"; }
dt_tune() { run_child "$DT295" tune --isdbs "$TP" --tsid "$TSID" --lock-seconds "$TUNE_SECS" "${DT_LOG[@]}" > "$1" 2>&1; }
dt_stream() {
    run_child "$DT295" stream --isdbs "$TP" --tsid "$TSID" --raw-config --out "$1" \
        --seconds "$STREAM_SECS" "${DT_LOG[@]}" > "$2" 2>&1
}

# Values of a `KEY VALUE` report into R_KEY.
read_kv() {
    local k v
    while read -r k v; do
        [[ $k =~ ^[A-Z_]+$ ]] || continue
        printf -v "R_$k" '%s' "$v"
    done < "$1"
}

# --- one step -------------------------------------------------------------
MODE=live
TX_OFF=0
# Sets the level: a live write read back, or (--restart, after a write that
# did not take effect) the broadcast restarted at it. ATT_READ: the read-back.
set_level() {
    local a=$1
    if [ "$MODE" = live ]; then
        if set_att_live "$a"; then
            return 0
        fi
        [ "$RESTART" = 1 ] || fail "live attenuation write of $a dB did not take effect (no --restart): stopping"
        log "falling back to restarting the broadcast at each level (--restart)"
        MODE=restart
    fi
    stop_tx || fail "could not stop the broadcast for a restart"
    start_tx "$a"
    WROTE=1
    ATT_READ=$(att_read || true)
}
txlog_path() { if [ "$TX_OURS" = 1 ]; then echo "$OUT/tx.log"; else echo "$TX_LOG"; fi; }
mark_tx() { local p; p=$(txlog_path); TX_OFF=$( [ -n "$p" ] && log_size "$p" || echo 0); }
slice_tx() {
    local p
    p=$(txlog_path)
    if [ -n "$p" ] && [ -f "$p" ]; then tail -c +"$((TX_OFF + 1))" "$p" > "$1"; else : > "$1"; fi
}

# Fresh tune (and the recording when it locks) into files under $1.
ACQ_TUNE_RC=
ACQ_STREAM_RC=
acquire() {
    local b=$1
    ACQ_TUNE_RC=0
    dt_tune "$b.tune.txt" || ACQ_TUNE_RC=$?
    grep -q '^lock: ' "$b.tune.txt" || fail "dt295 tune failed without a lock result (see $b.tune.txt)"
    ACQ_STREAM_RC=
    if [ "$ACQ_TUNE_RC" = 0 ] && [ "$STREAM_SECS" -gt 0 ]; then
        ACQ_STREAM_RC=0
        rm -f "$b.ts"
        dt_stream "$b.ts" "$b.stream.txt" || ACQ_STREAM_RC=$?
        if [ -s "$b.ts" ]; then
            run_child "$ISDB" ts inspect --json "$b.ts" > "$b.inspect.json" 2> "$b.inspect.err" ||
                log "isdb ts inspect failed on $b.ts"
        fi
    fi
}

# Appends the CSV row of a step; R_* from the helper.
row() {
    local phase=$1 a=$2 b=$3 note=${4:-}
    local args=(--csv "$CSV" --phase "$phase" --mode "$MODE" --att-set "$a" --att-read "$ATT_READ"
        --pad "$PAD" --utc "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --run "$RUN-$(g "$a")${5:-}"
        --note "$note" --loss-rounds "$LOSS_ROUNDS" --tx "$b.tx.txt")
    [ -f "$b.status.jsonl" ] && args+=(--status "$b.status.jsonl")
    [ -f "$b.tune.txt" ] && args+=(--tune "$b.tune.txt" --tune-rc "$ACQ_TUNE_RC")
    [ -n "$ACQ_STREAM_RC" ] && args+=(--stream-rc "$ACQ_STREAM_RC" --ts "$b.ts" --inspect "$b.inspect.json")
    python3 -I "$PY" step "${args[@]}" > "$b.kv" || fail "helper failed on $b"
    read_kv "$b.kv"
    if [ -f "$b.ts" ] && [ "$KEEP_TS" = 0 ] && [ "${R_TS_CLEAN:-0}" = 1 ]; then rm -f "$b.ts"; fi
}

base() { echo "$OUT/step-$(f "$1")-$2"; }

# Re-acquire at attenuation $1 (live mode) so the next hold starts locked.
relock() {
    local a=$1 b
    b=$(base "$a" relock)
    log "re-locking the tuner at $a dB"
    set_level "$a"
    sleep "$SETTLE"
    mark_tx
    local keep=$STREAM_SECS
    STREAM_SECS=0
    acquire "$b"
    STREAM_SECS=$keep
    slice_tx "$b.tx.txt"
    row relock "$a" "$b" "re-lock before the next hold step" "-r"
    [ "$ACQ_TUNE_RC" = 0 ]
}

# Hysteresis walk from the loss point $1 (live mode, no retune).
hysteresis() {
    local loss=$1 a b low
    low=$(f "$loss - $HYST_SPAN")
    t "$low < $FROM" && low=$FROM
    a=$(f "$loss - $FINEST")
    log "hysteresis: lowering the attenuation from $loss to $low dB without a retune"
    while t "$a >= $low"; do
        b=$(base "$a" hyst)
        set_level "$a"
        sleep "$SETTLE"
        mark_tx
        dt_status "$b.status.jsonl" "$HYST_DWELL" || fail "dt295 status failed (see $b.status.jsonl)"
        slice_tx "$b.tx.txt"
        ACQ_TUNE_RC=
        ACQ_STREAM_RC=
        rm -f "$b.tune.txt"
        row hyst "$a" "$b" "after the loss at $loss dB, no retune" "-h"
        if [ "$R_LOCKED_ALL" = 1 ]; then
            log "hysteresis: locked again by itself at $a dB"
            return 0
        fi
        a=$(f "$a - $FINEST")
    done
    log "hysteresis: no re-lock without a retune down to $low dB"
}

# --- start ----------------------------------------------------------------
cat >&2 <<EOF
isdbs-level-sweep: conducted bench (acknowledged with --conducted):
  TX$((${CHAN#voltage} + 1)) of the Pluto at $URI -> 30 dB attenuator (+$PAD dB this pass) -> DC block -> PIX-DT295
  attenuation $FROM .. $TO dB (exit: $EXIT_ATT dB), transponder $TP TS_ID 0x$TSID
EOF
{
    echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "isdb=$ISDB ($("$ISDB" --version 2>/dev/null || echo '?'))"
    echo "dt295=$DT295"
    echo "uri=$URI channel=$CHAN transponder=$TP tsid=0x$TSID pad_db=$PAD"
    echo "from=$FROM to=$TO coarse=$COARSE fine=$FINE finest=$FINEST fine_from=$FINE_FROM"
    echo "dwell=$DWELL settle=$SETTLE loss_rounds=$LOSS_ROUNDS hyst_span=$HYST_SPAN hyst_dwell=$HYST_DWELL"
    echo "tune_secs=$TUNE_SECS stream_secs=$STREAM_SECS exit_att=$EXIT_ATT restart=$RESTART"
    echo "tx=$( [ -n "$START_TX" ] && echo "started by the sweep: $BENCH_TX ${TX_OPTS[*]} $START_TX" || echo "operator, pid file $TX_PID_FILE")"
} > "$OUT/sweep.meta"

if [ -n "$TX_PID_FILE" ]; then
    [ -r "$TX_PID_FILE" ] || die "--tx-pid $TX_PID_FILE: not readable"
    TX_PID=$(tr -dc '0-9' < "$TX_PID_FILE")
    [ -n "$TX_PID" ] || die "--tx-pid $TX_PID_FILE: no pid in it"
    kill -0 "$TX_PID" 2>/dev/null || die "the broadcast (pid $TX_PID from $TX_PID_FILE) is not running"
    is_transmitter "$TX_PID" || die "pid $TX_PID is neither tools/isdbs-bench-tx.sh nor an isdb tx"
    log "broadcast on air: pid $TX_PID"
else
    start_tx "$FROM"
fi
NOW=$(att_read) || fail "cannot read ad9361-phy $CHAN hardwaregain through iio_attr at $URI"
log "TX attenuation now $NOW dB"
"$DT295" status --isdbs --json "${DT_LOG[@]}" > "$OUT/preflight.status.jsonl" 2> "$OUT/preflight.status.err" ||
    fail "dt295 status failed (see $OUT/preflight.status.err)"
grep -q '"running":true' "$OUT/preflight.status.jsonl" ||
    fail "the tuner's firmware is not running ($(head -1 "$OUT/preflight.status.jsonl")): power-cycle and dt295 init first"

# Lock at the start level with a fresh tune.
set_level "$FROM"
sleep "$SETTLE"
B=$(base "$FROM" start)
dt_tune "$B.tune.txt" || fail "no lock at the start level $FROM dB (see $B.tune.txt): check the chain and the broadcast"
log "locked at $FROM dB"

ATT=$FROM
STEP=$COARSE
LAST_HOLD=
LAST_ACQ=$FROM
LOSS=
while :; do
    B=$(base "$ATT" hold)
    set_level "$ATT"
    sleep "$SETTLE"
    mark_tx
    ACQ_TUNE_RC=
    ACQ_STREAM_RC=
    if [ "$MODE" = restart ]; then
        # A restart drops the lock: acquisition first, then the stability.
        acquire "$B"
        dt_status "$B.status.jsonl" "$DWELL" || fail "dt295 status failed (see $B.status.jsonl)"
        slice_tx "$B.tx.txt"
        row restart "$ATT" "$B" "broadcast restarted at this level"
        LOST=$R_LOST
        [ "$R_TUNE_LOCKED" = 1 ] || LOST=1
    else
        dt_status "$B.status.jsonl" "$DWELL" || fail "dt295 status failed (see $B.status.jsonl)"
        python3 -I "$PY" peek --status "$B.status.jsonl" --loss-rounds "$LOSS_ROUNDS" > "$B.peek"
        read_kv "$B.peek"
        [ "$R_ROUNDS" -gt 0 ] || fail "dt295 status gave no rounds (see $B.status.jsonl)"
        LOST=$R_LOST
        FINAL=0
        if [ "$LOST" = 1 ] && { t "$STEP <= $FINEST" || [ -z "$LAST_HOLD" ]; }; then FINAL=1; fi
        # The re-lock by itself is looked for before anything retunes.
        if [ "$FINAL" = 1 ]; then
            hysteresis "$ATT"
            set_level "$ATT"
            sleep "$SETTLE"
        fi
        acquire "$B"
        slice_tx "$B.tx.txt"
        row hold "$ATT" "$B" "$( [ "$LOST" = 1 ] && echo "hold lost" || true)"
    fi
    log "att $ATT: every round locked ${R_LOCKED_ALL:-?}, hold lost $LOST, fresh tune locked ${R_TUNE_LOCKED:-?}, TS clean ${R_TS_CLEAN:-?}"
    if [ "$LOST" = 1 ]; then
        if t "$STEP > $FINEST" && [ -n "$LAST_HOLD" ]; then
            log "lost at $ATT dB: back to $LAST_HOLD dB, then $FINEST dB steps"
            STEP=$FINEST
            if [ "$MODE" = live ]; then relock "$LAST_HOLD" || fail "cannot re-lock at $LAST_HOLD dB"; fi
            ATT=$(f "$LAST_HOLD + $STEP")
            continue
        fi
        LOSS=$ATT
        break
    fi
    LAST_HOLD=$ATT
    [ "$R_TUNE_LOCKED" = 1 ] && LAST_ACQ=$ATT
    if t "$STEP == $COARSE && $STEP > $FINE"; then
        if t "$ATT >= $FINE_FROM" || [ "$R_CN_LEFT_FLOOR" = 1 ] || [ "$R_ERR_ANY" = 1 ]; then
            log "from $ATT dB on: $FINE dB steps"
            STEP=$FINE
        fi
    fi
    # The next hold starts locked: re-acquire where a tune last locked.
    if [ "$MODE" = live ] && [ "$R_TUNE_LOCKED" != 1 ]; then
        relock "$LAST_ACQ" || fail "cannot re-lock at $LAST_ACQ dB"
    fi
    t "$ATT >= $TO" && break
    ATT=$(f "$ATT + $STEP")
    t "$ATT > $TO" && ATT=$(f "$TO")
done
if [ -n "$LOSS" ]; then
    log "sweep done: hold lost at $LOSS dB (last held: ${LAST_HOLD:-none})"
else
    log "sweep done: held up to $ATT dB (--to)"
fi
exit 0
