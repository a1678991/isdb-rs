#!/usr/bin/env python3
"""Largest check-node degree of each ARIB STD-B44 LDPC code.

Reads crates/isdbs3-core/src/ldpc_tables.rs and rebuilds the parity-check
rows the way `isdb_fec::ldpc::Code::new` does (entry x of a row hits check
(x + j*q) mod p for j in 0..374, plus the two staircase parity bits), then
prints, per rate, n, k, the largest check degree and the edge count. The
fixed-point decoder (`isdb_fec::simd`) holds an edge index of one check in
an `i8`, so the largest degree must stay below 128.

Usage: tools/ldpc-check-degree.py
"""

import re
import sys
from pathlib import Path

GROUP = 374
N = 44_880

src = Path(__file__).resolve().parent.parent / "crates/isdbs3-core/src/ldpc_tables.rs"
text = src.read_text()
worst = 0
for m in re.finditer(r"\(\s*(\d+),\s*&\[(.*?)\]\s*,\s*\)", text, re.S):
    rate = int(m.group(1))
    rows = [[int(v) for v in r.split(",") if v.strip()]
            for r in re.findall(r"&\[([\d,\s]+)\]", m.group(2))]
    k = GROUP * len(rows)
    p = N - k
    q = p // GROUP
    deg = [2] * p
    deg[0] = 1
    for row in rows:
        for j in range(GROUP):
            for x in row:
                deg[(x + j * q) % p] += 1
    worst = max(worst, max(deg))
    print(f"r={rate:3}/120 n={N} k={k:5} max check degree={max(deg):3} edges={sum(deg)}")
print(f"largest degree {worst}")
sys.exit(0 if worst < 128 else 1)
