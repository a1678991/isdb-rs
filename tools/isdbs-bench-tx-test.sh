#!/usr/bin/env bash
# The checks are strings eval'd by check(), RC included.
# shellcheck disable=SC2016,SC2034
# Self-test of tools/isdbs-bench-tx.sh with a dummy `isdb` in place of the
# real one (ISDB=…): no radio is opened, nothing is built or transmitted.
# Checks the argument gate (attenuation ≥ 45 unless --margin), the command
# line of each feed mode, that isdb's exit status is propagated, that
# INT/TERM reach isdb, and that no `cat`/`pv` of the FIFO pacer survives
# any exit path (the `while :; do cat …` loop and `exec` that dropped the
# EXIT trap, docs/plans/isdbs-next/media-and-margin.md §1).
#
# Usage: tools/isdbs-bench-tx-test.sh     (needs pv and setsid; exits 0 on PASS)
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
SCRIPT=${BENCH_TX:-$HERE/tools/isdbs-bench-tx.sh}  # BENCH_TX: test another copy
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
WORK=$T/work
mkdir -p "$WORK"
# A small stand-in for bench60.ts (the script then skips `isdb mux`).
head -c $((188 * 4000)) /dev/zero > "$WORK/bench60.ts"

# The dummy: logs its arguments; DUMMY=exit:N exits N at once,
# DUMMY=read:N reads 4 KiB of its --ts input then exits N, DUMMY=wait runs
# until a TERM/INT and exits 7.
cat > "$T/isdb" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$DUMMY_LOG"
input=
for a in "$@"; do case $a in 0x4010=*) input=${a#0x4010=} ;; esac; done
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
# Processes still running in WORK (the pacer's cat/pv/bash, the dummy).
leftovers() {
    local n=0 p
    for p in /proc/[0-9]*; do
        [ "$(readlink "$p/cwd" 2>/dev/null)" = "$WORK" ] && n=$((n + 1))
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
check "loop: --feed loop on bench60.ts, --rf-enable, att, uri" \
    'grep -q -- "--rf-enable --gain att=45 --ts 0x4010=$WORK/bench60.ts --feed loop" "$DUMMY_LOG" && grep -q "pluto:uri=ip:192.0.2.1" "$DUMMY_LOG"'
check "loop: no FIFO" '[ ! -e "$WORK/ts.fifo" ]'

DUMMY=exit:0 run --dry-run "$WORK"
check "--dry-run replaces --rf-enable" 'grep -q -- "--dry-run" "$DUMMY_LOG" && ! grep -q -- "--rf-enable" "$DUMMY_LOG"'

DUMMY=read:0 run "$WORK"
check "fifo: isdb read the FIFO and exited 0" '[ $RC = 0 ] && grep -q -- "--feed live" "$DUMMY_LOG"'
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
for sig in TERM INT; do
    : > "$DUMMY_LOG"
    DUMMY="wait" sigdfl "$SCRIPT" "$WORK" > "$T/out" 2>&1 &
    S=$!
    for _ in $(seq 100); do grep -q tx "$DUMMY_LOG" 2>/dev/null && break; sleep 0.05; done
    sleep 0.3
    kill -s "$sig" "$S"
    for _ in $(seq 200); do kill -0 "$S" 2>/dev/null || break; sleep 0.05; done
    if kill -0 "$S" 2>/dev/null; then
        echo "FAIL fifo: the script did not end within 10 s of $sig"
        kill -KILL "$S"
        FAIL=1
    fi
    set +e; wait "$S"; RC=$?; set -e
    check "fifo: $sig reaches isdb, its status 7 propagated" '[ $RC = 7 ] && grep -q "got TERM" "$DUMMY_LOG"'
    sleep 0.3
    check "fifo: no pacer left after $sig" '[ "$(leftovers)" = 0 ]'
done

if [ "$FAIL" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi
