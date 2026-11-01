#!/usr/bin/env python3
"""Packet-level comparison of two sets of TLV stream files.

    tlv-packet-diff.py DIR_A DIR_B [--json OUT]

Each directory holds one TLV stream file per stream id, `NAME-XXXX.tlv`
(as `isdb rx --system isdbs3 --out 'DIR/NAME-{tlv_id}.tlv'` writes them).
The files of the same stream id are parsed into TLV packets (01111111b
sync, type, 16-bit length, data: ARIB STD-B32 Part 3 §3.5) and aligned
packet by packet (difflib's matching blocks on the packets' bytes), so a
stretch one side lacks (frames the other did not decode) shows as packets
only on one side, and a packet that differs as a replacement.

Prints per stream: packets of each side, packets equal (aligned and
identical), and the differing runs: (A start, A end, B start, B end) in
packet indices, with their kind: `only A`, `only B`, `replaced`. A run of
packets missing on one side between equal packets is what an undecoded
frame or a discontinuity leaves; `replaced` runs are packets with
different bytes at the same place (a decoding difference, or the partial
packet at the edge of a discontinuity).

Exit status 0 always (this reports; the caller decides).
"""

import argparse
import difflib
import hashlib
import json
import pathlib
import re
import sys

SYNC = 0x7F


def packets(path):
    """The TLV packets of a file, as bytes (header included); bytes that
    do not start a packet are skipped to the next sync byte."""
    data = path.read_bytes()
    out, i, skipped = [], 0, 0
    n = len(data)
    while i + 4 <= n:
        if data[i] != SYNC:
            j = data.find(bytes([SYNC]), i + 1)
            j = n if j < 0 else j
            skipped += j - i
            i = j
            continue
        length = int.from_bytes(data[i + 2:i + 4], "big")
        if i + 4 + length > n:
            break
        out.append(data[i:i + 4 + length])
        i += 4 + length
    return out, skipped + (n - i)


def streams(d):
    m = {}
    for p in sorted(pathlib.Path(d).glob("*.tlv")):
        g = re.search(r"([0-9a-fA-F]{4})\.tlv$", p.name)
        if g:
            m[g.group(1).lower()] = p
    return m


def compare(a, b):
    ha = [hashlib.blake2b(x, digest_size=16).digest() for x in a]
    hb = [hashlib.blake2b(x, digest_size=16).digest() for x in b]
    sm = difflib.SequenceMatcher(None, ha, hb, autojunk=False)
    equal = 0
    runs = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            equal += i2 - i1
            continue
        kind = {"delete": "only A", "insert": "only B", "replace": "replaced"}[tag]
        runs.append({"kind": kind, "a": [i1, i2], "b": [j1, j2],
                     "a_packets": i2 - i1, "b_packets": j2 - j1,
                     "a_bytes": sum(len(x) for x in a[i1:i2]),
                     "b_bytes": sum(len(x) for x in b[j1:j2]),
                     # where in A the run starts, as a fraction of A's bytes
                     "a_at": 0.0})
    total = sum(len(x) for x in a) or 1
    pos = [0]
    for x in a:
        pos.append(pos[-1] + len(x))
    for r in runs:
        r["a_at"] = pos[r["a"][0]] / total
    return equal, runs


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--json", help="write the result as JSON here")
    ap.add_argument("--runs", type=int, default=20, help="differing runs to print per stream")
    args = ap.parse_args()
    sa, sb = streams(args.a), streams(args.b)
    result = {}
    for sid in sorted(set(sa) | set(sb)):
        pa, ka = packets(sa[sid]) if sid in sa else ([], 0)
        pb, kb = packets(sb[sid]) if sid in sb else ([], 0)
        equal, runs = compare(pa, pb)
        r = {
            "a_packets": len(pa), "b_packets": len(pb), "equal": equal,
            "a_skipped_bytes": ka, "b_skipped_bytes": kb,
            "a_bytes": sum(len(x) for x in pa), "b_bytes": sum(len(x) for x in pb),
            "only_a_bytes": sum(x["a_bytes"] for x in runs if x["kind"] != "only B"),
            "only_b_bytes": sum(x["b_bytes"] for x in runs if x["kind"] != "only A"),
            "only_a": sum(x["a_packets"] for x in runs if x["kind"] == "only A"),
            "only_b": sum(x["b_packets"] for x in runs if x["kind"] == "only B"),
            "replaced_a": sum(x["a_packets"] for x in runs if x["kind"] == "replaced"),
            "replaced_b": sum(x["b_packets"] for x in runs if x["kind"] == "replaced"),
            "runs": runs,
        }
        result[sid] = r
        print(f"stream {sid}: A {len(pa)} packets, B {len(pb)}; equal {equal}; "
              f"only A {r['only_a']}, only B {r['only_b']}, replaced {r['replaced_a']} (A) / "
              f"{r['replaced_b']} (B) in {len(runs)} run(s); skipped bytes A {ka}, B {kb}")
        print(f"  bytes: A {r['a_bytes']}, B {r['b_bytes']}; in differing runs A {r['only_a_bytes']}, "
              f"B {r['only_b_bytes']}")
        for x in runs[:args.runs]:
            print(f"  {x['kind']:8} at {100 * x['a_at']:5.1f} % of A: A[{x['a'][0]}:{x['a'][1]}] "
                  f"({x['a_packets']} packets, {x['a_bytes']} B) B[{x['b'][0]}:{x['b'][1]}] "
                  f"({x['b_packets']} packets, {x['b_bytes']} B)")
        if len(runs) > args.runs:
            print(f"  … {len(runs) - args.runs} more")
    if args.json:
        pathlib.Path(args.json).write_text(json.dumps(result, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
