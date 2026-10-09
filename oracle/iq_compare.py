#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Carrier-level comparison of two ISDB-T cf32 files (Rust TX vs gr-isdbt TX).

  iq_compare.py RUST.cf32 GR.cf32 MODE GI [--offset 1] [--skip 6]

Both files must start at an OFDM-frame boundary (isdbt-phy's Transmitter and
grisdbt_tx.py do).  Rust frame f+offset is compared with gr frame f (the Rust
modulator's first output is on-air frame -1, gr-isdbt's first output is frame
0 after its 2-symbol skiphead).  The first --skip gr frames are ignored
(time-interleaver start-up content differs: zeros in gr-isdbt, pseudo-random
fill in Rust).  Each file is FFT'd at the exact symbol timing, scaled by a
single least-squares gain, and every carrier of every compared symbol is
classified as equal (|diff| < 1e-2 of unit power) or different.  The report
lists, for differing carriers, which carriers and which symbol numbers of the
frame are involved, so that differences can be attributed (e.g. AC carriers,
TMCC bits B17-B19).

--seg-offsets gives one Rust-vs-gr frame offset per data segment 0..12
(default: --offset for all).  It is needed because gr-isdbt delays layers
with time-interleaving length I = 0 by one extra frame (its
time_interleaver uses a delay adjustment of 204 - (95*I mod 204) symbols,
i.e. 204 for I = 0, where B31 Table 3-12 gives 0); the data carriers of an
I = 0 layer then line up with offset - 1.  This works when every
frequency-interleaving group holds layers of one kind (I = 0 or I > 0).
"""
import argparse
from collections import Counter

import numpy as np


def frames(path, mode, gi):
    n = 2048 * 2 ** (mode - 1)
    ng = n // gi
    k = 13 * 108 * 2 ** (mode - 1) + 1
    kc = (k - 1) // 2
    x = np.fromfile(path, dtype=np.complex64)
    nsym = len(x) // (n + ng)
    s = x[: nsym * (n + ng)].reshape(nsym, n + ng)[:, ng:]
    c = np.fft.fft(s, axis=1)[:, (np.arange(k) - kc) % n]
    nf = nsym // 204
    return c[: nf * 204].reshape(nf, 204, k)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("rust")
    p.add_argument("gr")
    p.add_argument("mode", type=int)
    p.add_argument("gi", type=int)
    p.add_argument("--offset", type=int, default=1)
    p.add_argument("--skip", type=int, default=6)
    p.add_argument("--seg-offsets", default="", help="13 comma-separated offsets, data segment 0..12")
    a = p.parse_args()
    r = frames(a.rust, a.mode, a.gi)
    g = frames(a.gr, a.mode, a.gi)
    segoff = [int(x) for x in a.seg_offsets.split(",")] if a.seg_offsets else [a.offset] * 13
    c = 108 * 2 ** (a.mode - 1)
    order = [11, 9, 7, 5, 3, 1, 0, 2, 4, 6, 8, 10, 12]  # Fig. 3-32, ascending frequency
    k = r.shape[2]
    caroff = np.array([segoff[order[i // c]] if i < 13 * c else a.offset for i in range(k)])
    omax = max(segoff + [a.offset])
    nf = min(len(g), len(r) - omax)
    gg = g[a.skip: nf]
    fidx = np.arange(a.skip, nf)[:, None] + caroff[None, :]          # rust frame per (frame, carrier)
    rr = np.stack([r[fidx[:, i], :, i] for i in range(k)], axis=2)  # (frames, 204, carriers)
    if len(gg) == 0:
        raise SystemExit("not enough frames")
    # robust single gain: median ratio over carriers with non-negligible power
    # (a least-squares fit would be biased by the carriers that differ)
    m = np.abs(gg) > 0.5 * np.sqrt(np.mean(np.abs(gg) ** 2))
    q = rr[m] / gg[m]
    gain = np.median(q.real) + 1j * np.median(q.imag)
    rr = rr / gain
    rms = np.sqrt(np.mean(np.abs(gg) ** 2))
    diff = np.abs(rr - gg) / rms > 1e-2
    print(f"compared {len(gg)} frames x 204 symbols x {gg.shape[2]} carriers (gr frames {a.skip}..{nf - 1}, "
          f"rust offset {a.offset}, per-segment {segoff}); gain {abs(gain):.4g}")
    print(f"differing carrier-symbols: {int(diff.sum())} of {diff.size}")
    if diff.any():
        car = Counter(np.nonzero(diff)[2].tolist())
        sym = Counter(np.nonzero(diff)[1].tolist())
        print(f"  carriers involved: {len(car)}; first: {sorted(car)[:20]}")
        print(f"  symbols involved: {len(sym)}; first: {sorted(sym)[:20]}")
        per_car = Counter(car.values())
        print(f"  differing symbols per carrier (count: n carriers): {dict(sorted(per_car.items()))}")
        # DBPSK view of the differing carriers (TMCC/AC are DBPSK, B'0 = reference):
        # B_n = sign change between symbols n-1 and n of the frame.
        kinds = Counter()
        for kk in sorted(car):
            br = (rr[:, 1:, kk] * np.conj(rr[:, :-1, kk])).real < 0
            bg = (gg[:, 1:, kk] * np.conj(gg[:, :-1, kk])).real < 0
            ref = bool(np.all(np.sign(rr[:, 0, kk].real) == np.sign(gg[:, 0, kk].real)))
            dif = sorted({int(n) + 1 for n in np.nonzero(br != bg)[1]})
            desc = (f"B'0 equal={ref}; differing B: {compress(dif)}; rust B1-B203 "
                    f"{'all 1' if br.all() else 'all 0' if not br.any() else 'mixed'}, gr "
                    f"{'all 1' if bg.all() else 'all 0' if not bg.any() else 'mixed'}")
            kinds[desc] += 1
        for d, n in kinds.items():
            print(f"  {n} carriers: {d}")


def compress(v):
    out, i = [], 0
    while i < len(v):
        j = i
        while j + 1 < len(v) and v[j + 1] == v[j] + 1:
            j += 1
        out.append(f"{v[i]}" if i == j else f"{v[i]}-{v[j]}")
        i = j + 1
    return ",".join(out)


if __name__ == "__main__":
    main()
