#!/usr/bin/env python3
"""Classify the MMTP packet-sequence gaps of one HEVC asset by what they
hit, from `mmtpdump` output of a clear (descrambled) TLV file: a gap inside
one fragmented MFU (a slice) whose remaining bytes the lost packets account
for exactly ("mid->cont exact"), one that spans MFUs ("MISMATCH" / "mid->start"),
or one at an MFU boundary. Development tool for isdb-remux's loss handling
(src/ts.rs, Session::lost): on the BS17 capture almost every gap lands
inside a single slice.

    target/release/examples/mmtpdump FILE f300 100000000 > dump.txt
    tools/mmtp-gap-classes.py dump.txt
"""
import sys
# mmtpdump lines: id 0xf300 pt 0x00 seq N rap R scr S len L: HEX(48 bytes)
cls = {}
prev = None
partial = None  # [declared_total(incl 4-byte prefix), received, S]
events = []
for line in open(sys.argv[1]):
    f = line.split()
    seq = int(f[5]); rap = int(f[7]); h = f[-1]
    plen = int(h[0:4], 16) + 2
    flags = int(h[4:6], 16); frag = (flags >> 1) & 3; agg = flags & 1
    mpu = int(h[8:16], 16)
    data = plen - 8 - 14  # non-aggregated MFU data bytes
    gap = prev is not None and seq != prev[0] + 1
    if gap:
        lost = seq - prev[0] - 1
        if partial:
            rem = partial[0] - partial[1]
            S = partial[2]
            need = -(-rem // S)
            if frag in (2, 3):
                kind = 'mid->cont'  # resolved at Last
                pending = (lost, need, rem, S)
            else:
                kind = 'mid->start ' + ('exact' if lost == need else f'extra {lost-need}')
                pending = None
        else:
            kind = 'boundary->' + ('cont(head lost)' if frag in (2, 3) else 'start')
            pending = None
        events.append([kind, lost, mpu, pending])
        if frag in (2,3) and partial:
            partial = partial + [len(events)-1, lost]
        else:
            partial = None if frag in (2,3) else partial
    if frag == 1:
        total = int(h[44:52], 16) + 4
        partial = [total, data, data]
    elif frag in (2, 3) and partial:
        partial[1] += data
        if frag == 3:
            if len(partial) > 3:
                ev = events[partial[3]]
                lost = partial[4]
                ok = partial[0] - partial[1] == lost * partial[2]
                ev[0] = 'mid->cont ' + ('exact' if ok else 'MISMATCH')
            partial = None
    elif frag == 0:
        partial = None
    prev = (seq, frag, mpu)
from collections import Counter
c = Counter(e[0] for e in events)
for k, v in c.most_common(): print(v, k)
print(sum(e[1] for e in events), 'packets in', len(events), 'gaps')
