"""Stream simulation of the chain's blocks: one engine API, two backends.

`pysim` (the default, always available): Amaranth's Python simulator.

`cxxrtl` (an optional accelerator): the block is written out with Yosys's
CXXRTL backend (`write_cxxrtl` of the venv's YoWASP Yosys) and compiled
with the system C++ compiler (`$CXX`, else `c++`) into a shared library
together with `harness.cc`, a generic stream driver loaded through
ctypes. The CXXRTL runtime headers come from the yowasp-yosys wheel
(`cxxrtl_include()`: share/include/backends/cxxrtl/runtime, passed with
-I; an editor's clangd will not find `cxxrtl/capi/cxxrtl_capi.h` in
harness.cc without that include path, which is harmless). Nothing else is
needed: `python -m isdb_fpga.chain.sim VECTORS --backend cxxrtl` builds
and caches the library in fpga/build/cxxrtl/ (keyed by the design's
RTLIL, harness.cc and this file) on first use, about 5-60 s per block.
`available()` says whether it can be used; the tests that need it skip
otherwise.

Why: the chain's vector sets are long (chain-1-sync: 2.24 M symbols,
chain-6-chain: 5.59 M) and the Python simulator runs the stage-1 block at
about 6 500 cycles per second against about 1.1 M for the compiled one
(README "The ISDB-S3 chain"). Both backends apply the same per-cycle
sequence (inputs set, a clock edge, the output groups sampled after it),
and the tests run both on the same prefix.

Engine API:
  e = engine(dut, ports, backend)   ports: the top-level signals by name
  e.set(name, value); e.get(name); e.tick(n)
  g = e.group(valid, [data...])     records sampled whenever valid is high
  e.run(valid, ready, [fields...], records, tail, gaps=None)
  e.read(g) -> list of tuples
The compiled libraries are cached in fpga/build/cxxrtl/ by the hash of the
design's RTLIL.
"""

import ctypes
import hashlib
import os
import shutil
import subprocess
import sys

from amaranth.back import rtlil
from amaranth.sim import Simulator

HERE = os.path.dirname(__file__)
BUILD = os.path.join(HERE, "..", "..", "build", "cxxrtl")


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


def default_backend():
    """`$FPGA_SIM` (pysim or cxxrtl), else pysim."""
    return os.environ.get("FPGA_SIM", "pysim")


def _signame(s):
    return s.name


class _Cxx:
    def __init__(self, dut, ports, tag):
        il = rtlil.convert(dut, name="top", ports=list(ports.values()))
        key = hashlib.sha256(il.encode() + open(os.path.join(HERE, "harness.cc"), "rb").read()
                             + open(__file__, "rb").read()).hexdigest()[:16]
        os.makedirs(BUILD, exist_ok=True)
        so = os.path.join(BUILD, f"{tag}-{key}.so")
        if not os.path.exists(so):
            ilp = os.path.join(BUILD, f"{tag}-{key}.il")
            cc = os.path.join(BUILD, f"{tag}-{key}.cc")
            with open(ilp, "w") as f:
                f.write(il)
            r = subprocess.run(
                [yosys_path(), "-q", "-p",
                 f"read_rtlil {ilp}; hierarchy -top top; write_cxxrtl -g2 {cc}"],
                capture_output=True, text=True)
            if r.returncode:
                raise RuntimeError("write_cxxrtl: " + r.stdout + r.stderr)
            tmp = so + ".tmp"
            r = subprocess.run(
                [os.environ.get("CXX", "c++"), "-O1", "-std=c++17", "-shared", "-fPIC", "-w",
                 "-I", cxxrtl_include(), cc,
                 os.path.join(cxxrtl_include(), "cxxrtl", "capi", "cxxrtl_capi.cc"),
                 os.path.join(HERE, "harness.cc"), "-o", tmp],
                capture_output=True, text=True)
            if r.returncode:
                raise RuntimeError("c++: " + r.stderr[-4000:])
            os.replace(tmp, so)
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
        self.lib = lib
        self.h = lib.h_create()
        self.idx = {}
        self.width = {}
        for name, sig in ports.items():
            i = lib.h_port(self.h, _signame(sig).encode(), int(sig.shape().signed))
            if i < 0:
                raise RuntimeError(f"port {name} ({_signame(sig)}) not in the compiled design")
            self.idx[name] = i
            self.width[name] = len(sig)
        self.groups = []

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
        return self.lib.h_run(self.h, self.idx[valid], self.idx[ready] if ready else -1, fl, k, rec, n,
                              gp, tail)

    def close(self):
        if self.h:
            self.lib.h_destroy(self.h)
            self.h = None


class _Py:
    """The Python simulator with the same per-cycle sequence, one-shot:
    `set`, `tick` and `run` are queued and executed in one testbench when
    a result is first asked for (`get`, `read`, `cycles`); nothing can be
    queued after that (Amaranth cannot add a testbench to a simulation
    that has run)."""

    def __init__(self, dut, ports, tag):
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


def engine(dut, ports, backend=None, tag="dut"):
    backend = backend or default_backend()
    if backend == "cxxrtl":
        return _Cxx(dut, ports, tag)
    return _Py(dut, ports, tag)
