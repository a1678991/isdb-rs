#!/usr/bin/env bash
# Add the isdb-rs symbol front end to a maia-sdr checkout at the revision
# fpga/tezuka/build.sh pins (F5OEO/maia-sdr f7092ba). Called by build.sh in
# `isdb` mode; safe to run on a freshly checked-out tree only.
#
# Usage: fpga/tezuka/apply.sh MAIA_SDR_DIR REPO_DIR
#
# Edits two upstream files in MAIA_SDR_DIR, maia-hdl/projects/tezuka/
# system_bd.tcl (MIT, maia-hdl): the plutoskyr2 case sets `isdb_symbols`,
# and `common/isdb_symbols.tcl` is sourced right after cs12_cs8.tcl, whose
# packer connections it takes over. The block-design Tcl and the Verilog
# come from this repository (fpga/tezuka/isdb_symbols.tcl,
# fpga/build/isdb_symbols_rx.v). The edits exist only in the build tree.
set -euo pipefail

MAIA=${1:?usage: apply.sh MAIA_SDR_DIR REPO_DIR}
REPO=${2:?usage: apply.sh MAIA_SDR_DIR REPO_DIR}
BD="$MAIA/maia-hdl/projects/tezuka/system_bd.tcl"
COMMON="$MAIA/maia-hdl/projects/common"
V="$REPO/fpga/build/isdb_symbols_rx.v"

# Always regenerate: fpga/build is not versioned, and a stale file would put
# an older format into the bitstream (it happened once: the file predated
# format 3).
(cd "$REPO" && UV_DEFAULT_INDEX=${UV_DEFAULT_INDEX:-https://pypi.org/simple} mise run fpga:verilog)
[ -f "$V" ] || { echo "missing $V after mise run fpga:verilog" >&2; exit 1; }
grep -q 'isdb_symbols' "$BD" && { echo "$BD already patched" >&2; exit 1; }

# 1. the plutoskyr2 board flag
grep -q '^       set dvb "dvb"$' "$BD" || { echo "anchor 'set dvb' not found in $BD" >&2; exit 1; }
python3 -I - "$BD" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
case = '    "plutoskyr2" {\n'
i = s.index(case)
end = s.index('    }\n', i)
body = s[i:end]
if 'set dvb "dvb"' not in body:
    sys.exit("plutoskyr2 case has no 'set dvb'")
s = s[:end] + '       set isdb_symbols "isdb_symbols"\n' + s[end:]
open(p, "w").write(s)
EOF

# 2. source our block-design Tcl right after cs12_cs8.tcl
anchor='source $::tezuka_hdl_dir/common/cs12_cs8.tcl'
[ "$(grep -c -F "$anchor" "$BD")" = 1 ] || { echo "anchor '$anchor' not found once in $BD" >&2; exit 1; }
python3 -I - "$BD" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
anchor = "source $::tezuka_hdl_dir/common/cs12_cs8.tcl\n"
add = ("if {[info exists isdb_symbols]} {\n"
       "    set ::isdb_symbols_rx_v $::tezuka_hdl_dir/common/isdb_symbols_rx.v\n"
       "    source $::tezuka_hdl_dir/common/isdb_symbols.tcl\n"
       "}\n")
s = s.replace(anchor, anchor + add, 1)
open(p, "w").write(s)
EOF

# 3. our block-design Tcl and the Verilog next to the upstream ones (copies:
#    the build tree does not depend on where this repository lives)
cp "$REPO/fpga/tezuka/isdb_symbols.tcl" "$COMMON/isdb_symbols.tcl"
cp "$V" "$COMMON/isdb_symbols_rx.v"

echo "patched $BD"
grep -n 'isdb_symbols' "$BD"
