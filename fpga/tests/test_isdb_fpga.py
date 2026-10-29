"""HDL tests: the front end and the transmit block against their models'
test vectors.

Receive vectors are JSON files written by
`cargo run -p isdb-frontend --example vectors` (`mise run fpga:vectors`
writes the default set into fpga/build); FPGA_VECTORS names another
directory. Without them the receive vector tests are skipped.

Transmit vectors (files with "samples" and "stalls" in the same
directory: the Rust model's, or `python -m isdb_fpga.txmodel`'s, which
`mise run fpga:tx-vectors` writes) are simulated likewise; without any,
the transmit tests run on the Python model's two presets, made in
process.
"""

import functools
import gc
import glob
import os
import random
import sys
import unittest
import warnings

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from amaranth.hdl import Elaboratable, Module, Signal  # noqa: E402
from amaranth.hdl import UnusedElaboratable  # noqa: E402
from amaranth.sim import Simulator  # noqa: E402

from isdb_fpga import tx, txmodel  # noqa: E402
from isdb_fpga.frontend import crc32c_word, crc32c_word_terms  # noqa: E402
from isdb_fpga.sim import compare, load, simulate, simulate_top  # noqa: E402
from isdb_fpga.sim import compare_tx, is_tx, simulate_tx, simulate_tx_top  # noqa: E402
from isdb_fpga.top import SymbolsTx  # noqa: E402

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


@functools.lru_cache(maxsize=None)
def _is_tx(path):
    return is_tx(load(path))


def vector_files():
    """The receive vector files (transmit ones share the directory)."""
    return [p for p in sorted(glob.glob(os.path.join(VECTORS, "*.json"))) if not _is_tx(p)]


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


# --- the transmit block -----------------------------------------------------


@functools.lru_cache(maxsize=None)
def tx_docs():
    """(name, doc) of every transmit vector set: the files, else the
    Python model's presets."""
    files = [p for p in sorted(glob.glob(os.path.join(VECTORS, "*.json"))) if _is_tx(p)]
    if files:
        return tuple((os.path.basename(p), load(p)) for p in files)
    return tuple((f"txmodel {name}", txmodel.vectors(name, 3000)) for name in ("isdbs", "stress"))


def first_hit(doc, odd, far):
    """(q, t) of a bank entry that a sample of `doc` reads: a point with an
    odd (or even) angle index at a tap of the far (or near) half of the
    line, so a change of that entry is known to reach the output."""
    span = doc["params"]["span"]
    taps = range(span, 2 * span) if far else range(span)
    for r in doc["samples"]:
        for t in taps:
            c = r["line"][t]
            if c & 8 and (c & 1) == odd:
                return r["q"], t
    raise AssertionError("no sample reads such an entry")


def mirrored(bank, q, t, delta):
    """`bank` with entry [q][t] and its mirror changed by `delta` (still
    symmetric, so the block accepts it as initial contents)."""
    out = [list(row) for row in bank]
    p, w = len(bank), len(bank[0])
    out[q][t] += delta
    out[p - 1 - q][w - 1 - t] += delta
    return out


class TxUnitTest(unittest.TestCase):
    def test_id_word(self):
        self.assertEqual(tx.id_word(512, 12), 0xB5D11918)

    def test_model_bank_is_symmetric_and_fits(self):
        a, b, s = txmodel.bank(512, 12)
        tx.check_bank("a", a, 512, 12)
        tx.check_bank("b", b, 512, 12)
        peak = max(abs(c) for row in a for c in row)
        self.assertLessEqual(peak, (1 << 17) - 1)
        self.assertGreater(2 * peak, (1 << 17) - 1)  # the largest shift
        self.assertEqual(s, 7)  # 10 dB backoff (interpolator.md §4)

    def test_an_asymmetric_bank_is_refused(self):
        """The block stores one copy of each tap pair: a bank whose far
        tap differs from its mirror by one LSB is refused, as the model's
        validate refuses it."""
        a, b, _ = txmodel.bank(512, 12)
        a[3][20] += 1
        with warnings.catch_warnings():
            # the half-built block is never elaborated: no "unused" warning
            warnings.simplefilter("ignore", UnusedElaboratable)
            with self.assertRaises(ValueError):
                tx.SymbolTx(512, 12, a, b)
            gc.collect()

    def test_transparent_when_not_selected(self):
        """After reset: the rfifo's request reads the upack, the upack's
        words and valid go to the rfifo, gpio_in shows the ID word."""
        dut = SymbolsTx(512, 12)
        sim = Simulator(dut)
        sim.add_clock(1e-8)
        rng = random.Random(3)
        bad = []

        async def bench(ctx):
            await ctx.tick()
            for _ in range(200):
                r, v = rng.randrange(2), rng.randrange(2)
                d0, d1 = rng.randrange(1 << 16), rng.randrange(1 << 16)
                for sig, val in ((dut.req, r), (dut.pass_valid, v), (dut.pass_data0, d0),
                                 (dut.pass_data1, d1), (dut.rd_valid, v),
                                 (dut.rd_data0, d0), (dut.rd_data1, d1)):
                    ctx.set(sig, val)
                got = (ctx.get(dut.rd_en), ctx.get(dut.out_valid), ctx.get(dut.out_data0),
                       ctx.get(dut.out_data1), ctx.get(dut.gpio_in))
                if got != (r, v, d0, d1, 0xB5D11918):
                    bad.append(got)
                await ctx.tick()

        sim.add_testbench(bench)
        sim.run()
        self.assertEqual(bad, [])


class TxVectorTest(unittest.TestCase):
    """The block against the transmit vectors, bit for bit: every sample's
    state (frac, q, the line, the carry and the code entered), its sums
    and its output, and the counters."""

    def check(self, doc, **kw):
        samples, counters = simulate_tx(doc, **kw)
        self.assertEqual(compare_tx(doc, samples, counters), [])

    def test_bit_exact_request_every_clock(self):
        for name, doc in tx_docs():
            with self.subTest(vectors=name):
                self.check(doc, pattern="every")

    def test_bit_exact_rfifo_bursts(self):
        """util_rfifo's bursts of eight strobes."""
        for name, doc in tx_docs():
            with self.subTest(vectors=name):
                self.check(doc, pattern="rfifo")

    def test_bit_exact_irregular_strobe_and_slow_upack(self):
        """Random strobes and the upack answering three clocks after the
        request (its PIPELINE_STAGES 2 at four channels)."""
        for name, doc in tx_docs():
            with self.subTest(vectors=name):
                self.check(doc, pattern="jitter", latency=3)

    def test_bank_loaded_through_the_port(self):
        """Empty memories, every tap of every row written (the far taps
        land on their mirrors, so each entry is written twice), and only
        the near half (which is the whole symmetric bank)."""
        name, doc = tx_docs()[0]
        for load_ in ("full", "half"):
            with self.subTest(vectors=name, load=load_):
                self.check(doc, pattern="rfifo", load=load_)

    def test_configured_through_gpio(self):
        """The vectors' command words on gpio_out (the banks too, when
        they write them, into empty memories), the ID word, the readback
        of STEP, SHIFT, STATUS and the counters through gpio_in."""
        for name, doc in tx_docs():
            with self.subTest(vectors=name):
                samples, counters, problems = simulate_tx_top(doc, pattern="rfifo")
                self.assertEqual(problems, [])
                self.assertEqual(compare_tx(doc, samples, counters), [])

    def test_start_while_running_restarts_cleanly(self):
        """START in the middle of a stream: frac, the line, the unpack
        register and the counters start over, and the pipeline's content
        from before never reaches the output."""
        name, doc = tx_docs()[0]
        lead = []
        samples, counters = simulate_tx(doc, pattern="rfifo", restart_after=500, lead=lead)
        self.assertEqual(compare_tx(doc, samples, counters), [])
        self.assertEqual(lead, [(0, 0)] * tx.LATENCY)

    def test_one_lsb_in_bank_a_is_caught(self):
        name, doc = tx_docs()[0]
        p = doc["params"]
        q, t = first_hit(doc, odd=0, far=False)
        a = mirrored(p["a"], q, t, 1)
        samples, counters = simulate_tx(doc, banks=(a, p["b"]))
        self.assertNotEqual(compare_tx(doc, samples, counters), [])

    def test_one_lsb_in_bank_b_is_caught(self):
        name, doc = tx_docs()[0]
        p = doc["params"]
        q, t = first_hit(doc, odd=1, far=False)
        b = mirrored(p["b"], q, t, -1)
        samples, counters = simulate_tx(doc, banks=(p["a"], b))
        self.assertNotEqual(compare_tx(doc, samples, counters), [])

    def test_one_lsb_in_a_far_tap_is_caught(self):
        """A far tap (t >= S, read through port B at ~q) written one LSB
        off through the load port: the output differs (with the right
        value it does not: test_bank_loaded_through_the_port)."""
        name, doc = tx_docs()[0]
        p = doc["params"]
        for odd, bank in ((0, p["a"]), (1, p["b"])):
            with self.subTest(bank="b" if odd else "a"):
                q, t = first_hit(doc, odd=odd, far=True)
                samples, counters = simulate_tx(doc, writes=[(odd, q, t, bank[q][t] + 1)])
                self.assertNotEqual(compare_tx(doc, samples, counters), [])

    def test_the_comparison_sees_the_counters(self):
        """One request more stalled at the start than the vectors say: the
        samples shift and MISSED differs."""
        name, doc = tx_docs()[0]
        changed = dict(doc, stalls=[[0, 1]] + doc["stalls"])
        samples, counters = simulate_tx(changed)
        bad = compare_tx(doc, samples, counters)
        self.assertTrue(any(m.startswith("counter missed") for m in bad), bad)


if __name__ == "__main__":
    unittest.main()
