"""Stream simulation of the chain's blocks: one engine API, three backends.

`pysim` (always available): Amaranth's Python simulator.

`cxxrtl` (a compiled accelerator): the block is written out with Yosys's
CXXRTL backend (`write_cxxrtl` of the venv's YoWASP Yosys) and compiled
with the system C++ compiler (`$CXX`, else `c++`) into a shared library
together with `harness.cc`, a generic stream driver loaded through
ctypes. The CXXRTL runtime headers come from the yowasp-yosys wheel
(`cxxrtl_include()`: share/include/backends/cxxrtl/runtime, passed with
-I; an editor's clangd will not find `cxxrtl/capi/cxxrtl_capi.h` in
harness.cc without that include path, which is harmless). Nothing else is
needed: `python -m isdb_fpga.chain.sim VECTORS --backend cxxrtl` builds
and caches the library in fpga/build/cxxrtl/ (keyed by the design's
RTLIL, harness.cc and the compiler flags) on first use, seconds to
minutes per block. `available()` says whether it can be used.

`verilator` (`simv.py`, the faster compiled accelerator): the same
harness.cc around the design Verilated (Verilator 5 on PATH), cached in
fpga/build/verilator/. `simv.available()` says whether it can be used.

`compiled_backend()` is the compiled backend the tests use for the whole
sets: `$FPGA_SIM` if it names one, else verilator where it is available,
else cxxrtl where it is, else none (those tests skip).

The hash seed: Amaranth collects a design's implicit clock domains in sets,
so with more than one of them the RTLIL's port order, and with it the
library's key, follows Python's hash seed (`tools/sim_hashseed.py`). The
`fpga:*` tasks that simulate set PYTHONHASHSEED=0, so test workers, later
runs and CI share one library per design; a direct command needs the same
variable before Python starts, or it builds its own library.

FPGA_SIM_PROGRESS=1 (the compiled backends): the elaboration, the cache's
answer and, every 10 s of a `run`, its records, drain cycles and cycles per
second on stderr. `run` then hands the harness `PROGRESS_CHUNK` records or
drain cycles at a time, the same clock sequence (harness.cc: a call ends
between two cycles, with `valid` low; only a record's wait for `ready`
counts against the stall limit, and it stays inside one call). A record
that blocks still spends its whole stall limit inside one native call.

Why: the chain's vector sets are long (chain-1-sync: 2.24 M symbols,
chain-6-chain: 5.59 M) and the Python simulator runs the stage-1 block at
about 6 500 cycles per second against about 1 M for the compiled ones
(README "The ISDB-S3 chain"). Every backend applies the same per-cycle
sequence (inputs set, a clock edge, the output groups sampled after it),
and the tests run them on the same sets.

Combinational outputs: the harness reads a group after the edge as the
logic is then (an output combinational from registers included: CXXRTL
needs a second step for it, harness.cc), and before the first edge as
the initial state gives it, on every backend alike; tests/test_simx.py
checks both compiled backends against the Python simulator on a bench of
such outputs, `synth/sim_compare.py` against each other on the tests'
benches, value for value.

Clocks: the design's `sync` domain is the symbol clock (one engine cycle
per period); the domains named in `clocks` (default `CLOCKS`: `fast` at
4x, the chain's time-multiplexed datapaths, and `ldpc` at 5x, the
decoder's 200 MHz) run phase-aligned with it, one edge of each on every
edge of `sync` (both backends: Amaranth's simulator with the clocks'
phases aligned, the harness with the faster edges scheduled between the
cycles' edges). The engine's inputs change right after an edge of `sync`,
as the slow domain's registers do; the ready and output-group signals must
be the slow domain's (they are read once per cycle).

Engine API:
  e = engine(dut, ports, backend)   ports: the top-level signals by name
  e.set(name, value); e.get(name); e.tick(n)
  g = e.group(valid, [data...])     records sampled whenever valid is high
  e.run(valid, ready, [fields...], records, tail, gaps=None)
  e.read(g) -> list of tuples
"""

import ctypes
import hashlib
import os
import shutil
import fcntl
import subprocess
import sys
import time

from amaranth.back import rtlil
from amaranth.sim import Simulator

HERE = os.path.dirname(__file__)
BUILD = os.path.join(HERE, "..", "..", "build", "cxxrtl")
# -O1: -O0 on the generated code took 98 minutes and 30 GB here (measured
# on the whole chain, 2026-11-02); -O1 takes 214 s and 1.8 GB.
CXXFLAGS = ["-O1", "-std=c++17", "-shared", "-fPIC", "-w"]
# the related clocks the engine drives (domain: edges per symbol clock)
CLOCKS = {"fast": 4, "ldpc": 5}
# FPGA_SIM_PROGRESS=1: the records or drain cycles of one native call
PROGRESS_CHUNK = 10_000


def yosys_path():
    return os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")


def cxxrtl_include():
    import yowasp_yosys

    base = os.path.dirname(yowasp_yosys.__file__)
    return os.path.join(base, "share", "include", "backends", "cxxrtl", "runtime")


def have_cxx():
    return shutil.which(os.environ.get("CXX", "c++")) is not None


def available():
    """Whether the cxxrtl backend can be built here (a C++ compiler and
    the CXXRTL runtime headers)."""
    try:
        inc = cxxrtl_include()
    except ImportError:
        return False
    return have_cxx() and os.path.exists(os.path.join(inc, "cxxrtl", "capi", "cxxrtl_capi.h"))


COMPILED = ("verilator", "cxxrtl")  # the compiled backends, the preferred first


def compiled_backend():
    """The compiled backend used where one is wanted: `$FPGA_SIM` if it
    names one, else verilator where it is available, else cxxrtl where it
    is, else None."""
    env = os.environ.get("FPGA_SIM")
    if env in COMPILED:
        return env
    from . import simv

    if simv.available():
        return "verilator"
    return "cxxrtl" if available() else None


def default_backend():
    """`$FPGA_SIM` (pysim, cxxrtl or verilator), else pysim."""
    return os.environ.get("FPGA_SIM", "pysim")


def _signame(s):
    return s.name


def _progress():
    return os.environ.get("FPGA_SIM_PROGRESS") == "1"


def _report(message):
    print(f"simulation {message}", file=sys.stderr, flush=True)


class _Cxx:
    build_dir = BUILD

    def __init__(self, dut, ports, tag, clocks=CLOCKS):
        started = time.monotonic()
        if _progress():
            _report(f"{tag}: elaborating")
        il = rtlil.convert(dut, name="top", ports=list(ports.values()))
        key = hashlib.sha256(self._key(il)).hexdigest()[:16]
        os.makedirs(self.build_dir, exist_ok=True)
        so = os.path.join(self.build_dir, f"{tag}-{key}.so")
        if _progress():
            _report(f"{tag}: {'cache hit' if os.path.exists(so) else 'build or wait'} "
                    f"{os.path.basename(so)} after {time.monotonic() - started:.1f} s")
        if not os.path.exists(so):
            # One builder per design at a time: parallel test workers (xdist)
            # may want the same library, and the write_cxxrtl input and output
            # must not be shared half-written. The lock is held for the whole
            # build; a waiter finds the finished .so and skips the build.
            with open(f"{so}.lock", "w") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                if not os.path.exists(so):
                    self._build(il, so)
        lib = ctypes.CDLL(so)
        lib.h_create.restype = ctypes.c_void_p
        lib.h_port.restype = ctypes.c_int
        lib.h_port.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        lib.h_set.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int64]
        lib.h_get.restype = ctypes.c_int64
        lib.h_get.argtypes = [ctypes.c_void_p, ctypes.c_int]
        lib.h_tick.argtypes = [ctypes.c_void_p, ctypes.c_int]
        lib.h_group.restype = ctypes.c_int
        lib.h_group.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_int), ctypes.c_int]
        lib.h_group_len.restype = ctypes.c_int64
        lib.h_group_len.argtypes = [ctypes.c_void_p, ctypes.c_int]
        lib.h_group_read.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.POINTER(ctypes.c_int64)]
        lib.h_cycles.restype = ctypes.c_uint64
        lib.h_cycles.argtypes = [ctypes.c_void_p]
        lib.h_run.restype = ctypes.c_uint64
        lib.h_run.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.c_int),
                              ctypes.c_int, ctypes.POINTER(ctypes.c_int64), ctypes.c_int64,
                              ctypes.POINTER(ctypes.c_int32), ctypes.c_int64]
        lib.h_destroy.argtypes = [ctypes.c_void_p]
        lib.h_stall_limit.argtypes = [ctypes.c_int64]
        lib.h_stalled.restype = ctypes.c_int64
        lib.h_fast_clock.restype = ctypes.c_int
        lib.h_fast_clock.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        self.lib = lib
        self.h = lib.h_create()
        for name, ratio in clocks.items():
            lib.h_fast_clock(self.h, name.encode(), ratio)
        self.idx = {}
        self.width = {}
        for name, sig in ports.items():
            i = lib.h_port(self.h, _signame(sig).encode(), int(sig.shape().signed))
            if i < 0:
                raise RuntimeError(f"port {name} ({_signame(sig)}) not in the compiled design")
            self.idx[name] = i
            self.width[name] = len(sig)
        self.groups = []
        self.stall_limit = 20_000_000

    def set(self, name, v):
        self.lib.h_set(self.h, self.idx[name], int(v))

    def get(self, name):
        return self.lib.h_get(self.h, self.idx[name])

    def tick(self, n=1):
        self.lib.h_tick(self.h, n)

    def group(self, valid, data):
        arr = (ctypes.c_int * len(data))(*[self.idx[d] for d in data])
        g = self.lib.h_group(self.h, self.idx[valid], arr, len(data))
        self.groups.append(len(data))
        return g

    def read(self, g):
        n = self.lib.h_group_len(self.h, g)
        buf = (ctypes.c_int64 * n)()
        self.lib.h_group_read(self.h, g, buf)
        k = self.groups[g]
        flat = list(buf)
        return [tuple(flat[i:i + k]) for i in range(0, n, k)]

    def cycles(self):
        return self.lib.h_cycles(self.h)

    def run(self, valid, ready, fields, records, tail=0, gaps=None):
        k = len(fields)
        n = len(records) // k if records and not isinstance(records[0], (tuple, list)) else len(records)
        if records and isinstance(records[0], (tuple, list)):
            flat = [v for r in records for v in r]
        else:
            flat = list(records)
        rec = (ctypes.c_int64 * len(flat))(*flat)
        fl = (ctypes.c_int * k)(*[self.idx[f] for f in fields])
        gp = (ctypes.c_int32 * n)(*gaps) if gaps is not None else None
        self.lib.h_stall_limit(self.stall_limit)
        v, r = self.idx[valid], self.idx[ready] if ready else -1
        if _progress():
            return self._run_chunks(v, r, fl, k, rec, n, gp, tail)
        c = self.lib.h_run(self.h, v, r, fl, k, rec, n, gp, tail)
        self.stalled = self.lib.h_stalled()
        if self.stalled >= 0:
            raise RuntimeError(f"record {self.stalled} not accepted within {self.stall_limit} cycles")
        return c

    def _run_chunks(self, v, r, fl, k, rec, n, gp, tail):
        """`run`'s native call in pieces, with reports between them (the
        module's docstring: the same clock sequence). `stalled` stays the
        record's index in the whole run."""
        int64, int32 = ctypes.c_int64, ctypes.c_int32
        started = reported = time.monotonic()
        cycles = 0

        def report(done, drained):
            nonlocal reported
            now = time.monotonic()
            if now - reported >= 10 or (done == n and drained == tail):
                _report(f"run: {done}/{n} records, {drained}/{tail} drain cycles, {cycles} cycles, "
                        f"{cycles / max(now - started, 1e-9):.0f} cycles/s")
                reported = now

        for at in range(0, n, PROGRESS_CHUNK):
            count = min(PROGRESS_CHUNK, n - at)
            rp = ctypes.cast(ctypes.byref(rec, at * k * ctypes.sizeof(int64)), ctypes.POINTER(int64))
            gaps = (None if gp is None else
                    ctypes.cast(ctypes.byref(gp, at * ctypes.sizeof(int32)), ctypes.POINTER(int32)))
            cycles += self.lib.h_run(self.h, v, r, fl, k, rp, count, gaps, 0)
            stalled = self.lib.h_stalled()
            self.stalled = at + stalled if stalled >= 0 else -1
            if self.stalled >= 0:
                raise RuntimeError(f"record {self.stalled} not accepted within {self.stall_limit} cycles")
            report(at + count, 0)
        self.stalled = -1
        for at in range(0, tail, PROGRESS_CHUNK):
            count = min(PROGRESS_CHUNK, tail - at)
            cycles += self.lib.h_run(self.h, v, r, fl, k, rec, 0, None, count)
            report(n, at + count)
        return cycles

    def close(self):
        if self.h:
            self.lib.h_destroy(self.h)
            self.h = None

    @staticmethod
    def _key(il):
        # The library depends on the design, the harness and the compiler
        # flags, and on nothing else: a change to this module's Python side
        # must not invalidate the libraries cached across CI runs.
        return (il.encode() + open(os.path.join(HERE, "harness.cc"), "rb").read()
                + " ".join(CXXFLAGS).encode() + b"write_cxxrtl -g2")

    @staticmethod
    def _build(il, so):
        pid = os.getpid()
        ilp, cc, tmp = f"{so}.{pid}.il", f"{so}.{pid}.cc", f"{so}.{pid}.tmp"
        with open(ilp, "w") as f:
            f.write(il)
        r = subprocess.run(
            [yosys_path(), "-q", "-p",
             f"read_rtlil {ilp}; hierarchy -top top; write_cxxrtl -g2 {cc}"],
            capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError("write_cxxrtl: " + r.stdout + r.stderr)
        r = subprocess.run(
            [os.environ.get("CXX", "c++"), *CXXFLAGS,
             "-I", cxxrtl_include(), cc,
             os.path.join(cxxrtl_include(), "cxxrtl", "capi", "cxxrtl_capi.cc"),
             os.path.join(HERE, "harness.cc"), "-o", tmp],
            capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError("c++: " + r.stderr[-4000:])
        os.replace(tmp, so)
        # the generated sources are kept for inspection under the library's name
        os.replace(ilp, so[:-3] + ".il")
        os.replace(cc, so[:-3] + ".cc")

class _Py:
    """The Python simulator with the same per-cycle sequence, one-shot:
    `set`, `tick` and `run` are queued and executed in one testbench when
    a result is first asked for (`get`, `read`, `cycles`); nothing can be
    queued after that (Amaranth cannot add a testbench to a simulation
    that has run)."""

    def __init__(self, dut, ports, tag, clocks=CLOCKS):
        self.clocks = clocks
        self.ports = ports
        self.dut = dut
        self.groups = []
        self.out = []
        self.ncycles = 0
        self.ops = []
        self.done = False
        self.final = {}

    def _sample(self, ctx):
        for gi, (v, ds) in enumerate(self.groups):
            if ctx.get(self.ports[v]):
                self.out[gi].append(tuple(ctx.get(self.ports[d]) for d in ds))

    async def _tick(self, ctx, n):
        for _ in range(n):
            await ctx.tick()
            self.ncycles += 1
            self._sample(ctx)

    def _queue(self, op):
        if self.done:
            raise RuntimeError("the pysim engine is one-shot: queue everything before reading")
        self.ops.append(op)

    def set(self, name, v):
        sig = self.ports[name]

        async def op(ctx):
            ctx.set(sig, v)
        self._queue(op)

    def tick(self, n=1):
        async def op(ctx):
            await self._tick(ctx, n)
        self._queue(op)

    def group(self, valid, data):
        self.groups.append((valid, list(data)))
        self.out.append([])
        return len(self.groups) - 1

    def run(self, valid, ready, fields, records, tail=0, gaps=None):
        if records and not isinstance(records[0], (tuple, list)):
            k = len(fields)
            records = [records[i:i + k] for i in range(0, len(records), k)]
        ps = self.ports

        async def op(ctx):
            for i, r in enumerate(records):
                for f, v in zip(fields, r):
                    ctx.set(ps[f], v)
                ctx.set(ps[valid], 1)
                while True:
                    ok = ready is None or ctx.get(ps[ready])
                    await ctx.tick()
                    self.ncycles += 1
                    self._sample(ctx)
                    if ok:
                        break
                ctx.set(ps[valid], 0)
                if gaps is not None:
                    await self._tick(ctx, gaps[i])
            await self._tick(ctx, tail)
        self._queue(op)

    def _flush(self):
        if self.done:
            return
        self.done = True
        sim = Simulator(self.dut)
        sim.add_clock(1e-8)
        for name, ratio in self.clocks.items():
            # the first edge with sync's (at half its period)
            sim.add_clock(1e-8 / ratio, phase=5e-9, domain=name, if_exists=True)
        ops = self.ops

        async def bench(ctx):
            for op in ops:
                await op(ctx)
            for name, sig in self.ports.items():
                self.final[name] = ctx.get(sig)

        sim.add_testbench(bench)
        sim.run()

    def get(self, name):
        self._flush()
        return self.final[name]

    def read(self, g):
        self._flush()
        r, self.out[g] = self.out[g], []
        return r

    def cycles(self):
        self._flush()
        return self.ncycles

    def close(self):
        pass


def engine(dut, ports, backend=None, tag="dut", clocks=CLOCKS):
    backend = backend or default_backend()
    if backend == "verilator":
        from .simv import Verilator

        return Verilator(dut, ports, tag, clocks)
    if backend == "cxxrtl":
        return _Cxx(dut, ports, tag, clocks)
    if backend == "pysim":
        return _Py(dut, ports, tag, clocks)
    raise ValueError(f"unknown simulation backend {backend!r} (pysim, cxxrtl, verilator)")
