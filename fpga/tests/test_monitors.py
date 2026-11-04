"""The invariant monitors (isdb_fpga/chain/monitors.py, issue #30 item 2):
each one fires on the defect it targets.

The defects are re-introduced on a scratch copy of the module's source
(`monitors.source_patch`: the fix reverted, or a one-line fault), so the
HDL carries no fault-injection parameters; where the class is an input a
correct design must never see (a decoder that answers a codeword it was
never given), the bench drives that input. Every test also checks that
the same run without the fault leaves every monitor quiet, where the
correct run is cheap enough. The board-found defects and their fix
commits (README "Invariant monitors"):

- e95c563 (#28): the decoder's bank leak across stop/START (the
  allocation rule before the fix: `begin` always takes a new bank, no
  `abort`) -> `bank_accounting`, `abort_closes`, `bank_leak`;
- 243d672: n0 cleared one cycle before the ended frame's division ->
  `noise_count`;
- d53de60: the learned points one word shifted -> `status_point_settled`;
- 5f60649: TAPS_FROM one write behind -> `taps_from_written`;
- a052ece: the sum readback's transient 0 -> `sum_readback_hold` (and the
  read waiting on the MAC unit -> `sum_read_slow`).

Run alone: `uv run --locked pytest -q tests/test_monitors.py`. The
patched designs are other RTLIL, so the compiled backends build each one
once (cached in fpga/build/ like every design).
"""

import functools
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from amaranth.hdl import Cat, ClockDomain, Elaboratable, Module, Mux, Signal  # noqa: E402
from amaranth.sim import Simulator  # noqa: E402

from isdb_fpga import ldpcmodel, simldpc  # noqa: E402
from isdb_fpga.chain import monitors, simx  # noqa: E402
from isdb_fpga.chain.monitors import MonitorError, source_patch  # noqa: E402
from isdb_fpga.chain.sim import load  # noqa: E402

VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build"))
COMPILED = simx.compiled_backend()
BACKEND = COMPILED or "pysim"


def path(name):
    return os.path.join(VECTORS, f"chain-{name}.json")


@functools.lru_cache(maxsize=None)
def doc(name):
    return load(path(name))


def needs(*names):
    missing = [n for n in names if not os.path.exists(path(n))]
    return unittest.skipIf(missing, f"no {missing} vectors (mise run fpga:vectors)")


def fired(msgs, name):
    """The messages of check `name` (a path suffix: "dec.bank_leak" or "bank_leak")."""
    return [x for x in msgs if f".{name} (" in x or f" {name} (" in x]


class MonitorBasicsTest(unittest.TestCase):
    def test_off_by_default(self):
        """Without `monitors` no module has a Monitor: the synthesised design
        is unchanged (fpga/README.md "Invariant monitors": the Verilog of
        `SlotsRx` without monitors is byte-identical)."""
        from isdb_fpga.chain.common import cos_table
        from isdb_fpga.chain.top import SlotsRx

        dut = SlotsRx(32, cos_table(), decoder=True)
        self.assertEqual(monitors.monitors_of(dut), [])
        _silence(dut)

    def test_every_monitored_module_is_reached(self):
        from isdb_fpga.chain.common import cos_table
        from isdb_fpga.chain.top import SlotsRx

        dut = SlotsRx(32, cos_table(), decoder=True, monitors=True)
        owners = {type(o).__name__ for o in _walk(dut) if getattr(o, "monitor", None) is not None}
        self.assertEqual(owners, {"SlotsRx", "LdpcDecoder", "LdpcBridge", "Demap", "Back", "TmccSoft",
                                  "FrameCtl", "Equaliser", "ResultFifo", "Finish", "Packer"})
        names = monitors.Collector(dut).names()
        self.assertEqual(len(names), len(set(names)))
        _silence(dut)


def _silence(root):
    """Never elaborated here: no UnusedElaboratable warnings."""
    for o in _walk(root):
        o._MustUse__silence = True


def _walk(root):
    out, seen = [], set()

    def go(o):
        if id(o) in seen:
            return
        seen.add(id(o))
        out.append(o)
        for _k, c in monitors._children(o):
            go(c)
    go(root)
    return out


# --- the LDPC decoder ------------------------------------------------------------

def decoder_run(plan, edits=(), banks=3, tail=60_000):
    """The toy-a decoder (monitors on, `ldpc.py` patched with `edits`) driven
    by a simldpc-style plan: ("begin", i, ctx), ("llr", i, ctx, lo, hi),
    ("start", i, ctx), ("abort", ctx), ("tick", n); then `tail` cycles.
    Returns the monitor messages. (Not simldpc.simulate: it waits for every
    commit's result, which a broken design may never give.)"""
    doc = ldpcmodel.vectors("toy-a")
    words = doc["codewords"]
    with source_patch("isdb_fpga.ldpc", list(edits)) as lm:
        kw, ridx = simldpc._design(doc, banks=banks)
        dut = lm.LdpcDecoder(**kw, sim=True, monitors=True)
        col = monitors.Collector(dut)
        e = simx.engine(dut, dict(simldpc._ports(dut), **col.ports()), BACKEND, tag="mondec")
        e.set("o_ready", 1)
        e.set("in_rate", ridx)
        for op in plan:
            if op[0] in ("begin", "start", "abort"):
                e.set(op[0], 1)
                e.set("in_ctx", op[-1])
                if op[0] == "start":
                    e.set("st_rate", ridx)
                    e.set("st_max_iter", doc["max_iter"])
                    e.set("st_offset", doc["offset"])
                e.tick()
                e.set(op[0], 0)
            elif op[0] == "llr":
                _, i, c, lo, hi = op
                llr = words[i]["llr"]
                e.set("in_ctx", c)
                e.run("in_valid", None, ["in_pos", "in_llr"],
                      [(p_, llr[p_]) for p_ in range(lo, len(llr) if hi is None else hi)])
            else:
                e.tick(op[1])
        e.tick(tail)
        return col.report(e.get)


TOY = ldpcmodel.vectors("toy-a")
HALF = TOY["n"] // 2
SLOW = [i for i, c in enumerate(TOY["codewords"]) if c["iterations"] == TOY["max_iter"]]


def whole(i, c=0):
    return [("begin", i, c), ("llr", i, c, 0, None), ("start", i, c)]


# test_isdb_fpga's issue #28 recipe at the decoder: five half-loaded
# codewords, each given up (abort), then three decoded back to back
ABORT_PLAN = [op for _ in range(5) for op in (("begin", 4, 0), ("llr", 4, 0, 0, HALF), ("abort", 0),
                                               ("tick", 3))] + whole(0) + whole(1) + whole(2)
# two slow codewords committed, a third begun: every bank held
FULL_PLAN = whole(SLOW[0]) + whole(SLOW[1]) + [("begin", 0, 0), ("tick", 10), ("llr", 0, 0, 0, None),
                                               ("start", 0, 0)]


class DecoderMonitorTest(unittest.TestCase):
    """`LdpcDecoder`'s bank monitors on the toy code."""

    def test_bank_leak_e95c563(self):
        """The allocation rule before e95c563: a `begin` always takes a new
        bank (no reuse) and `abort` does nothing; each abandoned codeword
        keeps its bank."""
        self.assertEqual(decoder_run(ABORT_PLAN), [])
        bad = decoder_run(ABORT_PLAN, [
            ("reuse.eq(self.begin & holds & ~self.start)", "reuse.eq(0)"),
            ("with m.Elif(self.abort):", "with m.Elif(self.abort & 0):")])
        print("\n".join(bad))
        for name in ("bank_accounting", "abort_closes", "bank_leak"):
            self.assertTrue(fired(bad, name), (name, bad))

    def test_abort_leaves_the_context_open(self):
        bad = decoder_run(ABORT_PLAN, [
            ("m.d.sync += [ctx_open[self.in_ctx].eq(0), ctx_drop[self.in_ctx].eq(1)]",
             "m.d.sync += [ctx_drop[self.in_ctx].eq(1)]")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "abort_closes"), bad)

    def test_held_and_load_ready(self):
        self.assertEqual(decoder_run(FULL_PLAN), [])
        bad = decoder_run(FULL_PLAN, [("self.held.eq(sum(alloc[b] for b in range(K)))", "self.held.eq(0)")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "held"), bad)
        bad = decoder_run(FULL_PLAN, [("self.load_ready.eq(~full)", "self.load_ready.eq(1)")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "load_ready"), bad)

    def test_commit_without_begin(self):
        """A second `start` with no `begin` (a chain that commits twice)
        would decode the same bank twice and free it twice."""
        bad = decoder_run(whole(0) + [("start", 0, 0)])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "commit_without_begin"), bad)

    def test_queue_overflow(self):
        """More commits than the 8-entry result queue holds (dropped
        codewords take no bank but an entry) while the engine decodes."""
        plan = whole(SLOW[0]) + whole(SLOW[1]) + whole(SLOW[2])
        plan += [op for _ in range(12) for op in (("begin", 0, 0), ("start", 0, 0))]
        bad = decoder_run(plan)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "queue_overflow"), bad)


# --- the demapper's noise count ---------------------------------------------------

class NoiseMonitorTest(unittest.TestCase):
    @needs("3-eq", "4-tmcc", "5-demap")
    @unittest.skipUnless(COMPILED, "a whole frame: the compiled backend")
    def test_noise_count_243d672(self):
        """DemapTest.test_next_frame_starts_at_once's run (the next frame a
        cycle after the frame's last symbol) with 243d672 reverted: its
        start clears the count before the division."""
        from isdb_fpga.chain.stages import run_back

        with source_patch("isdb_fpga.chain.demap", [
                ("with m.Elif((self.f_start | self.f_drop) & ~drain):",
                 "with m.Elif(self.f_start | self.f_drop):")]):
            _what, bad = run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), COMPILED, None, True)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "noise_count"), bad)


# --- the finish stage: status points, the packer ----------------------------------

class FinishMonitorBench(Elaboratable):
    """`Finish` fed as tests/test_chain.py's FinishBench (kind 0 a frame,
    1 a result header, 2 a word), with a point RAM holding 16APSK's
    learned points (as StatusPointsTest)."""

    PTS = [(100 + 3 * lb, -(200 + 5 * lb)) for lb in range(16)]

    def __init__(self):
        from isdb_fpga.chain.finish import Finish
        from isdb_fpga.chain.stages import monitored

        self.fin = Finish()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(2)
        self.a = Signal(32)
        self.b = Signal(32)
        self.c = Signal(8)
        self.d = Signal()
        self.e = Signal()
        monitored(self)

    def elaborate(self, platform):
        from amaranth.lib.memory import Memory

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
        st = f.status
        init = [0] * 256
        for lb, (re, im) in enumerate(self.PTS):
            init[3 << 5 | lb] = (re & 0xFFFF) | (im & 0xFFFF) << 16
        m.submodules.pts = mem = Memory(shape=32, depth=256, init=init)
        rp = mem.read_port()
        m.d.comb += [rp.addr.eq(st.pt_addr), st.pt_data.eq(rp.data), st.snap["learned"].eq(1 << 3)]
        return m

    def run(self, recs, tag):
        """(Slots words, monitor messages)."""
        f = self.fin
        ports = {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
                 "b": self.b, "c": self.c, "d": self.d, "e": self.e, "ov": f.o_valid, "ow": f.o_word,
                 **self.mon.ports()}
        e = simx.engine(self, ports, BACKEND, tag=tag)
        g = e.group("ov", ["ow"])
        e.stall_limit = 1_000_000 if BACKEND != "pysim" else None
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=20_000)
        out = [w for (w,) in e.read(g)]
        return out, self.mon.report(e.get)


# frame 5 ends, its TMCC codeword comes back dropped: the TMCC and status blocks
STATUS_RECS = [(0, 5, 0, 0, 0, 0), (1, 1 << 7 | 3 << 8, 5, 0, 0, 1)]
# then frame 6's slot 121 (an index no layout has; 93/120, dropped)
SLOT121_RECS = STATUS_RECS + [(1, 121 | 7 << 8 | 4 << 12, 6, 0, 0, 1)]


class FinishMonitorTest(unittest.TestCase):
    def test_status_points_d53de60(self):
        """The status words taken a cycle after `next` (the pacing before
        d53de60): each learned point's re word is the previous point's."""
        out, bad = FinishMonitorBench().run(STATUS_RECS, "monfin")
        self.assertEqual(bad, [])
        with source_patch("isdb_fpga.chain.finish", [("with m.If(st_pace != 2):", "with m.If(st_pace != 1):")]):
            out, bad = FinishMonitorBench().run(STATUS_RECS, "monfin")
        print("\n".join(bad))
        self.assertTrue(fired(bad, "status_point_settled"), bad)

    def test_sequence_numbers(self):
        """The packer's sequence number advancing by 2 per block."""
        with source_patch("isdb_fpga.chain.packer", [("m.d.sync += seq.eq(seq + 1)",
                                                       "m.d.sync += seq.eq(seq + 2)")]):
            _out, bad = FinishMonitorBench().run(STATUS_RECS, "monfin")
        print("\n".join(bad))
        self.assertTrue(fired(bad, "seq_contiguous"), bad)

    def test_framing_and_kind(self):
        """A wrong header check word (framing), a wrong format version in
        header word 2 (block_kind)."""
        with source_patch("isdb_fpga.chain.packer", [("hw.append((0xFFFF - s)[:16])",
                                                       "hw.append((0xFFFE - s)[:16])")]):
            _out, bad = FinishMonitorBench().run(STATUS_RECS, "monfin")
        print("\n".join(bad))
        self.assertTrue(fired(bad, "framing"), bad)
        with source_patch("isdb_fpga.chain.packer", [("w2 = 0xF | (kind << 4) | (1 << 8)",
                                                       "w2 = 0xF | (kind << 4) | (2 << 8)")]):
            _out, bad = FinishMonitorBench().run(STATUS_RECS, "monfin")
        print("\n".join(bad))
        self.assertTrue(fired(bad, "block_kind"), bad)

    def test_slot_index(self):
        """A slot result tagged with slot 121 (no layout has it): the finish
        stage sends dummies up to it and its block; the slot index check
        fires at the block of slot 120."""
        _out, bad = FinishMonitorBench().run(SLOT121_RECS, "monfin")
        print("\n".join(bad))
        self.assertTrue(fired(bad, "slot_index"), bad)


class PackerStartTest(unittest.TestCase):
    """A START (SlotsRx's CTRL START, one cycle) restarts the `Slots`
    sequence numbers at the next block boundary: a START while a block is
    being sent leaves that block whole with its old number, the next block
    has sequence 0 (found by `seq_contiguous` and the chaos runs: the
    packer honoured `start` only between blocks, so the next block carried
    the old count + 1); a START in the cycle a block is offered gives that
    block 0 and does not lose it (the finish stage offers a block for one
    cycle)."""

    def run_blocks(self, start_at):
        """Three 20-word status blocks; `start` high in block `start_at[0]`'s
        cycle `start_at[1]` after its offer (-1: the offer's cycle). Returns
        (the sequence numbers, the monitor messages)."""
        from isdb_fpga.chain import slots as S
        from isdb_fpga.chain.packer import Packer

        pk = Packer()
        col = monitors.Collector(monitors.enable(pk))
        sim = Simulator(pk)
        sim.add_clock(25e-9)
        words, report = [], []

        async def bench(ctx):
            for blk in range(3):
                while not ctx.get(pk.b_ready):
                    await ctx.tick()
                # the finish stage's offer: b_valid for one cycle
                ctx.set(pk.b_valid, 1)
                ctx.set(pk.b_kind, 3)
                ctx.set(pk.b_len, 20)
                ctx.set(pk.b_frame, blk)
                ctx.set(pk.start, int((blk, -1) == start_at))
                await ctx.tick()
                ctx.set(pk.b_valid, 0)
                ctx.set(pk.w_valid, 1)
                for i in range(40):
                    ctx.set(pk.start, int((blk, i) == start_at))
                    await ctx.tick()
                    if ctx.get(pk.o_valid):
                        words.append(ctx.get(pk.o_word))
                ctx.set(pk.start, 0)
                ctx.set(pk.w_valid, 0)
                await ctx.tick().repeat(5)
            report.extend(col.report(lambda n: ctx.get(col.ports()[n])))

        sim.add_testbench(bench)
        sim.run()
        blocks = S.blocks(words)
        self.assertEqual([b[2] for b in blocks], [0, 1, 2], "every block sent whole, in order")
        return [b[1] for b in blocks], report

    def test_start_during_a_block(self):
        for at in ((1, 1), (1, 5), (1, 25), (1, 30)):  # header, payload, trailer of block 1
            seqs, report = self.run_blocks(at)
            print(at, seqs, report)
            self.assertEqual(seqs, [0, 1, 0], at)
            self.assertEqual(report, [], at)

    def test_start_with_an_offer(self):
        seqs, report = self.run_blocks((2, -1))
        print(seqs, report)
        self.assertEqual(seqs, [0, 1, 0])
        self.assertEqual(report, [])

    def test_start_between_blocks(self):
        seqs, report = self.run_blocks((1, 39))  # block 1 has left: idle
        self.assertEqual((seqs, report), ([0, 1, 0], []))

    def test_a_start_ignored_mid_block_is_caught(self):
        """The defect re-introduced (the latch removed: `start` honoured in
        IDLE only): `seq_contiguous` fires."""
        with source_patch("isdb_fpga.chain.packer", [("with m.If(self.start | restart):",
                                                       "with m.If(self.start):")]):
            seqs, report = self.run_blocks((1, 5))
        print(seqs, report)
        self.assertEqual(seqs, [0, 1, 2])
        self.assertTrue(fired(report, "seq_contiguous"), report)


# --- SlotsRx's registers ----------------------------------------------------------

def slots_rx_run(edits=()):
    """`SlotsRx` (no decoder, monitors on) through `gpio_out` on the Python
    simulator, `top.py` patched with `edits`: TAPS_FROM written 5 and 87,
    VERSION read, then a sum register read (its word 3 symbol clocks
    later). Returns the monitor messages."""
    from isdb_fpga.chain.common import cos_table
    from isdb_fpga.control import OP_ADDR, OP_READ, OP_WRITE_S

    with source_patch("isdb_fpga.chain.top", list(edits)) as top:
        dut = top.SlotsRx(32, cos_table(), decoder=False, monitors=True)
        col = monitors.Collector(dut)
        m = Module()
        m.submodules.dut = dut
        m.domains.sync = ClockDomain()
        m.domains.fast = ClockDomain()
        m.domains.ldpc = ClockDomain()
        sim = Simulator(m)
    sim.add_clock(25e-9, domain="sync")
    sim.add_clock(6.25e-9, phase=12.5e-9, domain="fast")
    sim.add_clock(5e-9, domain="ldpc")
    out = []

    async def bench(ctx):
        t = 0

        async def cmd(op, arg=0, wait=8):
            nonlocal t
            t ^= 1
            ctx.set(dut.gpio_out, t << 31 | op << 28 | (arg & 0x7FFFFFF) << 1)
            await ctx.tick("sync").repeat(wait)

        for n in (5, 87):
            await cmd(OP_ADDR, 0x41B)
            await cmd(OP_WRITE_S, n)
        await cmd(OP_ADDR, 0x400)
        await cmd(OP_READ)
        await cmd(OP_ADDR, 0x500 + 4 * 5)
        await cmd(OP_READ, wait=40)
        out.extend(col.report(lambda n: ctx.get(col.ports()[n])))

    sim.add_testbench(bench)
    sim.run()
    return out


# 5f60649 reverted: TAPS_FROM registered, the equaliser armed from the register
TAPS_FROM_REVERT = [
    ("m.d.comb += [eqm.tap_at.eq(value + EQ_APPLY_FRAMES), eqm.tap_from_in.eq(value)]",
     "tap_at = Signal(32)\n        tap_from = Signal(32)\n"
     "        m.d.comb += [eqm.tap_at.eq(tap_at), eqm.tap_from_in.eq(tap_from)]"),
    ("with m.Case(REG_TAPS_FROM):\n                            m.d.comb += eqm.tap_arm.eq(1)",
     "with m.Case(REG_TAPS_FROM):\n"
     "                            m.d.sync += [tap_from.eq(value), tap_at.eq(value + EQ_APPLY_FRAMES)]\n"
     "                            m.d.comb += eqm.tap_arm.eq(1)"),
]
# a052ece's readback half reverted: a READ of a sum register shows 0 until its word
READBACK_REVERT = [("with m.If(~(sums_r | sums_p)):", "with m.If(sums_r | ~sums_r):")]


class SlotsRxMonitorTest(unittest.TestCase):
    def test_quiet(self):
        self.assertEqual(slots_rx_run(), [])

    def test_taps_from_5f60649(self):
        bad = slots_rx_run(TAPS_FROM_REVERT)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "taps_from_written"), bad)

    def test_sum_readback_a052ece(self):
        bad = slots_rx_run(READBACK_REVERT)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "sum_readback_hold"), bad)

    def test_sum_read_never_answered(self):
        """A sum READ the equaliser never answers (its read port never
        granted): the 16-clock bound."""
        with source_patch("isdb_fpga.chain.eq", [("m.d.comb += rd_go.eq(rd_pend & ~rd_s1)",
                                                   "m.d.comb += rd_go.eq(0)")]):
            bad = slots_rx_run()
        print("\n".join(bad))
        self.assertTrue(fired(bad, "sum_read_slow"), bad)


# --- the equaliser's taps ---------------------------------------------------------

class EqTapsBench(Elaboratable):
    """The equaliser's tap path alone: a TAPS_FROM set armed for frame 0
    from frame 7, then numbered symbols (the switch at the centre)."""

    def __init__(self):
        from isdb_fpga.chain.eq import Equaliser
        from isdb_fpga.chain.stages import monitored

        self.eq = Equaliser()
        self.go = Signal()
        self.arm = Signal()
        monitored(self)

    def elaborate(self, platform):
        m = Module()
        m.submodules.eq = eq = self.eq
        from isdb_fpga.chain.stages import pack_meta

        m.d.comb += [eq.tap_arm.eq(self.arm), eq.tap_at.eq(0), eq.tap_from_in.eq(7),
                     eq.i_valid.eq(self.go), eq.i_meta.eq(pack_meta(numbered=1, frame=1))]
        return m


def eq_taps_run(edits=()):
    with source_patch("isdb_fpga.chain.eq", list(edits)):
        b = EqTapsBench()
        sim = Simulator(b)
    sim.add_clock(25e-9)
    sim.add_clock(6.25e-9, phase=12.5e-9, domain="fast")
    out = []

    async def bench(ctx):
        ctx.set(b.arm, 1)
        await ctx.tick()
        ctx.set(b.arm, 0)
        ctx.set(b.go, 1)
        await ctx.tick().repeat(20)
        out.append(ctx.get(b.eq.taps_from))
        out.extend(b.mon.report(lambda n: ctx.get(b.mon.ports()[n])))

    sim.add_testbench(bench)
    sim.run()
    return out


class EqMonitorTest(unittest.TestCase):
    def test_taps_from_source(self):
        self.assertEqual(eq_taps_run(), [7])
        bad = eq_taps_run([("m.d.sync += [self.taps_from.eq(pend_from), pend_valid.eq(0)]",
                            "m.d.sync += [self.taps_from.eq(pend_at), pend_valid.eq(0)]")])
        print("\n".join(bad[1:]))
        self.assertTrue(fired(bad[1:], "taps_from_source"), bad)


# --- FIFOs: the bridge, the result FIFO -------------------------------------------

def bridge_run(beats=(), results=0, out_words=0, out_ready=1, edits=()):
    """`LdpcBridge` (monitors on) on the Python simulator: `beats` port
    beats (op, ctx) written one per symbol clock, `results` result records
    and `out_words` words from the decoder side, the chain's `out_ready`."""
    from isdb_fpga.chain.ldpc_if import LdpcPort

    with source_patch("isdb_fpga.chain.ldpc_if", list(edits)) as li:
        port = LdpcPort()
        br = li.LdpcBridge(port, monitors=True)
        col = monitors.Collector(br)
        m = Module()
        m.submodules.br = br
        m.domains.sync = ClockDomain()
        m.domains.ldpc = ClockDomain()
        sim = Simulator(m)
    sim.add_clock(25e-9, domain="sync")
    sim.add_clock(5e-9, domain="ldpc")
    out = []

    async def chain(ctx):
        ctx.set(port.out_ready, out_ready)
        for op, cx in beats:
            ctx.set(port.in_valid, 1)
            ctx.set(port.in_op, op)
            ctx.set(port.in_ctx, cx)
            ctx.set(port.in_n, 1)
            ctx.set(port.in_rate, 3)
            await ctx.tick("sync")
        ctx.set(port.in_valid, 0)
        await ctx.tick("sync").repeat(300)
        out.extend(col.report(lambda n: ctx.get(col.ports()[n])))

    async def decoder(ctx):
        await ctx.tick("ldpc").repeat(50)
        for _ in range(results):
            ctx.set(br.res_valid, 1)
            await ctx.tick("ldpc")
            ctx.set(br.res_valid, 0)
            await ctx.tick("ldpc")
        for i in range(out_words):
            ctx.set(br.o_valid, 1)
            ctx.set(br.o_data, i)
            await ctx.tick("ldpc")
        ctx.set(br.o_valid, 0)

    sim.add_testbench(chain)
    sim.add_testbench(decoder, background=True)
    sim.run()
    return out


class FifoMonitorTest(unittest.TestCase):
    def test_bridge_input_fifo(self):
        from isdb_fpga.chain.ldpc_if import OP_WRITE

        beats = [(OP_WRITE, 0)] * 70
        self.assertEqual(bridge_run(beats), [])
        # the serialiser stalled: the 64-beat FIFO fills
        bad = bridge_run(beats, edits=[("m.d.comb += [fin.r_en.eq(~have & ~need_start), tags.w_data.eq(f[\"tag\"])]",
                                        "m.d.comb += [fin.r_en.eq(0), tags.w_data.eq(f[\"tag\"])]")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "in_fifo_overflow"), bad)

    def test_bridge_tags(self):
        from isdb_fpga.chain.ldpc_if import OP_BEGIN, OP_DONE

        # 17 commits and no result: the 16-entry tag queue overflows
        bad = bridge_run([(OP_BEGIN, 1), (OP_DONE, 1)] * 17)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "tag_overflow"), bad)
        # a result record with no commit before it
        bad = bridge_run(results=1)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "tag_underflow"), bad)

    def test_bridge_output_fifo(self):
        # the decoder's words while the chain takes none, `o_ready` ignored
        bad = bridge_run(out_words=80, out_ready=0, edits=[
            ("self.o_ready.eq(fout.w_level < self.OUT_DEPTH - 4)", "self.o_ready.eq(1)")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "out_fifo_overflow"), bad)

    def test_result_fifo(self):
        """Two 93/120 results (a header, then 2 174 kept words each) into the
        4 096-entry FIFO while the finish stage takes nothing: the words
        wait for room; with the data words' ready forced, they are written
        into the full FIFO."""
        def run(edits=()):
            with source_patch("isdb_fpga.chain.results", list(edits)) as r:
                rf = r.ResultFifo()
                monitors.enable(rf)
                col = monitors.Collector(rf)
                sim = Simulator(rf)
            sim.add_clock(25e-9)
            out = []

            async def push(ctx, head, n):
                ctx.set(rf.i_head, head)
                ctx.set(rf.i_valid, 1)
                for _ in range(n):
                    for _ in range(100):
                        if ctx.get(rf.i_ready):
                            break
                        await ctx.tick()
                    else:
                        return
                    await ctx.tick()
                ctx.set(rf.i_valid, 0)

            async def bench(ctx):
                ctx.set(rf.o_ready, 0)
                ctx.set(rf.i_tag, 7 << 8)
                for _ in range(2):
                    await push(ctx, 1, 1)
                    await push(ctx, 0, 2200)
                await ctx.tick().repeat(10)
                out.extend(col.report(lambda n: ctx.get(col.ports()[n])))

            sim.add_testbench(bench)
            sim.run()
            return out

        self.assertEqual(run(), [])
        bad = run([("self.i_ready.eq(Mux(self.i_head, room, f.w_rdy | (idx >= kw)))",
                    "self.i_ready.eq(Mux(self.i_head, room, 1))")])
        print("\n".join(bad))
        self.assertTrue(fired(bad, "overflow"), bad)


# --- the back end: the port, TMCC staging, frame order ----------------------------

class BackMonitorTest(unittest.TestCase):
    @needs("3-eq", "4-tmcc", "5-demap")
    def test_port_lost(self):
        """The decoder port never ready: the demapper's beats (which cannot
        wait) are lost."""
        from isdb_fpga.chain import stages

        with source_patch("isdb_fpga.chain.stages", [(
                "            p.in_ready.eq(1),\n            # the demapper's beats",
                "            p.in_ready.eq(0),\n            # the demapper's beats")]):
            _what, bad = stages.run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), BACKEND, 12_000)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "port_lost"), bad)

    def test_tmcc_staging_and_burst(self):
        """TmccSoft: the next frame's soft bits written into the staging
        buffer ahead of the burst's reads (staging_overrun), and a frame's
        end while the burst runs (burst_lost): inputs a correct back end
        never gives (a frame's TMCC symbols are 9 000 symbols after the
        burst's end)."""
        from isdb_fpga.chain import stages

        recs = [(stages.T_SYM, 1, 2, 0, b) for b in range(64)] + [(stages.T_DONE, 5, 0, 0, 0)]
        recs += [(stages.T_SYM, 1, 2, 0, b) for b in range(4000)] + [(stages.T_DONE, 6, 0, 0, 0)]
        b = stages.TmccBench()
        e = simx.engine(b, b.ports(), BACKEND, tag="montmcc")
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d"], recs, tail=100)
        bad = b.mon.report(e.get)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "staging_overrun"), bad)
        self.assertTrue(fired(bad, "burst_lost"), bad)

    def test_frame_order(self):
        """FrameCtl: frame 5 begun, then frames 3 and 2: 5 and 3 dropped,
        3 after 5."""
        from isdb_fpga.chain import stages

        recs = []
        for fr in (5, 3, 2):
            recs += [(stages.K_SYM, 0, 0, 0, stages.pack_meta(j=j, number=0, numbered=1, frame=fr, word=1), 0)
                     for j in range(20)]
        b = stages.EqBench()
        e = simx.engine(b, b.ports(), BACKEND, tag="monframe")
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "meta", "at"], recs, tail=100)
        bad = b.mon.report(e.get)
        print("\n".join(bad))
        self.assertTrue(fired(bad, "frame_order"), bad)


if __name__ == "__main__":
    unittest.main()
