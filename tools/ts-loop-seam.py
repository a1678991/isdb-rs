#!/usr/bin/env python3
"""Loop seams of a TS file (docs/plans/isdbs-next/pcr-pacing.md §4, §7.6).

ts-loop-seam.py IN.ts
    What playing IN.ts in a loop as it is would break at the seam: for each
    PID, does the first packet's continuity counter follow the last one's
    when the file is played again from the start, and how far is the
    first PCR from the last one.

ts-loop-seam.py IN.ts PLAYED.ts
    Check PLAYED.ts, IN.ts played in a loop by `isdb tx --system isdbs
    --feed loop` (the placed TS, or what `isdb rx` received of it), from its
    first packet: its non-null packets are IN.ts's, pass after pass. Reports
      - continuity_counter errors per H.222.0 §2.4.3.3 (payload packets +1,
        adaptation-only packets unchanged, one duplicate allowed), in total
        and at the seams;
      - per PCR PID: backward or > 100 ms PCR steps, the CBR fit of PCR
        against byte position (max and rms residual), and the PCR step
        across each seam against the fit;
      - per PES PID: DTS (else PTS) steps that go back, and the step across
        each seam against the median step;
      - bytes changed other than the PCR, PTS/DTS and continuity_counter
        fields (pass 0: other than the PCR), which must be none.
    Exit status 0 when there is no CC error, no PCR discontinuity, no PES
    time stamp going back and no other byte changed; else 1.
"""
import statistics
import sys

PCR_MOD = (1 << 33) * 300
TS_MOD = 1 << 33


def packets(path):
    d = open(path, "rb").read()
    return [d[i:i + 188] for i in range(0, len(d) - 187, 188)]


def pid(p):
    return ((p[1] & 0x1F) << 8) | p[2]


def has_payload(p):
    return bool(p[3] & 0x10)


def pcr(p):
    if not (p[3] & 0x20) or p[4] < 7 or not (p[5] & 0x10):
        return None
    b = p[6:12]
    base = (b[0] << 25) | (b[1] << 17) | (b[2] << 9) | (b[3] << 1) | (b[4] >> 7)
    return base * 300 + (((b[4] & 1) << 8) | b[5])


def payload_at(p):
    afc = (p[3] >> 4) & 3
    if afc == 1:
        return 4
    if afc == 3:
        return 5 + p[4]
    return None


def pes_times(p):
    """(offset of the PTS field, number of fields 1 or 2) of a PES header
    that starts in the packet, or None (Table 2-21)."""
    if not (p[1] & 0x40):
        return None
    at = payload_at(p)
    if at is None or at + 19 > 188 or p[at:at + 3] != b"\x00\x00\x01":
        return None
    sid = p[at + 3]
    if sid < 0xBC or sid in (0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xF2, 0xF8, 0xFF):
        return None
    flags = p[at + 7] >> 6
    n = {2: 1, 3: 2}.get(flags)
    return (at + 9, n) if n else None


def read_ts(f):
    return (((f[0] >> 1) & 7) << 30) | (f[1] << 22) | ((f[2] >> 1) << 15) | (f[3] << 7) | (f[4] >> 1)


def plain(path):
    pk = packets(path)
    first, last, pcrs = {}, {}, {}
    for i, p in enumerate(pk):
        q = pid(p)
        if q == 0x1FFF:
            continue
        cc = p[3] & 15
        first.setdefault(q, (i, cc, has_payload(p)))
        last[q] = (i, cc, has_payload(p))
        v = pcr(p)
        if v is not None:
            pcrs.setdefault(q, []).append((i, v))
    n = len(pk)
    print("packets", n)
    for q in sorted(first):
        fi, fc, fp = first[q]
        li, lc, lp = last[q]
        exp = (lc + 1) % 16 if fp else lc
        print(f"pid {q:#06x} first@{fi} cc{fc} last@{li} cc{lc} -> next pass cc {fc} expected {exp} "
              f"{'OK' if exp == fc else 'CC ERROR'}")
    for q, s in pcrs.items():
        (i0, v0), (i1, v1) = s[0], s[-1]
        tpp = (v1 - v0) / (i1 - i0)
        print(f"PCR pid {q:#06x}: first@{i0} {v0} last@{i1} {v1}; ticks/pkt {tpp:.4f}; period by packets "
              f"{n * tpp:.1f} ticks = {n * tpp / 27e6:.6f} s; seam gap {(v0 + n * tpp - v1) / 27e6 * 1e3:.3f} ms "
              f"vs interval {(s[1][1] - v0) / 27e6 * 1e3:.3f} ms")


def played(src_path, out_path):
    src = [p for p in packets(src_path) if pid(p) != 0x1FFF]
    out = packets(out_path)
    m = len(src)
    if m == 0:
        sys.exit(f"{src_path}: no packets")
    # index in `out` of every non-null packet; k-th non-null is src[k % m]
    real = [i for i, p in enumerate(out) if pid(p) != 0x1FFF]
    seams = [real[k] for k in range(m, len(real), m)]
    passes = len(real) / m
    print(f"{out_path}: {len(out)} packets, {len(real)} non-null = {passes:.2f} passes of {src_path} "
          f"({m} non-null packets); seams at packets {seams}")
    near = lambda i: any(abs(i - s) <= 2 * m // 100 + 50 for s in seams)

    # bytes other than PCR / PTS / DTS / CC
    changed = 0
    for k, i in enumerate(real):
        p, q = bytearray(out[i]), bytearray(src[k % m])
        if pcr(q) is not None:
            p[6:12] = q[6:12]
        if k >= m:
            p[3] = (p[3] & 0xF0) | (q[3] & 15)
            t = pes_times(q)
            if t:
                at, n = t
                p[at:at + 5 * n] = q[at:at + 5 * n]
        if p != q:
            changed += 1
            if changed <= 5:
                print(f"  packet {i} (pass {k // m}, source packet {k % m}): bytes changed")
    print(f"bytes changed outside PCR/PTS/DTS/CC (pass 0: outside PCR): {changed} packets")

    # continuity_counter, H.222.0 §2.4.3.3
    last, dup, cc_err, cc_seam = {}, {}, 0, 0
    for i, p in enumerate(out):
        q = pid(p)
        if q == 0x1FFF:
            continue
        cc = p[3] & 15
        if not has_payload(p):
            ok = q not in last or last[q] == cc
        elif q not in last:
            ok = True
        elif cc == (last[q] + 1) % 16:
            ok, dup[q] = True, 0
        elif cc == last[q] and dup.get(q, 0) == 0:
            ok, dup[q] = True, 1
        else:
            ok = False
        if has_payload(p):
            last[q] = cc
        if not ok:
            cc_err += 1
            cc_seam += near(i)
            if cc_err <= 5:
                print(f"  CC error: pid {q:#06x} at packet {i}")
    print(f"CC errors: {cc_err} ({cc_seam} at seams)")

    # PCR
    pcr_disc = 0
    pcrs = {}
    for i, p in enumerate(out):
        v = pcr(p)
        if v is not None:
            pcrs.setdefault(pid(p), []).append((i, v))
    for q, s in sorted(pcrs.items()):
        un, disc = [s[0][1]], 0
        for (i0, v0), (i1, v1) in zip(s, s[1:]):
            d = (v1 - v0) % PCR_MOD
            if d == 0 or d > 27_000_000 // 10:
                disc += 1
            un.append(un[-1] + d)
        pcr_disc += disc
        xs = [i * 188 + 10 for i, _ in s]  # byte of the PCR base's last bit
        if len(xs) > 2:
            mx, my = statistics.fmean(xs), statistics.fmean(un)
            sxx = sum((x - mx) ** 2 for x in xs)
            b = sum((x - mx) * (y - my) for x, y in zip(xs, un)) / sxx
            res = [(y - (my + b * (x - mx))) / 27e6 * 1e9 for x, y in zip(xs, un)]
            rate = 8 * 27e6 / b
            mx_ns, rms_ns = max(abs(r) for r in res), (sum(r * r for r in res) / len(res)) ** 0.5
            at = [res[j] for j, (i, _) in enumerate(s) if near(i)]
            seam = f"; at seams max {max(abs(r) for r in at):.0f} ns" if at else ""
            print(f"PCR pid {q:#06x}: {len(s)} PCRs, {disc} discontinuities, rate {rate / 1e6:.6f} Mbit/s, "
                  f"CBR-fit max {mx_ns:.0f} ns rms {rms_ns:.0f} ns{seam}")
    print(f"PCR discontinuities: {pcr_disc}")

    # PES time stamps
    pes_back = 0
    stamps = {}
    for i, p in enumerate(out):
        t = pes_times(p)
        if t:
            at, n = t
            v = read_ts(p[at + 5:at + 10]) if n == 2 else read_ts(p[at:at + 5])
            stamps.setdefault(pid(p), []).append((i, v))
    for q, s in sorted(stamps.items()):
        steps = [((v1 - v0 + TS_MOD // 2) % TS_MOD - TS_MOD // 2, i1) for (_, v0), (i1, v1) in zip(s, s[1:])]
        back = sum(1 for d, _ in steps if d < 0)
        pes_back += back
        med = statistics.median(d for d, _ in steps) if steps else 0
        at_seam = []
        for sp in seams:
            for d, i1 in steps:
                if i1 >= sp:
                    at_seam.append(d)
                    break
        print(f"PES pid {q:#06x}: {len(s)} PES, {back} going back, median step {med / 90:.2f} ms, "
              f"steps across seams {[round(d / 90, 2) for d in at_seam]} ms")
    print(f"PES time stamps going back: {pes_back}")
    ok = cc_err == 0 and pcr_disc == 0 and pes_back == 0 and changed == 0
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    if len(sys.argv) == 2:
        plain(sys.argv[1])
    elif len(sys.argv) == 3:
        sys.exit(played(sys.argv[1], sys.argv[2]))
    else:
        sys.exit(__doc__)
