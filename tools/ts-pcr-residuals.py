#!/usr/bin/env python3
"""PCR residuals of a TS against its own packet index: for each PCR of PID
(default: the first PCR PID seen) print index, PCR ticks, and the residual
against a least-squares fit PCR = a + b·index, in ns; then the residual's
shape: rms, max, the lag-1 autocorrelation and the dominant period in
packets (a periodic sawtooth means the writer of the file inserts or drops
packets on its own clock; white noise means per-packet jitter).

Usage: ts-pcr-residuals.py FILE.ts [PID_HEX] [--print N]
"""
import sys, math
argv = sys.argv[1:]
n_print = 0
if "--print" in argv:
    k = argv.index("--print"); n_print = int(argv[k + 1]); del argv[k:k + 2]
args = argv
d = open(args[0], "rb").read()
# sync: the first offset with three 0x47 bytes 188 apart (a recording may start mid-packet)
off = next(o for o in range(188) if d[o] == 0x47 and d[o+188] == 0x47 and d[o+376] == 0x47)
d = d[off:]; n = len(d) // 188
want = int(args[1], 16) if len(args) > 1 else None
def pid(p): return ((p[1] & 0x1F) << 8) | p[2]
def pcr(p):
    if not (p[3] & 0x20) or p[4] < 7 or not (p[5] & 0x10): return None
    b = p[6:12]; base = (b[0] << 25) | (b[1] << 17) | (b[2] << 9) | (b[3] << 1) | (b[4] >> 7)
    return base * 300 + (((b[4] & 1) << 8) | b[5])
xs, ys, last = [], [], None
for i in range(n):
    p = d[i*188:(i+1)*188]; v = pcr(p)
    if v is None: continue
    q = pid(p)
    if want is None: want = q
    if q != want: continue
    if last is not None and v < last: v += (1 << 33) * 300
    last = v; xs.append(i); ys.append(v)
m = len(xs); mx = sum(xs)/m; my = sum(ys)/m
b = sum((x-mx)*(y-my) for x, y in zip(xs, ys)) / sum((x-mx)**2 for x in xs); a = my - b*mx
r = [(y - (a + b*x)) * 1000.0 / 27.0 for x, y in zip(xs, ys)]  # ns (27 MHz ticks)
rms = math.sqrt(sum(v*v for v in r)/m); mxr = max(abs(v) for v in r)
ac1 = sum(r[i]*r[i+1] for i in range(m-1)) / sum(v*v for v in r)
rate = 1504 * 27e6 / b
print(f"pid {want:#06x}: {m} PCRs, ticks/packet {b:.6f} (rate {rate/1e6:.6f} Mbit/s), residual rms {rms:.0f} ns max {mxr:.0f} ns, lag-1 autocorrelation {ac1:+.2f}")
# dominant period by a coarse DFT over 2..m/2 cycles
best = (0, 0)
for k in range(1, m//2):
    c = sum(v*math.cos(2*math.pi*k*j/m) for j, v in enumerate(r)); s = sum(v*math.sin(2*math.pi*k*j/m) for j, v in enumerate(r))
    if c*c+s*s > best[0]: best = (c*c+s*s, k)
if best[1]: print(f"dominant period: {m/best[1]:.1f} PCRs = {(xs[-1]-xs[0])/(m-1)*m/best[1]:.0f} packets")
for x, v in list(zip(xs, r))[:n_print]: print(f"{x:8d} {v:+8.0f} ns")
