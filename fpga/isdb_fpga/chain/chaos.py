"""Chaos runs of the chain with the real decoder (issue #30, item 3).

Issue #28's bank leak hid behind vector tests that never took the abort
path. `stages.run_chain_restarts` replays its one recipe; this module
draws random host events into the real-decoder scenarios (chain-7-hdl,
chain-8-loss, chain-9-fade, chain-10-bch), runs them on
`stages.RealChainBench` (a compiled backend: Verilator where installed)
and checks, after every event and at the end, the decoder's and bridge's
health and the `Slots` stream, then compares the stream with the Rust
model restarted at the same points (`isdbs3-phy`'s `examples/chainscenario`).

Events (`draw_events`, from a seed; positions are input indices):

- `long`: the host's stop (no input for 200 000 - 400 000 symbol clocks:
  committed codewords decode and leave, the open one stays begun), then
  CTRL START (C_START: what `SlotsRx`'s START does to the chain: a loss of
  0 symbols, CARRIER (0) and the taps last written loaded, the `Slots`
  sequence from 0);
- `short`: a stop of 100 - 20 000 clocks (inside a codeword's load: the
  chain has not drained), then START;
- `start`: START while the input runs;
- `loss`: an input loss (C_LOSS, as chain-8's): `lost` symbols are not
  delivered and the chain is told so (1 - 4 096 or 4 096 - 150 000);
- `taps`: the host writes TAPS (identity, chain-3's taps or those
  perturbed); the next START puts them in use (INIT bit 0);
- `setloss`: a set's own loss (chain-8's 6 144 symbols), each time the
  input passes it.

After a START the input continues where it was (`resume` = `at`), skips
the front end's symbols of the stop (`resume` > `at`), or replays from
an earlier index (`resume` < `at`: a new run; that is how a set of 5 or
6 frames gets more than one decoded frame to abort), within a budget of
delivered symbols. A restart drops the layouts, so the chain decodes
again only in its third whole frame after it (`decoding_span`): three
in four events are drawn into that frame of the current run (its slot
codewords loading: the abort path), and a START replays when no such
frame is left in the input. `place` fills in a hand-written list
(`CI_EVENTS`, the opt-in CI case).

The model is restarted at the same points (`Chain::start`, `Chain::loss`,
in stream order). The model has no clock: a stop is invisible to it, and
it hands a frame out when the frame's last look-ahead symbol is in. The
HDL is behind it by its pipeline (stage 3 releases a slot once the next
head is in, then the FIR, the demapper, the TMCC burst and the decode),
so an event that does not let the chain drain first (`short`, `start`,
`loss`, `setloss`, and a lost sync) can find the HDL still inside a frame
the model has already handed out: the HDL drops it (or, the TMCC burst
being never cut, decodes its TMCC after the restart and keeps its layout:
the restart epoch, README "What is left"), the model does not. Such a
frame (the model's `done` within `LATENCY` symbols before the event) is
reported as a by-design difference, not a failure, and from that event
on the comparison drops to what survives a different n0 / layout
history: the slot blocks' fields but the iteration count, the TMCC's
bits and BCH result (`bits` mode); frames F + 1 and F + 2 of such a frame
may be decoded by the HDL alone (the restart epoch). Everything else
must be equal block for block, per (frame, kind, slot), as in
`stages.run_chain_real`: frames the model does not send whose slot
blocks the chain sent before a drop (orphans) are counted, not compared.
One divergence is known and counted apart (`counters_explained`, a
finding, not a failure): the chain's status counters (slots, LDPC and
BCH failures, iterations) include the orphan slots, the model's count
whole frames; a status block that differs by exactly the orphans before
it is reported as that.

Health (`check_sample`), from the bench's ports (`RealChainBench.ports`)
sampled after each event, `SETTLE` symbols later and at the end:

- always: no queue overflow, no lost input beat, no port loss, nothing
  late, no TMCC overrun, no decoder overrun (none of these sets loads
  the decoder enough for one), at most K banks held; any monitor flag
  (ports named `mon_*`, or listed in the bench's `MONITOR_PORTS`) clear;
- after a long stop, before START: the bridge's FIFOs, tag queue and the
  result FIFO empty, at most the open codeword's bank held;
- after a long stop's START (3 000 clocks on): every bank free, a bank
  free to load, every queue empty, one more abort beat;
- `SETTLE` symbols after any restart (no codeword can begin until a
  whole frame after it): the same, if no other event came between;
- at the end (`TAIL` clocks after the input, which ends inside a
  frame): as after a long stop;
- the `Slots` stream parses (headers, CRC), its sequence numbers are
  contiguous, from 0 after each START (the first block whose first word
  comes after the START's cycle; `cyc` in `ChaosBench`).

`run_chaos(name, seed)` returns a dict with the events, the samples,
`problems` (failures) and `findings` (by-design differences and notes);
`report(result)` formats it. `tools/chaos_run.py` runs one from the
command line; `tests/test_chaos.py` has the opt-in tests.
"""

import json
import os
import random
import re
import subprocess
import time

from amaranth.hdl import Signal

from . import slots as S
from .common import FRAME, SLOT
from .simx import engine
from .stages import C_LOSS, C_START, C_SYM, C_TAP, C_TAPNOW, RealChainBench

HERE = os.path.dirname(__file__)
FPGA = os.path.normpath(os.path.join(HERE, "..", ".."))
ROOT = os.path.normpath(os.path.join(FPGA, ".."))
BUILD = os.path.join(FPGA, "build")
VECTORS = os.environ.get("FPGA_VECTORS", BUILD)

SETS = ("7-hdl", "8-loss", "9-fade", "10-bch")
# the taps the host writes at open: chain-3's for the sets written with
# them, else identity (written too: a START loads the taps last written,
# and TAPS is 0 until written)
OPEN_TAPS = {"7-hdl": "3-eq", "8-loss": None, "9-fade": None, "10-bch": "3-eq"}
IDENTITY = [16384 if k == 10 else 0 for k in range(22)]
BANKS = 3
# the front end's rate in the 40 MHz symbol clock (stages.run_chain_real)
RATIO = 40e6 / 33.7561e6
LONG_PAUSE = (200_000, 400_000)  # clocks: the chain drains (run_chain_restarts: 200 000)
SHORT_PAUSE = (100, 20_000)
AFTER_START = 3_000  # clocks after a long stop's START before its sample
SETTLE = 350_000  # symbols after a restart: no codeword can have begun
LATENCY = 8 * SLOT  # symbols the HDL may be behind the model at an undrained event
TAIL = 600_000  # clocks after the input
GAP = 20_000  # symbols at least between drawn events
ACQ = 10 * SLOT  # symbols from a restart to the first frame head it can use (at most; 6 slots were not)
END_MARGIN = 50_000
UNDRAINED = ("short", "start", "loss", "setloss")
WEIGHTS = {"long": 3, "short": 2, "start": 2, "loss": 3}
HEALTH = ("held", "load_ready", "in_level", "tag_level", "out_level", "rf_level", "aborted", "aborts",
          "reused", "dec_overruns", "queue_overflow", "port_lost", "late", "in_lost", "overrun", "tdone",
          "frames_done", "fifo_max")
NEVER = ("queue_overflow", "in_lost", "port_lost", "late", "overrun", "dec_overruns")
EMPTY = ("in_level", "tag_level", "out_level", "rf_level")


def vector(name):
    with open(os.path.join(VECTORS, f"chain-{name}.json")) as f:
        return json.load(f)


def open_taps(name):
    src = OPEN_TAPS[name]
    return list(vector(src)["taps"]) if src else list(IDENTITY)


def set_info(doc):
    """The set's input length, its losses (input index, symbols), the
    frame heads in input indices and the points where the set itself
    restarts the chain (its losses, the end of chain-9's fade)."""
    n = len(doc["input"]) // 2
    losses = [tuple(x) for x in doc.get("losses", [])]

    def to_input(at):  # a chain index less the losses before it
        return at - sum(k for b, k in losses if b + k <= at)
    base = doc["frames"][0]["at"] % FRAME
    heads = [h for h in (to_input(base + k * FRAME) for k in range(n // FRAME + 2)) if h < n]
    inner = [b for b, _ in losses]
    m = re.search(r"noise for (\d+) symbols from input index (\d+)", json.dumps(doc.get("source", "")))
    if m:
        inner.append(int(m.group(2)) + int(m.group(1)))
    return n, losses, heads, sorted(inner)


def decoding_span(r, heads, inner, n):
    """The input span of the first frame the chain can decode after it
    (re)starts at input index r (its third whole frame: the layout of the
    first one's TMCC), or None if the input ends first."""
    e = max([r] + [p for p in inner if p >= r])
    hs = [h for h in heads if h >= e + ACQ]
    if len(hs) < 3 or hs[2] + FRAME + SLOT > n - END_MARGIN:
        return None
    return hs[2], hs[2] + FRAME + SLOT


# --- the events ----------------------------------------------------------------


def draw_events(name, seed, count=5, budget=2.0, doc=None):
    """`count` random host events (stops, STARTs, losses; tap writes
    besides) for set `name` from `seed` (deterministic), in stream order,
    with the set's own losses where the input passes them. Most events
    fall into the frame the chain would decode next after the previous
    restart (where slot codewords are being loaded: the abort path); a
    START replays the input from an earlier index when no such frame is
    left, while the symbols delivered stay under `budget` inputs."""
    doc = doc or vector(name)
    n, losses, heads, inner = set_info(doc)
    rng = random.Random(f"chaos/{name}/{seed}")
    lpos = {b for b, _ in losses}
    events = []
    ptr, delivered = 0, 0
    replay_to = max(1, n - 4 * FRAME)

    def deliver(to):
        nonlocal ptr, delivered
        for b, k in losses:
            if ptr <= b < to:
                events.append({"kind": "setloss", "at": b, "lost": k, "resume": b,
                               "stream": delivered + b - ptr})
        delivered += to - ptr
        ptr = to

    def clear(at):  # not on a set loss's index
        while any(abs(at - b) <= 2 for b in lpos):
            at += 3
        return at

    for _ in range(count):
        if ptr >= n - END_MARGIN - 2 * GAP:
            break
        span = decoding_span(ptr, heads, inner, n)
        if span and rng.random() < 0.75:
            at = rng.randrange(span[0] + SLOT // 2, span[1])
        else:
            at = rng.randrange(ptr + GAP, n - END_MARGIN - GAP)
        at = clear(at)
        kind = rng.choices(list(WEIGHTS), weights=list(WEIGHTS.values()))[0]
        later = decoding_span(at, heads, inner, n)
        can_replay = delivered + (at - ptr) + (n - replay_to // 2) <= budget * n
        if kind == "loss" and later is None and can_replay:
            kind = rng.choice(("long", "short", "start"))  # a START that replays
        if kind != "loss" and rng.random() < 0.3:
            # a new tap set written before this START
            t_at = min(clear(rng.randrange(ptr, at)), at)
            deliver(t_at)
            which = rng.choice(("identity", "eq", "perturbed"))
            taps = list(IDENTITY) if which == "identity" else list(vector("3-eq")["taps"])
            if which == "perturbed":
                for _k in range(rng.randint(1, 4)):
                    i = rng.choice([j for j in range(22) if j not in (10, 11)])
                    taps[i] += rng.choice((-1, 1)) * rng.randint(1, 400)
            events.append({"kind": "taps", "at": t_at, "taps": taps, "which": which, "stream": delivered})
        deliver(at)
        ev = {"kind": kind, "at": at, "stream": delivered}
        if kind == "loss":
            lost = rng.randint(1, 4096) if rng.random() < 0.5 else rng.randint(4096, 150_000)
            nxt = [b for b in lpos if b > at]
            lost = max(1, min(lost, n - END_MARGIN - at, (min(nxt) - at) if nxt else lost))
            ev.update(lost=lost, resume=at + lost)
        else:
            if kind != "start":
                ev["pause"] = rng.randint(*(LONG_PAUSE if kind == "long" else SHORT_PAUSE))
            r = rng.random()
            if can_replay and (later is None or r < 0.3):
                ev["resume"] = rng.randrange(0, replay_to)  # a new run
            elif kind != "start" and r < 0.6:
                # the front end's symbols during the stop are not delivered
                nxt = [b for b in lpos if b > at]
                ev["resume"] = max(at, min(at + int(ev["pause"] / RATIO), n - END_MARGIN - GAP,
                                           min(nxt) - 3 if nxt else n))
            else:
                ev["resume"] = at
        events.append(ev)
        ptr = ev["resume"]
    deliver(n)
    return events


def place(name, events, doc=None):
    """Events given by hand (kind, at, and pause / lost / resume / taps as
    `draw_events` writes them), in stream order: the stream positions and
    the set's own losses filled in."""
    doc = doc or vector(name)
    n, losses, _heads, _inner = set_info(doc)
    out, ptr, delivered = [], 0, 0
    for ev in list(events) + [None]:
        to = n if ev is None else ev["at"]
        assert to >= ptr, f"event {ev} before input index {ptr}"
        out += [{"kind": "setloss", "at": b, "lost": k, "resume": b, "stream": delivered + b - ptr}
                for b, k in losses if ptr <= b < to]
        delivered += to - ptr
        ptr = to
        if ev is None:
            break
        ev = dict(ev, stream=delivered)
        if ev["kind"] == "loss":
            ev.setdefault("resume", ev["at"] + ev["lost"])
        elif ev["kind"] != "taps":
            ev.setdefault("resume", ev["at"])
        out.append(ev)
        ptr = ev.get("resume", ptr)
    return out


# The opt-in CI case (tests/test_chaos.py): chain-7-hdl, identity taps
# written, an early loss, #28's long stop inside frame 2's first
# codewords (a bank held, freed by the abort) and a replay from before
# frame 0, a short stop and a START while running before the replay's
# first frame head; then the replay's frame 2 decodes and is compared.
CI_EVENTS = [
    {"kind": "taps", "at": 200_000, "taps": IDENTITY, "which": "identity"},
    {"kind": "loss", "at": 500_000, "lost": 6_144},
    {"kind": "long", "at": 3_347_560 + SLOT + 6_000, "pause": 250_000, "resume": 700_000},
    {"kind": "short", "at": 730_000, "pause": 5_000},
    {"kind": "start", "at": 760_000},
]


def model_events(name, events):
    """The event list for `examples/chainscenario`."""
    out = []
    for e in events:
        if e["kind"] == "taps":
            out.append({"at": e["at"], "op": "taps", "taps": e["taps"]})
        elif e["kind"] in ("loss", "setloss"):
            out.append({"at": e["at"], "op": "loss", "lost": e["lost"], "resume": e["resume"]})
        else:
            out.append({"at": e["at"], "op": "start", "resume": e["resume"]})
    return {"eq_every": 0, "taps": open_taps(name), "carrier": 0, "events": out}


def model_cmd(set_path, ev_path, out_path):
    return ["cargo", "run", "--release", "--locked", "-q", "--manifest-path", os.path.join(ROOT, "Cargo.toml"),
            "-p", "isdbs3-phy", "--example", "chainscenario", "--", set_path, ev_path, out_path]


def start_model(name, events, stem):
    """Starts the model's run (a subprocess); returns (process, output path)."""
    ev_path, out_path = f"{stem}.events.json", f"{stem}.model.json"
    with open(ev_path, "w") as f:
        json.dump(model_events(name, events), f)
    p = subprocess.Popen(model_cmd(os.path.join(VECTORS, f"chain-{name}.json"), ev_path, out_path),
                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, cwd=ROOT)
    return p, out_path


# --- the HDL run ----------------------------------------------------------------


class ChaosBench(RealChainBench):
    """`RealChainBench` with a cycle counter in the output group and a
    record of each START's cycle (both registered: groups are sampled
    after the edge), so blocks are attributed to the START before them."""

    def __init__(self, **kw):
        super().__init__(**kw)
        self.cyc = Signal(32)
        self.started = Signal()

    def elaborate(self, platform):
        m = super().elaborate(platform)
        m.d.sync += [self.cyc.eq(self.cyc + 1),
                     self.started.eq(self.i_valid & self.i_ready & (self.kind == C_START))]
        return m

    def ports(self):
        p = super().ports()
        p.update(cyc=self.cyc, started=self.started)
        return p


def monitor_names(bench, ports):
    """Sticky violation flags of invariant monitors, when the bench has
    them: ports named `mon_*`, or listed in its `MONITOR_PORTS`."""
    names = [k for k in getattr(bench, "MONITOR_PORTS", ()) if k in ports]
    return names + [k for k in ports if k.startswith("mon_") and k not in names]


def check_sample(label, kind, st, before=None):
    """What must hold at a sample (module docstring); a list of problems."""
    bad = [f"{k} {st[k]}" for k in NEVER if st[k]]
    bad += [f"monitor {k} {v}" for k, v in st.get("monitors", {}).items() if v]
    if st["held"] > BANKS:
        bad.append(f"held {st['held']} > {BANKS}")
    if label in ("stopped", "end"):
        # the open codeword (the input stopped inside it) keeps its bank
        # until a restart's abort or its context's next begin
        bad += [f"{k} {st[k]}" for k in EMPTY if st[k]]
        if st["held"] > 1:
            bad.append(f"held {st['held']} (only the open codeword may hold a bank)")
    if label in ("after START", "settled"):
        if st["held"] or not st["load_ready"] or any(st[k] for k in EMPTY):
            bad.append("not drained: " + ", ".join(f"{k} {st[k]}" for k in ("held", "load_ready") + EMPTY))
    if label in ("after START", "settled") and before is not None and st["aborts"] <= before["aborts"]:
        bad.append(f"no abort beat since the {kind} (aborts {before['aborts']} -> {st['aborts']})")
    return [f"{label}: {b}" for b in bad]


def run_hdl(doc, name, events, backend=None, log=None):
    """The events on `ChaosBench`; returns (samples, words with cycles,
    START cycles, problems, timing)."""
    from . import simx

    backend = backend or simx.compiled_backend()
    if backend is None:
        raise RuntimeError("the real-decoder chain needs a compiled backend (simx.compiled_backend)")
    x = doc["input"]
    n = len(x) // 2
    b = ChaosBench(errors=doc.get("bch_errors", ()))
    ports = b.ports()
    mons = monitor_names(b, ports)
    t0 = time.time()
    e = engine(b, ports, backend, tag="chaos")
    t_build = time.time() - t0
    e.stall_limit = 3_000_000
    g = e.group("ov", ["ow", "cyc"])
    gs = e.group("started", ["cyc"])
    acc = [0.0]
    words, starts, samples, bad = [], [], [], []

    def run(recs, gaps, tail=0):
        if recs:
            try:
                e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], recs, tail=tail, gaps=gaps)
            except RuntimeError as err:
                raise ChaosStuck(str(err)) from err
        elif tail:
            e.tick(tail)
        words.extend(e.read(g))
        starts.extend(c for (c,) in e.read(gs))

    def symbols(a, z):
        recs, gaps = [], []
        for i in range(a, z):
            recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0))
            acc[0] += RATIO - 1
            k = int(acc[0])
            gaps.append(k)
            acc[0] -= k
        return recs, gaps

    def sample(label, ev_i, kind, before=None):
        st = {k: e.get(k) for k in HEALTH}
        st["monitors"] = {k: e.get(k) for k in mons}
        st["cycle"] = e.cycles()
        probs = check_sample(label, kind, st, before)
        samples.append({"event": ev_i, "kind": kind, "label": label, "state": st, "problems": probs})
        bad.extend(f"event {ev_i} ({kind}) {p}" if ev_i is not None else p for p in probs)
        if log:
            log(f"  [{ev_i}] {kind} {label}: held {st['held']} levels "
                f"{[st[k] for k in EMPTY]} aborts {st['aborts']} aborted {st['aborted']} reused {st['reused']}"
                + (f"  PROBLEMS {probs}" if probs else ""))
        return st

    taps = open_taps(name)
    run([(C_TAP, i, taps[2 * i], taps[2 * i + 1]) for i in range(11)] + [(C_TAPNOW, 0, 0, 0)], [0] * 12)
    ptr, stream = 0, 0
    settle = None  # (stream position, event index, kind, state before)

    def deliver(to):
        nonlocal ptr, stream, settle
        while ptr < to:
            if settle is not None and settle[0] < stream + (to - ptr):
                cut = ptr + (settle[0] - stream)
                run(*symbols(ptr, cut))
                stream += cut - ptr
                ptr = cut
                sample("settled", settle[1], settle[2], settle[3])
                settle = None
                continue
            run(*symbols(ptr, to))
            stream += to - ptr
            ptr = to

    stuck = None
    try:
        for i, ev in enumerate(events):
            kind = ev["kind"]
            deliver(ev["at"])
            if kind == "taps":
                t = ev["taps"]
                run([(C_TAP, k, t[2 * k], t[2 * k + 1]) for k in range(11)], [0] * 11)
                continue
            settle = None  # another event came first
            before = {k: e.get(k) for k in HEALTH}
            if kind in ("long", "short"):
                e.tick(ev["pause"])
                if kind == "long":
                    sample("stopped", i, kind)
            if kind in ("loss", "setloss"):
                run([(C_LOSS, ev["lost"], 0, 0)], [0])
            else:
                run([(C_START, 0, 0, 0)], [0], tail=AFTER_START if kind == "long" else 0)
            sample("after START" if kind == "long" else "at the event", i, kind, before)
            if kind != "long":
                settle = (stream + SETTLE, i, kind, before)
            ptr = ev["resume"]
        deliver(n)
        run([], [], tail=TAIL)
        sample("end", None, "end")
    except ChaosStuck as err:
        stuck = str(err)
        bad.append(f"the run stalled: {stuck}")
    timing = {"build": t_build, "sim": time.time() - t0 - t_build, "cycles": e.cycles(), "delivered": stream}
    e.close()
    return samples, words, starts, bad, timing


class ChaosStuck(RuntimeError):
    pass


# --- the stream checks and the comparison ----------------------------------------


def parse(words):
    """[(word index, kind, seq, frame, payload)] up to the first malformed
    block, and the problem there (or None)."""
    out, i = [], 0
    while i < len(words):
        w = words[i:i + S.HEADER_WORDS]
        if len(w) < S.HEADER_WORDS:
            return out, f"a block cut short at word {i}"
        if tuple(w[:2]) != S.SYNC:
            return out, f"no sync word at word {i}"
        kind, seq, p, frame = (w[2] >> 4) & 0xF, w[3] | w[4] << 16, w[5], w[6] | w[7] << 16
        if w != S.header(kind, seq, p, frame):
            return out, f"a bad header at word {i}"
        pay = words[i + S.HEADER_WORDS:i + S.HEADER_WORDS + p]
        crc = S.crc32c(pay, seq)
        if words[i + S.HEADER_WORDS + p:i + S.HEADER_WORDS + p + 2] != [crc & 0xFFFF, crc >> 16]:
            return out, f"a bad CRC in the block at word {i}"
        out.append((i, kind, seq, frame, pay))
        i += S.HEADER_WORDS + p + 2
    return out, None


def check_sequence(blocks, cycles, starts):
    """Sequence numbers contiguous, from 0 at the start and after each
    START (the first block whose first word is after the START's cycle)."""
    bad = []
    si = 0
    expect = 0
    for (i, kind, seq, frame, _p) in blocks:
        reset = False
        while si < len(starts) and cycles[i] > starts[si]:
            si += 1
            reset = True
        if reset:
            if seq != 0:
                bad.append(f"the first block after START {si} (cycle {starts[si - 1]}; kind {kind}, frame "
                           f"{frame}, first word at cycle {cycles[i]}) has sequence {seq}, not 0")
            expect = seq
        elif seq != expect:
            bad.append(f"block at word {i} (kind {kind}, frame {frame}) has sequence {seq}, {expect} expected")
            expect = seq
        expect = (expect + 1) & 0xFFFFFFFF
        if len(bad) > 8:
            break
    return bad


# the counters at the end of a status block: (name, words)
COUNTERS = (("frames", 2), ("undecoded", 2), ("tmcc_failed", 2), ("slots", 2), ("ldpc_failed", 2),
            ("bch_failed", 2), ("iterations", 4))


def status_counters(pay):
    """A status block's counters (`isdb_frontend::slots::Status`: after
    the five constellation entries, a count word and 2^m points each)."""
    i = 47
    for _ in range(5):
        i += 1 + 2 * pay[i]
    out = {}
    for name, n in COUNTERS:
        out[name] = sum(pay[i + k] << (16 * k) for k in range(n))
        i += n
    return i - sum(n for _, n in COUNTERS), out


def orphan_counts(gb, whole):
    """Per frame counter, the slot results of frames the chain sent slot
    blocks of but no TMCC / status (`whole`: the frames that are not
    orphans), summed over the orphan frames before it: what the chain's
    counters hold beyond the model's (the model counts whole frames)."""
    per = {}
    for (fr, kind, _s), pay in gb.items():
        if kind != S.SLOT or fr in whole:
            continue
        f = S.slot_fields(pay)
        if f["dummy"]:
            continue
        c = per.setdefault(fr, {"slots": 0, "ldpc_failed": 0, "bch_failed": 0, "iterations": 0})
        c["slots"] += 1
        c["ldpc_failed"] += int(not f["converged"])
        c["bch_failed"] += int(f["converged"] and not f["bch_ok"])
        c["iterations"] += f["iterations"]

    def before(frame):
        tot = {"slots": 0, "ldpc_failed": 0, "bch_failed": 0, "iterations": 0}
        for fr, c in per.items():
            if fr < frame:
                for k in tot:
                    tot[k] += c[k]
        return tot
    return before


def counters_explained(g_, m_, counted, frame):
    """Whether a status block differs from the model's only in its
    counters, by the slot results of the orphan frames before it (the
    chain counts every slot it decodes, the model those of whole frames):
    a description, or None."""
    if len(g_) != len(m_):
        return None
    start, gc = status_counters(g_)
    _, mc = status_counters(m_)
    if g_[:start] != m_[:start]:
        return None
    extra = counted(frame)
    diff = {k: gc[k] - mc[k] for k in gc if gc[k] != mc[k]}
    if not diff or any(diff.get(k, 0) != extra.get(k, 0) for k in set(diff) | set(extra)):
        return None
    return (f"status counters exceed the model's by the orphan slots before it ({diff}): the chain counts the "
            f"slots it decoded of frames dropped later, the model counts whole frames")


def keyed(blocks):
    out, twice = {}, []
    for (_i, kind, _seq, frame, pay) in blocks:
        key = (frame, kind, pay[0] & 0x7F if kind == S.SLOT else 0)
        if key in out:
            twice.append(key)
        out[key] = pay
    return out, twice


def compare(model, hdl_blocks, events):
    """The chain's blocks against the model's per (frame, kind, slot),
    with the by-design allowances of the module docstring. Returns
    (problems, findings, per-frame outcomes)."""
    mb, _ = parse(model["slots_words"])
    eb, mtwice = keyed(mb)
    gb, gtwice = keyed(hdl_blocks)
    problems = [f"the model sends block {k} twice" for k in mtwice]
    findings = []
    mframes = {f["frame"]: f for f in model["frames"]}
    # undrained restarts in stream positions: the events and the model's lost syncs
    undrained = [(e["stream"], f"event {i} ({e['kind']})") for i, e in enumerate(events) if e["kind"] in UNDRAINED]
    undrained += [(s, "a lost sync") for s, lost in model["restarts"] if lost]
    drains = [e["stream"] for e in events if e["kind"] == "long"]

    def cut(f, s):  # the HDL may still be inside frame f at s
        return s - LATENCY < f["done"] <= s and not any(f["done"] <= d <= s for d in drains)
    ambiguous = {}  # frame -> (why, distance)
    for f in model["frames"]:
        for s, why in undrained:
            if cut(f, s):
                ambiguous[f["frame"]] = (why, s - f["done"])
    bits_from = min((s for s, _ in undrained if any(cut(f, s) for f in model["frames"])), default=None)
    epoch = {g + d: g for g in ambiguous for d in (1, 2)}
    hdl_sent = {k[0] for k in gb if k[1] in (S.TMCC, S.STATUS)}
    orphans = sorted({k[0] for k in gb if k[1] == S.SLOT} - hdl_sent - set(mframes))
    counted = orphan_counts(gb, hdl_sent | set(mframes))
    problems += [f"the chain sends block {k} twice" for k in gtwice if k[0] in hdl_sent]
    outcomes, known = {}, []
    for fr in sorted(set(mframes) | hdl_sent):
        mf = mframes.get(fr)
        bits = bits_from is not None and (mf is None or mf["done"] > bits_from - LATENCY)
        if fr not in hdl_sent:
            if fr in ambiguous:
                why, d = ambiguous[fr]
                outcomes[fr] = "cut (latency)"
                findings.append(f"frame {fr}: handed out by the model {d} symbols before {why}, not by the "
                                f"chain (its pipeline was still in the frame: by design)")
            else:
                outcomes[fr] = "MISSING"
                problems.append(f"frame {fr}: the model sends it, the chain does not")
            continue
        if mf is None:
            outcomes[fr] = "EXTRA"
            problems.append(f"frame {fr}: the chain sends its TMCC/status, the model does not")
            continue
        keys = sorted(k for k in set(eb) | set(gb) if k[0] == fr)
        diffs, iters, status_differs, orphan_counted = [], 0, False, False
        extra_slots = [k for k in keys if k not in eb and k[1] == S.SLOT]
        epoch_frame = bool(extra_slots) and not any(k[1] == S.SLOT for k in keys if k in eb) and fr in epoch
        if epoch_frame:
            # the restart epoch explains the slot blocks the model has not;
            # the frame's TMCC block is compared as any other, and its
            # status block but for the counters (the chain counted those slots)
            findings.append(f"frame {fr}: decoded by the chain alone ({len(extra_slots)} slot blocks), with "
                            f"frame {epoch[fr]}'s layout kept across {ambiguous[epoch[fr]][0]} (the restart "
                            f"epoch: by design); its TMCC and status blocks compared")
            keys = [k for k in keys if k not in extra_slots]
        for k in keys:
            if k not in gb:
                diffs.append(f"block {k} missing")
                continue
            if k not in eb:
                diffs.append(f"block {k} not in the model's stream")
                continue
            g_, m_ = gb[k], eb[k]
            if g_ == m_:
                continue
            if k[1] == S.STATUS and epoch_frame and len(g_) == len(m_) \
                    and g_[:status_counters(g_)[0]] == m_[:status_counters(m_)[0]]:
                continue  # the counters hold the epoch's slots
            if k[1] == S.STATUS:
                why = counters_explained(g_, m_, counted, fr)
                if why:
                    known.append(f"frame {fr}: {why}")
                    orphan_counted = True
                    continue
                if bits:
                    status_differs = True
                    continue
            if bits and k[1] == S.SLOT:
                gf_, mf_ = S.slot_fields(g_), S.slot_fields(m_)
                dn = [x for x in mf_ if x != "iterations" and gf_[x] != mf_[x]]
                if not dn:
                    iters += 1
                    continue
                diffs.append(f"block {k}: {dn} differ")
                continue
            if bits and k[1] == S.TMCC and g_[0] == m_[0] and g_[4:] == m_[4:]:
                iters += 1
                continue
            w = next((i for i, (u, v) in enumerate(zip(g_, m_)) if u != v), None)
            diffs.append(f"block {k}: payload word {w} differs" if w is not None
                         else f"block {k}: {len(g_)} words, {len(m_)} expected")
        if diffs:
            outcomes[fr] = "DIFFERS"
            problems.extend(f"frame {fr}: {d}" for d in diffs[:6])
        else:
            loose = [f"{iters} iteration counts"] * bool(iters) + ["the status block"] * status_differs
            outcomes[fr] = ("restart epoch (TMCC equal)" if epoch_frame
                            else f"bits equal ({' and '.join(loose)} differ)" if loose
                            else "equal but the counters (orphans)" if orphan_counted else "equal")
    if bits_from is not None:
        findings.append(f"from stream position {bits_from - LATENCY} on (an undrained restart right after a "
                        f"frame the model handed out) compared in bits mode: slot fields but the iteration "
                        f"count, the TMCC's bits and BCH result; status blocks not compared")
    if known:
        findings.append(f"KNOWN DIVERGENCE (counted, not a failure; see README \"Chaos runs\"): {known[0]}"
                        + (f" (and {len(known) - 1} more frames)" if len(known) > 1 else ""))
    if orphans:
        findings.append(f"slot blocks of frames the model does not send (dropped later or unfinished, "
                        f"orphans): {orphans}")
    return problems, findings, outcomes


# --- the whole run -----------------------------------------------------------------


def run_chaos(name, seed, count=5, budget=2.0, backend=None, events=None, log=None):
    """One chaos run: the events (drawn from `seed`, or `events`), the
    HDL on them, the checks and the comparison with the model. Returns a
    dict (module docstring); the chain's and the model's outputs stay in
    build/chaos/<set>-s<seed>.{hdl,model}.json (`judge` again)."""
    t0 = time.time()
    doc = vector(name)
    events = events if events is not None else draw_events(name, seed, count, budget, doc)
    os.makedirs(os.path.join(BUILD, "chaos"), exist_ok=True)
    stem = os.path.join(BUILD, "chaos", f"{name}-s{seed}")
    proc, model_path = start_model(name, events, stem)
    if log:
        log(f"chain-{name}, seed {seed}: {len(events)} events")
        for i, ev in enumerate(events):
            log(f"  [{i}] {describe(ev)}")
    samples, wc, starts, problems, timing = run_hdl(doc, name, events, backend, log)
    hdl = {"words": [w for w, _c in wc], "cycles": [c for _w, c in wc], "starts": starts}
    with open(f"{stem}.hdl.json", "w") as f:
        json.dump(hdl, f)
    out, _ = proc.communicate()
    model = None
    if proc.returncode:
        problems.append(f"the model failed: {out[-2000:]}")
    else:
        with open(model_path) as f:
            model = json.load(f)
    r = {"set": name, "seed": seed, "events": events, "samples": samples, "health_problems": problems,
         "timing": dict(timing, total=time.time() - t0)}
    return judge(r, hdl, model)


def judge(r, hdl, model):
    """The stream checks and the comparison of a run `r` (its events and
    health problems) from the chain's output `hdl` and the model's run
    `model` (None: it failed); fills in `problems`, `findings`, `frames`."""
    events = r["events"]
    problems = list(r["health_problems"])
    words, cycles, starts = hdl["words"], hdl["cycles"], hdl["starts"]
    blocks, perr = parse(words)
    if perr:
        problems.append(f"the Slots stream does not parse: {perr}")
    problems += check_sequence(blocks, cycles, starts)
    nstarts = sum(1 for e in events if e["kind"] in ("long", "short", "start"))
    if len(starts) != nstarts:
        problems.append(f"{len(starts)} STARTs recorded, {nstarts} sent")
    findings, outcomes = [], {}
    if model is not None:
        p, findings, outcomes = compare(model, blocks, events)
        problems += p
    return dict(r, problems=problems, findings=findings, frames=outcomes, blocks=len(blocks),
                model_frames=[(f["frame"], f["decoded"], f["segment"]) for f in model["frames"]] if model else None)


def rejudge(path):
    """`judge` again on a saved result (build/chaos/<set>-s<seed>.json and
    its .hdl.json / .model.json beside it): after a change to the checks."""
    with open(path) as f:
        r = json.load(f)
    stem = path[:-5]
    with open(f"{stem}.hdl.json") as f:
        hdl = json.load(f)
    with open(f"{stem}.model.json") as f:
        model = json.load(f)
    r.setdefault("health_problems", [p for p in r["problems"] if p.startswith(("event ", "end", "the run"))])
    return judge(r, hdl, model)


def describe(ev):
    k = ev["kind"]
    at = f"input {ev['at']} (stream {ev['stream']})"
    if k == "taps":
        return f"taps ({ev.get('which', 'given')}) written at {at}"
    if k in ("loss", "setloss"):
        return f"{k} of {ev['lost']} symbols at {at}"
    how = ("continues" if ev["resume"] == ev["at"] else
           f"skips to {ev['resume']}" if ev["resume"] > ev["at"] else f"replays from {ev['resume']}")
    stop = f"stop {ev['pause']} clocks, " if "pause" in ev else ""
    return f"{k}: {stop}START at {at}; the input {how}"


def report(r):
    lines = [f"chain-{r['set']} seed {r['seed']}: {len(r['events'])} events, {r['blocks']} blocks, "
             f"{r['timing']['cycles']} cycles ({r['timing']['delivered']} symbols) in {r['timing']['sim']:.0f} s "
             f"(build {r['timing']['build']:.0f} s, total {r['timing']['total']:.0f} s)"]
    for i, ev in enumerate(r["events"]):
        lines.append(f"  [{i}] {describe(ev)}")
        for s in r["samples"]:
            if s["event"] == i:
                st = s["state"]
                lines.append(f"      {s['label']}: held {st['held']}, load_ready {st['load_ready']}, levels "
                             f"in/tag/out/rf {st['in_level']}/{st['tag_level']}/{st['out_level']}/{st['rf_level']}, "
                             f"aborts {st['aborts']}, aborted {st['aborted']}, reused {st['reused']}, "
                             f"overruns {st['dec_overruns']}" + (" PROBLEM" if s["problems"] else ""))
    for s in r["samples"]:
        if s["event"] is None:
            st = s["state"]
            lines.append(f"  end: held {st['held']}, levels {[st[k] for k in EMPTY]}, aborts {st['aborts']}, "
                         f"aborted {st['aborted']}, reused {st['reused']}, overruns {st['dec_overruns']}, "
                         f"fifo peak {st['fifo_max']}")
    lines.append(f"  frames: {r['frames']}")
    lines += [f"  finding: {f}" for f in r["findings"]]
    lines += [f"  PROBLEM: {p}" for p in r["problems"]]
    lines.append("  ok" if not r["problems"] else f"  {len(r['problems'])} problems")
    return "\n".join(lines)
