#!/usr/bin/env python3
"""Check an RX capture of the AD9361 digital loopback against the cs12 tone
`isdb-radio`'s `txrate` example transmits (docs/plans/isdbs-fpga-tx.md §5):
is the loop sample-transparent?

    python3 -I tools/loopback-tone-check.py CAPTURE.sigmf-data [PERIOD]

The capture is cs16 (12 bits sign-extended, `isdb capture recorder=iio`).
The tone has PERIOD samples per cycle (txrate: 64). Prints whether the
samples repeat exactly every period (a digital copy has at most PERIOD
distinct complex values), the amplitude against the expected 10 dB backoff
(2048 x 10^(-0.5) = 647.6 LSB) and the residual against the best-fit tone.
"""
import cmath
import math
import sys

path = sys.argv[1]
period = int(sys.argv[2]) if len(sys.argv) > 2 else 64
raw = open(path, "rb").read()
n = len(raw) // 4
x = []
for k in range(n):
    i = int.from_bytes(raw[4 * k:4 * k + 2], "little", signed=True)
    q = int.from_bytes(raw[4 * k + 2:4 * k + 4], "little", signed=True)
    x.append(complex(i, q))
# skip the first 10 % (AGC / stream start), analyse the rest
x = x[n // 10:]
n = len(x)
power = sum(abs(v) ** 2 for v in x) / n
print(f"{n} samples, RMS {math.sqrt(power):.1f} LSB (expected 647.6 for a 10 dB backoff)")
exact = sum(1 for k in range(n - period) if x[k] == x[k + period])
print(f"exactly periodic (x[k] == x[k+{period}]): {exact} of {n - period} ({100 * exact / (n - period):.3f} %)")
distinct = len(set((v.real, v.imag) for v in x))
print(f"distinct complex values: {distinct} (a digital copy of the tone has at most {period})")
# best-fit tone at +1/period cycles per sample
c = sum(v * cmath.exp(-2j * math.pi * k / period) for k, v in enumerate(x)) / n
resid = sum(abs(v - c * cmath.exp(2j * math.pi * k / period)) ** 2 for k, v in enumerate(x)) / n
print(f"tone amplitude {abs(c):.2f} LSB, phase {math.degrees(cmath.phase(c)):.1f} deg; residual {10 * math.log10(abs(c) ** 2 / max(resid, 1e-12)):.1f} dB below the tone")
c2 = sum(v * cmath.exp(2j * math.pi * k / period) for k, v in enumerate(x)) / n
print(f"image (negative frequency) amplitude {abs(c2):.2f} LSB")
vals = sorted(set((v.real, v.imag) for v in x))[:8]
print("first distinct values:", vals)
