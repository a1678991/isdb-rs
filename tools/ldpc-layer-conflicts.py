#!/usr/bin/env python3
"""Layer structure of the ARIB STD-B44 LDPC codes for a layered FPGA decoder.

Rebuilds each code the way `isdb_fec::ldpc::Code::new` does (entry x of
table row g hits check (x + j*q) mod P for j in 0..374, plus the two
staircase parity bits), then groups the P checks into q layers by residue
mod q: within a layer every information group connects each of its 374
bits to exactly one check (a cyclic shift), so the layer's checks share no
variable and may be processed in parallel. The exception is a row with two
entries congruent mod q: that group connects twice to the layer, every
check of the layer then shares a bit with one other check (the pair's
shift difference apart), and a parallel schedule must keep those two apart
(docs/plans/isdbs3-fpga-rx.md, "LDPC decoder"). This prints, per rate,
the layer count q, the edges per layer-step, the conflicting entry pairs
with their shift differences, and the largest layer degree.

Usage: tools/ldpc-layer-conflicts.py
"""

import re
from collections import Counter
from pathlib import Path

GROUP = 374
N = 44_880

src = Path(__file__).resolve().parent.parent / "crates/isdbs3-core/src/ldpc_tables.rs"
text = src.read_text()
for m in re.finditer(r"\(\s*(\d+),\s*&\[(.*?)\]\s*,\s*\)", text, re.S):
    rate = int(m.group(1))
    rows = [[int(v) for v in r.split(",") if v.strip()]
            for r in re.findall(r"&\[([\d,\s]+)\]", m.group(2))]
    k = GROUP * len(rows)
    p = N - k
    q = p // GROUP
    layer_deg = Counter()
    conflicts = []
    for g, row in enumerate(rows):
        seen = {}
        for x in row:
            r, s = x % q, x // q  # layer and cyclic shift
            layer_deg[r] += 1
            if r in seen:
                conflicts.append((g, r, (s - seen[r]) % GROUP))
            else:
                seen[r] = s
    edges = sum(len(r) for r in rows) * GROUP + 2 * p - 1
    worst = max(layer_deg.values()) + 2
    print(f"rate {rate:3}/120: q={q:3} layers, P={p:5}, edges {edges:6}, "
          f"edges per 374-parallel step {edges / GROUP:6.1f}, "
          f"largest layer degree {worst:2}, conflicting pairs {len(conflicts):2}"
          + (f" shifts {sorted({c[2] for c in conflicts})}" if conflicts else ""))
