#!/usr/bin/env python3
"""Byte-exact comparison of a recorded TS with the TS it should be, after
aligning the two on the first packet they share.

Usage: ts-align-compare.py [--ignore-nulls] RECORDING.ts REFERENCE.ts

Both files are 188-byte packets (a recording that starts mid-packet is
re-synchronised on the first offset with 0x47 at three successive packet
starts). The alignment is the first non-null packet of RECORDING whose
bytes occur exactly once in REFERENCE (a PCR packet is unique by its PCR,
a section packet usually by its CC); the two are then compared packet by
packet over their overlap. --ignore-nulls drops null packets (PID 0x1FFF)
from both first, for a receiver that does not output them.

Written for tools/isdbs-bench-check.sh --placed: the reference is the
transmitter's placed TS (`isdb tx --placed-ts`), the recording the tuner's
output (docs/plans/isdbs-next/pcr-pacing.md §7.8, §8 bench acceptance).

Prints the alignment, the packets compared and the mismatches (the first
few with their PIDs). Exit status: 0 identical over a non-empty overlap,
1 mismatch or no alignment, 2 usage or unreadable input.
"""
import sys

SIZE = 188
SHOW = 5


def packets(path):
    data = open(path, "rb").read()
    start = next(
        (o for o in range(min(SIZE, len(data)))
         if all(o + k * SIZE < len(data) and data[o + k * SIZE] == 0x47 for k in range(3))),
        None,
    )
    if start is None:
        raise ValueError(f"{path}: no 188-byte packet sync")
    n = (len(data) - start) // SIZE
    out = [data[start + i * SIZE:start + (i + 1) * SIZE] for i in range(n)]
    return start, len(data) - start - n * SIZE, out


def pid(p):
    return ((p[1] & 0x1F) << 8) | p[2]


def main(argv):
    ignore_nulls = "--ignore-nulls" in argv
    args = [a for a in argv if a != "--ignore-nulls"]
    if len(args) != 2 or any(a.startswith("-") for a in args):
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    try:
        (rs, rt, rec), (fs, ft, ref) = packets(args[0]), packets(args[1])
    except (OSError, ValueError) as e:
        print(f"ts-align-compare: {e}", file=sys.stderr)
        return 2
    for name, s, t in ((args[0], rs, rt), (args[1], fs, ft)):
        if s or t:
            print(f"note: {name}: {s} bytes before the first packet, {t} after the last")
    if ignore_nulls:
        rec = [p for p in rec if pid(p) != 0x1FFF]
        ref = [p for p in ref if pid(p) != 0x1FFF]
    where = {}
    for j, p in enumerate(ref):
        if pid(p) != 0x1FFF:
            where.setdefault(p, []).append(j)
    align = next(
        ((i, where[p][0]) for i, p in enumerate(rec)
         if pid(p) != 0x1FFF and len(where.get(p, ())) == 1),
        None,
    )
    nulls = " (null packets ignored)" if ignore_nulls else ""
    if align is None:
        print(f"FAIL: no packet of the recording occurs exactly once in the reference{nulls}")
        return 1
    i0, j0 = align
    off = j0 - i0
    lo = max(0, -off)
    hi = min(len(rec), len(ref) - off)
    bad = [i for i in range(lo, hi) if rec[i] != ref[i + off]]
    print(
        f"aligned{nulls}: recording packet {i0} = reference packet {j0} (pid {pid(rec[i0]):#06x}); "
        f"overlap recording {lo}..{hi} of {len(rec)}, reference {lo + off}..{hi + off} of {len(ref)}"
    )
    for i in bad[:SHOW]:
        a, b = rec[i], ref[i + off]
        k = next(k for k in range(SIZE) if a[k] != b[k])
        print(
            f"  mismatch at recording packet {i} / reference {i + off}: "
            f"pid {pid(a):#06x} vs {pid(b):#06x}, first differing byte {k}"
        )
    n = hi - lo
    if bad or n <= 0:
        print(f"FAIL: {len(bad)} of {n} packets differ")
        return 1
    print(f"PASS: {n} packets identical")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
