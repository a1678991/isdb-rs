"""Throwaway (not merged): simulate the `tx_sketch` mux datapath against a
plain Python model of the same arithmetic (floor phase with half-phase
rows, A/B rows by parity, sign/zero mux, round >> S, 12-bit saturation,
zero symbol on underrun), bit for bit.

python synth/tx_sketch_sim.py PHASES SPAN [CLOCKS]
"""

import os
import random
import sys

from amaranth.sim import Simulator

sys.path.insert(0, os.path.dirname(__file__))
from tx_sketch import FRAC, SPS, TxInterp, bank  # noqa: E402


def model(P, span, step, codes, valid):
    a, b, S = bank(P, span, half=True)
    w = 2 * span
    pb = P.bit_length() - 1
    acc, line, idx, out = 0, [8] * w, 0, []
    for v in valid:
        q = acc >> (FRAC - pb)
        y = [0, 0]
        for t, k in enumerate(line):
            if k >= 8:
                continue
            val = b[q][t] if k & 1 else a[q][t]
            si = [1, 1, 0, -1, -1, -1, 0, 1][k]
            sq = [0, 1, 1, 1, 0, -1, -1, -1][k]
            y[0] += si * val
            y[1] += sq * val
        out.append(tuple(max(-2048, min(2047, (s + (1 << (S - 1))) >> S)) for s in y))
        acc += step
        if acc >> FRAC:
            acc -= 1 << FRAC
            line = [codes[idx] if v else 8] + line[:-1]
            if v:
                idx += 1
    return out


def main(argv):
    P, span = int(argv[1]), int(argv[2])
    n = int(argv[3]) if len(argv) > 3 else 3000
    rng = random.Random(1)
    codes = [rng.randrange(8) for _ in range(n)]
    # valid most of the time, with underrun gaps
    valid = [not (200 <= i < 230 or rng.random() < 0.02) for i in range(n)]
    step = round((1 << FRAC) / SPS)
    want = model(P, span, step, codes, valid)
    dut = TxInterp("mux", P, span)
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    got = []

    async def bench(ctx):
        ctx.set(dut.step, step)
        idx = 0
        for i in range(n + 20):
            v = valid[i] if i < n else 1
            ctx.set(dut.s_valid, v)
            ctx.set(dut.s_code, codes[idx] if idx < n else 0)
            if ctx.get(dut.s_ready):
                idx += 1
            await ctx.tick()
            o = (ctx.get(dut.o_i), ctx.get(dut.o_q))
            got.append(tuple(x - 4096 if x >= 2048 else x for x in o))

    sim.add_testbench(bench)
    sim.run()
    for lat in range(30):
        seg = got[lat:lat + n - 40]
        if seg == want[:len(seg)]:
            nz = sum(1 for s in seg if s != (0, 0))
            print(f"P {P} span {span}: bit-exact over {len(seg)} samples at latency {lat} clocks "
                  f"({nz} non-zero, {sum(1 for v in valid if not v)} underrun clocks)")
            return 0
    print("MISMATCH", got[:20], want[:20])
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
