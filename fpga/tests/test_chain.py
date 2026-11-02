"""HDL tests of the ISDB-S3 chain (isdb_fpga/chain) against the vectors of
`isdbs3-phy`'s `examples/chainvectors` (`mise run fpga:vectors` writes
fpga/build/chain-*.json; FPGA_VECTORS names another directory). Without a
stage's file its tests are skipped.

The default set runs on Amaranth's Python simulator over prefixes that
still cover what the stage must do (stated per test), and, where the
optional compiled backend is available (`isdb_fpga.chain.simx`:
`available()`, a C++ compiler and the CXXRTL headers of the venv's
yowasp-yosys), over the whole sets. FPGA_CHAIN_FULL=1 also runs the whole
sets on the Python simulator (slow: minutes to hours, README).
"""

import functools
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import simx  # noqa: E402
from isdb_fpga.chain.common import model_constants  # noqa: E402
from isdb_fpga.chain.sim import check_constants, compare_stage1, load, simulate_stage1  # noqa: E402
from isdb_fpga.chain.tables import pilot_signs  # noqa: E402

VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build"))
FULL = os.environ.get("FPGA_CHAIN_FULL") == "1"


def path(name):
    return os.path.join(VECTORS, f"chain-{name}.json")


@functools.lru_cache(maxsize=None)
def doc(name):
    return load(path(name))


def needs(name):
    return unittest.skipUnless(os.path.exists(path(name)),
                               f"no {path(name)} (mise run fpga:vectors)")


cxx = unittest.skipUnless(simx.available(), "no C++ compiler / CXXRTL headers (simx.available)")


@needs("2-phase")
class ConstantsTest(unittest.TestCase):
    def test_constants_are_the_models(self):
        self.assertEqual(check_constants(doc("2-phase")), [])

    def test_pilot_signs(self):
        self.assertEqual(doc("2-phase")["chain"]["pilot_signs"], pilot_signs())

    def test_every_constant_is_compared(self):
        missing = set(model_constants()) - set(doc("2-phase")["chain"])
        self.assertEqual(missing, set())


@needs("1-sync")
class SyncTest(unittest.TestCase):
    """Stages 2a, 1, 2b (`SyncFront`). The synthetic set (2 241 336 symbols,
    5 frames' worth of input up to one frame after the first frame head):
    lock at the 4th period (37 184 symbols), the first head at 47 480, the
    parity, the FSync signs; the first frame head at 1 116 520 and the one
    carrier update at 1 163 024 need the whole set."""

    PREFIX = 120_000  # lock, parity, 8 heads with FSync/!FSync signs

    def check(self, backend, symbols):
        d = doc("1-sync")
        res = simulate_stage1(d, backend, symbols)
        bad = compare_stage1(d, res)
        self.assertEqual(bad, [], "\n".join(bad[:5]))
        return res

    def test_prefix_pysim(self):
        res = self.check("pysim", self.PREFIX)
        self.assertGreaterEqual(len(res["heads"]), 7)

    @cxx
    def test_whole_set_cxxrtl(self):
        res = self.check("cxxrtl", None)
        self.assertEqual(len(res["heads"]), len(doc("1-sync")["heads"]))
        self.assertEqual(len(res["carrier"]), 1)
        self.assertTrue(any(h[6] and h[7] == 0 for h in res["heads"]), "a frame head")

    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_whole_set_pysim(self):
        self.check("pysim", None)

    @cxx
    def test_a_wrong_theta_is_caught(self):
        d = dict(doc("1-sync"))
        heads = [list(h) for h in d["heads"]]
        heads[3][4] ^= 1
        d["heads"] = heads
        res = simulate_stage1(doc("1-sync"), "cxxrtl", self.PREFIX)
        bad = compare_stage1(d, res)
        self.assertTrue(any("head" in b for b in bad), bad)


@needs("2-phase")
class PhaseTest(unittest.TestCase):
    """Stage 3 (`SlotPhase`): two slots between three heads, every
    symbol's phi, gain, y and r (18 592 symbols), with the heads fed as
    stage 1 gives them (after their window's last symbol)."""

    def run_stage(self, backend):
        from isdb_fpga.chain.stages import run_phase

        what, bad = run_phase(doc("2-phase"), backend)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    def test_pysim(self):
        self.run_stage("pysim")

    @cxx
    def test_cxxrtl(self):
        self.run_stage("cxxrtl")


@needs("3-eq")
@needs("4-tmcc")
class EqTest(unittest.TestCase):
    """Stage 4 (`Equaliser` with `FrameCtl`) on chain-3-eq's frame (non-
    identity taps written at open): z of every symbol, the 6 720 training
    references, the 77 latched 48-bit sums (308 words through the readback
    port), their count and frame. The Python simulator runs a prefix of
    two slots (z and the references of slots 0 and 1, both slots' MACs);
    the sums need the whole frame."""

    PREFIX = 2 * 9296

    def run_stage(self, backend, symbols):
        from isdb_fpga.chain.stages import run_eq

        what, bad = run_eq(doc("3-eq"), doc("4-tmcc")["layout"], backend, symbols)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    def test_prefix_pysim(self):
        self.run_stage("pysim", self.PREFIX)

    @cxx
    def test_whole_frame_cxxrtl(self):
        self.run_stage("cxxrtl", None)

    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_whole_frame_pysim(self):
        self.run_stage("pysim", None)


@needs("4-tmcc")
class TmccTest(unittest.TestCase):
    """Stage 5 (`TmccSoft`, `TmccResult`, `TmccLayout`, the BCH unit) on
    chain-4-tmcc's frame with the LDPC stub (ldpc_if.py): the 31 680 soft
    bits, the burst (OP_BEGIN, 31 680 writes in order with their positions
    and values, OP_DONE, the tag) with the port granted on about half the
    cycles, the 9 422 information bits, the result and the 120 layout
    entries with their commit for frame + 2."""

    def run_stage(self, backend, flips=()):
        from isdb_fpga.chain.stages import run_tmcc

        what, bad = run_tmcc(doc("4-tmcc"), backend, flips)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    def test_pysim(self):
        self.run_stage("pysim")

    @cxx
    def test_cxxrtl(self):
        self.run_stage("cxxrtl")

    @cxx
    def test_bch_corrects_the_tmcc(self):
        # 7 bit errors in the data bits (positions 1 870 ..): corrected, the
        # same information bits and layout, 7 reported
        self.run_stage("cxxrtl", flips=(1870, 2000, 5003, 7777, 9000, 11000, 11483))


@needs("3-eq")
@needs("4-tmcc")
@needs("5-demap")
class DemapTest(unittest.TestCase):
    """Stage 6 (`Demap`) in the back end (`Back`: stage 4, FrameCtl, stage
    5's input side, stage 6, the decoder port's arbiter) on chain-3-eq's
    frame, which chain-5 demaps: every beat of the 96 codewords (count,
    first/last, tag), the two codewords chain-5 carries in full (every
    symbol's positions and LLRs in label order, the de-interleaved words),
    the 16 learned points, the next frame's n0 and K, and the TMCC soft
    bits. The Python simulator runs the frame's first 12 000 symbols (the
    first codeword, 11 220 symbols, whole)."""

    def run_stage(self, backend, symbols):
        from isdb_fpga.chain.stages import run_back

        what, bad = run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), backend, symbols)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    def test_prefix_pysim(self):
        self.run_stage("pysim", 12_000)

    @cxx
    def test_whole_frame_cxxrtl(self):
        self.run_stage("cxxrtl", None)


class BchTest(unittest.TestCase):
    """The BCH unit (finish.py) against gf.decode, the Python port of
    `isdb_fec::bch::decode`: random words of the TMCC (9 614) and 93/120
    slot (34 776) lengths with 0, 1, 2, 5, 11, 12, 13 and 20 bit errors:
    the result, the count and the roots (the bits flipped)."""

    def test_pysim(self):
        from isdb_fpga.chain.bchsim import run

        _, _, bad = run("pysim", 2)
        self.assertEqual(bad, [])

    @cxx
    def test_cxxrtl(self):
        from isdb_fpga.chain.bchsim import run

        _, _, bad = run("cxxrtl", 32)
        self.assertEqual(bad, [])

    def test_python_port_corrects(self):
        import random

        from isdb_fpga.chain import gf

        r = random.Random(3)
        w = gf.encode([r.randrange(2) for _ in range(9614 - 192)])
        e = list(w)
        for p in r.sample(range(len(w)), 12):
            e[p] ^= 1
        self.assertEqual(gf.decode(e), (12, w))


if __name__ == "__main__":
    unittest.main()
