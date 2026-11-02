"""Simulation of `finish.BchUnit` against `gf.decode` (the Python port of
`isdb_fec::bch::decode`) on words with chosen error patterns.

python -m isdb_fpga.chain.bchsim [--backend pysim|cxxrtl] [--words N]
"""

import random
import sys

from amaranth.hdl import Elaboratable, Module, Signal

from . import gf
from .finish import FastBch
from .simx import engine

TMCC_N = 9614
SLOT_N = 374 * 93 - 6  # 93/120: header + data + parity


class BchBench(Elaboratable):
    def __init__(self, lengths):
        self.dut = FastBch(lengths)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal()
        self.v = Signal(16)
        self.last = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.dut = d = self.dut
        go = self.i_valid & self.i_ready
        m.d.comb += [
            self.i_ready.eq(~d.busy),
            d.start.eq(go & (self.kind == 0)), d.n_sel.eq(self.v),
            d.i_valid.eq(go & (self.kind == 1)), d.i_bits.eq(self.v), d.i_last.eq(self.last),
        ]
        return m

    def ports(self):
        d = self.dut
        p = {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "v": self.v,
             "last": self.last, "done": d.done, "ok": d.ok, "cor": d.corrected, "nr": d.nroots}
        for k in range(gf.T):
            p[f"r{k}"] = d.roots[k]
        return p


def beats(word):
    """The word as aligned 16-bit beats (leading zeros), bit b = word order."""
    z = (-len(word)) % 16
    w = [0] * z + list(word)
    return [sum(w[i + b] << b for b in range(16)) for i in range(0, len(w), 16)]


def cases(n_words, seed=1):
    r = random.Random(seed)
    out = []
    for i in range(n_words):
        n = TMCC_N if i % 2 == 0 else SLOT_N
        msg = [r.randrange(2) for _ in range(n - gf.PARITY)]
        w = gf.encode(msg)
        ne = [0, 1, 2, 5, 11, 12, 13, 20][i % 8]
        for p in r.sample(range(n), ne):
            w[p] ^= 1
        out.append(w)
    return out


def run(backend=None, n_words=8):
    lengths = [TMCC_N, SLOT_N]
    words = cases(n_words)
    b = BchBench(lengths)
    p = b.ports()
    e = engine(b, p, backend, tag="bch")
    g = e.group("done", ["ok", "cor", "nr"] + [f"r{k}" for k in range(gf.T)])
    recs = []
    for w in words:
        recs.append((0, lengths.index(len(w)), 0))
        bs = beats(w)
        recs += [(1, v, int(i == len(bs) - 1)) for i, v in enumerate(bs)]
    e.run("i_valid", "i_ready", ["kind", "v", "last"], recs, tail=50_000)
    got = e.read(g)
    bad = []
    for i, (w, gres) in enumerate(zip(words, got)):
        res, after = gf.decode(w)
        flips = [j for j in range(len(w)) if after[j] != w[j]]
        ok, cor, nr = gres[0], gres[1], gres[2]
        roots = sorted(gres[3:3 + min(nr, gf.T)]) if (ok or res is None) else []
        want = (res is not None, res or 0)
        if (bool(ok), cor) != want:
            bad.append(f"word {i} (n {len(w)}): result {(bool(ok), cor)} != {want}")
        elif sorted(roots) != flips[:gf.T] and nr <= gf.T:
            bad.append(f"word {i}: roots {roots[:5]} != {flips[:5]}")
    if len(got) != len(words):
        bad.append(f"{len(got)} results for {len(words)} words")
    return words, got, bad


def main(argv):
    backend = argv[argv.index("--backend") + 1] if "--backend" in argv else None
    n = int(argv[argv.index("--words") + 1]) if "--words" in argv else 8
    words, got, bad = run(backend, n)
    print(f"BCH: {len(words)} words; " + ("bit-exact" if not bad else f"{len(bad)} mismatches"))
    for x in bad:
        print("  " + x)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
