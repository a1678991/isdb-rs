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
from isdb_fpga.sim import compare_tx, is_tx, run_tx_top, simulate_tx, simulate_tx_top  # noqa: E402
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
    """The receive front end's vector files (the transmit ones and the
    LDPC decoder's `ldpc-*.json` share the directory)."""
    return [
        p
        for p in sorted(glob.glob(os.path.join(VECTORS, "*.json")))
        if not os.path.basename(p).startswith("ldpc-") and not _is_tx(p)
    ]


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


class TxModelTest(unittest.TestCase):
    """The Python model (the HDL's stand-in reference before the Rust
    vectors existed) against every transmit vector set: the same banks,
    samples, counters and command words."""

    def test_python_model_reproduces_the_vectors(self):
        for name, doc in tx_docs():
            with self.subTest(vectors=name):
                p, d = doc["params"], doc["design"]
                pre = doc.get("prefetch", 0)
                samples, counters = txmodel.run(p, doc["words"], doc["stalls"], len(doc["samples"]),
                                                prefetch=pre)
                self.assertEqual(compare_tx(doc, samples, counters), [])
                self.assertEqual(p["bank_tag"], tx.bank_tag(p["a"], p["b"], p["span"]))
                a, b, s = txmodel.bank(p["phases"], p["span"], d["roll_off"], d["backoff_db"])
                self.assertEqual((a, b), (p["a"], p["b"]))
                if "preset" not in d:  # the Python stress preset lowers SHIFT on purpose
                    self.assertEqual(s, p["shift"])
                self.assertEqual(txmodel.Encoder(0).configure_tx(p, prefetch=pre), doc["gpio"])


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
        """START in the middle of a stream: frac, the line, the counters
        and the rest of the current word start over, the DMA words the
        model has not requested yet (the block's prefetched ones) are
        kept, and the pipeline's content from before never reaches the
        output. Expected: the Python model with the same restart. On the
        first set, twelve consecutive restart points (more than a word's
        8 codes enter between the first and the last, so START meets the
        unpack register at every fill, an untouched word included)."""
        runs = [(name, doc, 500) for name, doc in tx_docs()]
        runs += [(tx_docs()[0][0], tx_docs()[0][1], k) for k in range(501, 513)]
        for name, doc, k in runs:
            with self.subTest(vectors=name, restart_after=k):
                info = {}
                samples, counters = simulate_tx(doc, pattern="every", restart_after=k, info=info)
                exp, exp_counters = txmodel.run(doc["params"], doc["words"], doc["stalls"],
                                                len(doc["samples"]), restart_at=info["restart_at"],
                                                prefetch=doc.get("prefetch", 0))
                want = dict(doc, samples=exp, counters=exp_counters)
                self.assertEqual(compare_tx(want, samples, counters), [])
                self.assertEqual(info["lead"], [(0, 0)] * tx.LATENCY)

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


def first_doc():
    """The first transmit set without stalls (`tx-1-continuous` with the
    Rust vectors; else the first set made stall-free by the Python
    model)."""
    for _, doc in tx_docs():
        if not doc["stalls"] and not doc.get("prefetch"):
            return doc
    doc = tx_docs()[0][1]
    samples, counters = txmodel.run(doc["params"], doc["words"], [], len(doc["samples"]))
    return dict(doc, stalls=[], samples=samples, counters=counters)


def stalled_doc():
    """The first transmit set with stalls and no prefetch."""
    return next(doc for _, doc in tx_docs() if doc["stalls"] and not doc.get("prefetch"))


class TxPrefetchTest(unittest.TestCase):
    """The prefetch while selected (FEATURES bit 0): the block polls the
    DMA from SELECT on, drops the answers without a word until RUN and
    holds up to two words, so RUN starts without a lead-in miss; a DMA
    later than RUN is still counted; deselecting drops the held words."""

    def test_primes_soon_after_select(self):
        """With the DMA's words there at SELECT, PRIMED within ARM +
        latency + 2 clocks; RUN on two held words: bit-exact, MISSED 0."""
        doc = first_doc()
        for latency in (1, 3):
            with self.subTest(latency=latency):
                info = {}
                samples, counters = simulate_tx(doc, prefetch=2, latency=latency, info=info)
                self.assertLessEqual(info["primed_at"], tx.ARM + latency + 2)
                self.assertEqual(compare_tx(doc, samples, counters), [])

    def test_dry_dma_before_run_is_not_a_miss(self):
        """START|SELECT, the DMA dry for 300 clocks (300 polls answered
        with an underflow), RUN once two words are held: the vectors'
        samples bit for bit, MISSED 0."""
        doc = first_doc()
        info = {}
        samples, counters = simulate_tx(doc, prefetch=2, dry_until=300, pattern="rfifo",
                                        info=info)
        self.assertGreaterEqual(info["primed_at"], 290)
        dropped = sum(1 for c, w in info["answers"] if w is None and c < info["run_clock"])
        self.assertGreater(dropped, 250)
        self.assertEqual(compare_tx(doc, samples, counters), [])
        self.assertEqual(counters["missed"], 0)

    def test_dma_later_than_run_is_counted(self):
        """RUN one clock after START|SELECT, the DMA dry for 200 clocks:
        the underflow answers after RUN are missing words, as the model
        with that many stalls before word 0 has them."""
        doc = first_doc()
        info = {}
        samples, counters = simulate_tx(doc, run_at=1, dry_until=200, info=info)
        n = info["late_misses"]
        self.assertGreater(n, 5)
        exp, exp_counters = txmodel.run(doc["params"], doc["words"], [[0, n]] + doc["stalls"],
                                        len(doc["samples"]))
        want = dict(doc, samples=exp, counters=exp_counters)
        self.assertEqual(compare_tx(want, samples, counters), [])
        self.assertEqual(counters["missed"], 8 * n)
        self.assertEqual(counters["first_miss"], 0)

    def test_deselect_drops_the_held_words(self):
        """CTRL 0 in the middle of a stream (with stalls), then
        START|SELECT, the prefetch and RUN: the samples are the model's
        on the DMA's words not yet requested, none of the words the block
        held or had requested when it was deselected."""
        doc = stalled_doc()
        for k in (500, 503):
            with self.subTest(reselect_after=k):
                info = {}
                samples, counters = simulate_tx(doc, reselect_after=k, info=info)
                j, stalls = info["resume"]
                exp, exp_counters = txmodel.run(doc["params"], doc["words"][j:], stalls,
                                                len(doc["samples"]), prefetch=2)
                want = dict(doc, samples=exp, counters=exp_counters)
                self.assertEqual(compare_tx(want, samples, counters), [])

    def test_vectors_with_prefetch_exist(self):
        """The vectors cover the prefetch when they are the Rust model's
        (`tx-6-prefetch`: stalls on words 0 and 1 not seen)."""
        names = [name for name, _ in tx_docs()]
        if not any(n.startswith("tx-") for n in names):
            self.skipTest("no Rust TX vectors (mise run fpga:vectors)")
        pre = [doc for _, doc in tx_docs() if doc.get("prefetch")]
        self.assertTrue(pre, names)
        for doc in pre:
            seen = txmodel.run(doc["params"], doc["words"], doc["stalls"], len(doc["samples"]))[1]
            self.assertGreater(seen["missed"], doc["counters"]["missed"])

    def test_old_host_sequence_misses_nothing(self):
        """The deployed host's sequence through gpio_out: STEP, SHIFT,
        CTRL START|SELECT, then (its buffers pushed, the DMA dry for its
        first 400 clocks) CTRL RUN|SELECT after a fixed wait, without
        reading PRIMED; and the DMA not answering for the 12 clocks from
        that command's write on (the first requests of an image without
        the prefetch, RUN + 4 and + 5, fall in them: the start miss the
        board showed). Bit-exact, MISSED 0 and STATUS without underrun;
        PRE_UNF counts the polls dropped before RUN."""
        doc = first_doc()
        p = doc["params"]

        async def script(h):
            await h.send(h.enc.write(tx.STEP, [p["step"], p["shift"]]))
            await h.cmd(tx.OP_CTRL, tx.CTRL_START | tx.CTRL_SELECT)
            await h.tick(600)
            window = (h.tb.clock, h.tb.clock + 12)
            h.upack.dry.append(window)
            await h.cmd(tx.OP_CTRL, tx.CTRL_RUN | tx.CTRL_SELECT)
            regs = {}

            async def read_regs():
                for name, addr in (("missed", tx.MISSED), ("status", tx.STATUS),
                                   ("pre_unf", tx.PRE_UNF)):
                    regs[name] = await h.read(addr)

            samples, counters = await h.stream(doc, after_n=read_regs)
            return samples, counters, regs, list(h.upack.answers), window

        samples, counters, regs, answers, window = run_tx_top(doc, script, dry_until=400)
        self.assertEqual(compare_tx(doc, samples, counters), [])
        self.assertEqual(regs["missed"], 0)
        self.assertEqual(regs["status"] & (tx.STATUS_UNDERRUN | 3), 3)
        first = next(c for c, w in answers if w is not None)
        self.assertEqual(regs["pre_unf"], sum(1 for c, w in answers if w is None and c < first))
        w0, w1 = window
        self.assertEqual([c for c, w in answers if w0 < c <= w1], [],
                         "a request in the window after RUN")
        self.assertGreater(regs["pre_unf"], 300)


class TxRegisterTest(unittest.TestCase):
    """FEATURES, STATUS PRIMED, BANK_TAG and the 64-bit counters with
    HI, through gpio_out / gpio_in."""

    def test_features_and_primed(self):
        """FEATURES reads its three bits; STATUS PRIMED after
        START|SELECT with words in the DMA, not before, and cleared (the
        held words dropped) by CTRL 0."""
        doc = first_doc()

        async def script(h):
            out = {"features": await h.read(tx.FEATURES), "idle": await h.read(tx.STATUS)}
            await h.cmd(tx.OP_CTRL, tx.CTRL_START | tx.CTRL_SELECT)
            out["selected"] = await h.read(tx.STATUS)
            out["held"] = h.ctx.get(h.core.held_words)
            await h.cmd(tx.OP_CTRL, 0)
            out["released"] = await h.read(tx.STATUS)
            out["held_after"] = h.ctx.get(h.core.held_words)
            return out

        out = run_tx_top(doc, script)
        self.assertEqual(out["features"], 7)
        self.assertEqual(out["features"], tx.FEATURES_VALUE)
        self.assertEqual(out["idle"], 0)
        self.assertEqual(out["selected"], tx.STATUS_SELECTED | tx.STATUS_PRIMED)
        self.assertEqual(out["held"], 2)
        self.assertEqual(out["released"], 0)
        self.assertEqual(out["held_after"], 0)

    def test_bank_tag(self):
        """The bitstream's value is the initial banks' tag (the Rust
        model's, when the vectors carry it); a bank write clears it; a
        WRITE sets it (HI/WRITE and WRITE_S); another bank write clears it
        again; empty memories start at 0. The register has no reset."""
        doc = first_doc()
        p = doc["params"]
        tag = tx.bank_tag(p["a"], p["b"], p["span"])
        self.assertEqual(p.get("bank_tag", tag), tag)

        async def script(h):
            out = [await h.read(tx.BANK_TAG)]
            await h.send(h.enc.write(tx.A_BASE + 3 * 32 + 1, [p["a"][3][1]]))
            out.append(await h.read(tx.BANK_TAG))
            await h.send(h.enc.write(tx.BANK_TAG, [0xDEADBEEF]))
            out.append(await h.read(tx.BANK_TAG))
            await h.send(h.enc.write(tx.BANK_TAG, [0x123]))
            out.append(await h.read(tx.BANK_TAG))
            await h.send(h.enc.write(tx.B_BASE + 511 * 32 + 23, [p["b"][511][23]]))
            out.append(await h.read(tx.BANK_TAG))
            out.append(h.control.bank_tag.reset_less)
            return out

        self.assertEqual(run_tx_top(doc, script), [tag, 0, 0xDEADBEEF, 0x123, 0, True])

        async def empty(h):
            return await h.read(tx.BANK_TAG)

        self.assertEqual(run_tx_top(doc, empty, banks=False), 0)

    def test_64_bit_counters_and_hi(self):
        """CONSUMED and SAMPLES set just below 2**32 before RUN carry into
        bit 32 (LO then HI equals the core's 64-bit value); HI is latched
        by the READ of CONSUMED, FIRST_MISS or SAMPLES and kept over other
        reads and over the counter's later changes; MISSED does not touch
        it."""
        doc = first_doc()
        p = doc["params"]

        async def script(h):
            core, ctx = h.core, h.ctx
            await h.send(h.enc.write(tx.STEP, [p["step"], p["shift"]]))
            await h.cmd(tx.OP_CTRL, tx.CTRL_START | tx.CTRL_SELECT)
            await h.tick(2)  # START is over
            ctx.set(core.consumed, (1 << 32) - 3)
            ctx.set(core.samples, (1 << 32) - 5)
            await h.cmd(tx.OP_CTRL, tx.CTRL_RUN | tx.CTRL_SELECT)
            await h.stream(dict(doc, samples=doc["samples"][:60]))
            out = {"direct": (ctx.get(core.consumed), ctx.get(core.samples)),
                   "read": (await h.read64(tx.CONSUMED), await h.read64(tx.SAMPLES))}
            # no strobes from here on: the counters change only when set
            ctx.set(core.consumed, 0x1_FFFF_FFFF)
            lo = await h.read(tx.CONSUMED)
            ctx.set(core.consumed, 0x2_0000_0000)
            await h.read(tx.MISSED)
            await h.read(tx.STATUS)
            out["latched"] = (lo, await h.read(tx.HI), await h.read(tx.HI))
            ctx.set(core.first_miss, 0x3_0000_0005)
            out["first_miss"] = await h.read64(tx.FIRST_MISS)
            ctx.set(core.samples, 0x4_0000_0007)
            out["samples"] = await h.read64(tx.SAMPLES)
            return out

        out = run_tx_top(doc, script)
        consumed, samples = out["direct"]
        self.assertGreaterEqual(consumed, 1 << 32)
        self.assertGreaterEqual(samples, 1 << 32)
        self.assertEqual(out["read"], out["direct"])
        self.assertEqual(out["latched"], (0xFFFF_FFFF, 1, 1))
        self.assertEqual(out["first_miss"], 0x3_0000_0005)
        self.assertEqual(out["samples"], 0x4_0000_0007)


if __name__ == "__main__":
    unittest.main()
