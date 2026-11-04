"""Compare two simulation backends (`chain.simx`) on the test benches:
every value an engine hands back (each `run`'s cycle count, every record
of every output group, every `get`, `cycles`) recorded in call order on
each backend and compared, the first difference named; plus every port's
value right after the engine is made (before any clock). Also the
seconds per case (the engine's construction, i.e. elaboration and a
cached or fresh library, apart) and the harness cycles per second.

python synth/sim_compare.py [CASE...] [--backends cxxrtl,verilator]
                            [--x-unique SEED]   (verilator: random X values)

Cases: sync phase eq tmcc tmcc-bch demap chain bch ldpc-17 ldpc-34
ldpc-py toy plans (default: these), ldpc-187 toy-187 (S = 187: CXXRTL
takes about 16 minutes to build it). Vectors in fpga/build ($FPGA_VECTORS).
"""

import hashlib
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga import ldpcmodel, simldpc  # noqa: E402
from isdb_fpga.chain import bchsim, sim, simv, simx, stages  # noqa: E402

VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build"))


def doc(name):
    with open(os.path.join(VECTORS, f"chain-{name}.json")) as f:
        return json.load(f)


class Recorder:
    """An engine whose every result is appended to `trace`."""

    def __init__(self, e, trace, timing):
        self.e, self.trace, self.timing = e, trace, timing
        self.initial = {name: e.get(name) for name in getattr(e, "idx", {})}

    def __getattr__(self, name):
        return getattr(self.e, name)

    def _rec(self, *x):
        self.trace.append(x)
        return x[-1]

    def run(self, *a, **kw):
        t0 = time.perf_counter()
        c = self.e.run(*a, **kw)
        self.timing["sim"] += time.perf_counter() - t0
        return self._rec("run", c)

    def tick(self, n=1):
        t0 = time.perf_counter()
        self.e.tick(n)
        self.timing["sim"] += time.perf_counter() - t0

    def read(self, g):
        return self._rec("read", g, self.e.read(g))

    def get(self, name):
        return self._rec("get", name, self.e.get(name))

    def cycles(self):
        return self._rec("cycles", self.e.cycles())

    def total(self):
        return self.closed_at if hasattr(self, "closed_at") else self.e.cycles()

    def close(self):
        if not hasattr(self, "closed_at"):
            self.closed_at = self.e.cycles()
        self.e.close()


def patched(backend, trace, timing, initial):
    real = simx.engine

    def engine(dut, ports, backend_=None, tag="dut"):
        t0 = time.perf_counter()
        e = real(dut, ports, backend, tag)
        timing["setup"] += time.perf_counter() - t0
        r = Recorder(e, trace, timing)
        initial.append((tag, r.initial))
        timing["engines"].append(r)
        return r

    return engine


def cases():
    out = {
        "sync": lambda be: sim.simulate_stage1(doc("1-sync"), be, None),
        "phase": lambda be: stages.run_phase(doc("2-phase"), be),
        "eq": lambda be: stages.run_eq(doc("3-eq"), doc("4-tmcc")["layout"], be, None),
        "tmcc": lambda be: stages.run_tmcc(doc("4-tmcc"), be),
        "tmcc-bch": lambda be: stages.run_tmcc(doc("4-tmcc"), be,
                                               (1870, 2000, 5003, 7777, 9000, 11000, 11483)),
        "demap": lambda be: stages.run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), be, None),
        "chain": lambda be: stages.run_chain(doc("6-chain"), doc("3-eq")["taps"], be, 2_400_000),
        "bch": lambda be: bchsim.run(be, 32),
    }

    def b44(step, python_model=False):
        def f(be):
            res = []
            for p in sorted(os.listdir(VECTORS)):
                if not (p.startswith("ldpc-") or p.startswith("ldpcvec-py-")) or "-toy-" in p:
                    continue
                if p == "ldpc-tables.json" or p.startswith("ldpcvec-") != python_model:
                    continue
                with open(os.path.join(VECTORS, p)) as fh:
                    d = json.load(fh)
                if d["schedule"] != {"kind": "layered", "step": step}:
                    continue
                r = simldpc.simulate(d, None, fill_known=bool(d.get("shortened")),
                                     all_rates=True, backend=be)
                for x in r:
                    x.pop("seconds")
                res.append((p, r, simldpc.compare(d, r)))
            return res
        return f

    def toy(be, names=("toy-a", "toy-a-short", "toy-a-11", "toy-a-22", "toy-a-34", "toy-b",
                       "toy-b-34")):
        res = []
        for name in names:
            d = ldpcmodel.vectors(name)
            r = simldpc.simulate(d, backend=be)
            for x in r:
                x.pop("seconds")
            res.append((name, r, simldpc.compare(d, r)))
        return res

    def plans(be):
        d = ldpcmodel.vectors("toy-a")
        cw = d["codewords"]
        slow = [i for i, c in enumerate(cw) if c["iterations"] == d["max_iter"]][:2]
        fast = [i for i, c in enumerate(cw) if c["converged"] and c["iterations"] > 0]
        a, b, c_, e = slow[0], slow[1], fast[0], fast[1]
        overrun = [("begin", a, 0, True), ("llr", a, 0, 0, None), ("start", a, 0),
                   ("begin", b, 0, True), ("llr", b, 0, 0, None), ("start", b, 0),
                   ("begin", c_, 0, False), ("llr", c_, 0, 0, None), ("start", c_, 0),
                   ("begin", e, 0, True), ("llr", e, 0, 0, None), ("start", e, 0)]
        half = d["n"] // 2
        two = [("begin", 1, 1, True), ("llr", 1, 1, 0, half),
               ("begin", 2, 0, True), ("llr", 2, 0, 0, None), ("start", 2, 0),
               ("llr", 1, 1, half, None), ("start", 1, 1)]
        res = [simldpc.simulate(d, banks=2, plan=overrun, backend=be),
               simldpc.simulate(d, plan=two, backend=be),
               simldpc.simulate(ldpcmodel.vectors("toy-a-short"), fill_known=True, backend=be)]
        for r in res:
            for x in r:
                x.pop("seconds")
        return res

    out.update({"ldpc-17": b44(17), "ldpc-34": b44(34), "ldpc-py": b44(17, True), "toy": toy,
                "plans": plans, "ldpc-187": b44(187),
                "toy-187": lambda be: toy(be, ("toy-a-187",))})
    return out


def run_case(fn, backend):
    trace, initial = [], []
    timing = {"setup": 0.0, "sim": 0.0, "engines": []}
    saved = (sim.engine, stages.engine, bchsim.engine, simx.engine)
    simldpc._ENGINES.clear()
    sim.engine = stages.engine = bchsim.engine = simx.engine = patched(backend, trace, timing,
                                                                       initial)
    try:
        t0 = time.perf_counter()
        result = fn(backend)
        wall = time.perf_counter() - t0
        cycles = sum(e.total() for e in timing["engines"])
    finally:
        sim.engine, stages.engine, bchsim.engine, simx.engine = saved
        for e, *_ in simldpc._ENGINES.values():
            e.close()
        simldpc._ENGINES.clear()
    return dict(trace=trace, initial=initial, result=result, wall=wall, setup=timing["setup"],
                sim=timing["sim"], cycles=cycles)


def scrub(x):
    """A result without its wall-clock times (the "seconds" fields, the
    "in 1.2 s" and "cycles/s" of the stages' summaries)."""
    if isinstance(x, dict):
        return {k: scrub(v) for k, v in x.items() if k != "seconds"}
    if isinstance(x, (list, tuple)):
        return type(x)(scrub(v) for v in x)
    if isinstance(x, str):
        return re.sub(r"[\d.,]+ (s|cycles/s)\b", "_", x)
    return x


def mismatches(result):
    """The comparison against the vectors in a result: the stage
    functions' (what, bad), simldpc's [(name, records, bad)]."""
    if isinstance(result, tuple) and len(result) == 2 and isinstance(result[1], list):
        return len(result[1])
    if isinstance(result, tuple) and len(result) == 3 and isinstance(result[2], list):
        return len(result[2])
    if isinstance(result, list) and result and isinstance(result[0], tuple):
        return sum(len(r[2]) for r in result)
    return "-"


def digest(x):
    return hashlib.sha256(repr(x).encode()).hexdigest()[:16]


def first_difference(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            if x[0] == "read" and y[0] == "read":
                for j, (u, v) in enumerate(zip(x[2], y[2])):
                    if u != v:
                        return f"call {i} (read group {x[1]}), record {j}: {u} vs {v}"
                return f"call {i} (read group {x[1]}): {len(x[2])} vs {len(y[2])} records"
            return f"call {i}: {str(x)[:200]} vs {str(y)[:200]}"
    return f"{len(a)} vs {len(b)} calls"


def main(argv):
    sys.stdout.reconfigure(line_buffering=True)
    backends = ["cxxrtl", "verilator"]
    if "--backends" in argv:
        i = argv.index("--backends")
        backends = argv[i + 1].split(",")
        del argv[i:i + 2]
    if "--x-unique" in argv:
        i = argv.index("--x-unique")
        os.environ["FPGA_VERILATOR_SEED"] = argv[i + 1]
        os.environ["FPGA_VERILATOR_FLAGS"] = "--x-initial unique --x-assign unique"
        del argv[i:i + 2]
    table = cases()
    names = argv[1:] or [n for n in table if "187" not in n]
    print(f"{simv.version()}; flags {' '.join(simv._flags())}; "
          f"seed {os.environ.get('FPGA_VERILATOR_SEED')}", flush=True)
    ok = True
    for name in names:
        runs = {be: run_case(table[name], be) for be in backends}
        ref = runs[backends[0]]
        for be in backends:
            r = runs[be]
            n = sum(len(x[2]) if x[0] == "read" else 1 for x in r["trace"])
            print(f"{name:9} {be:9} setup {r['setup']:7.1f} s  sim {r['sim']:7.2f} s  "
                  f"wall {r['wall']:7.1f} s  {r['cycles']:>10} cycles  "
                  f"{r['cycles'] / max(r['sim'], 1e-9):>12,.0f} cycles/s  "
                  f"{len(r['trace'])} calls, {n} values, trace {digest(r['trace'])}, "
                  f"result {digest(scrub(r['result']))}, mismatches {mismatches(r['result'])}",
                  flush=True)
        for be in backends[1:]:
            r = runs[be]
            if r["trace"] != ref["trace"]:
                ok = False
                print(f"  DIFFERENT trace {be}: {first_difference(ref['trace'], r['trace'])}")
            elif repr(scrub(r["result"])) != repr(scrub(ref["result"])):
                ok = False
                print(f"  DIFFERENT result {be}")
            else:
                print(f"  same trace and result: {backends[0]} = {be}")
            for (tag, a), (_, b) in zip(ref["initial"], r["initial"]):
                diff = {k: (a[k], b[k]) for k in a if a[k] != b.get(k)}
                if diff:
                    print(f"  ports before the first clock differ ({tag}): "
                          f"{dict(list(diff.items())[:8])}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
