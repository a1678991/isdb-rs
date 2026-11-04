"""The simulation backends (`isdb_fpga.chain.simx`) against each other:
one bench through the stream driver's whole protocol (backpressure,
gaps, two output groups, signed and 64-bit ports, a memory, set / get /
tick / cycles) on the Python simulator, CXXRTL and Verilator, every
value the same; a vectors set (chain-2-phase) on both compiled backends,
every record the same; `FPGA_SIM_PROGRESS`'s chunks against one native
call; the cache key under one hash seed; the Verilator adapter's port list.
A backend that is not available here is skipped (`simx.available()`,
`simv.available()`)."""

import contextlib
import io
import os
import random
import subprocess
import sys
import unittest
from unittest.mock import patch

from amaranth.hdl import Elaboratable, Module, Signal, signed
from amaranth.lib.memory import Memory

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import simv, simx  # noqa: E402

VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build"))
BACKENDS = {"cxxrtl": simx.available, "verilator": simv.available}


def need(*backends):
    missing = [b for b in backends if not BACKENDS[b]()]
    return unittest.skipIf(missing, f"{', '.join(missing)} not available here")


class Bench(Elaboratable):
    """A stream sink: `ready` low one cycle in four (a free-running
    counter), each accepted record summed (40-bit signed into 48), its
    63-bit word mixed into a 64-bit signed register, its low 7 bits
    negated, its low 32 bits written to a memory read back a record
    later; `tick8`, combinational from the counter, high one cycle in
    eight. (A 64-bit unsigned port reads back as a signed int64 on the
    compiled backends.)"""

    def __init__(self):
        self.valid = Signal()
        self.ready = Signal()
        self.word = Signal(signed(40))
        self.wide = Signal(63)
        self.limit = Signal(16)
        self.free = Signal(16)
        self.count = Signal(16)
        self.total = Signal(signed(48))
        self.mix = Signal(signed(64))
        self.neg = Signal(signed(7))
        self.out = Signal()
        self.rdata = Signal(32)
        self.tick8 = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.mem = mem = Memory(shape=32, depth=64, init=[])
        wp = mem.write_port()
        rp = mem.read_port()
        go = self.valid & self.ready
        m.d.comb += [
            self.ready.eq((self.free[:2] != 3) & ((self.limit == 0) | (self.count < self.limit))),
            wp.addr.eq(self.count[:6]), wp.data.eq(self.word[:32]), wp.en.eq(go),
            rp.addr.eq(self.count[:6] - 1),
            self.rdata.eq(rp.data),
            self.tick8.eq(self.free[:3] == 0),
        ]
        m.d.sync += [self.free.eq(self.free + 1), self.out.eq(go)]
        with m.If(go):
            m.d.sync += [
                self.count.eq(self.count + 1),
                self.total.eq(self.total + self.word),
                self.mix.eq((self.mix ^ self.wide) + (self.wide << 1)),
                self.neg.eq(-self.word[:7].as_signed()),
            ]
        return m

    def ports(self):
        return {n: getattr(self, n) for n in ("valid", "ready", "word", "wide", "limit", "free",
                                              "count", "total", "mix", "neg", "out", "rdata",
                                              "tick8")}


def run_bench(backend, n=3000, limit=0, tail=17):
    """Every value the engine hands back, in order."""
    rng = random.Random(7)
    recs = [(rng.randrange(-(1 << 39), 1 << 39), rng.randrange(1 << 63)) for _ in range(n)]
    recs[:4] = [(-(1 << 39), (1 << 63) - 1), ((1 << 39) - 1, 0), (-1, 1 << 62), (0, 1)]
    gaps = [rng.choice((0, 0, 1, 3)) for _ in range(n)]
    b = Bench()
    e = simx.engine(b, b.ports(), backend, tag="simx-bench")
    g = e.group("out", ["count", "total", "mix", "neg", "rdata"])
    g8 = e.group("tick8", ["free", "count"])
    e.set("limit", limit)
    e.tick(5)
    e.run("valid", "ready", ["word", "wide"], recs, tail=tail, gaps=gaps)
    e.tick(3)
    out = (e.read(g), e.read(g8), [e.get(p) for p in b.ports()], e.cycles())
    e.close()
    return out


class EngineEquivalenceTest(unittest.TestCase):
    def test_bench_on_cxxrtl(self):
        if not simx.available():
            self.skipTest("cxxrtl not available here")
        self.compare("cxxrtl")

    def test_bench_on_verilator(self):
        if not simv.available():
            self.skipTest("verilator not available here")
        self.compare("verilator")

    def compare(self, backend):
        ref = run_bench("pysim")
        got = run_bench(backend)
        self.assertEqual(len(ref[0]), 3000)
        self.assertTrue(any(r[1] < 0 for r in ref[0]) and any(r[2] < 0 for r in ref[0]))
        # tick8: a group whose valid is combinational from a register,
        # sampled as it is after the edge (harness.cc's second CXXRTL step)
        self.assertEqual({f % 8 for f, _ in ref[1]}, {0})
        for i, what in enumerate(("records", "tick8 records", "ports", "cycles")):
            self.assertEqual(got[i], ref[i], f"{backend}: {what} differ from the Python simulator's")

    @need("cxxrtl", "verilator")
    def test_phase_set_on_both_compiled(self):
        from isdb_fpga.chain import stages
        from isdb_fpga.chain.sim import load

        path = os.path.join(VECTORS, "chain-2-phase.json")
        if not os.path.exists(path):
            self.skipTest(f"no {path} (mise run fpga:vectors)")
        doc = load(path)
        real = stages.engine
        out = {}

        def recording(got):
            def engine(*a, **kw):
                e = real(*a, **kw)
                read = e.read

                def recorded(g):
                    r = read(g)
                    got.append(r)
                    return r
                e.read = recorded
                return e
            return engine

        for backend in ("cxxrtl", "verilator"):
            out[backend] = []
            stages.engine = recording(out[backend])
            try:
                _, bad = stages.run_phase(doc, backend)
            finally:
                stages.engine = real
            self.assertEqual(bad, [], f"{backend}: " + "\n".join(bad[:5]))
        self.assertEqual(sum(len(r) for r in out["cxxrtl"]), 18592)
        self.assertEqual(out["verilator"], out["cxxrtl"])


class ClockBench(Elaboratable):
    """A stream sink in `sync` with a counter in each of the engine's faster
    clocks, sampled with every record: where the clocks' edges fall against
    the records."""

    def __init__(self):
        self.valid = Signal()
        self.ready = Signal()
        self.word = Signal(16)
        self.count = Signal(16)
        self.total = Signal(32)
        self.free = Signal(2)
        self.fast = Signal(32)
        self.ldpc = Signal(32)
        self.out = Signal()

    def elaborate(self, platform):
        m = Module()
        m.d.comb += self.ready.eq(self.free != 3)
        m.d.sync += [self.free.eq(self.free + 1), self.out.eq(self.valid & self.ready)]
        with m.If(self.valid & self.ready):
            m.d.sync += [self.count.eq(self.count + 1), self.total.eq(self.total + self.word)]
        m.d.fast += self.fast.eq(self.fast + 1)
        m.d.ldpc += self.ldpc.eq(self.ldpc + 1)
        return m

    def ports(self):
        return {n: getattr(self, n) for n in ("valid", "ready", "word", "count", "total", "fast",
                                              "ldpc", "out")}


def run_clocks(backend, n, tail):
    """The three clocks' trace, the ports, the cycles, what `run` returned
    (the Python simulator's `run` is queued: None)."""
    b = ClockBench()
    e = simx.engine(b, b.ports(), backend, tag="simx-clocks")
    g = e.group("out", ["count", "total", "fast", "ldpc"])
    c = e.run("valid", "ready", ["word"], [(i % 251,) for i in range(n)], tail=tail,
              gaps=[i % 3 for i in range(n)])
    out = (e.read(g), [e.get(p) for p in b.ports()], e.cycles(), c)
    e.close()
    return out


def chunked(chunk):
    """FPGA_SIM_PROGRESS on, `chunk` records or drain cycles per native call."""
    return patch.object(simx, "PROGRESS_CHUNK", chunk), patch.dict(os.environ, FPGA_SIM_PROGRESS="1")


class ProgressTest(unittest.TestCase):
    """`FPGA_SIM_PROGRESS=1` on each compiled backend: the same clock
    sequence as one native call, chunk boundaries inside the records (a
    `ready` low across one), the gaps and the drain."""

    def compiled(self):
        return [b for b in ("cxxrtl", "verilator") if BACKENDS[b]()]

    def test_chunks_keep_every_value(self):
        for backend in self.compiled():
            with self.subTest(backend=backend):
                with patch.dict(os.environ, FPGA_SIM_PROGRESS="0"):
                    ref = run_bench(backend, tail=2500)
                for chunk in (7, 1000):
                    out = io.StringIO()
                    a, b = chunked(chunk)
                    with a, b, contextlib.redirect_stderr(out):
                        got = run_bench(backend, tail=2500)
                    for i, what in enumerate(("records", "tick8 records", "ports", "cycles")):
                        self.assertEqual(got[i], ref[i], f"chunk {chunk}: {what} differ")
                    self.assertIn("simx-bench: elaborating", out.getvalue())
                    self.assertIn("simx-bench: cache hit", out.getvalue())
                    self.assertIn("run: 3000/3000 records, 2500/2500 drain cycles", out.getvalue())

    def test_three_clocks(self):
        for backend in self.compiled():
            with self.subTest(backend=backend):
                with patch.dict(os.environ, FPGA_SIM_PROGRESS="0"):
                    ref = run_clocks(backend, 53, 31)
                    self.assertEqual(ref[:3], run_clocks("pysim", 53, 31)[:3])
                self.assertEqual(ref[3], ref[2])
                a, b = chunked(5)
                with a, b, contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(run_clocks(backend, 53, 31), ref)

    def test_no_records(self):
        for backend in self.compiled():
            with self.subTest(backend=backend):
                with patch.dict(os.environ, FPGA_SIM_PROGRESS="0"):
                    ref = run_clocks(backend, 0, 31)
                a, b = chunked(5)
                with a, b, contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(run_clocks(backend, 0, 31), ref)
                self.assertEqual(ref[2], 31)

    def stalled(self, backend, limit):
        """The stalled record's index in the whole run, and the message."""
        b = Bench()
        e = simx.engine(b, b.ports(), backend, tag="simx-bench")
        e.set("limit", limit)
        e.stall_limit = 3
        try:
            with self.assertRaises(RuntimeError) as err:
                e.run("valid", "ready", ["word", "wide"], [(1, 1)] * 40)
            return e.stalled, str(err.exception)
        finally:
            e.close()

    def test_stall_index_is_the_whole_runs(self):
        for backend in self.compiled():
            with self.subTest(backend=backend):
                # record 25, inside the fourth chunk of seven
                with patch.dict(os.environ, FPGA_SIM_PROGRESS="0"):
                    ref = self.stalled(backend, 25)
                self.assertEqual(ref[0], 25)
                a, b = chunked(7)
                with a, b, contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(self.stalled(backend, 25), ref)


KEY = """
import hashlib, sys
sys.path[:0] = {path!r}
from amaranth.back import rtlil
from sim_hashseed import design
from isdb_fpga.chain import simx
d, ports = design("three")
il = rtlil.convert(d, name="top", ports=list(ports.values()))
print(hashlib.sha256(simx._Cxx._key(il)).hexdigest())
"""


class HashSeedTest(unittest.TestCase):
    def test_one_seed_one_key(self):
        # what PYTHONHASHSEED=0 in the fpga:* tasks relies on: a design of
        # three clock domains has one key in every new process
        fpga = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
        code = KEY.format(path=[fpga, os.path.join(fpga, "tools")])
        env = dict(os.environ, PYTHONHASHSEED="0")
        keys = {subprocess.run([sys.executable, "-c", code], env=env, capture_output=True, text=True,
                               check=True).stdout for _ in range(2)}
        self.assertEqual(len(keys), 1, keys)
        self.assertRegex(keys.pop(), r"^[0-9a-f]{64}\n$")


class AdapterTest(unittest.TestCase):
    def test_ports(self):
        from amaranth.back import rtlil

        b = Bench()
        il = rtlil.convert(b, name="top", ports=list(b.ports().values()))
        ps = {n: (w, d) for n, w, d in simv.ports(il)}
        self.assertEqual(ps["valid"], (1, "input"))
        self.assertEqual(ps["word"], (40, "input"))
        self.assertEqual(ps["mix"], (64, "output"))
        self.assertEqual(ps["neg"], (7, "output"))
        self.assertEqual(ps["clk"], (1, "input"))
        self.assertEqual(set(ps) - {"clk", "rst"}, set(b.ports()))
        hdr = simv.adapter(il)
        self.assertIn("objects[", hdr)
        self.assertIn("model->mix >> 32", hdr)

    def test_wider_than_64_refused(self):
        from amaranth.back import rtlil

        m = Module()
        a, y = Signal(65), Signal(65)
        m.d.sync += y.eq(a)
        il = rtlil.convert(m, name="top", ports=[a, y])
        with self.assertRaisesRegex(ValueError, "65 bits"):
            simv.ports(il)


if __name__ == "__main__":
    unittest.main()
