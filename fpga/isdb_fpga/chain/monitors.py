"""Invariant monitors: properties that hold in every correct run of the
chain and the LDPC decoder, checked on every cycle of every simulation,
independent of the vectors (issue #30, item 2).

Six defects reached the board although every bit-exact vector test had
passed (fpga/README.md "Invariant monitors"); each broke a property that
could be asserted inside the simulation. The monitors are such
assertions, compiled into the design only when asked for:

- A monitored module (`LdpcDecoder`, `LdpcBridge`, `Demap`, `Back`,
  `TmccSoft`, `FrameCtl`, `Equaliser`, `ResultFifo`, `Finish`,
  `SumsReader`, `Packer`, `SlotsRx`) declares its checks in the class attribute `MONITORS`
  ({name: (domain, what must hold)}) and has `monitor = None`.
  `enable(design)` walks a design's submodule objects and gives each such
  module a `Monitor`; the module's `elaborate` then adds the checks (a
  hook at its end, calling `Monitor.watch` with each check's violation
  condition). Without `enable` nothing is added: the design, its RTLIL
  and its bitstream are those of a build without monitors (`monitors=False`
  is the default of `SlotsChain`, `SlotsRx`, `LdpcDecoder` and
  `LdpcBridge`; the simulation benches default to `monitors=True`).
- Each check is a sticky register (48 bits, in the check's clock domain)
  holding 1 + the cycle of its first violation (0: never violated); the
  cycle is counted in that domain from reset (`sync`: the symbol clock,
  the engine's cycle; `ldpc`: the decoder's clock, 5 per symbol clock in
  the simulations). The registers are plain signals, so every backend
  (Amaranth's simulator, CXXRTL, Verilator) reads them the same way, as
  top-level ports of the bench (`Collector.ports()`), through the
  engine's `get` after a run.
- `Collector(design)` gathers a design's monitors; `report(get)` turns
  their values into messages ("monitor ch.back.demap.noise_count (n0 ...)
  first violated at sync cycle 1234567"), `check(...)` raises
  `MonitorError` with them. `stages.py`'s runs add the messages to their
  mismatches after every run.

Monitors observe only: nothing they add drives a signal of the design.
Amaranth's `Assert` is not used: Yosys's `write_verilog` and Verilator's
handling of the `$check` cells it makes are not something the backends
are known to honour alike; the registers are.

The proofs that each monitor fires (the defect it targets re-introduced
on a scratch copy of the source) are in tests/test_monitors.py, through
`source_patch`, which is also what `tools/pipeline_mutation.py` uses.
"""

import contextlib
import importlib
import sys

from amaranth.hdl import Cat, Elaboratable, Mux, Signal

CYCLE_BITS = 48


class MonitorError(AssertionError):
    pass


class Monitor:
    """The checks of one module instance. `path`: its place in the design
    (the attribute path from the root given to `enable`, which names the
    ports); `checks`: the class's `MONITORS`; `clocks`: the actual clock of
    each of the module's domains where a `DomainRenamer` moves it (for the
    report only)."""

    def __init__(self, path, checks, clocks=None):
        self.path = path
        self.checks = checks
        self.clocks = clocks or {}
        tag = path.replace(".", "_") or "top"
        self.first = {n: Signal(CYCLE_BITS, name=f"mon_{tag}_{n}") for n in checks}
        self.watched = set()
        self._cycle = {}

    def cycle(self, m, domain):
        """The module's cycle counter in `domain` (one per domain)."""
        if domain not in self._cycle:
            c = Signal(CYCLE_BITS, name=f"mon_cycle_{domain}")
            m.d[domain] += c.eq(c + 1)
            self._cycle[domain] = c
        return self._cycle[domain]

    def watch(self, m, name, bad):
        """Check `name`: `bad` (a value of the module, in the check's domain)
        is a violation in every cycle it is high."""
        if name not in self.checks:
            raise KeyError(f"{self.path}: no check {name!r} in MONITORS")
        if name in self.watched:
            raise ValueError(f"{self.path}: check {name!r} watched twice")
        self.watched.add(name)
        domain = self.checks[name][0]
        first = self.first[name]
        cyc = self.cycle(m, domain)  # (outside the condition: it counts every cycle)
        with m.If(bad & (first == 0)):
            m.d[domain] += first.eq(cyc + 1)

    def clock(self, name):
        d = self.checks[name][0]
        return self.clocks.get(d, d)


def _children(obj):
    for key, v in sorted(vars(obj).items()):
        if isinstance(v, Elaboratable):
            yield key, v
        elif isinstance(v, (list, tuple)):
            for i, x in enumerate(v):
                if isinstance(x, Elaboratable):
                    yield f"{key}{i}", x


def enable(root, rename=None, path=""):
    """Give every monitored module reachable from `root` (through its
    attributes, lists of them included) a `Monitor`, unless it has one.
    `rename`: {attribute path: {domain: clock}} for the modules a
    `DomainRenamer` moves (e.g. {"dec": {"sync": "ldpc"}}): their reports
    name the actual clock. Returns `root`."""
    rename = rename or {}
    seen = set()

    def walk(obj, p, clocks):
        if id(obj) in seen:
            return
        seen.add(id(obj))
        clocks = rename.get(p, clocks)
        if getattr(type(obj), "MONITORS", None) and getattr(obj, "monitor", None) is None:
            obj.monitor = Monitor(p or type(obj).__name__, type(obj).MONITORS, clocks)
        for key, child in _children(obj):
            walk(child, f"{p}.{key}" if p else key, clocks)

    walk(root, path, {})
    return root


def monitors_of(root):
    """Every `Monitor` reachable from `root`, in walk order."""
    out, seen = [], set()

    def walk(obj):
        if id(obj) in seen:
            return
        seen.add(id(obj))
        # (a DomainRenamer's wrapper forwards attributes: one entry per monitor)
        mon = getattr(obj, "monitor", None)
        if isinstance(mon, Monitor) and id(mon) not in seen:
            seen.add(id(mon))
            out.append(mon)
        for _key, child in _children(obj):
            walk(child)

    walk(root)
    return out


class Collector:
    """A design's monitors as ports and as a report."""

    def __init__(self, root):
        self.monitors = monitors_of(root)

    def ports(self):
        """{port name: signal} of every check (add them to the bench's ports)."""
        return {s.name: s for mon in self.monitors for s in mon.first.values()}

    def report(self, get):
        """The violations, one message each, first cycle first. `get(name)`:
        a port's value after the run (the engine's `get`; for a Python
        simulator testbench, `lambda n: ctx.get(ports[n])`)."""
        found = []
        for mon in self.monitors:
            missing = set(mon.checks) - mon.watched
            if missing:
                found.append((-1, f"monitor {mon.path}: checks {sorted(missing)} were never watched "
                                  "(the module's hook does not drive them)"))
            for name, sig in mon.first.items():
                v = get(sig.name)
                if v:
                    clk = mon.clock(name)
                    at = f"{clk} cycle {v - 1}"
                    if clk == "ldpc":
                        at += f" (symbol clock {(v - 1) // 5})"
                    found.append((v - 1, f"monitor {mon.path}.{name} ({mon.checks[name][1]}) first "
                                         f"violated at {at}"))
        return [msg for _, msg in sorted(found, key=lambda x: x[0])]

    def check(self, get):
        bad = self.report(get)
        if bad:
            raise MonitorError("; ".join(bad))

    def names(self):
        return [f"{mon.path}.{n}" for mon in self.monitors for n in mon.checks]


def popcount(v):
    return sum(v[i] for i in range(len(v)))


def fifo_overflow(m, mon, name, fifo):
    """A write while the FIFO is full: the write is lost (Amaranth's FIFOs
    ignore it), whatever counts it."""
    mon.watch(m, name, fifo.w_en & ~fifo.w_rdy)


def fifo_underflow(m, mon, name, fifo):
    mon.watch(m, name, fifo.r_en & ~fifo.r_rdy)


# --- the checks of each module (called from its `elaborate` hook) -----------

LEAK_CYCLES = 32  # beyond the decoder's commit token's way into the queue (12 cycles)


def ldpc_banks(m, mon, dec, K, alloc, ctx_open, ctx_drop, holds, free_mask, out_active, q_wr, q_oq):
    """`LdpcDecoder`'s bank allocation (issue #28's class). `committed` is
    the monitor's own count of committed codewords that hold a bank (a
    `start` on a context holding one, until its output stream ends)."""
    sync = m.d.sync
    hold = [ctx_open[c] & ~ctx_drop[c] for c in range(2)]
    committed = Signal(range(K + 2), name="mon_committed")
    # a `start` commits the context's open codeword also when the next
    # codeword's `begin` shares the cycle (the decoder's ports allow it:
    # the bank is committed and a new one allocated; the formal `banks`
    # harness proves the accounting there, tools/monitor_begin_start.py
    # shows the case)
    commit = dec.start & holds
    sync += committed.eq(committed + commit - free_mask.any())
    mon.watch(m, "bank_accounting", popcount(alloc) != hold[0] + hold[1] + committed)
    alloc_d = Signal(K, name="mon_alloc_d")
    sync += alloc_d.eq(alloc)
    mon.watch(m, "held", dec.held != popcount(alloc_d))
    lr_d = Signal(init=1, name="mon_load_ready_d")
    sync += lr_d.eq(dec.load_ready)
    mon.watch(m, "load_ready", lr_d != (dec.held < K))
    ab = Signal(name="mon_abort_d")
    ab_ctx = Signal(name="mon_abort_ctx")
    sync += [ab.eq(dec.abort & ~dec.begin & ~dec.start), ab_ctx.eq(dec.in_ctx)]
    mon.watch(m, "abort_closes", ab & (ctx_open[ab_ctx] | ~ctx_drop[ab_ctx]))
    mon.watch(m, "commit_without_begin",
              dec.start & ~dec.begin & ~ctx_open[dec.in_ctx] & ~ctx_drop[dec.in_ctx])
    idle = ~dec.busy & ~out_active & (q_wr == q_oq) & ~hold[0] & ~hold[1]
    t = Signal(range(LEAK_CYCLES + 1), name="mon_leak_t")
    with m.If(idle & alloc.any()):
        with m.If(t != LEAK_CYCLES):
            sync += t.eq(t + 1)
    with m.Else():
        sync += t.eq(0)
    mon.watch(m, "bank_leak", t == LEAK_CYCLES)
    qo = Signal(name="mon_queue_overflow_d")
    sync += qo.eq(dec.queue_overflow)
    mon.watch(m, "queue_overflow", dec.queue_overflow & ~qo)


def demap_noise(m, mon, dm, sample, tag_frame, ncnt, noise_idle):
    """`Demap`'s noise count at a decoded frame's end (243d672's class): the
    monitor counts the noise samples by the frame each carries (the beat's
    context, `ctx[42:74]`); at `f_end` the demapper's count must equal the
    samples of the frame that ended (`dm.frame` at `f_swap`), 0 if the last
    samples belong to another frame."""
    sync = m.d.sync
    sf = Signal(32, name="mon_noise_frame")  # the frame of the last sample
    sn = Signal(len(ncnt), name="mon_noise_n")  # its samples
    seen = Signal(name="mon_noise_seen")
    with m.If(sample):
        with m.If(seen & (tag_frame == sf)):
            sync += sn.eq(sn + 1)
        with m.Else():
            sync += [sf.eq(tag_frame), sn.eq(1), seen.eq(1)]
    ended = Signal(32, name="mon_noise_ended")
    with m.If(dm.f_swap):
        sync += ended.eq(dm.frame)
    want = Signal(len(ncnt), name="mon_noise_want")
    m.d.comb += want.eq(Mux(seen & (sf == ended), sn, 0))
    mon.watch(m, "noise_count", dm.f_end & noise_idle & (ncnt != want))


def packer_stream(m, mon, pk):
    """`Packer`: the sequence numbers in the output's headers (contiguous
    from 0, and 0 again for the first block accepted in or after the cycle
    of a `start`: a `start` while a block leaves does not change that
    block), and the output words parsed as `Slots` blocks (sync words,
    header word 2, the header check, the payload length that puts the next
    block's sync words where they belong, a slot block's slot index <
    120)."""
    from .slots import SLOTS_VERSION

    sync = m.d.sync
    # which blocks must restart the count: decided at acceptance (the
    # packer takes every block offered while idle), checked when the
    # block's header words 3 and 4 leave (a block's header leaves before
    # the next block can be accepted)
    accept = pk.b_ready & pk.b_valid
    started = Signal(name="mon_seq_started")
    zero = Signal(name="mon_seq_zero")
    last = Signal(32, name="mon_seq_last")
    have = Signal(name="mon_seq_have")
    seq_lo = Signal(16, name="mon_seq_lo")
    with m.If(accept):
        sync += [zero.eq(started | pk.start | ~have), have.eq(1), started.eq(0)]
    with m.Elif(pk.start):
        sync += started.eq(1)
    expect = Mux(zero, 0, (last + 1)[:32])
    got = Cat(seq_lo, pk.o_word)
    bad_seq = Signal(name="mon_bad_seq")
    # the output parser
    ps = Signal(4, name="mon_ps")  # 0-8 header, 9 payload, 10-11 trailer
    pi = Signal(16, name="mon_pi")
    pl = Signal(16, name="mon_pl")
    cs = Signal(16, name="mon_csum")
    kind = Signal(4, name="mon_kind")
    w, v = pk.o_word, pk.o_valid
    bad_frame = Signal(name="mon_bad_frame")
    bad_kind = Signal(name="mon_bad_kind")
    bad_slot = Signal(name="mon_bad_slot")
    with m.If(v):
        with m.Switch(ps):
            with m.Case(0):
                m.d.comb += bad_frame.eq(w != 0x1ACF)
                sync += ps.eq(1)
            with m.Case(1):
                m.d.comb += bad_frame.eq(w != 0xFC1D)
                sync += ps.eq(2)
            with m.Case(2):
                k = w[4:8]
                m.d.comb += bad_kind.eq((w[0:4] != 0xF) | (w[8:16] != SLOTS_VERSION) | (k == 0) | (k > 4))
                sync += [cs.eq(w), kind.eq(k), ps.eq(3)]
            with m.Case(3, 4, 5, 6, 7):
                sync += [cs.eq(cs + w), ps.eq(ps + 1)]
                with m.If(ps == 3):
                    sync += seq_lo.eq(w)
                with m.If(ps == 4):
                    m.d.comb += bad_seq.eq(got != expect)
                    sync += last.eq(got)
                with m.If(ps == 5):
                    sync += pl.eq(w)
            with m.Case(8):
                m.d.comb += bad_frame.eq((cs + w)[:16] != 0xFFFF)
                sync += [pi.eq(0), ps.eq(Mux(pl == 0, 10, 9))]
            with m.Case(9):
                with m.If((pi == 0) & (kind == 1)):
                    m.d.comb += bad_slot.eq(w[:7] >= 120)
                sync += pi.eq(pi + 1)
                with m.If(pi + 1 == pl):
                    sync += ps.eq(10)
            with m.Case(10):
                sync += ps.eq(11)
            with m.Default():
                sync += ps.eq(0)
    mon.watch(m, "seq_contiguous", bad_seq)
    mon.watch(m, "framing", bad_frame)
    mon.watch(m, "block_kind", bad_kind)
    mon.watch(m, "slot_index", bad_slot)


READ_LIMIT = 16  # symbol clocks a sum register's READ may take (3 since a052ece)


def slots_rx(m, mon, eqm, readback, value, fifo):
    """`SlotsRx`'s register file: the sum readback (a052ece's class), the
    TAPS_FROM write (5f60649's class), the input FIFO."""
    from .top import EQ_APPLY_FRAMES

    sync = m.d.sync
    pend = Signal(name="mon_sum_pending")
    with m.If(eqm.rd_req):
        sync += pend.eq(1)
    with m.Elif(eqm.rd_ack):
        sync += pend.eq(0)
    rb_d = Signal(32, name="mon_readback_d")
    sync += rb_d.eq(readback)
    mon.watch(m, "sum_readback_hold", pend & (readback != rb_d))
    t = Signal(range(READ_LIMIT + 1), name="mon_sum_wait")
    with m.If(pend & ~eqm.rd_ack):
        with m.If(t != READ_LIMIT):
            sync += t.eq(t + 1)
    with m.Else():
        sync += t.eq(0)
    mon.watch(m, "sum_read_slow", t == READ_LIMIT)
    w = Signal(32, name="mon_taps_from_w")
    armed = Signal(name="mon_taps_armed")
    sync += armed.eq(eqm.tap_arm)
    with m.If(eqm.tap_arm):
        sync += w.eq(value)
    mon.watch(m, "taps_from_written",
              armed & ((eqm.pend_from != w) | (eqm.pend_at != (w + EQ_APPLY_FRAMES)[:32])))
    fifo_overflow(m, mon, "input_fifo_overflow", fifo)


# --- source patches: a defect re-introduced on a scratch copy of a module ----

@contextlib.contextmanager
def source_patch(module, edits):
    """Re-execute `module` (a dotted name or the module) with `edits`
    ([(old, new)], each `old` occurring exactly once) applied to its source,
    and point every loaded `isdb_fpga` module's references to the old
    classes and functions at the new ones; everything is restored on exit.
    Elaborate (build the engine) inside the block: the patched design is
    another RTLIL, so the compiled backends build and cache it separately.
    Used by the monitors' proofs (tests/test_monitors.py) and the pipeline
    mutation run (tools/pipeline_mutation.py)."""
    mod = importlib.import_module(module) if isinstance(module, str) else module
    with open(mod.__file__) as f:
        src = f.read()
    for old, new in edits:
        n = src.count(old)
        if n != 1:
            raise ValueError(f"{mod.__name__}: {old!r} occurs {n} times (1 expected)")
        src = src.replace(old, new)
    saved = dict(vars(mod))
    exec(compile(src, mod.__file__, "exec"), vars(mod))  # noqa: S102 - our own source, patched
    swap = {}  # id(old class or function) -> the patched module's
    for k, v in saved.items():
        nv = vars(mod).get(k)
        if nv is not None and nv is not v and (isinstance(v, type) or callable(v)):
            swap[id(v)] = nv
    undo = []
    for other in list(sys.modules.values()):
        name = getattr(other, "__name__", "")
        if other is mod or not name.startswith("isdb_fpga"):
            continue
        for k, v in list(vars(other).items()):
            if id(v) in swap:
                undo.append((other, k, v))
                setattr(other, k, swap[id(v)])
    try:
        yield mod
    finally:
        for other, k, v in undo:
            setattr(other, k, v)
        vars(mod).clear()
        vars(mod).update(saved)
