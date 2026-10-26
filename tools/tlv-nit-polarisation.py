#!/usr/bin/env python3
"""List the satellite delivery system descriptors in a TLV stream file:
frequency, orbit, polarisation and modulation, with how often each was
seen.

Usage: python3 -I tools/tlv-nit-polarisation.py FILE.tlv [FILE.tlv ...]

The descriptor (tag 0x43, length 11) is laid out as in ARIB STD-B32
Part 3 §3.11, Figure No. 5: frequency (8 BCD digits, 10 kHz), orbital
position (4 BCD digits, 0.1°), west_east_flag (1), polarization (2:
'00' horizontal, '01' vertical, '10' left-handed, '11' right-handed,
note 6), modulation (5), symbol rate (7 BCD digits), FEC (4). The TLV-NIT
is sent in the clear, so a raw byte search finds it without decoding the
TLV packets; a candidate counts only if its frequency lies in the 11–13
GHz satellite band and every BCD digit is valid, which rules out chance
matches in payload bytes. `isdb tlv inspect` decodes the same tables but
prints only the frequency. Used to confirm that BS14 (TLV stream 0xB0E0)
is left-hand circular (SPECS.md §1.2).
"""
import collections
import sys

POL = {0: "horizontal", 1: "vertical", 2: "left-handed", 3: "right-handed"}


def bcd(b):
    s = b.hex()
    return int(s) if s.isdigit() else None


def scan(data):
    seen = collections.Counter()
    i = data.find(b"\x43\x0b")
    while i >= 0:
        b = data[i + 2 : i + 13]
        f = bcd(b[:4]) if len(b) == 11 else None
        orbit = bcd(b[4:6]) if f is not None else None
        if f is not None and orbit is not None and 1_100_000 <= f <= 1_300_000:
            east = b[6] >> 7
            pol = (b[6] >> 5) & 3
            mod = b[6] & 0x1F
            seen[(f * 10, orbit, east, pol, mod)] += 1
        i = data.find(b"\x43\x0b", i + 1)
    return seen


def main(paths):
    if not paths:
        sys.exit(__doc__)
    for p in paths:
        with open(p, "rb") as fh:
            seen = scan(fh.read())
        print(p)
        for (f, orbit, east, pol, mod), n in sorted(seen.items()):
            print(
                f"  {f} kHz  orbit {orbit / 10:.1f}°{'E' if east else 'W'}  "
                f"polarization '{pol:02b}' ({POL[pol]})  modulation '{mod:05b}'  x{n}"
            )


if __name__ == "__main__":
    main(sys.argv[1:])
