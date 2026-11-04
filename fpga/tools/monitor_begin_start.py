"""The `bank_accounting` monitor on a `start` and the next `begin` in one
cycle (fpga/README.md "Formal checks", "Found"): the decoder's ports
allow it (the docstring: "`start` and `begin` of the next codeword may
share a cycle"), the formal `banks` harness allows it and passes, the
monitor counts a commit only on `start & ~begin`. The bridge never sends
the two together, so no simulation of the chain reaches it.

    uv run --locked python tools/monitor_begin_start.py

Prints the monitors' report after: `begin` on context 0, then `start` with
`begin` on context 0, then a few idle cycles (Amaranth's simulator, the
toy-a decoder).
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from amaranth.sim import Simulator  # noqa: E402

from isdb_fpga import ldpcmodel  # noqa: E402
from isdb_fpga.chain import monitors  # noqa: E402
from isdb_fpga.ldpc import LdpcDecoder  # noqa: E402


def main():
    dut = LdpcDecoder(17, {2: ldpcmodel.TOYS["toy-a"]}, ngroups=6, banks=3, monitors=True)
    col = monitors.Collector(dut)
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    values = {}

    async def bench(ctx):
        for begin, start in ((1, 0), (1, 1), (0, 0), (0, 0), (0, 0)):
            ctx.set(dut.begin, begin)
            ctx.set(dut.start, start)
            ctx.set(dut.in_ctx, 0)
            await ctx.tick()
        ctx.set(dut.begin, 0)
        ctx.set(dut.start, 0)
        await ctx.tick()
        values["held"] = ctx.get(dut.held)
        for name, sig in col.ports().items():
            values[name] = ctx.get(sig)

    sim.add_testbench(bench)
    sim.run()
    print(f"held {values['held']}")
    report = col.report(values.get)
    print("\n".join(report) if report else "no monitor fired")
    return 0


if __name__ == "__main__":
    sys.exit(main())
