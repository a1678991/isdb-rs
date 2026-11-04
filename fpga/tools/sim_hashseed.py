"""The compiled libraries' cache keys against Python's hash seed: each
design elaborated in a new process per `PYTHONHASHSEED`, the RTLIL's hash
and the keys of `simx._Cxx` and `simv.Verilator` (the library's file name,
`<tag>-<key>.so`) printed per seed, and whether the seeds gave one key.

Amaranth collects a design's implicit clock domains in sets, so the order
of their ports in the RTLIL follows the seed. A seed cannot be changed
inside a running interpreter, hence the processes.

python tools/sim_hashseed.py [--design NAME]... [--seed SEED]... [--jobs N]

From fpga/. Designs: `three` (a small bench with sync, fast and ldpc: about
a second), `decoder` (`LdpcDecoder` S = 17, one implicit domain), `chain`
(`ChainBench`, the stub chain), `chainreal` (`RealChainBench`, the library
RealDecoderChainTest uses; about 20 s). Default: all of them, seeds 0, 0,
1, 2. Exit status 1 when one seed value gives two keys. A Verilator key is
printed only where Verilator is installed.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from amaranth.back import rtlil  # noqa: E402
from amaranth.hdl import Elaboratable, Module, Signal  # noqa: E402

from isdb_fpga.chain import simv, simx  # noqa: E402


class ThreeDomains(Elaboratable):
    """A counter in each of the engine's clocks."""

    def __init__(self):
        self.count = Signal(16)
        self.fast = Signal(16)
        self.ldpc = Signal(16)

    def elaborate(self, platform):
        m = Module()
        m.d.sync += self.count.eq(self.count + 1)
        m.d.fast += self.fast.eq(self.fast + 1)
        m.d.ldpc += self.ldpc.eq(self.ldpc + 1)
        return m

    def ports(self):
        return {"count": self.count, "fast": self.fast, "ldpc": self.ldpc}


def design(name):
    """The design and its ports, as the engine is given them."""
    if name == "three":
        d = ThreeDomains()
        return d, d.ports()
    if name == "decoder":
        from isdb_fpga.ldpc import LdpcDecoder
        from isdb_fpga.simldpc import _ports

        d = LdpcDecoder(17, banks=3, sim=True)
        return d, _ports(d)
    from isdb_fpga.chain import stages

    d = {"chain": stages.ChainBench, "chainreal": stages.RealChainBench}[name]()
    return d, d.ports()


DESIGNS = ("three", "decoder", "chain", "chainreal")


def child(name):
    d, ports = design(name)
    il = rtlil.convert(d, name="top", ports=list(ports.values()))
    out = {"rtlil": hashlib.sha256(il.encode()).hexdigest()[:16],
           "cxxrtl": hashlib.sha256(simx._Cxx._key(il)).hexdigest()[:16]}
    if simv.available():
        out["verilator"] = hashlib.sha256(simv.Verilator._key(il)).hexdigest()[:16]
    print(json.dumps(out))


def one(name, seed):
    env = dict(os.environ, PYTHONHASHSEED=seed)
    r = subprocess.run([sys.executable, __file__, "--child", name], env=env, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(f"{name}, seed {seed}: {r.stderr[-2000:]}")
    return json.loads(r.stdout.splitlines()[-1])


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--design", action="append", choices=DESIGNS)
    p.add_argument("--seed", action="append")
    p.add_argument("--jobs", type=int, default=4)
    p.add_argument("--child", help=argparse.SUPPRESS)
    a = p.parse_args()
    if a.child:
        child(a.child)
        return 0
    seeds = a.seed or ["0", "0", "1", "2"]
    status = 0
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
        jobs = [(n, s, pool.submit(one, n, s)) for n in a.design or DESIGNS for s in seeds]
        results = {}
        for name, seed, job in jobs:
            r = job.result()
            results.setdefault(name, []).append((seed, r))
            print(f"{name:10} seed {seed}: " + " ".join(f"{k} {v}" for k, v in r.items()), flush=True)
    for name, rows in results.items():
        by_seed = {}
        for seed, r in rows:
            by_seed.setdefault(seed, set()).add(tuple(sorted(r.items())))
        same = all(len(v) == 1 for v in by_seed.values())
        keys = {v for s in by_seed.values() for v in s}
        print(f"{name}: {len(keys)} distinct key set(s) over seeds {sorted(by_seed)}; "
              f"one seed, one key: {'yes' if same else 'NO'}")
        status |= not same
    return status


if __name__ == "__main__":
    sys.exit(main())
