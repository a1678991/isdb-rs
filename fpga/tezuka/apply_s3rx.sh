#!/usr/bin/env bash
# The receive-only ISDB-S3 image's edits to a maia-sdr checkout at the
# revision fpga/tezuka/build.sh pins: `apply.sh MAIA_SDR_DIR REPO_DIR
# --isdbs3-rx` (build.sh mode `isdbs3-rx`) runs this; safe on a freshly
# checked-out tree only.
#
# Edits one upstream file, maia-hdl/projects/tezuka/system_bd.tcl (MIT,
# maia-hdl), in the build tree:
#   - the plutoskyr2 case: no `dvb` (the DVB-S2 transmitter), no `txfir`
#     (the x8 TX FIR), no `dac_dds`; sets `isdb_slots` and `isdb_rx_only`;
#   - with `isdb_rx_only`: maia.tcl, maiafirtoiq.tcl (the maia-sdr core, its
#     clock wizard, the DDC muxes) and sweeper.tcl are replaced by
#     common/isdb_rx_base.tcl (the DMAs' HP2 port and the same-named
#     sample muxes, path 0 fixed), and the AD9361 core gets
#     DAC_DATAPATH_DISABLE 1 right after xilinx_ad9361.tcl creates it;
#   - with `isdb_slots`: common/isdb_slots.tcl is sourced right after
#     cs12_cs8.tcl (the place isdb_symbols.tcl takes in the other images).
# Copies next to the upstream Tcl in projects/common: isdb_rx_base.tcl,
# isdb_slots.tcl, isdb_slots.xdc, isdb_slots_top.v and the generated
# isdb_slots_rx.v (`mise run fpga:slots-verilog`, always regenerated).
#
# Copyright isdb-rs contributors; this file is part of isdb-rs (see the
# repository licence).
set -euo pipefail

MAIA=${1:?usage: apply_s3rx.sh MAIA_SDR_DIR REPO_DIR}
REPO=${2:?usage: apply_s3rx.sh MAIA_SDR_DIR REPO_DIR}
BD="$MAIA/maia-hdl/projects/tezuka/system_bd.tcl"
COMMON="$MAIA/maia-hdl/projects/common"
V_S3="$REPO/fpga/build/isdb_slots_rx.v"

grep -q 'isdb_' "$BD" && { echo "$BD already patched" >&2; exit 1; }
# fpga/build is not versioned: regenerate, so the bitstream has this
# checkout's block
rm -f "$V_S3"
(cd "$REPO" && UV_DEFAULT_INDEX=${UV_DEFAULT_INDEX:-https://pypi.org/simple} mise run fpga:slots-verilog)
[ -f "$V_S3" ] || { echo "missing $V_S3 after mise run fpga:slots-verilog" >&2; exit 1; }

python3 -I - "$BD" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()

def once(old, new):
    global s
    if s.count(old) != 1:
        sys.exit(f"{p}: anchor {old!r} found {s.count(old)} times, expected once")
    s = s.replace(old, new, 1)

# 1. the plutoskyr2 board flags
case = '    "plutoskyr2" {\n'
i = s.index(case)
end = s.index('    }\n', i)
body = s[i:end]
for line in ('       set dvb "dvb"\n', '       set txfir "txfir"\n', '       set dac_dds "dac_dds"\n'):
    if line not in body:
        sys.exit(f"plutoskyr2 case has no {line.strip()!r} line to drop")
    body = body.replace(line, "", 1)
body += '       set isdb_slots "isdb_slots"\n       set isdb_rx_only "isdb_rx_only"\n'
s = s[:i] + body + s[end:]

# 2. the AD9361 core's TX datapath off (DDS, DMA data, IQ correction)
once("source $::tezuka_hdl_dir/common/xilinx_ad9361.tcl\n",
     "source $::tezuka_hdl_dir/common/xilinx_ad9361.tcl\n"
     "if {[info exists isdb_rx_only]} {\n"
     "    ad_ip_parameter axi_ad9361 CONFIG.DAC_DATAPATH_DISABLE 1\n"
     "}\n")

# 3. no maia-sdr core: isdb_rx_base.tcl in place of maia.tcl,
#    maiafirtoiq.tcl / maiaffttoiq.tcl and sweeper.tcl
once("source $::tezuka_hdl_dir/common/maia.tcl\n",
     "if {![info exists isdb_rx_only]} { source $::tezuka_hdl_dir/common/maia.tcl }\n")
once("if {[info exists fftraw]} {\n",
     "if {[info exists isdb_rx_only]} {\n"
     "      source $::tezuka_hdl_dir/common/isdb_rx_base.tcl\n"
     "} elseif {[info exists fftraw]} {\n")
once("source $::tezuka_hdl_dir/common/sweeper.tcl\n",
     "if {![info exists isdb_rx_only]} { source $::tezuka_hdl_dir/common/sweeper.tcl }\n")

# 4. the block after cs12_cs8.tcl
once("source $::tezuka_hdl_dir/common/cs12_cs8.tcl\n",
     "source $::tezuka_hdl_dir/common/cs12_cs8.tcl\n"
     "if {[info exists isdb_slots]} {\n"
     "    set ::isdb_slots_rx_v $::tezuka_hdl_dir/common/isdb_slots_rx.v\n"
     "    set ::isdb_slots_top_v $::tezuka_hdl_dir/common/isdb_slots_top.v\n"
     "    set ::isdb_slots_xdc $::tezuka_hdl_dir/common/isdb_slots.xdc\n"
     "    source $::tezuka_hdl_dir/common/isdb_slots.tcl\n"
     "}\n")
open(p, "w").write(s)
EOF

for f in isdb_rx_base.tcl isdb_slots.tcl isdb_slots.xdc isdb_slots_top.v; do
    cp "$REPO/fpga/tezuka/$f" "$COMMON/$f"
done
cp "$V_S3" "$COMMON/isdb_slots_rx.v"

echo "patched $BD"
grep -n 'isdb_' "$BD"
