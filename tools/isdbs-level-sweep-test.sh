#!/usr/bin/env bash
# The checks are strings eval'd by check(), RC included.
# shellcheck disable=SC2016,SC2034
# Self-test of tools/isdbs-level-sweep.sh with stubs in place of the real
# tools: `dt295` (status --json / tune / stream from a simulated tuner whose
# lock depends on the attenuation), `iio_attr` (keeps the attenuation in a
# file), `isdb` (ts inspect) and tools/isdbs-bench-tx.sh (BENCH_TX: a
# "broadcast" that prints the FPGA running line and MISSED status lines
# until TERM). No device is opened, nothing is built or transmitted.
#
# Checks: the gates (--conducted, attenuation >= 45 unless --margin,
# --restart only with --start-tx, a --tx-pid that is not a transmitter,
# --tx-opt --rf-enable refused), the sweep's path on a simulated tuner
# (coarse -> fine steps when the raw C/N leaves its floor, back to the last
# held level and 0.25 dB steps after the first loss, re-lock after a failed
# fresh tune, the hysteresis walk without a retune, the loss point), the
# CSV and the Markdown table, the exit attenuation 89.75 written with an
# operator's broadcast left running, a broadcast started by the sweep
# stopped on every exit path (normal, failure, SIGTERM), a live write that
# does not read back (stop, or --restart: one start per level), and that
# no command line ever carries --rf-enable.
#
# Usage: tools/isdbs-level-sweep-test.sh   (exits 0 on PASS)
# Scratch under ISDBS_SWEEP_TEST_DIR (default target/ of this checkout,
# not /tmp), removed at the end unless ISDBS_SWEEP_TEST_KEEP=1.
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT=${SWEEP:-$HERE/tools/isdbs-level-sweep.sh}  # SWEEP: test another copy
BASE=${ISDBS_SWEEP_TEST_DIR:-$HERE/target}
mkdir -p "$BASE"
T=$(mktemp -d -p "$BASE" level-sweep-test.XXXXXX)
OPTX=
cleanup() {
    if [ -n "$OPTX" ]; then kill -TERM "$OPTX" 2>/dev/null || true; wait "$OPTX" 2>/dev/null || true; fi
    if [ "${ISDBS_SWEEP_TEST_KEEP:-0}" = 1 ]; then echo "kept $T"; else rm -rf "$T"; fi
}
trap cleanup EXIT
S=$T/stub
mkdir -p "$S/bin"
export STUB=$S

# --- stubs ----------------------------------------------------------------
# State: $S/att (the TX attenuation), $S/locked (the simulated tuner), every
# call appended to $S/log. Thresholds (dB): STUB_HOLD (a locked tuner loses
# lock at or above it), STUB_ACQ (a fresh tune fails at or above it),
# STUB_RELOCK (an unlocked tuner re-locks by itself at or below it),
# STUB_TSERR (TEI in the recording at or above it), STUB_ERR (error
# counters move at or above it). STUB_IIO=ok|ignore (writes take effect or
# not). STUB_SLOW=1: status sleeps (for the signal test).
cat > "$S/bin/iio_attr" <<'SH'
#!/usr/bin/env bash
pos=()
for a in "$@"; do case $a in -u|-q|-o|-c|--) ;; *) pos+=("$a") ;; esac; done
# pos: [uri] ad9361-phy CHANNEL hardwaregain [VALUE]
i=0; [ "${pos[0]}" = ad9361-phy ] || i=1
chan=${pos[$((i + 1))]} val=${pos[$((i + 3))]:-}
if [ -n "$val" ]; then
    echo "iio_attr write $chan $val" >> "$STUB/log"
    if [ "${STUB_IIO:-ok}" = ok ]; then echo "${val#-}" > "$STUB/att"; fi
else
    printf -- '-%s dB\n' "$(cat "$STUB/att")"
fi
SH
cat > "$S/bin/dt295" <<'SH'
#!/usr/bin/env bash
att=$(cat "$STUB/att")
echo "dt295 $* (att $att)" >> "$STUB/log"
ge() { awk -v a="$att" -v b="$1" 'BEGIN { exit !(a >= b) }'; }
le() { awk -v a="$att" -v b="$1" 'BEGIN { exit !(a <= b) }'; }
locked=$(cat "$STUB/locked" 2>/dev/null || echo 0)
cn=$(awk -v a="$att" 'BEGIN { x = a - 52; if (x < 0) x = 0; printf "%d", 2930 + 60 * x }')
cmd=$1; shift
watch=0; out=
while [ $# -gt 0 ]; do
    case $1 in --watch) watch=$2; shift ;; --out) out=$2; shift ;; --help) echo "  --json  machine-readable"; exit 0 ;; esac
    shift
done
case $cmd in
    status)
        echo '{"type":"state","state":"0011","state_name":"Ready","stale_irq":0,"running":true,"side":"s"}'
        if [ "${STUB_SLOW:-0}" = 1 ] && [ "$watch" -gt 0 ]; then echo $$ > "$STUB/slow.pid"; sleep 30; fi
        if [ "$locked" = 1 ] && ge "$STUB_HOLD"; then locked=0; fi
        if [ "$locked" = 0 ] && le "$STUB_RELOCK"; then locked=1; fi
        echo "$locked" > "$STUB/locked"
        n=1; [ "$watch" -gt 0 ] && n=${STUB_ROUNDS:-4}
        e=0; ge "$STUB_ERR" && e=12
        for r in $(seq "$n"); do
            if [ "$locked" = 1 ]; then
                db=null below=true
                [ "$cn" -ge 3000 ] && { db=$(awk -v c="$cn" 'BEGIN { printf "%.1f", 60 - (c - 3000) / 30 }'); below=false; }
                printf '{"type":"round","side":"s","t_ms":%d,"unix_ms":1,"lock":"locked","locked":true,"c3":0,"ba":99,"cn_raw":%d,"cn_db":%s,"cn_below_formula":%s,"errors":[%d,0],"periods":[256,256],"ber":[%s,0.0],"extra":{"bc":"%04x"}}\n' \
                    $((r * 110)) "$cn" "$db" "$below" "$e" "$(awk -v e="$e" 'BEGIN { printf "%.6g", e / (256 * 1632) }')" "$cn"
            else
                printf '{"type":"round","side":"s","t_ms":%d,"unix_ms":1,"lock":"searching","locked":false,"c3":80,"ba":216,"cn_raw":null,"cn_db":null,"cn_below_formula":false,"errors":null,"periods":null,"ber":null,"extra":{"bc":"%04x"}}\n' \
                    $((r * 110)) $((cn + 900))
            fi
        done
        ;;
    tune|stream)
        if ge "$STUB_ACQ"; then locked=0; else locked=1; fi
        echo "$locked" > "$STUB/locked"
        if [ "$cmd" = stream ]; then
            [ "$locked" = 1 ] || { echo "dt295: no lock" >&2; exit 1; }
            head -c $((188 * 10)) /dev/zero > "$out"
            exit 0
        fi
        echo "tuned ISDB-S BS1 (IF 1049480 kHz) TS-ID 0x4017; lock poll up to 10 s:"
        echo "  +    6.0 ms  c3=50 ba=d8  Searching"
        if [ "$locked" = 1 ]; then
            echo "  +  121.0 ms  c3=00 ba=58  Locked  bc=0b70 (raw 2928, below the C/N formula's 3000)  err 000000/000000"
            echo "lock: Locked"
        else
            echo "  +  116.0 ms  c3=50 ba=d8  Searching"
            echo "lock: Searching"
            echo "dt295: no lock within 10 s" >&2
            exit 1
        fi
        ;;
esac
SH
cat > "$S/bin/isdb" <<'SH'
#!/usr/bin/env bash
echo "isdb $*" >> "$STUB/log"
[ "$1" = --version ] && { echo "isdb stub"; exit 0; }
att=$(cat "$STUB/att")
tei=0
awk -v a="$att" -v b="$STUB_TSERR" 'BEGIN { exit !(a >= b) }' && tei=3
echo "{\"packets\":10,\"sync_losses\":0,\"pids\":[{\"pid\":0,\"tei\":$tei}],\"cc\":[{\"kind\":\"duplicate\",\"pid\":0,\"count\":1}]}"
SH
# The broadcast: as tools/isdbs-bench-tx.sh WORK ATT URI, sets the
# attenuation as isdb tx does at open and 89.75 at close.
cat > "$S/bin/isdbs-bench-tx.sh" <<'SH'
#!/usr/bin/env bash
echo "bench-tx $*" >> "$STUB/log"
echo "$*" >> "$STUB/tx-args"
# [options] WORKDIR ATT URI
echo "${@: -2:1}" > "$STUB/att"
trap 'echo 89.75 > "$STUB/att"; echo "bench-tx stopped" >> "$STUB/log"; exit 0' TERM
echo "INFO isdb_radio: FPGA transmit block running (CTRL RUN|SELECT) with 2 of the device's 4 buffers queued" >&2
while :; do
    echo "   0.05 s on air, 4 superframes | TX block MISSED 0" >&2
    sleep 0.05
done
SH
chmod +x "$S/bin/"*
export PATH="$S/bin:$PATH"
unset DT295  # exercise the default dt295 lookup on PATH
export IIO_ATTR=$S/bin/iio_attr ISDB=$S/bin/isdb BENCH_TX=$S/bin/isdbs-bench-tx.sh
export STUB_HOLD=61 STUB_ACQ=60.5 STUB_RELOCK=59.5 STUB_TSERR=60 STUB_ERR=58 STUB_IIO=ok STUB_SLOW=0
unset ISDB_PLUTO_URI

FAIL=0
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; FAIL=1; fi; }
reset() { : > "$S/log"; : > "$S/tx-args"; echo "${1:-89.75}" > "$S/att"; echo 0 > "$S/locked"; }
N=0
run() {
    N=$((N + 1))
    OUT=$T/out$N
    set +e
    timeout 120 "$SCRIPT" "$@" "$OUT" > "$T/stdout" 2> "$T/stderr"
    RC=$?
    set -e
}
FAST=(--uri ip:192.0.2.1 --settle 0 --dwell 1 --hyst-dwell 1 --tune-secs 1 --stream-secs 1)
# An operator's broadcast: the stub, started here, pid in a file.
start_optx() {
    "$BENCH_TX" "$T/work" 45 ip:192.0.2.1 2> "$T/optx.log" &
    OPTX=$!
    echo "$OPTX" > "$T/optx.pid"
    sleep 0.3
}

# --- gates ----------------------------------------------------------------
reset
run --tx-pid /dev/null "${FAST[@]}"
check "refused without --conducted, nothing called" '[ $RC = 2 ] && grep -q -- "--conducted" "$T/stderr" && [ ! -s "$S/log" ]'
run --conducted --start-tx "$T/work" --from 44 "${FAST[@]}"
check "--from 44 refused without --margin" '[ $RC = 2 ] && [ ! -s "$S/log" ]'
run --conducted --tx-pid "$T/none" --restart "${FAST[@]}"
check "--restart refused without --start-tx" '[ $RC = 2 ] && grep -q "needs --start-tx" "$T/stderr"'
run --conducted --start-tx "$T/work" --tx-opt --rf-enable "${FAST[@]}"
check "--tx-opt --rf-enable refused" '[ $RC = 2 ] && [ ! -s "$S/log" ]'
run --conducted --start-tx "$T/work" --settle 0
check "no URI refused" '[ $RC = 2 ] && grep -q "no Pluto URI" "$T/stderr"'
sleep 30 &
NOTX=$!
echo "$NOTX" > "$T/notx.pid"
run --conducted --tx-pid "$T/notx.pid" "${FAST[@]}"
kill "$NOTX" 2>/dev/null || true; wait "$NOTX" 2>/dev/null || true
check "a --tx-pid that is not a transmitter refused, nothing written" \
    '[ $RC = 2 ] && grep -q "neither" "$T/stderr" && ! grep -q "iio_attr write" "$S/log"'

# --- operator's broadcast: the full path -----------------------------------
reset
start_optx
run --conducted --tx-pid "$T/optx.pid" "${FAST[@]}"
CSV=$OUT/sweep.csv
holds() { awk -F, 'NR > 1 && $2 == "hold" { printf "%s ", $4 }' "$CSV"; }
check "operator: the sweep ran (exit 0)" '[ $RC = 0 ]'
check "operator: hold steps 45 50 55 (raw C/N off its floor: 1 dB) … 61, then 0.25 dB from 60" \
    '[ "$(holds)" = "45 50 55 56 57 58 59 60 61 60.25 60.5 60.75 61 " ]'
check "operator: a re-lock at the last held level after the first loss, and after failed fresh tunes" \
    '[ "$(awk -F, '\''NR > 1 && $2 == "relock" { printf "%s ", $4 }'\'' "$CSV")" = "60 60.25 60.25 " ]'
check "operator: hysteresis walk 60.75 … 59.5 without a retune, ends when locked again" \
    '[ "$(awk -F, '\''NR > 1 && $2 == "hyst" { printf "%s ", $4 }'\'' "$CSV")" = "60.75 60.5 60.25 60 59.75 59.5 " ] && grep -q "^dt295 status .*(att 59.75)" "$S/log" && ! grep -q "^dt295 tune .*(att 59.75)" "$S/log"'
check "operator: exit attenuation 89.75 written, the operator's broadcast left running" \
    '[ "$(cat "$S/att")" = 89.75 ] && kill -0 "$OPTX" && grep -q "exit: attenuation 89.75 dB written" "$T/stderr"'
check "operator: no write below 45 or above 89.75, read back after each" \
    '! grep "iio_attr write" "$S/log" | awk '\''{ v = -$4; if (v < 45 || v > 89.75) bad = 1 } END { exit !bad }'\'''
check "operator: status --json with the raw C/N register every round" \
    'grep -q "^dt295 status --isdbs --watch 1 --json --read-reg bc:2" "$S/log"'
check "operator: recordings kept only when not clean (TEI from 60)" \
    '[ -e "$OUT/step-60.00-hold.ts" ] && [ ! -e "$OUT/step-55.00-hold.ts" ]'
MD=$OUT/sweep.md
check "table: the §6.3 header and a row per hold step" \
    'grep -q "^| TX att | Level vs. att 45 | Est. tuner input | Hold (c3 = 00 rounds) | Fresh tune: lock in | 0xBC raw mean (min…max) | C/N (tc90522 formula) | 0xBA | Errors 0xEB / 0xF0 (max) | TS 1 s: sync / TEI / CC | Run |" "$MD" && [ "$(grep -c "^| [0-9]" "$MD")" -ge 19 ]'
check "table: att 45 above the formula's range, 61 lost, 60.5 fresh tune fails" \
    'grep -q "^| 45 | 0 dB | −78…−68 dBm | 4/4 | 121 ms | 2930 (2930…2930) | above range | 63 | 0 / 0 | 0 / 0 / 0 | s1-45 |" "$MD" && grep -q "^| 61 | .*\*\*0/4 lost\*\*" "$MD" && grep -q "^| 60.5 | .*| \*\*no\*\* |" "$MD"'
check "summary: hold, acquisition, hysteresis, onsets, fit" \
    'grep -q "Hold threshold: last held att 60.75 (−15.75 dB vs. att 45); first loss att 61" "$MD" && grep -q "Acquisition threshold (fresh tune): last lock att 60.25" "$MD" && grep -q "Hysteresis: re-locked without a retune at att 59.5" "$MD" && grep -q "Error-counter onset (0xEB / 0xF0 > 0 in a locked round): att 58" "$MD" && grep -q "TS-error onset (sync / TEI / CC > 0): att 60 " "$MD" && grep -q "Raw 0xBC vs. att (held steps with raw > 3000, att ≤ 80): +60.0 per dB (raw = −190 + 60.0·att, r² 1.000, 9 steps)" "$MD" && grep -q "C/N vs. att: −2.00 dB per dB" "$MD"'
kill -TERM "$OPTX"; wait "$OPTX" 2>/dev/null || true; OPTX=

# --- --margin -------------------------------------------------------------
reset
start_optx
run --conducted --margin --tx-pid "$T/optx.pid" --from 44 --to 44 "${FAST[@]}"
check "--margin: 44 written" '[ $RC = 0 ] && grep -q "iio_attr write voltage0 -44$" "$S/log"'
kill -TERM "$OPTX"; wait "$OPTX" 2>/dev/null || true; OPTX=

# --- the sweep starts the broadcast ---------------------------------------
reset
export STUB_HOLD=99 STUB_ACQ=99
run --conducted --start-tx "$T/work" --to 50 --tx-opt --feed --tx-opt loop "${FAST[@]}"
check "start-tx: the bench script started once as WORK 45 URI with the passed options" \
    '[ $RC = 0 ] && [ "$(cat "$S/tx-args")" = "--feed loop $T/work 45 ip:192.0.2.1" ]'
check "start-tx: stopped at the end, pid file removed" \
    'grep -q "bench-tx stopped" "$S/log" && [ ! -e "$OUT/tx.pid" ] && [ "$(cat "$S/att")" = 89.75 ]'
check "start-tx: MISSED from its log in the CSV" 'awk -F, '\''NR == 1 { for (i = 1; i <= NF; i++) if ($i == "tx_missed") c = i } NR > 1 && $c == "0" { ok = 1 } END { exit !ok }'\'' "$OUT/sweep.csv"'
check "nowhere --rf-enable" '! grep -q -- "--rf-enable" "$S/log"'

# A live write that does not read back: stop, or restart per level.
reset
export STUB_IIO=ignore
run --conducted --start-tx "$T/work" --to 50 "${FAST[@]}"
check "ignored live write without --restart: exit 1, broadcast stopped" \
    '[ $RC = 1 ] && grep -q "did not take effect" "$T/stderr" && grep -q "bench-tx stopped" "$S/log" && [ ! -e "$OUT/tx.pid" ]'
reset
run --conducted --start-tx "$T/work" --to 55 --restart "${FAST[@]}"
# 45 reads back (the broadcast opened at 45), 50 does not: from there one
# broadcast per level.
check "--restart: from the first ignored write one broadcast per level (45 50 55), rows marked restart" \
    '[ $RC = 0 ] && [ "$(awk '\''{ printf "%s ", $2 }'\'' "$S/tx-args")" = "45 50 55 " ] && [ "$(awk -F, '\''NR > 1 && $2 == "restart" { n++ } END { print n }'\'' "$OUT/sweep.csv")" = 2 ] && grep -q "(restart)" "$OUT/sweep.md"'
check "--restart: each stopped, the last at the end" '[ "$(grep -c "bench-tx stopped" "$S/log")" = 3 ] && [ ! -e "$OUT/tx.pid" ]'
export STUB_IIO=ok

# SIGTERM in the middle of a status watch.
reset
export STUB_SLOW=1
rm -f "$S/slow.pid"
"$SCRIPT" --conducted --start-tx "$T/work" "${FAST[@]}" "$T/sig" > "$T/stdout" 2> "$T/stderr" &
SW=$!
for _ in $(seq 200); do [ -s "$S/slow.pid" ] && break; sleep 0.05; done
kill -TERM "$SW"
for _ in $(seq 200); do kill -0 "$SW" 2>/dev/null || break; sleep 0.05; done
set +e; wait "$SW"; RC=$?; set -e
SLOW=$(cat "$S/slow.pid" 2>/dev/null || echo 0)
check "SIGTERM: exit 143, the dt295 child stopped, exit attenuation, broadcast stopped" \
    '[ $RC = 143 ] && ! kill -0 "$SLOW" 2>/dev/null && [ ! -e "$T/sig/child.pid" ] && grep -q "bench-tx stopped" "$S/log" && [ ! -e "$T/sig/tx.pid" ] && [ "$(cat "$S/att")" = 89.75 ]'
export STUB_SLOW=0

check "the helper compiles" \
    'python3 -I -c "import py_compile, sys; py_compile.compile(sys.argv[1], cfile=sys.argv[2], doraise=True)" "$HERE/tools/isdbs-level-sweep.py" "$T/helper.pyc"'
if [ "$FAIL" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi
