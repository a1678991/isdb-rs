"""The decoder's queue (`isdb_fpga.ldpcqueue`) at the chain's pace, by S,
clock and iteration limit: the overruns of three frames of the measured
distribution (`measured_frame`: mean 2.1, three isolated codewords at the
limit) and of the same with two adjacent pairs at the limit (slots 17-18,
61-62). For the fit's decoder decisions (fpga/README.md): the 160 MHz
question at S = 34 and the S = 22 fallback at 200 MHz.

python tools/queue_sweep.py [--s 22,34] [--clock 160,200] [--limits 8,10,12,14,16] [--banks 3]
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
    print("S  MHz limit  overruns  overruns with pairs  (codewords: 3 x 121)")
    for s in arg("--s", [22, 34]):
        for mhz in arg("--clock", [200]):
            for lim in arg("--limits", [8, 10, 12, 14, 16]):
                fr = q.measured_frame(long_iter=lim)
                pairs = [lim if i in (17, 18, 61, 62) else v for i, v in enumerate(fr)]
                kw = dict(banks=banks, s_units=s, clock_hz=mhz * 1e6, finish_hz=40e6, limit=lim)
                r = q.run([fr] * 3, **kw)
                r2 = q.run([pairs] * 3, **kw)
                print(f"{s:2d} {mhz:4d} {lim:5d} {r['overruns']:9d} {r2['overruns']:20d}", flush=True)


if __name__ == "__main__":
    main()
