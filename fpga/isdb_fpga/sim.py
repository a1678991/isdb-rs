"""Simulate the blocks on test vectors and compare them bit for bit.

Receive (`SymbolsRx`): vectors from the Rust front-end model (`cargo run
-p isdb-frontend --example vectors -- OUT.json ...`); every symbol's
internals and the output words are compared.

Transmit (`SymbolsTx`, a vectors file with "samples" and "stalls": from
the Rust model, or `python -m isdb_fpga.txmodel`): every sample's state
(frac, q, the line, the carry and the code that enters), its sums and its
output, and the counters, with the DMA's stalls and the rfifo's request
strobe as given.

python -m isdb_fpga.sim VECTORS.json [--pattern every|wfifo|jitter] [--load-port] [--via-gpio]
python -m isdb_fpga.sim TXVECTORS.json [--pattern every|rfifo|jitter] [--latency N]
                                       [--load-port] [--via-gpio]
"""

import collections
import json
import random
import sys

from amaranth.sim import Simulator

from . import tx
from .control import ID_MAGIC
from .frontend import FORMAT_VERSION, SymbolFrontend
from .top import SymbolsRx, SymbolsTx
from .txmodel import Dma, Encoder

FIELDS = ("phase", "y", "dy", "yk", "gain", "period", "z", "dz", "zk", "q", "e", "err")


def load(path):
    with open(path) as f:
        return json.load(f)


def gaps(pattern, rng):
    """Idle cycles after each input sample: `every` = a sample on every
    clock (the most the core must take); `wfifo` = util_wfifo's bursts of
    eight samples on consecutive clocks with the gap a 40 Msps stream from
    the 160 MHz interface leaves between bursts (eight samples per
    32 l_clk = eight divided clocks: on average one sample per clock, so
    no gap; the burst start jitters by a clock either way); `jitter` = 0
    to 3 idle clocks at random."""
    n = 0
    while True:
        if pattern == "every":
            yield 0
        elif pattern == "wfifo":
            n += 1
            yield (rng.choice((0, 1)) if n % 8 == 0 else 0)
        else:
            yield rng.randrange(4)


def simulate(doc, pattern="every", load_port=False, seed=1):
    """Run the core on `doc`'s input; returns (symbols, words) as lists in
    the JSON's layout. `pattern` sets the input strobe (`gaps`): the
    result must not depend on it. `load_port` loads the coefficients
    through the core's load port instead of the memories' initial
    contents."""
    p = doc["params"]
    taps = p["taps"]
    if load_port:
        dut = SymbolFrontend(taps, doc["cos_table"])
    else:
        dut = SymbolFrontend(taps, doc["cos_table"], p["h"], p["g"], p["k"])
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    syms, words = [], []
    rng = random.Random(seed)

    def sample(ctx):
        if ctx.get(dut.s_valid):
            syms.append({
                "phase": ctx.get(dut.s_phase),
                "y": [ctx.get(s) for s in dut.s_y],
                "dy": [ctx.get(s) for s in dut.s_dy],
                "yk": [ctx.get(s) for s in dut.s_yk],
                "gain": ctx.get(dut.s_gain),
                "period": ctx.get(dut.s_period),
                "z": [ctx.get(s) for s in dut.s_z],
                "dz": [ctx.get(s) for s in dut.s_dz],
                "zk": [ctx.get(s) for s in dut.s_zk],
                "q": [ctx.get(s) for s in dut.s_q],
                "e": ctx.get(dut.s_e),
                "err": ctx.get(dut.s_err),
            })
        if ctx.get(dut.o_valid):
            words.append(ctx.get(dut.o_word))

    async def bench(ctx):
        async def tick():
            await ctx.tick()
            sample(ctx)

        for name, sig in dut.regs.fields().items():
            val = p[name]
            ctx.set(sig, val & ((1 << len(sig)) - 1) if not sig.shape().signed else val)
        if load_port:
            for ph in range(len(p["h"])):
                for t in range(taps):
                    ctx.set(dut.c_we_h, 1)
                    ctx.set(dut.c_we_g, 1)
                    ctx.set(dut.c_we_k, 1)
                    ctx.set(dut.c_phase, ph)
                    ctx.set(dut.c_tap, t)
                    ctx.set(dut.c_h, p["h"][ph][t])
                    ctx.set(dut.c_g, p["g"][ph][t])
                    ctx.set(dut.c_k, p["k"][ph][t])
                    await tick()
            ctx.set(dut.c_we_h, 0)
            ctx.set(dut.c_we_g, 0)
            ctx.set(dut.c_we_k, 0)
        ctx.set(dut.start, 1)
        await tick()
        ctx.set(dut.start, 0)
        gap = gaps(pattern, rng)
        for (i, q) in doc["input"]:
            ctx.set(dut.i_valid, 1)
            ctx.set(dut.i_i, i)
            ctx.set(dut.i_q, q)
            await tick()
            ctx.set(dut.i_valid, 0)
            for _ in range(next(gap)):
                await tick()
        for _ in range(400):
            await tick()

    sim.add_testbench(bench)
    sim.run()
    return syms, words


def simulate_top(doc, gap=6):
    """Configure `SymbolsRx` through gpio_out with the host's command words
    (`doc["commands"]`, encoded by `isdb_frontend::control::Encoder`),
    check the ID word and a register readback, then run the input and
    collect the words at the packer port. Returns (words, problems)."""
    p = doc["params"]
    dut = SymbolsRx(p["taps"], doc["cos_table"])
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    words, problems = [], []

    async def bench(ctx):
        async def tick(n=1):
            for _ in range(n):
                await ctx.tick()
                if ctx.get(dut.pack_valid):
                    words.append(ctx.get(dut.pack_data0))

        await tick(4)
        ident = ctx.get(dut.gpio_in)
        expect = ID_MAGIC << 16 | FORMAT_VERSION << 12 | p["taps"]
        if ident != expect:
            problems.append(f"ID word {ident:#010x} != {expect:#010x}")
        cmds = doc["commands"]
        last = 0
        for w in cmds[:-2]:  # configuration (without CTRL start/run)
            ctx.set(dut.gpio_out, w)
            last = w
            await tick(gap)
        # READ kp: ADDR 5 then READ, with the toggle continuing.
        t = (last >> 31) ^ 1
        for op, arg in ((1, 5), (4, 0)):
            ctx.set(dut.gpio_out, t << 31 | op << 28 | arg << 1)
            t ^= 1
            await tick(gap)
        kp = ctx.get(dut.gpio_in)
        if kp != p["kp"] & 0xFFFFFFFF:
            problems.append(f"READ kp {kp:#x} != {p['kp'] & 0xFFFFFFFF:#x}")
        # The host's last two commands toggle from its own count; re-encode
        # them with ours (op and argument unchanged).
        for w in cmds[-2:]:
            ctx.set(dut.gpio_out, t << 31 | (w & 0x7FFFFFFF))
            t ^= 1
            await tick(gap)
        for (i, q) in doc["input"]:
            ctx.set(dut.adc_valid, 1)
            ctx.set(dut.adc_i, i)
            ctx.set(dut.adc_q, q)
            await tick()
        ctx.set(dut.adc_valid, 0)
        await tick(400)

    sim.add_testbench(bench)
    sim.run()
    return words, problems


def compare(doc, syms, words):
    """Mismatch messages (empty when bit-exact)."""
    out = []
    exp = doc["symbols"]
    if len(syms) != len(exp):
        out.append(f"{len(syms)} symbols, {len(exp)} expected")
    for k, (a, b) in enumerate(zip(syms, exp)):
        bad = [f for f in FIELDS if a[f] != b[f]]
        if bad:
            out.append(f"symbol {k}: " + ", ".join(f"{f} {a[f]} != {b[f]}" for f in bad))
            if len(out) > 10:
                break
    ew = doc["words"]
    # The model emits a block's words as soon as its symbols exist; the
    # core can hold the last few in its packer at the end of the input.
    n = min(len(words), len(ew))
    if len(ew) - len(words) not in range(0, 3):
        out.append(f"{len(words)} words, {len(ew)} expected")
    for i in range(n):
        if words[i] != ew[i]:
            out.append(f"word {i}: {words[i]:#06x} != {ew[i]:#06x}")
            break
    return out


def main(argv):
    path = argv[1]
    doc = load(path)
    if is_tx(doc):
        return main_tx(path, doc, argv)
    if "--via-gpio" in argv:
        words, bad = simulate_top(doc)
        ew = doc["words"]
        if len(ew) - len(words) not in range(0, 3):
            bad.append(f"{len(words)} words, {len(ew)} expected")
        diff = next((i for i, (a, b) in enumerate(zip(words, ew)) if a != b), None)
        if diff is not None:
            bad.append(f"word {diff}: {words[diff]:#06x} != {ew[diff]:#06x}")
        print(f"{path} via gpio_out: {len(doc['commands'])} commands, {len(words)} words; "
              + ("bit-exact" if not bad else f"{len(bad)} problems"))
        for b in bad:
            print("  " + b)
        return 1 if bad else 0
    pattern = argv[argv.index("--pattern") + 1] if "--pattern" in argv else "every"
    syms, words = simulate(doc, pattern=pattern, load_port="--load-port" in argv)
    bad = compare(doc, syms, words)
    print(f"{path}: {len(syms)} symbols, {len(words)} words; "
          + ("bit-exact" if not bad else f"{len(bad)} mismatches"))
    for b in bad:
        print("  " + b)
    return 1 if bad else 0


# --- the transmit block ---------------------------------------------------

TX_SAMPLE_FIELDS = ("n", "frac", "q", "line", "sum_i", "sum_q", "out_i", "out_q", "wrap", "code")
TX_COUNTERS = ("consumed", "missed", "first_miss", "samples")


def is_tx(doc):
    """A transmit vectors file (`isdb_fpga.txmodel` docstring)."""
    return "samples" in doc and "stalls" in doc


def req_pattern(pattern, rng):
    """The rfifo's request strobe (`din_valid_0`), one value per clock:
    `every` = a strobe on every clock (the most the block must take, and
    the steady state on the PlutoSky R2, where the din clock is the sample
    rate); `rfifo` = util_rfifo's bursts of eight consecutive strobes, the
    next burst after 0 to 2 idle clocks and now and then after a pause of
    5 to 20; `jitter` = a strobe with probability 0.7."""
    while True:
        if pattern == "every":
            yield 1
        elif pattern == "rfifo":
            yield from (1,) * 8
            gap = rng.choice((0, 0, 0, 1, 2)) if rng.random() > 0.01 else rng.randrange(5, 21)
            yield from (0,) * gap
        elif pattern == "jitter":
            yield 1 if rng.random() < 0.7 else 0
        else:
            raise ValueError(f"request pattern {pattern!r}: every, rfifo or jitter")


class Upack:
    """util_upack2 as the block sees it: a request (`fifo_rd_en` high on a
    clock) is answered `latency` clocks later (1 at the IP's default
    PIPELINE_STAGES 0: util_upack2_impl.v registers data and valid) with
    `fifo_rd_valid` and the word, or with `fifo_rd_underflow` and zero data
    when the DMA has none (`txmodel.Dma`: the vectors' stalls, and every
    request after the last word). Requests before clock `dry_until`, and
    in any window [start, end) of clocks in `dry`, find no word either (a
    DMA not started yet, or not answering for a while: outside the
    vectors' stalls, which they do not use up). `answers` lists (clock,
    word or None) of every answer given."""

    def __init__(self, doc, latency=1, dry_until=0):
        self.dma = Dma(doc["words"], doc["stalls"])
        self.latency = latency
        self.dry = [(0, dry_until)]
        self.due = collections.deque()  # (clock, word or None)
        self.answers = []

    def response(self, clock):
        """(valid, underflow, word) on the lines during `clock`."""
        if self.due and self.due[0][0] == clock:
            word = self.due.popleft()[1]
            self.answers.append((clock, word))
            return (word is not None, word is None, word or 0)
        return (False, False, 0)

    def request(self, clock):
        dry = any(a <= clock < b for a, b in self.dry)
        word = None if dry else self.dma.request()
        self.due.append((clock + self.latency, word))

    def stalls_left(self):
        """(next word index, the stalls not yet met, renumbered from it)."""
        j = self.dma.next
        return j, [[i - j, n] for i, n in sorted(self.dma.left.items()) if i >= j and n > 0]


class _CorePort:
    """The bench's view of a bare `SymbolTx`."""

    def __init__(self, core):
        self.core = core
        self.req = core.req
        self.rd_en = core.rd_en

    def respond(self, ctx, valid, underflow, word):
        ctx.set(self.core.rd_valid, valid)
        ctx.set(self.core.rd_underflow, underflow)
        ctx.set(self.core.rd_data, word)

    def out(self, ctx):
        return ctx.get(self.core.o_i), ctx.get(self.core.o_q), None


class _TopPort:
    """The bench's view of `SymbolsTx`: its ports (the core's internals
    are read for the sample states only)."""

    def __init__(self, top):
        self.top = top
        self.core = top.core
        self.req = top.req
        self.rd_en = top.rd_en

    def respond(self, ctx, valid, underflow, word):
        ctx.set(self.top.rd_valid, valid)
        ctx.set(self.top.rd_underflow, underflow)
        ctx.set(self.top.rd_data0, word & 0xFFFF)
        ctx.set(self.top.rd_data1, word >> 16)

    def out(self, ctx):
        d0, d1 = ctx.get(self.top.out_data0), ctx.get(self.top.out_data1)
        bad = None
        if (d0 | d1) & 0xF or not ctx.get(self.top.out_valid):
            bad = f"out_data {d0:#06x} {d1:#06x} valid {ctx.get(self.top.out_valid)}"

        def s12(v):
            v >>= 4
            return v - 4096 if v >> 11 else v

        return s12(d0), s12(d1), bad


class _TxBench:
    """Clock by clock: the request strobe, the upack's responses and
    requests, and commands on `gpio_out`."""

    def __init__(self, ctx, port, upack):
        self.ctx = ctx
        self.port = port
        self.core = port.core
        self.upack = upack
        self.clock = 0
        self.problems = []

    def _pre(self, req):
        ctx = self.ctx
        ctx.set(self.port.req, req)
        self.port.respond(ctx, *self.upack.response(self.clock))
        if ctx.get(self.port.rd_en):
            self.upack.request(self.clock)

    async def tick(self, n=1, req=0):
        for _ in range(n):
            self._pre(req)
            await self.ctx.tick()
            self.clock += 1

    async def command(self, gpio_out, word, gap):
        self.ctx.set(gpio_out, word)
        await self.tick(gap)

    async def stream(self, doc, pattern, rng, after_n=None):
        """Strobe until every sample of `doc` has left the pipeline.
        Returns (samples, the core's counters right after the last
        sample's update). `after_n` is awaited at that point (no strobes
        meanwhile; the upack keeps answering)."""
        ctx, core = self.ctx, self.core
        n = len(doc["samples"])
        lat = core.latency
        states, outs, counters = [], [], None
        gen = req_pattern(pattern, rng)
        while len(outs) < n + lat:
            if len(states) == n and counters is None:
                counters = {k: ctx.get(getattr(core, k)) for k in TX_COUNTERS}
                if after_n is not None:
                    await after_n()
            self._pre(next(gen))
            en = ctx.get(core.en)
            if en:
                wrap = ctx.get(core.wrap)
                frac = ctx.get(core.frac)
                states.append({
                    "n": len(states),
                    "frac": frac,
                    "q": frac >> (tx.FRAC_BITS - core.pbits),
                    "line": [ctx.get(c) for c in core.line],
                    "wrap": wrap,
                    "code": ctx.get(core.code) if wrap else None,
                })
            await ctx.tick()
            self.clock += 1
            if en:
                o_i, o_q, bad = self.port.out(ctx)
                if bad and len(outs) >= lat and len(self.problems) < 5:
                    self.problems.append(f"output {len(outs) - lat}: {bad}")
                outs.append({"out_i": o_i, "out_q": o_q,
                             "sum_i": ctx.get(core.s_sum_i), "sum_q": ctx.get(core.s_sum_q)})
        ctx.set(self.port.req, 0)
        self.lead = [(o["out_i"], o["out_q"]) for o in outs[:lat]]
        samples = []
        for k in range(n):
            rec = dict(states[k])
            rec.update(outs[k + lat])
            samples.append(rec)
        return samples, counters


def tx_bank_entries(doc, load, a=None, b=None):
    """(bank, q, t, value) load-port writes of the vectors' banks: every
    tap of every row (`full`) or the near half t < S (`half`, which loads
    the whole symmetric bank)."""
    p = doc["params"]
    a = p["a"] if a is None else a
    b = p["b"] if b is None else b
    taps = 2 * p["span"] if load == "full" else p["span"]
    return [(bank, q, t, row[t]) for bank, rows in ((0, a), (1, b))
            for q, row in enumerate(rows) for t in range(taps)]


PRIME_LIMIT = 10_000  # clocks the bench waits for the prefetch


def simulate_tx(doc, pattern="every", latency=1, load=None, writes=(), banks=None, seed=1,
                restart_after=None, info=None, prefetch=None, dry_until=0, run_at=None,
                reselect_after=None):
    """Run `SymbolTx` on `doc`: its banks as the memories' initial contents
    (`banks` = (a, b) instead of the vectors'), or with `load` = "full" or
    "half" written through the load port into empty memories; `writes`
    = (bank, q, t, value) more load-port writes after that. With
    `restart_after` = k, the block first runs k samples (and its pipeline
    on), then gets START while running, the DMA's queue as it is: the
    samples returned are those after the restart, and `info` (a dict)
    receives "restart_at", the samples made before START, and "lead", the
    outputs of the first LATENCY enabled clocks after it (before sample
    0 can have arrived). Returns (samples, counters) in the vectors'
    layout.

    The start: RUN, SELECT and START on one clock, or with a `prefetch`
    (default: the vectors' "prefetch") START|SELECT first and RUN once the
    block holds two words and has no request under way (the host's
    sequence; "primed_at" in `info`: the clocks from SELECT to PRIMED).
    `dry_until`: the DMA has no word for requests before that clock
    (`Upack`). `run_at` = c: RUN c clocks after START|SELECT whether
    primed or not; "late_misses" in `info`: the underflow answers taken
    after RUN before the first word. `reselect_after` = k: after k samples
    CTRL 0 (run and selected cleared) for a few clocks, then START|SELECT,
    the prefetch and RUN again; the samples returned are those after it,
    and "resume" in `info` is `Upack.stalls_left()` at the new START (the
    words and stalls the DMA still has)."""
    p = doc["params"]
    a, b = banks if banks is not None else (p["a"], p["b"])
    init = load is None
    prefetch = doc.get("prefetch", 0) if prefetch is None else prefetch
    if prefetch not in (0, 2):
        raise ValueError(f"prefetch {prefetch}: the bench runs 0 or 2")
    dut = tx.SymbolTx(p["phases"], p["span"], a if init else None, b if init else None)
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    res = {}
    info = {} if info is None else info

    async def bench(ctx):
        upack = Upack(doc, latency, dry_until)
        tb = _TxBench(ctx, _CorePort(dut), upack)

        async def select_then_run():
            """START|SELECT, the prefetch, then RUN."""
            ctx.set(dut.selected, 1)
            ctx.set(dut.start, 1)
            await tb.tick()
            ctx.set(dut.start, 0)
            t0 = tb.clock
            while True:
                if "primed_at" not in info and ctx.get(dut.primed):
                    info["primed_at"] = tb.clock - t0
                if run_at is not None:
                    if tb.clock - t0 >= run_at:
                        break
                elif ctx.get(dut.held_words) == 2 and ctx.get(dut.pend) == 0:
                    break
                if tb.clock - t0 > PRIME_LIMIT:
                    raise AssertionError(f"not primed {PRIME_LIMIT} clocks after SELECT")
                await tb.tick()
            info["run_clock"] = tb.clock
            ctx.set(dut.run, 1)

        ctx.set(dut.regs.step, p["step"])
        ctx.set(dut.regs.shift, p["shift"])
        entries = (tx_bank_entries(doc, load, a, b) if load else []) + list(writes)
        for bank, q, t, v in entries:
            ctx.set(dut.c_we, 1)
            ctx.set(dut.c_bank, bank)
            ctx.set(dut.c_q, q)
            ctx.set(dut.c_t, t)
            ctx.set(dut.c_data, v)
            await tb.tick()
        ctx.set(dut.c_we, 0)
        await tb.tick(2)
        if prefetch or run_at is not None:
            await select_then_run()
        else:
            ctx.set(dut.run, 1)
            ctx.set(dut.selected, 1)
            ctx.set(dut.start, 1)
            await tb.tick()
            ctx.set(dut.start, 0)
            info["run_clock"] = tb.clock - 1
        rng = random.Random(seed)
        if restart_after:
            await tb.stream(dict(doc, samples=doc["samples"][:restart_after]), pattern, rng)
            info["restart_at"] = ctx.get(dut.samples)
            ctx.set(dut.start, 1)
            await tb.tick()
            ctx.set(dut.start, 0)
        if reselect_after:
            await tb.stream(dict(doc, samples=doc["samples"][:reselect_after]), pattern, rng)
            ctx.set(dut.run, 0)
            ctx.set(dut.selected, 0)
            await tb.tick(12)
            info["resume"] = upack.stalls_left()
            info.pop("primed_at", None)
            await select_then_run()
        res["samples"], res["counters"] = await tb.stream(doc, pattern, rng)
        info["lead"] = tb.lead
        info["answers"] = upack.answers

    sim.add_testbench(bench)
    sim.run()
    if run_at is not None:
        rc = info["run_clock"]
        first = next(c for c, w in info["answers"] if w is not None)
        info["late_misses"] = sum(1 for c, w in info["answers"] if w is None and rc <= c < first)
    return res["samples"], res["counters"]


def gpio_loads_banks(words):
    """Whether a command list addresses the banks (ADDR >= 0x4000)."""
    return any((w >> 28) & 7 == tx.OP_ADDR and ((w >> 1) & 0xFFFF) >= tx.A_BASE for w in words)


def simulate_tx_top(doc, pattern="every", latency=1, gap=6, seed=1):
    """Configure `SymbolsTx` through gpio_out with the vectors' command
    words (`doc["gpio"]`; when they write the banks, the memories start
    empty, so the load is what is tested), check the ID word and the
    readback of STEP, SHIFT, STATUS, FEATURES and BANK_TAG, run the strobe,
    then read the counters (and HI) through gpio_in. With a prefetch in
    the vectors, the last command (CTRL RUN|SELECT) waits for STATUS
    PRIMED and the second word. Returns (samples, counters, problems)."""
    p = doc["params"]
    loads = gpio_loads_banks(doc["gpio"])
    prefetch = doc.get("prefetch", 0)
    dut = SymbolsTx(p["phases"], p["span"], None if loads else p["a"], None if loads else p["b"])
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    res = {}

    async def bench(ctx):
        tb = _TxBench(ctx, _TopPort(dut), Upack(doc, latency))
        problems = tb.problems
        core = dut.core
        await tb.tick(4)
        ident = ctx.get(dut.gpio_in)
        if ident != p["id_word"]:
            problems.append(f"ID word {ident:#010x} != {p['id_word']:#010x}")
        cmds = doc["gpio"]
        last = 0
        for w in cmds[:-1] if prefetch else cmds:
            await tb.command(dut.gpio_out, w, gap)
            last = w
        enc = Encoder(last)

        async def read(addr):
            await tb.command(dut.gpio_out, enc.word(tx.OP_ADDR, addr), gap)
            await tb.command(dut.gpio_out, enc.word(tx.OP_READ, 0), gap)
            return ctx.get(dut.gpio_in)

        if prefetch:
            for _ in range(100):
                if await read(tx.STATUS) & tx.STATUS_PRIMED:
                    break
            else:
                problems.append("STATUS never PRIMED after CTRL START|SELECT")
            for _ in range(tx.ARM + 10 * latency + 100):
                if ctx.get(core.held_words) == 2 and ctx.get(core.pend) == 0:
                    break
                await tb.tick()
            await tb.command(dut.gpio_out, enc.word(tx.OP_CTRL, (cmds[-1] >> 1) & 0x07FFFFFF), gap)
        tag = 0 if loads else tx.bank_tag(p["a"], p["b"], p["span"])
        for addr, want, name in ((tx.STEP, p["step"] & 0xFFFFFF, "STEP"),
                                 (tx.SHIFT, p["shift"], "SHIFT"),
                                 (tx.FEATURES, tx.FEATURES_VALUE, "FEATURES"),
                                 (tx.BANK_TAG, tag, "BANK_TAG")):
            got = await read(addr)
            if got != want:
                problems.append(f"READ {name} {got:#x} != {want:#x}")
        status = await read(tx.STATUS)
        if status & 3 != 3:
            problems.append(f"STATUS {status:#x}: not running and selected after the commands")
        regs = {}

        async def counters():
            his = {}
            for name, addr in (("consumed", tx.CONSUMED), ("missed", tx.MISSED),
                               ("first_miss", tx.FIRST_MISS), ("samples", tx.SAMPLES)):
                regs[name] = await read(addr)
                if name != "missed":
                    his[name] = await read(tx.HI)
            st = await read(tx.STATUS)
            if bool(st & 4) != (regs["missed"] > 0):
                problems.append(f"STATUS {st:#x} underrun bit with MISSED {regs['missed']}")
            if any(his.values()):
                problems.append(f"HI words {his} on counters below 2**32")

        samples, direct = await tb.stream(doc, pattern, random.Random(seed), after_n=counters)
        if direct != regs:
            problems.append(f"counters read through gpio_in {regs} != the core's {direct}")
        res.update(samples=samples, counters=regs, problems=problems)

    sim.add_testbench(bench)
    sim.run()
    return res["samples"], res["counters"], res["problems"]


class TopHarness:
    """A host's view of `SymbolsTx` for a test script (`run_tx_top`):
    command words on gpio_out (`enc` keeps the toggle), register reads,
    clocks and the strobe, with the upack answering throughout."""

    def __init__(self, ctx, dut, upack, gap=6):
        self.ctx = ctx
        self.dut = dut
        self.core = dut.core
        self.control = dut.control
        self.upack = upack
        self.tb = _TxBench(ctx, _TopPort(dut), upack)
        self.enc = Encoder(0)
        self.gap = gap

    async def send(self, words):
        """Command words made with `enc`."""
        for w in words:
            await self.tb.command(self.dut.gpio_out, w, self.gap)

    async def cmd(self, op, arg=0):
        await self.send([self.enc.word(op, arg)])

    async def read(self, addr):
        """ADDR, READ: the register on gpio_in."""
        await self.cmd(tx.OP_ADDR, addr)
        await self.cmd(tx.OP_READ)
        return self.ctx.get(self.dut.gpio_in)

    async def read64(self, addr):
        """A 64-bit counter: its READ, then HI."""
        lo = await self.read(addr)
        return (await self.read(tx.HI)) << 32 | lo

    async def tick(self, n=1):
        await self.tb.tick(n)

    async def stream(self, doc, pattern="every", seed=1, after_n=None):
        """`_TxBench.stream`: (samples, the core's counters); `after_n`
        is awaited right after the last sample's update."""
        return await self.tb.stream(doc, pattern, random.Random(seed), after_n)


def run_tx_top(doc, script, latency=1, dry_until=0, banks=True):
    """Run `script(harness)` (an async function) on a `SymbolsTx` with the
    vectors' banks as initial contents (or empty memories) and the
    vectors' DMA (`Upack`, dry before `dry_until`); returns its result."""
    p = doc["params"]
    dut = SymbolsTx(p["phases"], p["span"], p["a"] if banks else None, p["b"] if banks else None)
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    out = {}

    async def bench(ctx):
        h = TopHarness(ctx, dut, Upack(doc, latency, dry_until))
        await h.tick(4)
        out["result"] = await script(h)

    sim.add_testbench(bench)
    sim.run()
    return out["result"]


def compare_tx(doc, samples, counters):
    """Mismatch messages (empty when bit-exact): every field of every
    sample, then the counters (a null `first_miss` in the vectors is not
    compared)."""
    out = []
    exp = doc["samples"]
    if len(samples) != len(exp):
        out.append(f"{len(samples)} samples, {len(exp)} expected")
    for a, b in zip(samples, exp):
        bad = [f for f in TX_SAMPLE_FIELDS if a[f] != b[f]]
        if bad:
            out.append(f"sample {b['n']}: " + ", ".join(f"{f} {a[f]} != {b[f]}" for f in bad))
            if len(out) > 10:
                break
    want = doc["counters"]
    for k in TX_COUNTERS:
        if want.get(k) is not None and counters.get(k) != want[k]:
            out.append(f"counter {k} {counters.get(k)} != {want[k]}")
    return out


def main_tx(path, doc, argv):
    pattern = argv[argv.index("--pattern") + 1] if "--pattern" in argv else "every"
    latency = int(argv[argv.index("--latency") + 1]) if "--latency" in argv else 1
    if "--via-gpio" in argv:
        samples, counters, bad = simulate_tx_top(doc, pattern, latency)
        how = f"via gpio_out, {len(doc['gpio'])} commands"
    else:
        load = "full" if "--load-port" in argv else None
        samples, counters = simulate_tx(doc, pattern, latency, load=load)
        bad = []
        how = "bank loaded through the port" if load else "bank as initial contents"
    bad = bad + compare_tx(doc, samples, counters)
    nz = sum(1 for r in samples if r["out_i"] or r["out_q"])
    print(f"{path} ({how}, requests {pattern}, upack latency {latency}): {len(samples)} samples "
          f"({nz} non-zero), counters {counters}; "
          + ("bit-exact" if not bad else f"{len(bad)} mismatches"))
    for b in bad:
        print("  " + b)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
