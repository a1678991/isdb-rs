#!/usr/bin/env python3
"""Offline model of PCR-paced placement of a CBR TS into one ISDB-S TS of
N slots/frame (8N packets per superframe of 319488 symbols at 28.86 Mbaud).

Usage: isdbs-pacing-sim.py IN.ts SECONDS MODE OUT.ts
  MODE: arrival   - today's --feed file: input packets (nulls kept) fill every slot in order
        sf        - the ISDB-T Pacer applied per superframe: packets due before the end of
                    superframe k go into k in order from its first slot, nulls after
        slot      - first slot at/after the due time, nulls dropped, PCR not touched
        restamp   - slot, and each PCR += (slot time - due time) (H.222.0 D.0.4)
Writes OUT.ts (the TS a receiver outputs for that TS: every slot, nulls included)
and prints PCR CBR-fit residuals and PAT intervals measured on the slot grid.
"""
import sys
from fractions import Fraction

N_SLOTS = 48
PER_SF = 8 * N_SLOTS
SF_TICKS = Fraction(319488 * 27_000_000, 28_860_000)  # 11059200/37
SLOT_TICKS = SF_TICKS / PER_SF
MOD = (1 << 33) * 300
NULL = bytes([0x47, 0x1F, 0xFF, 0x10]) + b"\xff" * 184


def pid(p):
    return ((p[1] & 0x1F) << 8) | p[2]


def pcr_of(p):
    if not (p[3] & 0x20) or p[4] == 0 or not (p[5] & 0x10):
        return None
    b = p[6:12]
    base = (b[0] << 25) | (b[1] << 17) | (b[2] << 9) | (b[3] << 1) | (b[4] >> 7)
    ext = ((b[4] & 1) << 8) | b[5]
    return base * 300 + ext


def set_pcr(p, v):
    v %= MOD
    base, ext = divmod(v, 300)
    q = bytearray(p)
    q[6] = (base >> 25) & 0xFF
    q[7] = (base >> 17) & 0xFF
    q[8] = (base >> 9) & 0xFF
    q[9] = (base >> 1) & 0xFF
    q[10] = ((base & 1) << 7) | 0x7E | ((ext >> 8) & 1)
    q[11] = ext & 0xFF
    return bytes(q)


def fit(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    b = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    r = [(y - (my + b * (x - mx))) for x, y in zip(xs, ys)]
    return b, max(abs(v) for v in r), (sum(v * v for v in r) / n) ** 0.5


def main():
    path, seconds, mode, out = sys.argv[1], float(sys.argv[2]), sys.argv[3], sys.argv[4]
    data = open(path, "rb").read()
    pk = [data[i:i + 188] for i in range(0, len(data) - 187, 188)]
    # due time of every packet: H.222.0 eq. 2-4 between the reference PID's PCRs
    ref = None
    pcrs = []
    for i, p in enumerate(pk):
        v = pcr_of(p)
        if v is not None and (ref is None or pid(p) == ref):
            ref = pid(p)
            pcrs.append((i, v))
    due = [0.0] * len(pk)
    for (i0, v0), (i1, v1) in zip(pcrs, pcrs[1:]):
        for i in range(i0, i1):
            due[i] = (v0 - pcrs[0][1]) + (v1 - v0) * (i - i0) / (i1 - i0)
    last_i, last_v = pcrs[-1]
    tpp = (pcrs[-1][1] - pcrs[-2][1]) / (pcrs[-1][0] - pcrs[-2][0])
    for i in range(last_i, len(pk)):
        due[i] = (last_v - pcrs[0][1]) + (i - last_i) * tpp
    first = pcrs[0][0]
    n_slots = int(seconds * 27e6 / float(SLOT_TICKS))
    slots = [None] * n_slots
    if mode == "arrival":
        for k in range(min(n_slots, len(pk) - first)):
            slots[k] = pk[first + k]
    else:
        k = 0
        sf_fill = {}
        for i in range(first, len(pk)):
            p = pk[i]
            if pid(p) == 0x1FFF:
                continue
            d = due[i]
            if mode == "sf":
                sf = int(d // SF_TICKS)
                j = max(sf * PER_SF + sf_fill.get(sf, 0), k)
                sf_fill[sf] = j - sf * PER_SF + 1
            else:
                j = max(k, -(-Fraction(d).limit_denominator(1 << 20) // SLOT_TICKS))
                j = int(j)
            if j >= n_slots:
                break
            if mode == "restamp":
                v = pcr_of(p)
                if v is not None:
                    p = set_pcr(p, v + round(float(j * SLOT_TICKS) - d))
            slots[j] = p
            k = j + 1
    with open(out, "wb") as f:
        for p in slots:
            f.write(p if p is not None else NULL)
    # measure on the slot grid
    by_pid = {}
    pat = []
    for j, p in enumerate(slots):
        if p is None:
            continue
        v = pcr_of(p)
        if v is not None:
            by_pid.setdefault(pid(p), []).append((j, v))
        if pid(p) == 0 and p[1] & 0x40:
            pat.append(j)
    for q, s in sorted(by_pid.items()):
        xs = [x for x, _ in s]
        ys, prev, acc = [], None, 0
        for _, v in s:
            if prev is not None and v < prev:
                acc += MOD
            prev = v
            ys.append((v + acc) / 27e6)
        b, mx, rms = fit(xs, ys)
        rate = 188 * 8 / b
        print(f"{mode}: pid {q:#06x} {len(s)} PCRs, rate {rate/1e6:.4f} Mbit/s, "
              f"CBR-fit max {mx*1e9:.0f} ns rms {rms*1e9:.0f} ns")
    t = [j * float(SLOT_TICKS) / 27e6 for j in pat]
    d = [b - a for a, b in zip(t, t[1:])]
    print(f"{mode}: PAT x{len(pat)} interval min {min(d):.4f} max {max(d):.4f} s (slot grid)")


main()
