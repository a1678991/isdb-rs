#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Compare a decoded layer TS against the source TS, anchored by content.

  ts_compare.py SOURCE.ts DECODED.ts [--cyclic]

Each decoded 188-byte packet is expected to equal source packet
(anchor + i) [mod len(source) with --cyclic].  The anchor is found by
searching the source for the first decoded packet that occurs exactly once in
the source (null / repeated packets are skipped for anchoring).  Reports the
anchor, packets compared, byte-exact matches, mismatching packets (with TEI
flag count) and the first few mismatch indices.
"""
import argparse
import sys
from collections import defaultdict


def packets(path):
    d = open(path, "rb").read()
    return [d[i:i + 188] for i in range(0, len(d) - len(d) % 188, 188)]


def main():
    p = argparse.ArgumentParser()
    p.add_argument("source")
    p.add_argument("decoded")
    p.add_argument("--cyclic", action="store_true")
    a = p.parse_args()
    src, dec = packets(a.source), packets(a.decoded)
    if not dec:
        print(f"{a.decoded}: no packets")
        return 2
    where = defaultdict(list)
    for i, s in enumerate(src):
        where[s].append(i)
    anchor = None
    for k, d in enumerate(dec):
        w = where.get(d)
        if w and len(w) == 1:
            anchor = w[0] - k
            break
    if anchor is None:
        print(f"{a.decoded}: {len(dec)} packets, no unique packet found in source -> 0 matches")
        return 1
    n = len(src)
    match = mism = tei = beyond = 0
    first_bad = []
    for k, d in enumerate(dec):
        j = anchor + k
        if a.cyclic:
            j %= n
        elif j < 0 or j >= n:
            beyond += 1
            continue
        if d == src[j]:
            match += 1
        else:
            mism += 1
            if d[1] & 0x80:
                tei += 1
            if len(first_bad) < 8:
                first_bad.append(k)
    print(f"{a.decoded}: anchor src[{anchor}] (= decoded[0]); compared {match + mism}, "
          f"exact {match}, mismatched {mism} (TEI set {tei}), outside source {beyond}"
          + (f", first bad decoded idx {first_bad}" if first_bad else ""))
    return 0 if mism == 0 and match > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
