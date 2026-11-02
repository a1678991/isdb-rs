#!/usr/bin/env bash
# Add the isdb-rs blocks to a maia-sdr checkout at the revision
# fpga/tezuka/build.sh pins (F5OEO/maia-sdr f7092ba). Called by build.sh in
# its `isdb` and `isdb-tx` modes; safe to run on a freshly checked-out tree
# only.
#
# Usage: fpga/tezuka/apply.sh MAIA_SDR_DIR REPO_DIR [--tx-only]
#   (default)  both blocks: the symbol front end (SymbolsRx) on the RX path
#              and the ISDB-S transmit block (SymbolsTx) on the DAC path
#   --tx-only  the transmit block alone
#
# Edits one upstream file in MAIA_SDR_DIR, maia-hdl/projects/tezuka/
# system_bd.tcl (MIT, maia-hdl): the plutoskyr2 case sets `isdb_symbols_tx`
# (and `isdb_symbols`) and no longer sets `dvb` (the DVB-S2 transmitter:
# its DSP slices are the front end's, and its AXIS switches would sit in
# the DMA path the transmit block reads) nor `txfir` (the x8 TX FIR
# interpolator, whose place between the DAC unpacker and the rfifo the
# transmit block takes); `common/isdb_symbols.tcl` is sourced right after
# cs12_cs8.tcl, whose packer connections it takes over, and
# `common/isdb_symbols_tx.tcl` where txfir.tcl would be. The block-design
# Tcl, the constraints and the Verilog come from this repository
# (fpga/tezuka/isdb_symbols*.tcl, *.xdc, fpga/build/isdb_symbols_rx.v,
# isdb_symbols_tx.v). The edits exist only in the build tree.
set -euo pipefail

MAIA=${1:?usage: apply.sh MAIA_SDR_DIR REPO_DIR [--tx-only|--isdbs3-rx]}
REPO=${2:?usage: apply.sh MAIA_SDR_DIR REPO_DIR [--tx-only|--isdbs3-rx]}
RX=1
case "${3:-}" in
    "") ;;
    --tx-only) RX=0 ;;
    --isdbs3-rx) RX=s3 ;;
    *) echo "usage: apply.sh MAIA_SDR_DIR REPO_DIR [--tx-only|--isdbs3-rx]" >&2; exit 2 ;;
esac
BD="$MAIA/maia-hdl/projects/tezuka/system_bd.tcl"
COMMON="$MAIA/maia-hdl/projects/common"
V_RX="$REPO/fpga/build/isdb_symbols_rx.v"
V_TX="$REPO/fpga/build/isdb_symbols_tx.v"

# Always regenerate: fpga/build is not versioned, and a stale file would put
# an older format into the bitstream (it happened once: the file predated
# format 3).
grep -q 'isdb_' "$BD" && { echo "$BD already patched" >&2; exit 1; }
if [ "$RX" = s3 ]; then
    "$(dirname "$0")/apply_s3rx.sh" "$MAIA" "$REPO"
    exit
fi
if [ "$RX" = 1 ]; then
    (cd "$REPO" && UV_DEFAULT_INDEX=${UV_DEFAULT_INDEX:-https://pypi.org/simple} mise run fpga:verilog)
    [ -f "$V_RX" ] || { echo "missing $V_RX after mise run fpga:verilog" >&2; exit 1; }
fi
(cd "$REPO" && UV_DEFAULT_INDEX=${UV_DEFAULT_INDEX:-https://pypi.org/simple} mise run fpga:tx-verilog)
[ -f "$V_TX" ] || { echo "missing $V_TX after mise run fpga:tx-verilog" >&2; exit 1; }

# 1. the plutoskyr2 board flags
grep -q '^       set dvb "dvb"$' "$BD" || { echo "anchor 'set dvb' not found in $BD" >&2; exit 1; }
grep -q '^       set txfir "txfir"$' "$BD" || { echo "anchor 'set txfir' not found in $BD" >&2; exit 1; }
python3 -I - "$BD" "$RX" <<'EOF'
import sys
p, rx = sys.argv[1], sys.argv[2] == "1"
s = open(p).read()
case = '    "plutoskyr2" {\n'
i = s.index(case)
end = s.index('    }\n', i)
body = s[i:end]
# The front end's 112 DSP48E1 (one sample per clock: fpga/README.md) do
# not fit next to all of Tezuka's 106 on the XC7Z020 (220): this image
# leaves out the DVB-S2 transmitter (dvb.tcl: encoder and its RRC
# interpolator, 40 DSP48E1), which reception does not use, and the x8 TX
# FIR interpolator (txfir.tcl, 20 DSP48E1), whose place the transmit block
# takes. The stock bitstream keeps both.
for line in ('       set dvb "dvb"\n', '       set txfir "txfir"\n'):
    if line not in body:
        sys.exit(f"plutoskyr2 case has no {line.strip()!r} line to drop")
    body = body.replace(line, "", 1)
if rx:
    body += '       set isdb_symbols "isdb_symbols"\n'
body += '       set isdb_symbols_tx "isdb_symbols_tx"\n'
s = s[:i] + body + s[end:]
open(p, "w").write(s)
EOF

# 2. source our block-design Tcl: the front end right after cs12_cs8.tcl,
#    the transmit block where txfir.tcl is sourced
anchor_rx='source $::tezuka_hdl_dir/common/cs12_cs8.tcl'
anchor_tx='if {[info exists txfir]} { source $::tezuka_hdl_dir/common/txfir.tcl }'
[ "$(grep -c -F "$anchor_rx" "$BD")" = 1 ] || { echo "anchor '$anchor_rx' not found once in $BD" >&2; exit 1; }
[ "$(grep -c -F "$anchor_tx" "$BD")" = 1 ] || { echo "anchor '$anchor_tx' not found once in $BD" >&2; exit 1; }
python3 -I - "$BD" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
anchor = "source $::tezuka_hdl_dir/common/cs12_cs8.tcl\n"
add = ("if {[info exists isdb_symbols]} {\n"
       "    set ::isdb_symbols_rx_v $::tezuka_hdl_dir/common/isdb_symbols_rx.v\n"
       "    set ::isdb_symbols_xdc $::tezuka_hdl_dir/common/isdb_symbols.xdc\n"
       "    source $::tezuka_hdl_dir/common/isdb_symbols.tcl\n"
       "}\n")
s = s.replace(anchor, anchor + add, 1)
anchor = "if {[info exists txfir]} { source $::tezuka_hdl_dir/common/txfir.tcl }\n"
add = ("if {[info exists isdb_symbols_tx]} {\n"
       "    set ::isdb_symbols_tx_v $::tezuka_hdl_dir/common/isdb_symbols_tx.v\n"
       "    set ::isdb_symbols_tx_xdc $::tezuka_hdl_dir/common/isdb_symbols_tx.xdc\n"
       "    source $::tezuka_hdl_dir/common/isdb_symbols_tx.tcl\n"
       "}\n")
s = s.replace(anchor, anchor + add, 1)
open(p, "w").write(s)
EOF

# 3. our block-design Tcl, constraints and Verilog next to the upstream
#    ones (copies: the build tree does not depend on where this repository
#    lives)
if [ "$RX" = 1 ]; then
    cp "$REPO/fpga/tezuka/isdb_symbols.tcl" "$COMMON/isdb_symbols.tcl"
    cp "$REPO/fpga/tezuka/isdb_symbols.xdc" "$COMMON/isdb_symbols.xdc"
    cp "$V_RX" "$COMMON/isdb_symbols_rx.v"
fi
cp "$REPO/fpga/tezuka/isdb_symbols_tx.tcl" "$COMMON/isdb_symbols_tx.tcl"
cp "$REPO/fpga/tezuka/isdb_symbols_tx.xdc" "$COMMON/isdb_symbols_tx.xdc"
cp "$V_TX" "$COMMON/isdb_symbols_tx.v"

echo "patched $BD"
grep -n 'isdb_symbols' "$BD"
