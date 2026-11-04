"""Bounded speed measurements of the whole chain with the real decoder:
`RealChainBench`, the library RealDecoderChainTest uses, on a short prefix
of a whole-chain set or on idle clocks, in seconds on Verilator. For the
stub chain and the decoder alone, and for build times: `synth/sim_speed.py`.

From fpga/:

PYTHONHASHSEED=0 uv run --locked python synth/chain_simspeed.py \\
    --vectors build/chain-7-hdl.json --symbols 100000 --tail 1000
PYTHONHASHSEED=0 uv run --locked python synth/chain_simspeed.py --backend cxxrtl \\
    --vectors build/chain-7-hdl.json --symbols 10000 --tail 1000 \\
    --compiler g++ --opt=-O1 --build-seconds 900

Backend: `--backend`, else `simx.compiled_backend()` ($FPGA_SIM, else
Verilator where installed, else CXXRTL). The library is built on first use
as the tests do (CXXRTL's takes minutes: `--library PATH` opens one that
exists, without elaborating); `--vectors` omitted: idle clocks.

Input: `stages.chain_real_records`, what `run_chain_real` feeds: the taps at
open (chain-7-hdl, chain-10-bch), the host's writes and the set's losses
that fall inside the prefix, the symbols at the front end's rate. Time: the
engine's `run` (or `tick`) call. `--chunk N`: through FPGA_SIM_PROGRESS's
chunks, N records a call, to measure what they cost (its reports on stderr).

`--compiler CXX --opt=-ON` (a CXXRTL library, with its .cc beside it)
compiles it again with `simx.CXXFLAGS` at that level into a library of its
own beside it, within `--build-seconds` (only that compiler's process group
is killed), and measures that too. Verilator's flags: $FPGA_VERILATOR_FLAGS,
part of its library's key.

One JSON line per step: the rates, and what must not depend on the compiler
or the repeat (cycles, `Slots` words, the chain's and decoder's counters,
the monitors' violations; exit status 1 when any differs). A short prefix
measures acquisition: its first output comes with frame 0's blocks, and its
first decoded codeword in frame 2, about 2.4 M symbols in. It compares no
output with the model; only the tests do.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

FPGA = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(FPGA))

from isdb_fpga.chain import simx, stages  # noqa: E402
from isdb_fpga.chain.sim import load  # noqa: E402

# run_chain_real's limit and diagnostics
STALL_LIMIT = 3_000_000
COUNTERS = ("tdone", "port_lost", "late", "overrun", "frames_done", "in_lost", "dec_overruns",
            "queue_overflow", "fifo_max", "stops")


def shown(path):
    """A library's path as printed: relative, no host's directories."""
    return os.path.relpath(path, FPGA)


def measure(so, bench, ports, input_, tail, idle):
    e = simx._Cxx.from_library(str(so), ports)
    try:
        e.stall_limit = STALL_LIMIT
        g = e.group("ov", ["ow"])
        started = time.perf_counter()
        if input_ is None:
            e.tick(idle)
        else:
            recs, gaps = input_
            e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], recs, tail=tail, gaps=gaps)
        seconds = time.perf_counter() - started
        cycles = e.cycles()
        words = [w for (w,) in e.read(g)]
        return dict(cycles=cycles, seconds=round(seconds, 3), cycles_per_second=round(cycles / seconds),
                    words=len(words),
                    output_sha256=hashlib.sha256(json.dumps(words).encode()).hexdigest()[:16],
                    counters={k: e.get(k) for k in COUNTERS},
                    monitors=bench.mon.report(e.get))
    finally:
        e.close()


def signature(r):
    return {k: v for k, v in r.items() if k not in ("seconds", "cycles_per_second")}


def compile_library(so, compiler, opt, seconds):
    """`so`'s generated C++ compiled again at `opt`: (library, status,
    build seconds, the command)."""
    cc = so.with_suffix(".cc")
    if not cc.exists():
        raise SystemExit(f"--compiler needs a CXXRTL library's C++: no {shown(cc)}")
    out = so.with_name(f"{so.stem}-experiment-{Path(compiler).name}{opt}.so")
    inc = simx.cxxrtl_include()
    flags = [f for f in simx.CXXFLAGS if not f.startswith("-O")]
    cmd = [compiler, opt, *flags, "-I", inc, str(cc), os.path.join(inc, "cxxrtl", "capi", "cxxrtl_capi.cc"),
           os.path.join(simx.HERE, "harness.cc"), "-o", str(out)]
    started = time.perf_counter()
    with out.with_suffix(".log").open("w") as log:
        proc = subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            status = proc.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            status = "timeout"
    return out, status, time.perf_counter() - started, [compiler, opt, *flags]


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--backend", choices=simx.COMPILED)
    p.add_argument("--library", type=Path, help="a library already built (no elaboration)")
    p.add_argument("--vectors", type=Path, help="a whole-chain set (default: idle clocks)")
    p.add_argument("--symbols", type=int, default=10_000)
    p.add_argument("--tail", type=int, default=1000)
    p.add_argument("--chunk", type=int, default=0)
    p.add_argument("--repeat", type=int, default=3)
    p.add_argument("--compiler")
    p.add_argument("--opt", choices=["-O1", "-O2", "-O3"], default="-O2")
    p.add_argument("--build-seconds", type=float, default=900)
    a = p.parse_args()
    if min(a.symbols, a.tail, a.chunk) < 0 or a.repeat < 1 or a.build_seconds <= 0:
        p.error("counts must be nonnegative, --repeat and --build-seconds positive")
    if a.chunk:
        simx.PROGRESS_CHUNK = a.chunk
        os.environ["FPGA_SIM_PROGRESS"] = "1"

    doc = load(a.vectors) if a.vectors else {}
    if a.vectors and "slots_words" not in doc:
        p.error(f"{a.vectors}: not a whole-chain set")
    input_ = None
    if a.vectors:
        if a.symbols > len(doc["input"]) // 2:
            p.error(f"--symbols: the set has {len(doc['input']) // 2}")
        stage = a.vectors.stem.removeprefix("chain-")
        taps = load(a.vectors.with_name("chain-3-eq.json"))["taps"] if stage in ("7-hdl", "10-bch") else None
        input_ = stages.chain_real_records(doc, taps, a.symbols)
        # flat: the engine's conversion costs a fraction of the run
        input_ = [v for r in input_[0] for v in r], input_[1]
    bench = stages.RealChainBench(errors=doc.get("bch_errors", ()), tmcc_errors=doc.get("tmcc_errors", ()))
    ports = bench.ports()

    if a.library:
        so = a.library.resolve()
    else:
        backend = a.backend or simx.compiled_backend()
        if backend is None:
            p.error("no compiled backend here (Verilator, or a C++ compiler for CXXRTL)")
        started = time.perf_counter()
        e = simx.engine(bench, ports, backend, tag="chainreal")
        so = Path(e.so).resolve()
        e.close()
        print(json.dumps(dict(backend=backend, library=shown(so),
                              setup_seconds=round(time.perf_counter() - started, 1))), flush=True)

    def runs(path):
        return [measure(path, bench, ports, input_, a.tail, a.symbols + a.tail) for _ in range(a.repeat)]

    base = runs(so)
    same = all(signature(r) == signature(base[0]) for r in base)
    print(json.dumps(dict(library=shown(so), symbols=a.symbols if a.vectors else 0, tail=a.tail, chunk=a.chunk,
                          repeats_match=same, results=base)), flush=True)
    if not a.compiler:
        return 0 if same else 1
    out, status, built, cmd = compile_library(so, a.compiler, a.opt, a.build_seconds)
    print(json.dumps(dict(compile=cmd, status=status, build_seconds=round(built, 1))), flush=True)
    if status != 0:
        return 1
    results = runs(out)
    matches = all(signature(r) == signature(base[0]) for r in results)
    print(json.dumps(dict(library=shown(out), matches_baseline=matches, results=results)), flush=True)
    return 0 if same and matches else 1


if __name__ == "__main__":
    sys.exit(main())
