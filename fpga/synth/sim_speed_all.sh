#!/usr/bin/env bash
# The backends table of README "Simulation backends": sim_speed.py on the
# chain's frame 0 and the S = 34 decoder, warm (one core) and cold (a
# fresh build: CXXRTL on one core, Verilator with 1 and 4 make jobs on
# 1 and 4 cores). CORE (default 8) is the first core used.
#
# synth/sim_speed_all.sh [CASE...]   (default: chain ldpc-34), from fpga/
set -euo pipefail
core=${CORE:-8}
one=$core
four=$core-$((core + 3))
cases=("$@")
[ ${#cases[@]} -eq 0 ] && cases=(chain ldpc-34)
run() {
  local cpus=$1
  shift
  taskset -c "$cpus" uv run --locked python synth/sim_speed.py "$@" 2>&1 | grep -v Unused
}
for c in "${cases[@]}"; do
  run "$one" "$c" verilator >/dev/null  # the cached libraries in place
  run "$one" "$c" cxxrtl >/dev/null
  echo "== $c warm, one core"
  run "$one" "$c" verilator
  run "$one" "$c" cxxrtl
  echo "== $c cold"
  run "$one" "$c" cxxrtl --cold
  run "$one" "$c" verilator --cold --jobs 1
  run "$four" "$c" verilator --cold --jobs 4
done
