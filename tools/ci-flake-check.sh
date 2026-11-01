#!/usr/bin/env bash
# Reproduce CI conditions for the wall-clock tests: run test targets
# pinned to a few CPUs (a GitHub-hosted runner has 4), optionally with busy
# loops competing for them, several times over, and count the failures.
#
#   tools/ci-flake-check.sh [-c CPUS] [-b BUSY] [-n RUNS] -- <cargo test args>
#
#   -c CPUS  taskset CPU list (default 0-3)
#   -b BUSY  busy-loop processes pinned to the same CPUs (default 0)
#   -n RUNS  repetitions (default 5)
#
# Examples:
#   tools/ci-flake-check.sh -n 3 -- -p isdb-cli --test live_feed
#   tools/ci-flake-check.sh -b 2 -n 10 -- -p isdb-cli --bins
#   tools/ci-flake-check.sh -n 3 -- --workspace
set -euo pipefail

cpus=0-3 busy=0 runs=5
while getopts c:b:n: o; do
  case $o in
    c) cpus=$OPTARG ;;
    b) busy=$OPTARG ;;
    n) runs=$OPTARG ;;
    *) sed -n '2,15p' "$0"; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ "${1:-}" = -- ] && shift
[ $# -gt 0 ] || { sed -n '2,15p' "$0"; exit 2; }

cargo test --locked --no-run "$@" >/dev/null 2>&1

pids=()
cleanup() { [ ${#pids[@]} -eq 0 ] || kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT
for _ in $(seq "$busy"); do
  taskset -c "$cpus" sh -c 'while :; do :; done' &
  pids+=($!)
done

log=$(mktemp)
fail=0
for i in $(seq "$runs"); do
  start=$SECONDS
  if taskset -c "$cpus" cargo test --locked "$@" >"$log" 2>&1; then
    echo "run $i: ok ($((SECONDS - start)) s)"
  else
    fail=$((fail + 1))
    echo "run $i: FAILED ($((SECONDS - start)) s)"
    grep -E '^test .* FAILED|panicked at|^  ' "$log" | grep -vE '^  +[0-9]+:|^ +at ' | head -12
  fi
done
rm -f "$log"
echo "$fail of $runs runs failed (cpus $cpus, $busy busy loops)"
[ "$fail" -eq 0 ]
