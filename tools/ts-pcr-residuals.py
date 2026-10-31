#!/usr/bin/env python3
"""PCR residuals of a TS against its own packet index.

For the PCRs of one PID (default: the first PID carrying a PCR) fit
PCR = a + b·index by least squares, index being the packet's number in
the file, and print the PCR-implied rate and the residual's shape: rms,
max, lag-1 autocorrelation, and the dominant period in packets with the
share of the residual power its fundamental carries.

In a CBR TS whose packets sit where their PCRs say (the placed TS of
`isdb tx --placed-ts`), the residual is the writer's PCR jitter, tens of
ns, close to white. A writer or receiver that adds or removes packets
without restamping the PCRs leaves a sawtooth of one packet time peak to
peak (1504 bits at the rate: 28.8 µs at 52.17 Mbit/s, so ±14.4 µs about
the fit) whose period is the spacing of the insertions or removals: the
PIX-DT295, whose PID filter drops the CAT without putting a null in its
place, shows one every CAT repetition. tools/ts-align-compare.py
re-inserts such packets against the placed TS and fits again.

The period is the peak of a periodogram over the actual packet indices
(the PCRs need not be evenly spaced in packets), searched from two
cycles within the PCR span up to half the number of PCRs, on a grid of
one cycle and then refined to 0.01 cycle around the peak. The share is
2·|Σ r·e^(−2πi·f·x)|² / (m·Σ r²): 1 for a pure sinusoid, 6/π² ≈ 0.61
for a sawtooth's fundamental, small for white noise (whose "period" is
then meaningless).

A backward PCR step by more than half the 2^33·300 modulus is taken as
the wrap and unwrapped; a smaller one is counted as a discontinuity and
left in place (the fit then shows it).

Usage: ts-pcr-residuals.py FILE.ts [PID_HEX] [--print N]
  PID_HEX    PCR PID, hex (default: the first PID carrying a PCR)
  --print N  also print the first N (packet index, residual ns) pairs

Also loaded by path as a module by tools/ts-align-compare.py.
"""
import math
import mmap
import re
import sys

SIZE = 188
NULL_PID = 0x1FFF
PCR_MODULUS = (1 << 33) * 300  # 27 MHz ticks
TICK_NS = 1000.0 / 27.0


def sync_offset(buf):
    """First offset with 0x47 at three successive packet starts (a
    recording may start mid-packet), or None."""
    for o in range(min(SIZE, len(buf))):
        if all(o + k * SIZE < len(buf) and buf[o + k * SIZE] == 0x47 for k in range(3)):
            return o
    return None


def pid(p):
    return ((p[1] & 0x1F) << 8) | p[2]


def pcr(p):
    """The PCR of a packet in 27 MHz ticks, or None."""
    if not (p[3] & 0x20) or p[4] < 7 or not (p[5] & 0x10):
        return None
    b = p[6:12]
    base = (b[0] << 25) | (b[1] << 17) | (b[2] << 9) | (b[3] << 1) | (b[4] >> 7)
    return base * 300 + (((b[4] & 1) << 8) | b[5])


def pcr_packets(buf, sync, want=None, start=0, stop=None):
    """(packet index, PCR) of the PCR packets of PID `want` (default: the
    first PID carrying a PCR) in buf, packets start..stop. A PID given,
    the packets are found by a regex on their header bytes, fast on a
    file of gigabytes. Returns (pid, list)."""
    n = (len(buf) - sync) // SIZE
    stop = n if stop is None else min(stop, n)
    if want is None:
        for i in range(start, stop):
            p = buf[sync + i * SIZE:sync + (i + 1) * SIZE]
            if pcr(p) is not None:
                want = pid(p)
                break
        else:
            return None, []
    hi = bytes(sorted({(f << 5) | (want >> 8) for f in range(8)}))
    pat = re.compile(b"\x47[" + re.escape(hi) + b"]" + re.escape(bytes([want & 0xFF])))
    out = []
    end = sync + stop * SIZE
    for m in pat.finditer(buf, sync + start * SIZE, end):
        o = m.start()
        if (o - sync) % SIZE:
            continue
        v = pcr(buf[o:o + SIZE])
        if v is not None:
            out.append(((o - sync) // SIZE, v))
    return want, out


def unwrap(values):
    """PCR values made monotonic across the 2^33 wrap; returns (values,
    count of other backward steps)."""
    out, add, back = [], 0, 0
    for v in values:
        if out and v + add < out[-1]:
            if out[-1] - (v + add) > PCR_MODULUS // 2:
                add += PCR_MODULUS
            else:
                back += 1
        out.append(v + add)
    return out, back


def fit(xs, ys):
    """Least squares y = a + b·x; returns (b, residuals in ns). xs, ys
    are integers; offsets are taken out first to keep double precision."""
    m = len(xs)
    if m < 2:
        return None, []
    x0, y0 = xs[0], ys[0]
    X = [x - x0 for x in xs]
    Y = [y - y0 for y in ys]
    mx, my = sum(X) / m, sum(Y) / m
    sxx = sum((x - mx) ** 2 for x in X)
    if sxx == 0:
        return None, []
    b = sum((x - mx) * (y - my) for x, y in zip(X, Y)) / sxx
    a = my - b * mx
    return b, [(y - (a + b * x)) * TICK_NS for x, y in zip(X, Y)]


def _power(xs, r, f):
    c = s = 0.0
    for x, v in zip(xs, r):
        t = 2 * math.pi * f * x
        c += v * math.cos(t)
        s += v * math.sin(t)
    return c * c + s * s


def dominant_period(xs, r):
    """(period in packets, cycles within the span, share of the residual
    power) of the periodogram peak, or None (fewer than 8 PCRs)."""
    m = len(xs)
    span = xs[-1] - xs[0] if m else 0
    energy = sum(v * v for v in r)
    if m < 8 or span <= 0 or energy == 0:
        return None
    X = [x - xs[0] for x in xs]
    top = m // 2
    best = max(range(2, top + 1), key=lambda c: _power(X, r, c / span), default=None)
    if best is None:
        return None
    fine = [best + k / 100 for k in range(-100, 101) if best + k / 100 >= 2]
    c = max(fine, key=lambda c: _power(X, r, c / span))
    share = 2 * _power(X, r, c / span) / (m * energy)
    return span / c, c, share


def shape(xs, ys):
    """The fit and residual shape of PCRs ys at packet indices xs."""
    b, r = fit(xs, ys)
    if b is None:
        return None
    m = len(r)
    energy = sum(v * v for v in r)
    out = {
        "pcrs": m,
        "ticks_per_packet": b,
        "rate_bps": SIZE * 8 * 27e6 / b if b > 0 else None,
        "rms_ns": math.sqrt(energy / m),
        "max_ns": max(abs(v) for v in r),
        "lag1": sum(r[i] * r[i + 1] for i in range(m - 1)) / energy if energy else 0.0,
        "residuals_ns": r,
    }
    p = dominant_period(xs, r)
    out["period_packets"], out["period_cycles"], out["period_share"] = p if p else (None, None, None)
    return out


def describe(s):
    """One line for a shape()."""
    rate = "-" if s["rate_bps"] is None else f"{s['rate_bps'] / 1e6:.6f}"
    line = (f"{s['pcrs']} PCRs, ticks/packet {s['ticks_per_packet']:.6f} (rate {rate} Mbit/s), "
            f"residual rms {s['rms_ns']:.0f} ns max {s['max_ns']:.0f} ns, "
            f"lag-1 autocorrelation {s['lag1']:+.2f}")
    if s["period_packets"] is not None:
        line += (f"; dominant period {s['period_packets']:.0f} packets "
                 f"({s['period_cycles']:.2f} cycles, {100 * s['period_share']:.0f}% of the residual power)")
    return line


def main(argv):
    n_print = 0
    if "--print" in argv:
        k = argv.index("--print")
        try:
            n_print = int(argv[k + 1])
        except (IndexError, ValueError):
            print(__doc__.split("Usage: ")[1].split("\n\n")[0], file=sys.stderr)
            return 2
        del argv[k:k + 2]
    if len(argv) not in (1, 2) or argv[0].startswith("-"):
        print("Usage: " + __doc__.split("Usage: ")[1].split("\n\n")[0], file=sys.stderr)
        return 2
    try:
        want = int(argv[1], 16) if len(argv) > 1 else None
        f = open(argv[0], "rb")
        buf = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    except (OSError, ValueError) as e:
        print(f"ts-pcr-residuals: {e}", file=sys.stderr)
        return 2
    sync = sync_offset(buf)
    if sync is None:
        print(f"ts-pcr-residuals: {argv[0]}: no 188-byte packet sync", file=sys.stderr)
        return 2
    want, found = pcr_packets(buf, sync, want)
    if len(found) < 2:
        print(f"ts-pcr-residuals: {argv[0]}: fewer than two PCRs", file=sys.stderr)
        return 2
    xs = [i for i, _ in found]
    ys, back = unwrap([v for _, v in found])
    s = shape(xs, ys)
    print(f"pid {want:#06x}: {describe(s)}")
    if back:
        print(f"note: {back} backward PCR steps (discontinuities) left in the fit")
    for x, v in list(zip(xs, s["residuals_ns"]))[:n_print]:
        print(f"{x:10d} {v:+10.0f} ns")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
