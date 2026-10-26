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

from isdb_fpga.frontend import crc32c_word, crc32c_word_terms  # noqa: E402
from isdb_fpga.sim import compare, load, simulate, simulate_top  # noqa: E402

VECTORS = os.environ.get(
    "FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build")
)


def crc32c_ref(crc, word):
    """Bit-serial reflected CRC-32C register over a 16-bit word, bit 0
    first (no initial value or final XOR)."""
    for bit in range(16):
        fb = (crc ^ (word >> bit)) & 1
        crc = (crc >> 1) ^ (0x82F63B78 if fb else 0)
    return crc


def crc32c_bytes(data):
    """The standard CRC-32/ISCSI of `data` through the word reference
    (pads nothing: even lengths only)."""
    crc = 0xFFFFFFFF
    for i in range(0, len(data), 2):
        crc = crc32c_ref(crc, data[i] | data[i + 1] << 8)
    return crc ^ 0xFFFFFFFF


class Crc(Elaboratable):
    def __init__(self):
        self.crc = Signal(32)
        self.word = Signal(16)
        self.out = Signal(32)

    def elaborate(self, platform):
        m = Module()
        m.d.comb += self.out.eq(crc32c_word(self.crc, self.word))
        return m


class CrcTest(unittest.TestCase):
    def test_reference_is_the_standard_crc32c(self):
        # CRC-32/ISCSI check value; "123456789" has an odd length, so check
        # the even-length "12345678" against a bytewise reference too.
        def bytewise(data):
            crc = 0xFFFFFFFF
            for b in data:
                crc ^= b
                for _ in range(8):
                    crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
            return crc ^ 0xFFFFFFFF

        self.assertEqual(bytewise(b"123456789"), 0xE3069283)
        self.assertEqual(crc32c_bytes(b"12345678"), bytewise(b"12345678"))

    def test_crc32c_word_matches_the_bitwise_definition(self):
        dut = Crc()
        sim = Simulator(dut)
        rng = random.Random(7)
        cases = [(0xFFFFFFFF, 0x3231), (0, 0), (0x12345678, 0xFC1D)]
        cases += [(rng.randrange(1 << 32), rng.randrange(1 << 16)) for _ in range(300)]
        bad = []

        async def bench(ctx):
            for c, w in cases:
                ctx.set(dut.crc, c)
                ctx.set(dut.word, w)
                await ctx.delay(1e-9)
                if ctx.get(dut.out) != crc32c_ref(c, w):
                    bad.append((c, w))

        sim.add_testbench(bench)
        sim.run()
        self.assertEqual(bad, [])

    def test_crc32c_word_is_one_xor_level_wide(self):
        # every output bit is a single XOR of register and word bits:
        # report the widest (the logic depth that timing depends on)
        widest = max(len(t) for t in crc32c_word_terms())
        self.assertLessEqual(widest, 32)


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
    def test_bit_exact_sample_every_clock(self):
        """The most the core must take: a sample on every clock."""
        for path in vector_files():
            with self.subTest(path=os.path.basename(path)):
                doc = load(path)
                syms, words = simulate(doc, pattern="every")
                self.assertEqual(compare(doc, syms, words), [])

    def test_bit_exact_wfifo_bursts(self):
        """util_wfifo's bursts of eight consecutive samples."""
        for path in vector_files():
            with self.subTest(path=os.path.basename(path)):
                doc = load(path)
                syms, words = simulate(doc, pattern="wfifo")
                self.assertEqual(compare(doc, syms, words), [])

    def test_bit_exact_irregular_strobe_and_coefficient_load(self):
        path = trimmed_vector()
        doc = load(path)
        syms, words = simulate(doc, pattern="jitter", load_port=True)
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

    def test_a_wrong_detector_bank_is_caught(self):
        """One coefficient of g (a centre tap, the only ones the core
        has) off by one: the timing error and from there the loop differ
        (the detector path is really compared too)."""
        doc = load(vector_files()[0])
        g = doc["params"]["g"]
        taps = len(g[0])
        t = taps // 2
        for ph in range(len(g)):
            g[ph][t] += 1
        syms, words = simulate(doc)
        self.assertNotEqual(compare(doc, syms, words), [])


if __name__ == "__main__":
    unittest.main()
