#!/usr/bin/env python3
"""Extract the untimed data streams of an `isdb-remux --ts` file.

`isdb-remux --ts` carries each untimed asset of the service (data
broadcasting `aapp`, ...) as a private-data pid (stream type 06h): one
`private_stream_2` PES per MFU (one item of the asset's package), its data
starting with the 4-byte MPU sequence number and the 4-byte item id
(big-endian; the top bit set on the continuation of an item split over
PESs).

Usage:
  tools/remux-ts-data.py FILE.ts                 # per pid: items, bytes, packages
  tools/remux-ts-data.py FILE.ts --out DIR       # DIR/PID/MPU/ITEM files
  tools/remux-ts-data.py FILE.ts --concat DIR    # DIR/PID.mfu, the items back to back
                                               # (the asset's MFUs back to back)
"""

import argparse
import collections
import os
import sys


def pmt_data_pids(f):
    """The pids of stream type 06h in the first PMT."""
    pat_pmt = None
    while True:
        p = f.read(188)
        if len(p) < 188:
            return []
        if p[0] != 0x47 or not p[1] & 0x40:
            continue
        pid = (p[1] & 0x1F) << 8 | p[2]
        start = 4 + (1 + p[4] if p[3] & 0x20 else 0)
        sec = p[start + 1 + p[start]:]
        if pid == 0 and sec[0] == 0x00:
            pat_pmt = (sec[10] & 0x1F) << 8 | sec[11]
        elif pid == pat_pmt and sec[0] == 0x02:
            length = (sec[1] & 0x0F) << 8 | sec[2]
            info = (sec[10] & 0x0F) << 8 | sec[11]
            i, end, pids = 12 + info, 3 + length - 4, []
            while i + 5 <= end:
                es_pid = (sec[i + 1] & 0x1F) << 8 | sec[i + 2]
                if sec[i] == 0x06:
                    pids.append(es_pid)
                i += 5 + ((sec[i + 3] & 0x0F) << 8 | sec[i + 4])
            return pids


def items(path):
    """Yield (pid, mpu, item, data) per item, continuations joined."""
    with open(path, "rb") as f:
        pids = set(pmt_data_pids(f))
        f.seek(0)
        buf = {p: bytearray() for p in pids}
        open_item = {}
        while True:
            p = f.read(188)
            if len(p) < 188:
                break
            pid = (p[1] & 0x1F) << 8 | p[2]
            if pid not in pids or not p[3] & 0x10:
                continue
            start = 4 + (1 + p[4] if p[3] & 0x20 else 0)
            if p[1] & 0x40:
                yield from flush_items(buf, open_item, pid)
            buf[pid].extend(p[start:])
        for pid in sorted(pids):
            yield from flush_items(buf, open_item, pid)
            if pid in open_item:
                pid_, mpu, item, data = open_item.pop(pid)
                yield pid_, mpu, item, bytes(data)


def flush_items(buf, open_item, pid):
    """Close the PES buffered on `pid`; yield the item it completes."""
    b = buf[pid]
    if len(b) >= 14 and b[:4] == b"\x00\x00\x01\xbf":
        n = b[4] << 8 | b[5]
        body = bytes(b[6:6 + n])
        mpu = int.from_bytes(body[:4], "big")
        item = int.from_bytes(body[4:8], "big")
        if item & 0x8000_0000 and pid in open_item:
            open_item[pid][3].extend(body[8:])
        else:
            if pid in open_item:
                pid_, m, i, data = open_item.pop(pid)
                yield pid_, m, i, bytes(data)
            open_item[pid] = [pid, mpu, item, bytearray(body[8:])]
    b.clear()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("ts")
    ap.add_argument("--out", help="write DIR/PID/MPU/ITEM files")
    ap.add_argument("--concat", help="write DIR/PID.mfu (the items back to back)")
    a = ap.parse_args()
    stats = collections.defaultdict(lambda: [0, 0, set()])
    concat = {}
    for pid, mpu, item, data in items(a.ts):
        s = stats[pid]
        s[0] += 1
        s[1] += len(data)
        s[2].add(mpu)
        if a.out:
            d = os.path.join(a.out, f"{pid:04x}", str(mpu))
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, str(item)), "wb") as f:
                f.write(data)
        if a.concat:
            if pid not in concat:
                os.makedirs(a.concat, exist_ok=True)
                concat[pid] = open(os.path.join(a.concat, f"{pid:04x}.mfu"), "wb")
            concat[pid].write(data)
    for f in concat.values():
        f.close()
    if not stats:
        print(f"{a.ts}: no data pid", file=sys.stderr)
        return 1
    for pid, (n, size, mpus) in sorted(stats.items()):
        print(f"pid {pid:#06x}: {n} item(s), {size} byte(s), {len(mpus)} package version(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
