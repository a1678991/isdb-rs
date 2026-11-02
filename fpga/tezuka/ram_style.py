#!/usr/bin/env python3
"""Mark the shallow memories of a generated Verilog block as LUT RAM for
Vivado: `(* ram_style = "distributed" *)` on every `reg [W-1:0] name
[D-1:0];` with D <= 64 (Yosys's `write_verilog` form of an Amaranth
`Memory`), ROMs from Yosys's proc_rom left alone.

    ram_style.py IN.v OUT.v [MAX_DEPTH]

Why: Vivado's synthesis maps a memory to block RAM by size, so a 64-deep
memory goes to block RAM when it is wide (the LDPC decoder's 64 x 714
input FIFO took ten RAMB36, the decoder bridge's 64 x 211 and 64 x 75
FIFOs four and a half) or when Vivado's choice falls that way (the front
end's 64 x 18 coefficient banks took one RAMB18 each, 31 of 96). A 64-deep
memory is the LUT RAM's native depth (RAM64M: 64 x 3 per four LUTs, no
read multiplexer), which is how the Yosys estimates
(`fpga/synth/*estimate.py`) count them, and the receive-only image needs
those block RAMs (fpga/README.md "Building the PlutoSky R2 bitstream").

The attribute selects the implementation only: the memory's behaviour (its
synchronous write, registered or combinational reads) is the RTL's either
way, so the block stays what the simulations verified.
"""

import re
import sys

DECL = re.compile(r"^(\s*)reg (\[(\d+):0\] )?(\\?\S+) \[(\d+):0\];\s*$")


def main(argv):
    src, dst = argv[1], argv[2]
    max_depth = int(argv[3]) if len(argv) > 3 else 64
    out = []
    marked = []
    prev = ""
    for line in open(src):
        m = DECL.match(line)
        if m and not m.group(4).startswith("\\$auto$proc_rom") and "ram_style" not in prev:
            depth = int(m.group(5)) + 1
            width = int(m.group(3)) + 1 if m.group(3) else 1
            if depth <= max_depth:
                out.append(f'{m.group(1)}(* ram_style = "distributed" *)\n')
                marked.append((m.group(4), depth, width))
        out.append(line)
        prev = line
    with open(dst, "w") as f:
        f.writelines(out)
    bits = sum(d * w for _, d, w in marked)
    print(f"{dst}: {len(marked)} memories of depth <= {max_depth} marked distributed ({bits} bits)")
    shapes = {}
    for name, d, w in marked:
        key = re.sub(r"\d+$", "*", name)
        shapes.setdefault((key, d, w), 0)
        shapes[(key, d, w)] += 1
    for (name, d, w), n in sorted(shapes.items()):
        print(f"  {n:3d} x {name} {d} x {w}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
