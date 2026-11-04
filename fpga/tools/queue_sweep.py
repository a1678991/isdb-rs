"""The decoder's queue (`isdb_fpga.ldpcqueue`) at the chain's pace, by S,
clock and iteration limit: the overruns of three frames of the measured
distribution (`measured_frame`: mean 2.1, three isolated codewords at the
limit) and of the same with two adjacent pairs at the limit (slots 17-18,
61-62). For the fit's decoder decisions (fpga/README.md): the 160 MHz
question at S = 34 and the S = 22 fallback at 200 MHz. `--rate` the slot
codewords' rate numerator (93: 16APSK 93/120, the default; 109: 32APSK
109/120, every slot valid). Then the input port's pace (`port_cycles`):
the decoder cycles per second the bridge needs for 16APSK and 32APSK main
symbols with the TMCC burst, at n cycles a beat of n values (the bridge
since 2026-11-04) and at n + 1 (before), against the clocks.

python tools/queue_sweep.py [--s 22,34] [--clock 160,200] [--limits 8,10,12,14,16] [--banks 3]
    [--rate 93]
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga import ldpcqueue as q  # noqa: E402


def arg(name, default):
    if name in sys.argv:
        return [int(v) for v in sys.argv[sys.argv.index(name) + 1].split(",")]
    return default


def main():
    banks = arg("--banks", [3])[0]
    rate = arg("--rate", [93])[0]
    print(f"rate {rate}/120, K = {banks}")
    print("S  MHz limit  overruns  overruns with pairs  (codewords: 3 x 121)")
    for s in arg("--s", [22, 34]):
        for mhz in arg("--clock", [200]):
            for lim in arg("--limits", [8, 10, 12, 14, 16]):
                fr = q.measured_frame(long_iter=lim)
                pairs = [lim if i in (17, 18, 61, 62) else v for i, v in enumerate(fr)]
                kw = dict(banks=banks, s_units=s, clock_hz=mhz * 1e6, finish_hz=40e6, limit=lim,
                          rate=rate)
                r = q.run([fr] * 3, **kw)
                r2 = q.run([pairs] * 3, **kw)
                print(f"{s:2d} {mhz:4d} {lim:5d} {r['overruns']:9d} {r2['overruns']:20d}", flush=True)
    print("input port (M decoder cycles/s): LLRs a symbol, burst, n a beat, n + 1 a beat")
    for bits in (4, 5):
        for burst in (False, True):
            now, before = (q.port_cycles(bits, burst, per_beat=k) / 1e6 for k in (0, 1))
            print(f"  {bits}  {'with' if burst else 'none':4s}  {now:6.1f}  {before:6.1f}")


if __name__ == "__main__":
    main()
