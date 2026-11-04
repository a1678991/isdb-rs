"""HDL tests of the ISDB-S3 chain (isdb_fpga/chain) against the vectors of
`isdbs3-phy`'s `examples/chainvectors` (`mise run fpga:vectors` writes
fpga/build/chain-*.json; FPGA_VECTORS names another directory). Without a
stage's file its tests are skipped.

The default set runs on Amaranth's Python simulator over prefixes that
still cover what the stage must do (stated per test), and, where a
compiled backend is available (`isdb_fpga.chain.simx.compiled_backend`:
Verilator where it is installed, else CXXRTL with a C++ compiler; FPGA_SIM
chooses), over the whole sets. FPGA_CHAIN_FULL=1 also runs the whole sets
on the Python simulator (slow: minutes to hours, README).
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

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal  # noqa: E402

from isdb_fpga.chain.finish import Finish  # noqa: E402

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


# the compiled backend of the whole-set tests: $FPGA_SIM if it names one,
# else Verilator where it is installed, else CXXRTL (simx.compiled_backend)
COMPILED = simx.compiled_backend()
cxx = unittest.skipUnless(COMPILED, "no compiled backend (a C++ compiler and Verilator or the "
                          "CXXRTL headers: simx.compiled_backend)")


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
    def test_whole_set_compiled(self):
        res = self.check(COMPILED, None)
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
        res = simulate_stage1(doc("1-sync"), COMPILED, self.PREFIX)
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
    def test_compiled(self):
        self.run_stage(COMPILED)


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
    def test_whole_frame_compiled(self):
        self.run_stage(COMPILED, None)

    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_whole_frame_pysim(self):
        self.run_stage("pysim", None)

    @cxx
    def test_sums_capture_holds_a_frame(self):
        # SUMS_CAPTURE before frame F, then frames F + 1 and F + 2 close:
        # the sums read after them are F's (bit for bit), VALID set; the
        # host's 308 reads may take many frames (on the board about 15)
        from isdb_fpga.chain.stages import run_eq_capture

        what, bad = run_eq_capture(doc("3-eq"), doc("4-tmcc")["layout"], COMPILED)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    @cxx
    def test_sums_latch_moves_without_a_capture(self):
        from isdb_fpga.chain.stages import run_eq_capture

        what, bad = run_eq_capture(doc("3-eq"), doc("4-tmcc")["layout"], COMPILED, strobe=False)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    @cxx
    def test_sums_read_while_the_mac_runs(self):
        # issue #27: the 308 words read while the MAC unit sums a slot of
        # F + 1 are the held F's, each answered 3 cycles after its request
        # (it used to wait up to about 4 400 cycles for the MAC unit, the
        # register reading 0 meanwhile); SEQ counts the one latch
        from isdb_fpga.chain.stages import run_eq_capture

        what, bad = run_eq_capture(doc("3-eq"), doc("4-tmcc")["layout"], COMPILED, read_during=True)
        self.assertEqual(bad, [], "\n".join(bad[:5]))


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
    def test_compiled(self):
        self.run_stage(COMPILED)

    @cxx
    def test_bch_corrects_the_tmcc(self):
        # 7 bit errors in the data bits (positions 1 870 ..): corrected, the
        # same information bits and layout, 7 reported
        self.run_stage(COMPILED, flips=(1870, 2000, 5003, 7777, 9000, 11000, 11483))


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

    def run_stage(self, backend, symbols, next_start=False):
        from isdb_fpga.chain.stages import run_back

        what, bad = run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), backend, symbols, next_start)
        self.assertEqual(bad, [], "\n".join(bad[:5]))

    def test_prefix_pysim(self):
        self.run_stage("pysim", 12_000)

    @cxx
    def test_whole_frame_compiled(self):
        self.run_stage(COMPILED, None)

    @cxx
    def test_next_frame_starts_at_once(self):
        """The next frame (with a layout) starts a cycle after the frame's
        last symbol, as on the board's gapless stream: its start must not
        clear the noise sum before the frame's n0 is divided (the board's
        n0 stayed at the default)."""
        self.run_stage(COMPILED, None, next_start=True)


@needs("6-chain")
@needs("3-eq")
class ChainTest(unittest.TestCase):
    """The whole chain (`SlotsChain`: stages 1-6, 8, 9) on chain-6: the
    input symbols in, every `Slots` word out compared with the model's,
    with the host's taps written at open and the LDPC stub (ldpc_if.py)
    answering each frame's codewords after its TMCC codeword is committed.
    The compiled backend only (about 300 000 cycles/s on Verilator, 37 000
    on CXXRTL here; the Python simulator would need hours): by default the first 2 260 000 symbols
    (lock, the frame head, frame 0's TMCC and status blocks, 683 words);
    the whole set (5 587 896 symbols, 3 frames, frame 2 decoded: 120 slot
    blocks, 211 281 words, about 3 minutes) with FPGA_CHAIN_FULL=1."""

    def run_chain(self, symbols):
        from isdb_fpga.chain.stages import run_chain

        what, bad = run_chain(doc("6-chain"), doc("3-eq")["taps"], COMPILED, symbols)
        self.assertEqual(bad, [], "\n".join(bad[:8]))
        return what

    @cxx
    def test_frame_0(self):
        self.run_chain(2_260_000)

    @cxx
    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_whole_set(self):
        self.run_chain(None)


REAL = os.environ.get("FPGA_CHAIN_REAL") == "1"


@needs("7-hdl")
@needs("3-eq")
class RealDecoderChainTest(unittest.TestCase):
    """The whole chain with the real LDPC decoder (`ldpc.LdpcDecoder`, S =
    34, K = 3, through `LdpcBridge`, its 200 MHz domain at 5x the symbol
    clock) on the sets written with its settings (layered 34, limit 16,
    offset 1), the input at the front end's 33.76 Msymbol/s: every block of
    the model's stream, compared per (frame, kind, slot) (the decoder's
    commit order puts a frame's TMCC and status after the next frame's
    first slot blocks; the host re-associates them). The compiled backend
    (`simx.compiled_backend`: Verilator where installed, about two minutes
    a set; on CXXRTL tens of minutes), opt-in (FPGA_CHAIN_REAL=1)."""

    def run_set(self, name, taps):
        from isdb_fpga.chain.stages import run_chain_real

        what, bad = run_chain_real(doc(name), taps, COMPILED)
        self.assertEqual(bad, [], "\n".join(bad[:8]))
        return what

    @cxx
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_chain7(self):
        self.run_set("7-hdl", doc("3-eq")["taps"])

    @cxx
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_stop_and_start_free_the_decoder(self):
        # issue #28: four times the input to inside frame 2's first
        # codeword, the host's stop and START (stages.run_chain_restarts);
        # after each no decoder bank held and the bridge empty, and the
        # last pass decodes every slot as the model does (the same bits
        # and BCH results; a restart keeps the NCO's phase, so iteration
        # counts may differ)
        from isdb_fpga.chain.stages import run_chain_restarts

        what, bad = run_chain_restarts(doc("7-hdl"), doc("3-eq")["taps"], COMPILED)
        self.assertEqual(bad, [], "\n".join(bad[:8]))

    @cxx
    @needs("8-loss")
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_block_loss(self):
        self.run_set("8-loss", None)

    @cxx
    @needs("9-fade")
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_signal_removed(self):
        self.run_set("9-fade", None)

    @cxx
    @needs("10-bch")
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_bch_corrections(self):
        # eight consecutive valid slots with 1-12 bit errors in the decoder's
        # output (the same flipped in the model's): corrected, no overrun
        self.run_set("10-bch", doc("3-eq")["taps"])


@needs("8-loss")
@needs("9-fade")
class LossChainTest(unittest.TestCase):
    """The whole chain (the LDPC stub) through a block loss (chain-8: the
    chain restarts, `Chain::loss`, and decodes the frames after it) and the
    signal removed for 0.3 frame (chain-9: the slot sync is lost and
    regained), every Slots word against the model's. The compiled backend,
    with FPGA_CHAIN_FULL=1 (6.7 M symbols each)."""

    def run_set(self, name):
        from isdb_fpga.chain.stages import run_chain

        what, bad = run_chain(doc(name), None, "cxxrtl")
        self.assertEqual(bad, [], "\n".join(bad[:8]))

    @cxx
    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_block_loss(self):
        self.run_set("8-loss")

    @cxx
    @unittest.skipUnless(FULL, "FPGA_CHAIN_FULL=1")
    def test_signal_removed(self):
        self.run_set("9-fade")


class FinishBench(Elaboratable):
    """`Finish` fed one record per accepted cycle: kind 0 a frame (a:
    frame, b: decoded), 1 a result header (a, b: the tag's low and high
    words, c: iterations, d: converged, e: overrun), 2 a word (a: bits, b:
    last)."""

    def __init__(self):
        self.fin = Finish()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(2)
        self.a = Signal(32)
        self.b = Signal(32)
        self.c = Signal(8)
        self.d = Signal()
        self.e = Signal()
        from isdb_fpga.chain.stages import monitored

        monitored(self)  # Finish's invariant monitors: their ports `mon.ports()`

    def elaborate(self, platform):
        m = Module()
        m.submodules.fin = f = self.fin
        go = self.i_valid & self.i_ready
        res = self.kind != 0
        m.d.comb += [
            self.i_ready.eq(Mux(res, f.r_ready, 1)),
            f.f_valid.eq(go & (self.kind == 0)), f.f_frame.eq(self.a), f.f_decoded.eq(self.b[0]),
            f.r_valid.eq(self.i_valid & res), f.r_head.eq(self.kind == 1),
            f.r_tag.eq(Cat(self.a, self.b)), f.r_iterations.eq(self.c), f.r_converged.eq(self.d),
            f.r_overrun.eq(self.e & (self.kind == 1)), f.r_bits.eq(self.a[:16]), f.r_last.eq(self.b[0]),
        ]
        return m


class TmccOverrunTest(unittest.TestCase):
    """A TMCC codeword the decoder dropped comes back as its header alone
    (res_overrun = 1, no words). `Finish` must consume just the header: the
    TMCC block of frame 5 as a failed decode (BCH 0xFFFF, iterations
    0xFFFF, layout from frame 7, zero information words), its status
    block (frames, undecoded and TMCC failed counted), and then the next
    result, slot 0 of frame 6 (93/120, not converged: its bits as decoded,
    the slot PRBS removed), as its own block; TMCC_OVERRUNS counts 1."""

    def test_header_only(self):
        import random

        from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal

        from isdb_fpga.chain import slots as S
        from isdb_fpga.chain.finish import Finish
        from isdb_fpga.chain.tables import SLOT_INIT, prbs

        Bench = FinishBench

        rng = random.Random(5)
        k = 374 * 93
        nw = (k + 15) // 16
        words = [rng.randrange(1 << 16) for _ in range(nw)]
        recs = [(0, 5, 0, 0, 0, 0),
                (1, 1 << 7 | 3 << 8, 5, 0, 0, 1),  # TMCC of frame 5, dropped
                (1, 0 | 7 << 8 | 4 << 12 | 0 << 16, 6, 50, 0, 0)]  # slot 0 of frame 6, not converged
        recs += [(2, w, int(i == nw - 1), 0, 0, 0) for i, w in enumerate(words)]
        b = Bench()
        f = b.fin
        ports = {"i_valid": b.i_valid, "i_ready": b.i_ready, "kind": b.kind, "a": b.a, "b": b.b, "c": b.c,
                 "d": b.d, "e": b.e, "ov": f.o_valid, "ow": f.o_word, "ovr": f.tmcc_overruns, **b.mon.ports()}
        backend = COMPILED or "pysim"
        e = simx.engine(b, ports, backend, tag="tovr")
        g = e.group("ov", ["ow"])
        e.stall_limit = 1_000_000 if backend != "pysim" else None
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=20_000)
        out = [w for (w,) in e.read(g)]
        self.assertEqual(e.get("ovr"), 1)
        self.assertEqual(b.mon.report(e.get), [])
        bl = S.blocks(out)
        self.assertEqual([(x[0], x[2]) for x in bl], [(S.TMCC, 5), (S.STATUS, 5), (S.SLOT, 6)])
        self.assertEqual(bl[0][3], [0xFFFF, 0xFFFF, 7, 0] + [0] * 589)
        st = bl[1][3]
        # the counters follow the 47 head words and 5 constellation counts
        self.assertEqual((st[52], st[54], st[56], st[58]), (1, 1, 1, 0))  # frames, undecoded, TMCC failed, slots
        # the slot: header and data bits as decoded, the PRBS removed
        bits = [(words[i // 16] >> (i % 16)) & 1 for i in range(k)]
        pr = prbs(SLOT_INIT, 22, 25, k)
        hd = [x ^ y for x, y in zip(bits, pr)][:k - 198]
        pay = bl[2][3]
        self.assertEqual(pay[:3], [0, 4 | 7 << 4 | 50 << 8, 0xFFFF])
        self.assertEqual(S.unpack_bits(pay[3:], k - 198), hd)


class StatusPointsTest(unittest.TestCase):
    """The status block's learned points (`StatusSource` through the
    demapper's point RAM, a registered read) in `Finish`'s pacing: with
    16APSK learned, words 47-49 the three empty counts, 50 the count 16,
    then each point's re and im in label order. The whole-chain sets end
    before a status block carries learned points; on build 7's symbols
    each point's re came out as the previous point's (the word taken in
    the cycle the RAM's address moved)."""

    def test_points_in_order(self):
        from amaranth.lib.memory import Memory

        from isdb_fpga.chain import slots as S

        pts = [(100 + 3 * lb, -(200 + 5 * lb)) for lb in range(16)]

        class Bench(FinishBench):
            def elaborate(self, platform):
                m = super().elaborate(platform)
                st = self.fin.status
                init = [0] * 256
                for lb, (re, im) in enumerate(pts):
                    init[3 << 5 | lb] = (re & 0xFFFF) | (im & 0xFFFF) << 16
                m.submodules.pts = mem = Memory(shape=32, depth=256, init=init)
                rp = mem.read_port()
                m.d.comb += [rp.addr.eq(st.pt_addr), st.pt_data.eq(rp.data), st.snap["learned"].eq(1 << 3)]
                return m

        # frame 5 ends, its TMCC codeword comes back dropped: the TMCC
        # block, then the status block
        recs = [(0, 5, 0, 0, 0, 0), (1, 1 << 7 | 3 << 8, 5, 0, 0, 1)]
        b = Bench()
        f = b.fin
        ports = {"i_valid": b.i_valid, "i_ready": b.i_ready, "kind": b.kind, "a": b.a, "b": b.b, "c": b.c,
                 "d": b.d, "e": b.e, "ov": f.o_valid, "ow": f.o_word, **b.mon.ports()}
        backend = COMPILED or "pysim"
        e = simx.engine(b, ports, backend, tag="stpts")
        g = e.group("ov", ["ow"])
        e.stall_limit = 1_000_000 if backend != "pysim" else None
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=20_000)
        bl = S.blocks([w for (w,) in e.read(g)])
        self.assertEqual(b.mon.report(e.get), [])
        self.assertEqual([(x[0], x[2]) for x in bl], [(S.TMCC, 5), (S.STATUS, 5)])
        st = bl[1][3]
        want = [v & 0xFFFF for p in pts for v in p]
        self.assertEqual(st[47:51], [0, 0, 0, 16])
        self.assertEqual(st[51:51 + 32], want)
        self.assertEqual(st[51 + 32], 0)  # 32APSK: none


class SlotBchTest(unittest.TestCase):
    """A converged slot codeword with bit errors in its BCH word (93/120,
    frame 6, slot 3): `Finish` corrects them (the BCH unit's roots flipped
    in its word buffer) and the slot block carries the sent header and data
    bits, BCH OK with the count (w2), the PRBS removed. The synthetic
    vectors never need a BCH correction; this is the slot path's."""

    def test_corrected(self):
        import random

        from isdb_fpga.chain import gf
        from isdb_fpga.chain import slots as S
        from isdb_fpga.chain.tables import SLOT_INIT, prbs

        rng = random.Random(11)
        k = 374 * 93
        n = 44880
        msg = [rng.randrange(2) for _ in range(k - 198)]
        word = gf.encode(msg) + [1] * 6 + [rng.randrange(2) for _ in range(n - k)]
        flips = sorted(rng.sample(range(k - 6), 5))
        for f in flips:
            word[f] ^= 1
        pr = prbs(SLOT_INIT, 22, 25, k) + [0] * (n - k)
        sent = [x ^ y for x, y in zip(word, pr)]
        nw = n // 16
        words = [sum(sent[16 * q + b] << b for b in range(16)) for q in range(nw)]
        recs = [(0, 6, 1, 0, 0, 0),
                (1, 3 | 7 << 8 | 4 << 12, 6, 2, 1, 0)]
        recs += [(2, w, int(i == nw - 1), 0, 0, 0) for i, w in enumerate(words)]
        b = FinishBench()
        f = b.fin
        ports = {"i_valid": b.i_valid, "i_ready": b.i_ready, "kind": b.kind, "a": b.a, "b": b.b, "c": b.c,
                 "d": b.d, "e": b.e, "ov": f.o_valid, "ow": f.o_word, **b.mon.ports()}
        backend = "cxxrtl" if simx.available() else "pysim"
        e = simx.engine(b, ports, backend, tag="slotbch")
        g = e.group("ov", ["ow"])
        e.stall_limit = 2_000_000 if backend == "cxxrtl" else None
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=60_000)
        bl = S.blocks([w for (w,) in e.read(g)])
        self.assertEqual(b.mon.report(e.get), [])
        slots = [x for x in bl if x[0] == S.SLOT]
        # the dummies before slot 3, then slot 3
        self.assertEqual(len(slots), 4)
        pay = slots[3][3]
        self.assertEqual(pay[0] & 0x7F, 3)
        self.assertEqual(pay[0] >> 12 & 3, 3, "converged, BCH OK")
        self.assertEqual(pay[2], len(flips))
        self.assertEqual(S.unpack_bits(pay[3:], k - 198), msg)


class BpskCodewordTest(unittest.TestCase):
    """A pi/2-BPSK codeword is 44 880 main symbols (5 slots' worth), more
    than 15 bits hold: the demapper's codeword cursor, on a pi/2-BPSK
    layout (61/120: slot 0 of each unit valid, slots 1-4 dummy), gives
    slot 0's codeword exactly 44 880 beats (first on the first, last on the
    44 880th, position = symbol index) before slot 5's starts. A directed
    test of the cursor (the synthetic generator `isdbs3_phy::chain::synth`
    makes 16APSK 93/120 frames only)."""

    def test_cursor(self):
        from amaranth.hdl import Elaboratable, Module, Signal, signed
        from amaranth.lib.memory import Memory

        from isdb_fpga.chain.demap import Demap
        from isdb_fpga.chain.frame import LAY_BITS, lay_pack

        N = 44880

        class Bench(Elaboratable):
            def __init__(self):
                self.dm = Demap()
                self.i_valid = Signal()
                self.i_ready = Signal()
                self.kind = Signal()
                self.a = Signal(signed(16))

            def elaborate(self, platform):
                m = Module()
                m.submodules.dm = dm = self.dm
                lay = [lay_pack(0, 1, 3, int(k % 5 != 0), (k // 5) * N, 0 if k % 5 else N) for k in range(120)]
                mem = Memory(shape=LAY_BITS, depth=128, init=lay + [0] * 8)
                m.submodules.lay = mem
                rp = mem.read_port()
                go = self.i_valid & self.i_ready
                m.d.comb += [
                    self.i_ready.eq(1), rp.addr.eq(dm.lay_addr), dm.lay_data.eq(rp.data),
                    dm.f_start.eq(go & (self.kind == 0)), dm.frame.eq(7),
                    dm.valid.eq(go & (self.kind == 1)), dm.is_main.eq(1), dm.z[0].eq(self.a),
                    dm.z[1].eq(-self.a), dm.slot_mod.eq(1),
                ]
                return m

        b = Bench()
        dm = b.dm
        ports = {"i_valid": b.i_valid, "i_ready": b.i_ready, "kind": b.kind, "a": b.a,
                 "ov": dm.o_valid, "on": dm.o_n, "of": dm.o_first, "ol": dm.o_last, "otag": dm.o_tag,
                 "op": dm.o_pos[0]}
        backend = COMPILED or "pysim"
        e = simx.engine(b, ports, backend, tag="bpsk")
        g = e.group("ov", ["on", "of", "ol", "otag", "op"])
        recs = [(0, 0)] + [(1, (s * 37) % 900 - 450) for s in range(N + 100)]
        gaps = [100] + [0] * (N + 100)
        e.run("i_valid", "i_ready", ["kind", "a"], recs, tail=50, gaps=gaps)
        beats = e.read(g)
        first = [x for x in beats if x[3] & 0x7F == 0]
        self.assertEqual(len(first), N)
        self.assertEqual([i for i, x in enumerate(first) if x[1]], [0])
        self.assertEqual([i for i, x in enumerate(first) if x[2]], [N - 1])
        self.assertTrue(all(x[0] == 1 and x[4] == i for i, x in enumerate(first)))
        nxt = beats[N:]
        self.assertTrue(nxt and nxt[0][3] & 0x7F == 5 and nxt[0][1], nxt[:1])


class SlotsRxRegisterTest(unittest.TestCase):
    """`SlotsRx`'s register file through `gpio_out` (the command protocol of
    control.py), at the host's addresses (isdb_frontend::control::chain): the
    ID word with the Slots format version (bit 8); a front-end register
    written and read back (PERIOD_NOMINAL, 0x01: its READ must show the
    front end's readback, not the chain's); the chain's VERSION (0x400),
    MAX_ITER (0x40C, reset ldpc.DEFAULT_MAX_ITER, 14) written and read, AGC (0x40D), FEATURES
    (0x401: TAPS | SUMS | SUMS_CAPTURE, the limit in bits 15-8, the step 34 in
    27-16), SUMS_CAPTURE (0x422: 0, then ARMED after a write), TAPS_FROM
    (0x41B) twice, each arming the equaliser with its own frame (in use
    from N + 2, reported as N), a tap
    (0x410 + 3, re | im << 16) written with WRITE_S and committed by START
    (INIT bit 0), CARRIER (0x402) written and loaded by START; the front
    end's STATUS (0x10) with bit 2 (the Slots words selected); then the
    front end's register again. Issues #27/#28: DECODER (0x439) and
    RECLAIMED (0x43A), the decoder's health through the bridge's
    handshake from its clock (the bridge's inputs set here: no decoder);
    SUMS_CAPTURE's NO_LAYOUT (bit 2: no frame decoded) and SEQ (15-8);
    a sum register's READ keeps the last value on `gpio_in` until the
    word arrives (the cycle after `rd_ack`), never a 0 in between."""

    def test_reads(self):
        from amaranth.hdl import ClockDomain, Module
        from amaranth.sim import Simulator

        from isdb_fpga.chain.common import cos_table
        from isdb_fpga.chain.top import ID_CHAIN, SlotsRx
        from isdb_fpga.control import ID_MAGIC, OP_ADDR, OP_CTRL, OP_HI, OP_NOP, OP_READ, OP_WRITE, OP_WRITE_S
        from isdb_fpga.frontend import FORMAT_VERSION

        from isdb_fpga.chain.monitors import Collector

        dut = SlotsRx(32, cos_table(), decoder=False, monitors=True)
        col = Collector(dut)
        top = Module()
        top.submodules.dut = dut
        top.domains.sync = ClockDomain()
        top.domains.fast = ClockDomain()
        top.domains.ldpc = ClockDomain()
        sim = Simulator(top)
        sim.add_clock(25e-9, domain="sync")
        sim.add_clock(6.25e-9, phase=12.5e-9, domain="fast")
        sim.add_clock(5e-9, domain="ldpc")
        seen = {}

        async def bench(ctx):
            t = 0

            async def cmd(op, arg=0):
                nonlocal t
                t ^= 1
                ctx.set(dut.gpio_out, t << 31 | op << 28 | (arg & 0x7FFFFFF) << 1)
                for _ in range(8):
                    await ctx.tick("sync")
                return ctx.get(dut.gpio_in)

            seen["id"] = await cmd(OP_NOP)
            await cmd(OP_ADDR, 0x01)
            await cmd(OP_HI, 0x1234)
            await cmd(OP_WRITE, 0x5678)
            await cmd(OP_ADDR, 0x01)
            seen["period"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x400)
            seen["version"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x40C)
            seen["max_iter_reset"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x40C)
            await cmd(OP_HI, 0)
            await cmd(OP_WRITE, 33)
            await cmd(OP_WRITE, 0x0400)  # 0x40D, AGC (the address advanced)
            await cmd(OP_ADDR, 0x40C)
            seen["max_iter"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x40D)
            seen["agc"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x401)
            seen["features"] = await cmd(OP_READ)
            # SUMS_CAPTURE: idle, then armed by a write (no frame ends here:
            # EqTest.test_sums_capture_holds_a_frame holds one)
            await cmd(OP_ADDR, 0x422)
            seen["capture_idle"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x422)
            await cmd(OP_WRITE_S, 1)
            await cmd(OP_ADDR, 0x422)
            seen["capture_armed"] = await cmd(OP_READ)
            # TAPS_FROM arms the equaliser with the frame written (in use
            # from N + 2), not the one written before it
            eqm = dut.chain.back.eq
            for n in (5, 87):
                await cmd(OP_ADDR, 0x41B)
                await cmd(OP_WRITE_S, n)
                seen.setdefault("armed", []).append((ctx.get(eqm.pend_from), ctx.get(eqm.pend_at)))
            # tap 3 = (-5, 7) and carrier -1234, loaded by START with INIT bit 0
            await cmd(OP_ADDR, 0x413)
            await cmd(OP_HI, 7)
            await cmd(OP_WRITE, (-5) & 0xFFFF)
            await cmd(OP_ADDR, 0x402)
            await cmd(OP_WRITE_S, -1234)
            await cmd(OP_ADDR, 0x404)
            await cmd(OP_WRITE_S, 1)
            await cmd(OP_CTRL, 0b1011)  # RUN, START, SLOTS
            await cmd(OP_ADDR, 0x413)
            seen["tap3"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x402)
            seen["carrier"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x10)
            seen["status"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x01)
            seen["period_again"] = await cmd(OP_READ)
            seen["id_again"] = await cmd(OP_NOP)
            # the decoder's health (the bridge's ldpc-side inputs: no decoder here)
            br = dut.bridge
            for sig, v in ((br.h_overruns, 0x1234), (br.h_overflow, 1), (br.h_held, 2), (br.h_aborted, 7),
                           (br.h_reused, 9)):
                ctx.set(sig, v)
            for _ in range(40):
                await ctx.tick("sync")
            await cmd(OP_ADDR, 0x439)
            seen["decoder"] = await cmd(OP_READ)
            await cmd(OP_ADDR, 0x43A)
            seen["reclaimed"] = await cmd(OP_READ)
            # a sum word's READ after VERSION's: gpio_in shows VERSION's 1
            # until the cycle after rd_ack, then the word (0: no sums here)
            await cmd(OP_ADDR, 0x400)
            await cmd(OP_READ)
            await cmd(OP_ADDR, 0x500 + 4 * 5)
            ctx.set(dut.gpio_out, (t ^ 1) << 31 | OP_READ << 28)
            trace = []
            for _ in range(16):
                await ctx.tick("sync")
                trace.append((ctx.get(dut.gpio_in), ctx.get(eqm.rd_ack)))
            acked = [i for i, (_, a) in enumerate(trace) if a]
            first = acked[0] if acked else None
            # (the front end's readback until the READ is decoded: ADDR's
            # show the front end's)
            seen["sum_read"] = (first is not None and first < 12,
                                first is not None and trace[first][0] == 1
                                and all(v != 0 for v, _ in trace[:first + 1]),
                                trace[-1][0])
            # the invariant monitors (the readback, TAPS_FROM, the FIFOs)
            seen["monitors"] = col.report(lambda n: ctx.get(col.ports()[n]))

        sim.add_testbench(bench)
        sim.run()
        ident = ID_MAGIC << 16 | FORMAT_VERSION << 12 | 32 | ID_CHAIN
        self.assertEqual(seen, {
            "id": ident, "period": 0x12345678, "version": 1, "max_iter_reset": 14, "max_iter": 33,
            "agc": 0x0400, "features": 0b1110011 | 33 << 8 | 34 << 16, "capture_idle": 0b100,
            "capture_armed": 0b110, "armed": [(5, 7), (87, 89)], "tap3": 7 << 16 | 0xFFFB,
            "carrier": (-1234) & 0xFFFFFFFF, "status": 0b101, "period_again": 0x12345678, "id_again": ident,
            "decoder": 0x1234 | 1 << 16 | 2 << 20 | 3 << 24, "reclaimed": 7 | 9 << 16,
            "sum_read": (True, True, 0), "monitors": [],
        })


class BridgeTest(unittest.TestCase):
    """The bridge from the chain's decoder port (40 MHz, 5-lane beats) to
    `ldpc.py`'s ports (200 MHz, one LLR per cycle, begin / start, results in
    commit order): a 3-lane codeword with first/last, a TMCC begin / write /
    done, each committed with the limit and offset of its own closing beat
    (16 / 1 and 50 / 4), an abort beat on context 0 (one `abort` cycle, no
    write, begin or start: issue #28), then a result record and two words
    back with the tags in order."""

    def test_two_clocks(self):
        from amaranth.hdl import Module
        from amaranth.sim import Simulator

        from isdb_fpga.chain.ldpc_if import OP_ABORT, OP_BEGIN, OP_DONE, OP_WRITE, LdpcBridge, LdpcPort

        from isdb_fpga.chain.monitors import Collector

        port = LdpcPort()
        br = LdpcBridge(port, monitors=True)
        col = Collector(br)
        mon = []
        top = Module()
        top.submodules.br = br
        from amaranth.hdl import ClockDomain

        top.domains.sync = ClockDomain()
        top.domains.ldpc = ClockDomain()
        sim = Simulator(top)
        sim.add_clock(25e-9, domain="sync")
        sim.add_clock(5e-9, domain="ldpc")
        writes, begins, starts, outs, aborts = [], [], [], [], []
        # the two codewords close with different limits and offsets (the
        # port's per-codeword in_max_iter, in_offset)
        beats = [
            (OP_WRITE, 0, 3, 1, 0, 7, [10, 20, 30], [1, -2, 3], 0xA1, 9, 2),
            (OP_WRITE, 0, 3, 0, 1, 7, [11, 21, 31], [4, -5, 6], 0xA1, 16, 1),
            (OP_BEGIN, 1, 1, 0, 0, 3, [0], [0], 0x80, 3, 3),
            (OP_WRITE, 1, 1, 0, 0, 3, [1870], [-7], 0x80, 7, 0),
            (OP_DONE, 1, 1, 0, 0, 3, [0], [0], 0x80, 50, 4),
            (OP_ABORT, 0, 1, 0, 0, 3, [0], [0], 0x80, 50, 4),
        ]

        async def chain(ctx):
            for op, cx, n, fi, la, rate, pos, llr, tag, mi, off in beats:
                ctx.set(port.in_max_iter, mi)
                ctx.set(port.in_offset, off)
                ctx.set(port.in_valid, 1)
                ctx.set(port.in_op, op)
                ctx.set(port.in_ctx, cx)
                ctx.set(port.in_n, n)
                ctx.set(port.in_first, fi)
                ctx.set(port.in_last, la)
                ctx.set(port.in_rate, rate)
                ctx.set(port.in_tag, tag)
                ctx.set(port.in_shortened, cx)
                for k in range(5):
                    ctx.set(port.in_pos[k], pos[k] if k < len(pos) else 0)
                    ctx.set(port.in_llr[k], llr[k] if k < len(llr) else 0)
                await ctx.tick("sync")
            ctx.set(port.in_valid, 0)
            ctx.set(port.out_ready, 1)
            for _ in range(200):
                await ctx.tick("sync")
                if ctx.get(port.out_valid):
                    if ctx.get(port.out_head):
                        outs.append(("head", ctx.get(port.out_tag), ctx.get(port.out_iterations)))
                    else:
                        outs.append(("word", ctx.get(port.out_bits), ctx.get(port.out_last)))
            mon.extend(col.report(lambda n: ctx.get(col.ports()[n])))

        async def decoder(ctx):
            for cyc in range(400):
                await ctx.tick("ldpc")
                if ctx.get(br.begin):
                    begins.append(ctx.get(br.d_ctx))
                if ctx.get(br.abort):
                    aborts.append(ctx.get(br.d_ctx))
                if ctx.get(br.d_valid):
                    writes.append((ctx.get(br.d_ctx), ctx.get(br.d_pos), ctx.get(br.d_llr), ctx.get(br.d_rate)))
                if ctx.get(br.start):
                    starts.append((ctx.get(br.d_ctx), ctx.get(br.st_rate), ctx.get(br.st_short),
                                   ctx.get(br.st_max_iter), ctx.get(br.st_offset)))
                if cyc == 300:
                    ctx.set(br.res_valid, 1)
                    ctx.set(br.res_iterations, 2)
                    ctx.set(br.res_converged, 1)
                elif cyc in (301, 302):
                    ctx.set(br.res_valid, 0)
                    ctx.set(br.o_valid, 1)
                    ctx.set(br.o_data, 0x1234 + cyc)
                    ctx.set(br.o_last, cyc == 302)
                else:
                    ctx.set(br.res_valid, 0)
                    ctx.set(br.o_valid, 0)

        sim.add_testbench(chain)
        sim.add_testbench(decoder)
        sim.run()
        self.assertEqual(mon, [])
        self.assertEqual(begins, [0, 1])
        self.assertEqual(aborts, [0])
        self.assertEqual(writes, [(0, 10, 1, 6), (0, 20, -2, 6), (0, 30, 3, 6), (0, 11, 4, 6),
                                  (0, 21, -5, 6), (0, 31, 6, 6), (1, 1870, -7, 2)])
        self.assertEqual(starts, [(0, 6, 0, 16, 1), (1, 2, 1, 50, 4)])
        self.assertEqual(outs, [("head", 0xA1, 2), ("word", 0x1234 + 301, 0), ("word", 0x1234 + 302, 1)])


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
    def test_compiled(self):
        from isdb_fpga.chain.bchsim import run

        _, _, bad = run(COMPILED, 32)
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
