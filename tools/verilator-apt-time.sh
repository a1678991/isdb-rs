#!/usr/bin/env bash
# How long the FPGA CI job's Verilator install takes, and which version it
# gets: `apt-get install verilator` in a fresh Ubuntu container (default
# ubuntu:26.04, the CI runner's release; g++ and make installed first, as
# they are on the runner image). With OUT set, the installed Verilator is
# copied there (OUT/bin, OUT/share/verilator) to run that version on the
# host: VERILATOR=OUT/bin/verilator VERILATOR_ROOT=OUT/share/verilator.
#
# tools/verilator-apt-time.sh [IMAGE]
set -euo pipefail
image=${1:-ubuntu:26.04}
out=${OUT:-}
docker pull -q "$image" >/dev/null
docker run --rm --network host ${out:+-v "$out:/out"} "$image" bash -c '
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null 2>&1
apt-get install -y -qq g++ make >/dev/null 2>&1
apt-get clean
rm -rf /var/lib/apt/lists/*
t0=$(date +%s%N)
apt-get update -qq >/dev/null
t1=$(date +%s%N)
apt-get install -y -qq --no-install-recommends verilator 2>&1 | grep -E "^(Need to get|After this)" || true
t2=$(date +%s%N)
echo "apt-get update $(( (t1 - t0) / 1000000 )) ms, install $(( (t2 - t1) / 1000000 )) ms"
verilator --version
if [ -d /out ]; then
  mkdir -p /out/bin /out/share
  cp -a /usr/bin/verilator* /out/bin/
  cp -a /usr/share/verilator /out/share/
fi
'
