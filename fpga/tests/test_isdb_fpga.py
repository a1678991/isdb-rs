"""HDL tests: the front end against the Rust model's test vectors.

Vectors are JSON files written by
`cargo run -p isdb-frontend --example vectors` (`mise run fpga:vectors`
writes the default set into fpga/build); FPGA_VECTORS names another
directory. Without vectors the vector tests are skipped.
"""

import glob
import os
import random
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from amaranth.hdl import Elaboratable, Module, Signal  # noqa: E402
from amaranth.sim import Simulator  # noqa: E402

from isdb_fpga.frontend import crc16_word  # noqa: E402
from isdb_fpga.sim import compare, load, simulate, simulate_top  # noqa: E402

VECTORS = os.environ.get(
    "FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build")
)


def crc16_ref(crc, word):
    for bit in reversed(range(16)):
        fb = ((crc >> 15) ^ (word >> bit)) & 1
        crc = ((crc << 1) & 0xFFFF) ^ (0x1021 if fb else 0)
    return crc


class Crc(Elaboratable):
    def __init__(self):
        self.crc = Signal(16)
        self.word = Signal(16)
        self.out = Signal(16)

    def elaborate(self, platform):
        m = Module()
        m.d.comb += self.out.eq(crc16_word(self.crc, self.word))
        return m


class CrcTest(unittest.TestCase):
    def test_crc16_word_matches_the_bitwise_definition(self):
        dut = Crc()
        sim = Simulator(dut)
        rng = random.Random(7)
        cases = [(0, 0x3132), (0xFFFF, 0), (0x1234, 0xFC1D)]
        cases += [(rng.randrange(1 << 16), rng.randrange(1 << 16)) for _ in range(200)]
        bad = []

        async def bench(ctx):
            for c, w in cases:
                ctx.set(dut.crc, c)
                ctx.set(dut.word, w)
                await ctx.delay(1e-9)
                if ctx.get(dut.out) != crc16_ref(c, w):
                    bad.append((c, w))

        sim.add_testbench(bench)
        sim.run()
        self.assertEqual(bad, [])


def vector_files():
    return sorted(glob.glob(os.path.join(VECTORS, "*.json")))


def trimmed_vector():
    """A vector set whose output bank differs from h (a nonzero trim), so
    the load paths are tested on k too; the first set otherwise."""
    for path in vector_files():
        doc = load(path)
        if doc["params"]["k"] != doc["params"]["h"]:
            return path
    return vector_files()[0]


@unittest.skipUnless(vector_files(), f"no test vectors in {VECTORS} (mise run fpga:vectors)")
class VectorTest(unittest.TestCase):
    def test_bit_exact_regular_strobe(self):
        for path in vector_files():
            with self.subTest(path=os.path.basename(path)):
                doc = load(path)
                syms, words = simulate(doc)
                self.assertEqual(compare(doc, syms, words), [])

    def test_bit_exact_irregular_strobe_and_coefficient_load(self):
        path = trimmed_vector()
        doc = load(path)
        syms, words = simulate(doc, jitter=True, load_port=True)
        self.assertEqual(compare(doc, syms, words), [])

    def test_configured_through_gpio(self):
        path = trimmed_vector()
        doc = load(path)
        words, problems = simulate_top(doc)
        self.assertEqual(problems, [])
        ew = doc["words"]
        self.assertIn(len(ew) - len(words), range(0, 3))
        self.assertEqual(words, ew[: len(words)])


    def test_a_wrong_output_bank_is_caught(self):
        """One coefficient of k off by one: the comparison must fail (the
        output path is really compared, not only the loop's)."""
        doc = load(trimmed_vector())
        k = doc["params"]["k"]
        ph, t = 0, len(k[0]) // 2
        k[ph][t] += 1
        syms, words = simulate(doc)
        doc["params"]["k"][ph][t] -= 1
        self.assertNotEqual(compare(doc, syms, words), [])


if __name__ == "__main__":
    unittest.main()
