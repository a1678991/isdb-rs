#!/usr/bin/env bash
# Run one of this directory's Vivado Tcl scripts in batch mode on a
# checkpoint, with Vivado from VIVADO_SETTINGS (the path of its
# settings64.sh), in the checkpoint's directory (Vivado's own log and
# journal stay out of the caller's directory):
#
#   fpga/tezuka/vivado-batch.sh timing.tcl CHECKPOINT.dcp [ARGS...]
#   fpga/tezuka/vivado-batch.sh utilization.tcl CHECKPOINT.dcp [OUT.rpt] [DEPTH]
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TCL=${1:?usage: vivado-batch.sh SCRIPT.tcl CHECKPOINT.dcp [ARGS...]}
DCP=$(realpath "${2:?usage: vivado-batch.sh SCRIPT.tcl CHECKPOINT.dcp [ARGS...]}")
shift 2
case "$TCL" in /*) ;; *) TCL="$HERE/$TCL" ;; esac
VIVADO_SETTINGS=${VIVADO_SETTINGS:?set VIVADO_SETTINGS to the path of the Vivado settings64.sh}
set +u
# shellcheck disable=SC1090
source "$VIVADO_SETTINGS"
set -u
cd "$(dirname "$DCP")"
exec vivado -mode batch -nojournal -nolog -source "$TCL" -tclargs "$DCP" "$@"
