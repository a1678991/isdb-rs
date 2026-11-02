"""Measure the LDPC decoder's pipeline in simulation: the cycle each
step's entry 0 presents its lane read address and the cycle each write
of that step's entry 0 reaches the memory, so the HDL's real write
latency is compared with `ldpc.WRITE_LATENCY`, which the hazard schedule
(`ldpcsched.gaps`) assumes. A real latency larger than the constant would
let a step read a word before its last write: this script fails then.

python synth/ldpc_latency.py
"""

import os
import sys

from amaranth.sim import Simulator

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga import ldpc, ldpcmodel, ldpcsched  # noqa: E402


def main(argv):
    rows = ldpcmodel.TOYS["toy-a"]
    s = 17
    dut = ldpc.LdpcDecoder(s, {2: rows}, ngroups=6)
    code = ldpcmodel.Code(6 * ldpcmodel.GROUP, rows)
    llr = ldpcmodel.noisy(code, (-3.0,), ldpcmodel.Rng(3))[0]
    sim = Simulator(dut)
    sim.add_clock(5e-9)
    found = {}

    async def bench(ctx):
        for pos, v in enumerate(llr):
            ctx.set(dut.begin, pos == 0)
            ctx.set(dut.in_valid, 1)
            ctx.set(dut.in_pos, pos)
            ctx.set(dut.in_llr, v)
            await ctx.tick()
        ctx.set(dut.begin, 0)
        ctx.set(dut.in_valid, 0)
        ctx.set(dut.start, 1)
        ctx.set(dut.st_rate, 0)
        ctx.set(dut.st_max_iter, 3)
        ctx.set(dut.st_offset, 1)
        await ctx.tick()
        ctx.set(dut.start, 0)
        mod = dut._probe
        while not ctx.get(dut.busy):  # the commit reaches the queue after the last write
            await ctx.tick()
        clock = 0
        while ctx.get(dut.busy) and clock < 4000:
            if ctx.get(mod["read_v"]) and ctx.get(mod["read_k"]) == 0 and "read" not in found:
                found["read"] = clock
            if ctx.get(mod["write_v"]) and ctx.get(mod["write_k"]) == 0 and "read" in found \
                    and "write" not in found:
                found["write"] = clock
            await ctx.tick()
            clock += 1

    sim.add_testbench(bench)
    sim.run()
    d = ldpcsched.Layout(rows, 6, s).d  # the layer degree (check 0 has one edge less)
    real = found["write"] - found["read"] - d
    print(f"entry 0: read address at {found['read']}, write at {found['write']}, d = {d}: "
          f"write latency {real} (ldpc.WRITE_LATENCY = {ldpc.WRITE_LATENCY})")
    return 0 if real <= ldpc.WRITE_LATENCY else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
