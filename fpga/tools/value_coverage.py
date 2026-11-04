"""Value coverage of the chain and LDPC vector sets (issue #30, item 1).

Every simulation of the ISDB-S3 chain compares the HDL against the
expected values of a vector set, so a value no set's expected output
contains is a value no simulation checks: builds 6-8 of the receive image
found two defects that every set passed because n0 was at its reset value
in every expected status block and no status block carried a learned
constellation. This tool reads every chain and LDPC set in `fpga/build/`
(`chain-<n>-<stage>.json`, `chain-rec-*`, `ldpc-<n>-*.json`,
`ldpcvec-py-*.json`) and reports, per field, the number of distinct
values and the sets that exercise them:

- every field of the `Slots` words of the whole-chain sets (6-10): the
  block header, the slot block, the TMCC block, the status block and the
  sums block (`isdb_frontend::slots` format 2: `SlotBlock`, `TmccBlock`,
  `Status`, `SumsBlock`; read with `isdb_fpga.chain.slots`), the status
  block per word;
- the per-stage values the stage sets carry (`isdbs3-phy`
  `examples/chainvectors.rs`: heads, periods, carrier updates, φ and gain,
  the FIR's taps and sums, the TMCC layout, points, LLRs, learned points,
  n0 and K) and the LDPC sets' (`examples/ldpcvectors.rs`: rate, schedule,
  limit, and per codeword iterations, convergence, unsatisfied checks,
  LLRs).

A field is **constant** when it has one distinct value over all sets that
carry it, **absent** when no set carries a value for it (a status block's
learned points without a learned constellation); each such field is
classified by its boundary (always 0, the reset value, "none", a flag
never set). Also listed: the boundary values a field never takes (an
uncorrectable BCH word, the iteration limit), the values of an enumerated
field never seen (a rate, a modulation) and the status-block words that
equal their predecessor in every block (a one-word shift there is
invisible).

`ALLOWED` lists the constants accepted by definition, `KNOWN_GAPS` the
real gaps of today's corpus (each with the input that would close it);
`tests/test_coverage.py` fails on a constant or absent field in neither
and on a known gap that is now covered. The gate runs on the default
synthetic corpus (`mise run fpga:vectors`, `fpga:ldpc-vectors`); the
opt-in recording sets (`*-rec-*`) are reported, not gated.

python tools/value_coverage.py [BUILD_DIR] [--markdown] [--all-fields] [--synthetic] [--no-excuses]

Without --markdown a short summary; --all-fields adds the table of every
field; --synthetic leaves out the recording sets (as the test does);
--no-excuses ignores ALLOWED and KNOWN_GAPS (the raw gap list). The exit
status is 1 when a constant or absent field is not excused.
"""

import fnmatch
import json
import os
import re
import sys
from dataclasses import dataclass, field

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import slots as fmt  # noqa: E402

BUILD = os.path.join(os.path.dirname(__file__), "..", "build")

CHAIN_STAGES = ["sync", "phase", "eq", "tmcc", "demap", "chain", "hdl", "loss", "fade", "bch"]
CHAIN_RE = re.compile(r"^chain-(rec-)?(\d+)-(" + "|".join(CHAIN_STAGES) + r")\.json$")
LDPC_RE = re.compile(r"^(ldpc-\d+-.*|ldpcvec-py-\d+-.*)\.json$")

# The default synthetic corpus: the test needs every one of these (the
# constants depend on the corpus, so a partial one proves nothing).
REQUIRED = [f"chain-{i + 1}-{s}.json" for i, s in enumerate(CHAIN_STAGES)] + [
    "ldpc-1-93-seq.json", "ldpc-2-93-layered34.json", "ldpc-3-93-layered187.json",
    "ldpc-4-41-layered34.json", "ldpc-5-109-layered34.json", "ldpc-6-tmcc-layered34.json",
    "ldpc-7-93-stress-layered34.json", "ldpc-8-93-trace.json", "ldpc-9-93-iter0-layered34.json",
    "ldpc-12-93-layered17.json", "ldpc-13-tmcc-layered17.json",
    "ldpcvec-py-1-toy-a.json", "ldpcvec-py-9-93-17.json", "ldpcvec-py-10-tmcc-17.json",
]

NONE32 = 0xFFFF_FFFF
NONE16 = 0xFFFF
MODS = ["bpsk", "qpsk", "8psk", "16apsk", "32apsk"]
MOD_BITS = [1, 2, 3, 4, 5]
B44_RATES = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109]
HDL_MAX_ITER = 14      # ldpc.DEFAULT_MAX_ITER, the chain's MAX_ITER reset
N0_DEFAULT = 52_556    # demap::N0_DEFAULT (each chain file's "chain"."n0_default")
LLR_SHIFT = 32         # demap::LLR_SHIFT
N0_MIN = 256           # demap::N0_MIN


def scale_of(n0):
    """demap::scale_of: round(2^32 / max(n0, N0_MIN))."""
    d = max(n0, N0_MIN)
    return ((1 << LLR_SHIFT) + d // 2) // d


K_DEFAULT = scale_of(N0_DEFAULT)  # 81 722


# --- the fields ------------------------------------------------------------

@dataclass
class Spec:
    """What is known of a field: its meaning, the sets that carry it, its
    reset / "none" value, whether it is a flag, its enumerated domain and
    its notable boundary values."""
    desc: str
    sources: tuple = ()          # set kinds that carry it ("chain-6", "ldpc", ...)
    reset: object = None         # the value it holds before anything is learned
    none: object = None          # the "absent" encoding (0xFFFF_FFFF, 0xFFFF)
    flag: bool = False
    domain: tuple = ()           # enumerated values (missing ones are reported)
    bounds: dict = field(default_factory=dict)  # value -> meaning (reported when never taken)


WHOLE = ("chain-6", "chain-7", "chain-8", "chain-9", "chain-10")
SLOTS_PARTS = ("header", "slot", "dummy", "tmcc", "status", "sums")
SPECS = {}


def spec(name, desc, sources, **kw):
    SPECS[name] = Spec(desc, tuple(sources), **kw)


def _specs():
    # the Slots block header (isdb_frontend::slots module docs)
    spec("header.kind", "block type (1 slot, 2 tmcc, 3 status, 4 sums)", WHOLE, domain=(1, 2, 3, 4))
    spec("header.version", "Slots format version (word 2 bits 8-11)", WHOLE)
    spec("header.flags", "header flags (word 2 bits 12-15, 0)", WHOLE)
    spec("header.payload", "payload words", WHOLE)
    spec("header.seq_step", "sequence number minus the previous block's", WHOLE)
    spec("header.frame", "frame counter", WHOLE)
    # slot block
    spec("slot.slot", "slot index", WHOLE, domain=tuple(range(120)))
    spec("slot.mode", "transmission mode", WHOLE, domain=tuple(range(8)))
    spec("slot.dummy", "dummy slot", WHOLE, flag=True)
    spec("slot.w0_reserved", "w0 bits 14-15", WHOLE)
    # the decoder's fields of the valid (non-dummy) slots
    spec("slot.converged", "valid slot: LDPC converged", WHOLE, flag=True)
    spec("slot.bch_ok", "valid slot: BCH ok (w0 bit 13)", WHOLE, flag=True)
    spec("slot.modulation", "valid slot: modulation code (Table 3.11-1)", WHOLE,
         domain=(1, 2, 3, 4, 5))
    spec("slot.rate", "valid slot: rate code (Table 3.11-2)", WHOLE, domain=tuple(range(1, 12)))
    spec("slot.iterations", "valid slot: LDPC iterations (saturated 255)", WHOLE,
         bounds={0: "iteration 0", HDL_MAX_ITER: "the HDL limit", 255: "saturated"})
    spec("slot.bch", "valid slot: BCH bits corrected, 0xFFFF uncorrectable", WHOLE, none=NONE16,
         bounds={0: "no correction", 12: "t = 12 corrections", NONE16: "uncorrectable"})
    # the same words of a dummy slot (not decoded)
    spec("dummy.converged", "dummy slot: converged bit", WHOLE, flag=True)
    spec("dummy.bch_ok", "dummy slot: BCH ok bit", WHOLE, flag=True)
    spec("dummy.modulation", "dummy slot: modulation code", WHOLE)
    spec("dummy.rate", "dummy slot: rate code", WHOLE)
    spec("dummy.iterations", "dummy slot: iterations", WHOLE)
    spec("dummy.bch", "dummy slot: BCH word", WHOLE, none=NONE16)
    spec("dummy.bits_words", "dummy slot: payload words after w2", WHOLE)
    # tmcc block
    spec("tmcc.bch", "TMCC BCH bits corrected, 0xFFFF failed", WHOLE, none=NONE16,
         bounds={0: "no correction", NONE16: "failed"})
    spec("tmcc.iterations", "TMCC LDPC iterations, 0xFFFF not converged", WHOLE, none=NONE16,
         bounds={0: "iteration 0", NONE16: "not converged"})
    spec("tmcc.lag", "applies_from minus the frame counter (the layout lag)", WHOLE)
    spec("tmcc.bits", "the 9 422 TMCC information bits", WHOLE, domain=(0, 1))
    # status block (Status, in its word order)
    spec("status.peak", "sync: the last period's peak", WHOLE)
    spec("status.peak_pos", "sync: its position", WHOLE)
    spec("status.sum", "sync: the period's accumulator sum (48 bits)", WHOLE)
    spec("status.locked", "sync: locked (flags bit 0)", WHOLE, flag=True)
    spec("status.ssync_parity", "sync: SSync parity (flags bit 1)", WHOLE, flag=True)
    spec("status.flags_reserved", "sync flags bits 2-15", WHOLE)
    spec("status.offset", "sync: locked offset", WHOLE)
    spec("status.stray_markers", "sync: stray frame markers", WHOLE, bounds={0: "none"})
    spec("status.missed_markers", "sync: missed frame markers", WHOLE, bounds={0: "none"})
    spec("status.carrier", "carrier register", WHOLE)
    spec("status.agc", "input AGC gain, Q6.10 (the host's AGC register, reset 0)", WHOLE,
         reset=0)
    for k in range(11):
        for c in ("re", "im"):
            spec(f"status.taps.{k}.{c}", f"equaliser tap {k} in use, {c}", WHOLE)
    spec("status.taps_from", "frame the taps were solved from (0xFFFF_FFFF none)", WHOLE,
         none=NONE32)
    spec("status.sums_frame", "frame of the training sums latched (0xFFFF_FFFF none)", WHOLE,
         none=NONE32)
    spec("status.n0", "n0 in force, Q.20", WHOLE, reset=N0_DEFAULT)
    spec("status.llr_scale", "LLR scale K in force", WHOLE, reset=K_DEFAULT)
    spec("status.learned_from", "frame the constellation and n0 were learned on "
         "(0xFFFF_FFFF none)", WHOLE, none=NONE32)
    for m, b in zip(MODS, MOD_BITS):
        spec(f"status.constellation.{m}.count", f"{m} learned points (0 nominal)", WHOLE,
             reset=0, domain=(0, 1 << b))
        for c in ("re", "im"):
            spec(f"status.constellation.{m}.{c}", f"{m} learned points, {c} (Q.10)", WHOLE)
    # cumulative counters (a status block follows a whole frame, so frames
    # >= 1, and the first frames have no layout, so frames_undecoded >= 1)
    for n in ("frames", "frames_undecoded", "tmcc_failed", "slots", "ldpc_failed",
              "bch_failed", "iterations"):
        spec(f"status.{n}", f"counter: {n}", WHOLE, reset=0)
    # sums block (format 2: SumsBlock, after its frame's status block)
    spec("sums.frame_lag", "the frame summed (payload words 0-1) minus the header's frame", WHOLE)
    spec("sums.count", "training symbols summed", WHOLE)
    spec("sums.r", "R's 66 upper-triangle sums, re and im (48 bit signed)", WHOLE)
    spec("sums.p", "p's 11 sums, re and im (48 bit signed)", WHOLE)
    spec("sums.after_status", "the block before it is its frame's status block", WHOLE, flag=True)
    # the whole-chain sets' own fields
    spec("chain.frames.decoded", "frames[].decoded", WHOLE, flag=True)
    spec("chain.frames.tmcc_bch", "frames[].tmcc_bch (None: failed)", WHOLE)
    spec("chain.frames.ldpc_iterations", "frames[].ldpc_iterations per valid slot", WHOLE)
    spec("chain.losses", "losses: symbols lost per event", WHOLE)
    spec("chain.bch_errors", "bch_errors: bit errors injected per slot", ("chain-10",))
    spec("input.re", "input symbols (6 bit), re", ("chain-1", "chain-2") + WHOLE,
         bounds={-32: "negative full scale", 31: "positive full scale"})
    spec("input.im", "input symbols (6 bit), im", ("chain-1", "chain-2") + WHOLE,
         bounds={-32: "negative full scale", 31: "positive full scale"})
    # stage 1
    s1 = ("chain-1",)
    spec("sync.r.re", "NCO output, re", s1 + ("chain-2",))
    spec("sync.r.im", "NCO output, im", s1 + ("chain-2",))
    spec("sync.nco_phase", "NCO phase (every 4 096 symbols; chain-2 every symbol)",
         s1 + ("chain-2",))
    for n in ("cf_re", "cf_im", "cs_re", "cs_im", "acc"):
        spec(f"sync.per_symbol.{n}", f"per-position {n}", s1)
    spec("sync.period.peak", "period peak", s1)
    spec("sync.period.peak_pos", "period peak position", s1)
    spec("sync.period.sum", "period accumulator sum", s1)
    spec("sync.period.locked", "period locked", s1, flag=True)
    spec("sync.period.offset", "period locked offset", s1)
    spec("sync.period.parity", "period SSync parity", s1, flag=True)
    spec("sync.carrier", "carrier register after an update", s1)
    for n in ("corr_re", "corr_im", "theta", "amp"):
        spec(f"head.{n}", f"head record {n}", s1 + ("chain-2",))
    spec("head.word", "head word (0 SSync, 1 FSync, 2 !FSync)", s1 + ("chain-2",),
         domain=(0, 1, 2))
    spec("head.number", "head slot number (-1 unknown)", s1 + ("chain-2",),
         domain=tuple(range(-1, 120)))
    spec("head.frame", "head frame counter (-1 unknown)", s1 + ("chain-2",))
    # stage 3
    s2 = ("chain-2",)
    spec("phase.j", "slot position j", s2)
    spec("phase.phi", "φ", s2)
    spec("phase.gain", "gain", s2)
    spec("phase.y.re", "y, re", s2)
    spec("phase.y.im", "y, im", s2)
    # stage 4
    s3 = ("chain-3",)
    spec("eq.taps.re", "taps in use (Q2.14), re", s3)
    spec("eq.taps.im", "taps in use (Q2.14), im", s3)
    spec("eq.taps_from", "taps_from (None: loaded by the host)", s3)
    spec("eq.y.re", "FIR input, re", s3)
    spec("eq.y.im", "FIR input, im", s3)
    spec("eq.z.re", "FIR output, re", s3)
    spec("eq.z.im", "FIR output, im", s3)
    spec("eq.training.re", "training reference, re", s3)
    spec("eq.training.im", "training reference, im", s3)
    spec("eq.sums.count", "training sums: count", s3)
    spec("eq.sums.r", "training sums: R (48 bit, re and im)", s3)
    spec("eq.sums.p", "training sums: p (48 bit, re and im)", s3)
    spec("eq.solved", "the host's taps for the sums", s3)
    # stage 5
    s4 = ("chain-4",)
    spec("tmccst.z.re", "TMCC symbols, re", s4)
    spec("tmccst.z.im", "TMCC symbols, im", s4)
    spec("tmccst.soft", "TMCC soft bits", s4,
         bounds={-127: "negative saturation", 127: "positive saturation"})
    spec("tmccst.known", "known-zero value (+127)", s4)
    spec("tmccst.ldpc_iterations", "TMCC LDPC iterations (None: failed)", s4)
    spec("tmccst.bch_corrected", "TMCC BCH corrected (None: failed)", s4)
    spec("tmccst.lag", "applies_to minus frame", s4)
    for n, dom in (("slot", tuple(range(120))), ("mode", tuple(range(8))),
                   ("modulation", (1, 2, 3, 4, 5)), ("rate", tuple(range(1, 12))),
                   ("dummy", (0, 1)), ("symbol_start", ()), ("symbols", ())):
        spec(f"tmccst.layout.{n}", f"layout entry {n}", s4, domain=dom, flag=n == "dummy")
    # stage 6
    s5 = ("chain-5",)
    spec("demap.k", "K in force", s5, reset=K_DEFAULT)
    spec("demap.n0", "n0 in force", s5, reset=N0_DEFAULT)
    spec("demap.k_next", "K for the next frame", s5, reset=K_DEFAULT)
    spec("demap.n0_next", "n0 for the next frame", s5, reset=N0_DEFAULT)
    spec("demap.slot.modulation", "slot modulation code", s5, domain=(1, 2, 3, 4, 5))
    spec("demap.slot.rate", "slot rate code", s5, domain=tuple(range(1, 12)))
    spec("demap.points.re", "points demapped with, re (Q.10)", s5)
    spec("demap.points.im", "points demapped with, im (Q.10)", s5)
    spec("demap.z.re", "codeword symbols, re", s5)
    spec("demap.z.im", "codeword symbols, im", s5)
    spec("demap.llr", "LLRs (label and codeword order)", s5,
         bounds={-128: "i8 minimum", 127: "i8 maximum"})
    for m, b in zip(MODS, MOD_BITS):
        spec(f"demap.learned.{m}.count", f"{m} learned points for the next frame (0 none)", s5,
             domain=(0, 1 << b))
    spec("demap.learned.re", "learned points, re", s5)
    spec("demap.learned.im", "learned points, im", s5)
    # LDPC
    L = ("ldpc",)
    spec("ldpc.code", "code (B44 rate numerator; 61s the shortened TMCC; toy-*)", L,
         domain=tuple(str(r) for r in B44_RATES) + ("61s",))
    spec("ldpc.schedule", "schedule kind", L, domain=("sequential", "layered"))
    spec("ldpc.step", "layered step S", L, domain=(11, 17, 22, 34, 187))
    spec("ldpc.max_iter", "iteration limit", L,
         bounds={0: "limit 0", HDL_MAX_ITER: "the HDL default"})
    spec("ldpc.offset", "min-sum offset", L)
    spec("ldpc.iterations", "iterations per codeword", L, bounds={0: "iteration 0"})
    spec("ldpc.at_limit", "iterations == max_iter", L, flag=True)
    spec("ldpc.converged", "converged per codeword", L, flag=True)
    spec("ldpc.unsatisfied", "unsatisfied checks per codeword", L, bounds={0: "zero"})
    spec("ldpc.llr", "input LLRs", L, bounds={-128: "i8 minimum", 127: "i8 maximum"})
    spec("ldpc.trace", "codeword carries trace_hex", L, flag=True)


_specs()


# --- the reader -------------------------------------------------------------

def set_kind(name):
    """'chain-<n>' (recording sets too), 'ldpc' or None for a file that is
    not a vector set."""
    m = CHAIN_RE.match(name)
    if m:
        return f"chain-{m.group(2)}"
    if LDPC_RE.match(name):
        return "ldpc"
    return None


def vector_sets(build=BUILD, synthetic=False):
    """The vector sets in `build`, sorted by kind and number."""
    out = []
    for n in os.listdir(build):
        if set_kind(n) is None or (synthetic and "-rec-" in n):
            continue
        out.append(n)

    def key(n):
        nums = [int(x) for x in re.findall(r"\d+", n)]
        return (0 if n.startswith("chain") else 1, "py" in n, nums, n)
    return sorted(out, key=key)


def missing_required(build=BUILD):
    have = set(os.listdir(build)) if os.path.isdir(build) else set()
    return [n for n in REQUIRED if n not in have]


class Values:
    """field -> set name -> the set of its values there."""

    def __init__(self):
        self.v = {}
        self.blocks = {}  # status payload length -> list of payloads (shift analysis)

    def add(self, name, setname, values):
        if name not in SPECS:
            raise KeyError(f"unregistered field {name}")
        self.v.setdefault(name, {}).setdefault(setname, set()).update(values)

    def one(self, name, setname, value):
        self.add(name, setname, (value,))


def s16(w):
    return w - 0x10000 if w & 0x8000 else w


def s32(v):
    return v - (1 << 32) if v & 0x8000_0000 else v


def _status(V, sn, frame, p):
    """One status block's payload into its fields (Status::parse)."""
    V.blocks.setdefault(len(p), []).append(p)
    i = 0

    def take(n):
        nonlocal i
        s = p[i:i + n]
        assert len(s) == n, "short status block"
        i += n
        return s

    def r32(s):
        return s[0] | s[1] << 16

    V.one("status.peak", sn, r32(take(2)))
    V.one("status.peak_pos", sn, take(1)[0])
    s = take(3)
    V.one("status.sum", sn, s[0] | s[1] << 16 | s[2] << 32)
    fl = take(1)[0]
    V.one("status.locked", sn, fl & 1)
    V.one("status.ssync_parity", sn, fl >> 1 & 1)
    V.one("status.flags_reserved", sn, fl >> 2)
    V.one("status.offset", sn, take(1)[0])
    V.one("status.stray_markers", sn, r32(take(2)))
    V.one("status.missed_markers", sn, r32(take(2)))
    V.one("status.carrier", sn, s32(r32(take(2))))
    V.one("status.agc", sn, take(1)[0])
    tw = take(22)
    for k in range(11):
        V.one(f"status.taps.{k}.re", sn, s16(tw[2 * k]))
        V.one(f"status.taps.{k}.im", sn, s16(tw[2 * k + 1]))
    V.one("status.taps_from", sn, r32(take(2)))
    V.one("status.sums_frame", sn, r32(take(2)))
    V.one("status.n0", sn, r32(take(2)))
    V.one("status.llr_scale", sn, r32(take(2)))
    V.one("status.learned_from", sn, r32(take(2)))
    for m in MODS:
        n = take(1)[0]
        V.one(f"status.constellation.{m}.count", sn, n)
        w = take(2 * n)
        V.add(f"status.constellation.{m}.re", sn, (s16(x) for x in w[0::2]))
        V.add(f"status.constellation.{m}.im", sn, (s16(x) for x in w[1::2]))
    for n in ("frames", "frames_undecoded", "tmcc_failed", "slots", "ldpc_failed", "bch_failed"):
        V.one(f"status.{n}", sn, r32(take(2)))
    it = take(4)
    V.one("status.iterations", sn, sum(it[k] << (16 * k) for k in range(4)))
    assert i == len(p), "long status block"


def _slots_words(V, sn, words):
    """The Slots words of a whole-chain set (the reader asserts the sync,
    header check and CRC of every block)."""
    prev = None
    before = None  # the previous block: (kind, frame)
    i = 0
    while i < len(words):
        h = words[i:i + fmt.HEADER_WORDS]
        V.one("header.version", sn, h[2] >> 8 & 0xF)
        V.one("header.flags", sn, h[2] >> 12)
        i += fmt.HEADER_WORDS + h[5] + 2
    for kind, seq, frame, pay in fmt.blocks(words):
        V.one("header.kind", sn, kind)
        V.one("header.payload", sn, len(pay))
        V.one("header.frame", sn, frame)
        if prev is not None:
            V.one("header.seq_step", sn, (seq - prev) & NONE32)
        prev = seq
        if kind == fmt.SLOT:
            w0, w1, w2 = pay[:3]
            V.one("slot.slot", sn, w0 & 0xFF)
            V.one("slot.mode", sn, w0 >> 8 & 7)
            V.one("slot.dummy", sn, w0 >> 11 & 1)
            V.one("slot.w0_reserved", sn, w0 >> 14)
            k = "dummy" if w0 >> 11 & 1 else "slot"
            V.one(f"{k}.converged", sn, w0 >> 12 & 1)
            V.one(f"{k}.bch_ok", sn, w0 >> 13 & 1)
            V.one(f"{k}.modulation", sn, w1 & 0xF)
            V.one(f"{k}.rate", sn, w1 >> 4 & 0xF)
            V.one(f"{k}.iterations", sn, w1 >> 8)
            V.one(f"{k}.bch", sn, w2)
            if k == "dummy":
                V.one("dummy.bits_words", sn, len(pay) - 3)
        elif kind == fmt.TMCC:
            V.one("tmcc.bch", sn, pay[0])
            V.one("tmcc.iterations", sn, pay[1])
            V.one("tmcc.lag", sn, ((pay[2] | pay[3] << 16) - frame) & NONE32)
            V.add("tmcc.bits", sn, fmt.unpack_bits(pay[4:], 9422))
        elif kind == fmt.STATUS:
            _status(V, sn, frame, pay)
        elif kind == fmt.SUMS:
            f = fmt.sums_fields(pay)
            V.one("sums.frame_lag", sn, (f["frame"] - frame) & NONE32)
            V.one("sums.count", sn, f["count"])
            V.add("sums.r", sn, (c for v in f["r"] for c in v))
            V.add("sums.p", sn, (c for v in f["p"] for c in v))
            V.one("sums.after_status", sn, int(before == (fmt.STATUS, frame)))
        before = (kind, frame)


def _cplx(V, name, sn, flat):
    V.add(name + ".re", sn, flat[0::2])
    V.add(name + ".im", sn, flat[1::2])


def _col(rows, k):
    return (r[k] for r in rows)


def _wide(hl):
    return hl[0] << 32 | hl[1]


def _head_rows(V, sn, heads):
    for k, n in enumerate(("word", "corr_re", "corr_im", "theta", "amp", "number", "frame")):
        V.add(f"head.{n}", sn, _col(heads, k + 1))


def _chain(V, sn, stage, d):
    if stage == "sync":
        _cplx(V, "input", sn, d["input"])
        _cplx(V, "sync.r", sn, d["r"])
        V.add("sync.nco_phase", sn, _col(d["nco_phase"], 1))
        for k, n in enumerate(("cf_re", "cf_im", "cs_re", "cs_im", "acc")):
            V.add(f"sync.per_symbol.{n}", sn, _col(d["per_symbol"], k + 1))
        pr = d["periods"]
        V.add("sync.period.peak", sn, _col(pr, 1))
        V.add("sync.period.peak_pos", sn, _col(pr, 2))
        V.add("sync.period.sum", sn, (r[3] | r[4] << 32 for r in pr))
        V.add("sync.period.locked", sn, _col(pr, 5))
        V.add("sync.period.offset", sn, _col(pr, 6))
        V.add("sync.period.parity", sn, _col(pr, 7))
        _head_rows(V, sn, d["heads"])
        V.add("sync.carrier", sn, _col(d["carrier"], 1))
    elif stage == "phase":
        _head_rows(V, sn, d["heads"])
        _cplx(V, "sync.r", sn, d["r"])
        _cplx(V, "input", sn, d["x"])
        V.add("sync.nco_phase", sn, d["phi"])
        o = d["out"]
        V.add("phase.j", sn, _col(o, 1))
        V.add("phase.phi", sn, _col(o, 2))
        V.add("phase.gain", sn, _col(o, 3))
        V.add("phase.y.re", sn, _col(o, 4))
        V.add("phase.y.im", sn, _col(o, 5))
    elif stage == "eq":
        _cplx(V, "eq.taps", sn, d["taps"])
        V.one("eq.taps_from", sn, d["taps_from"])
        _cplx(V, "eq.y", sn, d["y"])
        _cplx(V, "eq.z", sn, d["z"])
        V.add("eq.training.re", sn, _col(d["training"], 1))
        V.add("eq.training.im", sn, _col(d["training"], 2))
        su = d["sums"]
        V.one("eq.sums.count", sn, su["count"])
        V.add("eq.sums.r", sn, (_wide(c) for v in su["r"] for c in v))
        V.add("eq.sums.p", sn, (_wide(c) for v in su["p"] for c in v))
        V.add("eq.solved", sn, d["solved"] or [None])
    elif stage == "tmcc":
        _cplx(V, "tmccst.z", sn, d["z_tmcc"])
        V.add("tmccst.soft", sn, d["soft"])
        V.one("tmccst.known", sn, d["known"])
        V.one("tmccst.ldpc_iterations", sn, d["ldpc_iterations"])
        V.one("tmccst.bch_corrected", sn, d["bch_corrected"])
        V.one("tmccst.lag", sn, d["applies_to"] - d["frame"])
        for k, n in enumerate(("slot", "mode", "modulation", "rate", "dummy", "symbol_start",
                               "symbols")):
            V.add(f"tmccst.layout.{n}", sn, _col(d["layout"], k))
    elif stage == "demap":
        V.one("demap.k", sn, d["k"])
        V.one("demap.n0", sn, d["n0"])
        V.one("demap.k_next", sn, d["k_next"])
        V.one("demap.n0_next", sn, d["n0_next"])
        for s in d["slots"]:
            V.one("demap.slot.modulation", sn, s["modulation"])
            V.one("demap.slot.rate", sn, s["rate"])
            _cplx(V, "demap.points", sn, s["points"])
            _cplx(V, "demap.z", sn, s["z"])
            V.add("demap.llr", sn, s["llr_label"])
            V.add("demap.llr", sn, s["llr"])
        for m, pts in zip(MODS, d["learned"]):
            V.one(f"demap.learned.{m}.count", sn, 0 if pts is None else len(pts) // 2)
            if pts:
                _cplx(V, "demap.learned", sn, pts)
    else:  # a whole-chain set
        _cplx(V, "input", sn, d["input"])
        _slots_words(V, sn, d["slots_words"])
        for f in d["frames"]:
            V.one("chain.frames.decoded", sn, int(f["decoded"]))
            V.one("chain.frames.tmcc_bch", sn, f["tmcc_bch"])
            V.add("chain.frames.ldpc_iterations", sn, f["ldpc_iterations"])
        V.add("chain.losses", sn, (n for _, n in d["losses"]))
        if "bch_errors" in d:
            V.add("chain.bch_errors", sn, (len(p) for _, _, p in d["bch_errors"]))


def _ldpc(V, sn, d):
    if d["n"] == 44880:
        code = f"{d['rate']}s" if d["shortened"] else str(d["rate"])
    else:
        code = f"toy-{d.get('label', d['n'])}"
    V.one("ldpc.code", sn, code)
    sch = d["schedule"]
    V.one("ldpc.schedule", sn, sch["kind"])
    if sch["kind"] == "layered":
        V.one("ldpc.step", sn, sch["step"])
    V.one("ldpc.max_iter", sn, d["max_iter"])
    V.one("ldpc.offset", sn, d["offset"])
    for c in d["codewords"]:
        V.one("ldpc.iterations", sn, c["iterations"])
        V.one("ldpc.at_limit", sn, int(c["iterations"] == d["max_iter"]))
        V.one("ldpc.converged", sn, int(c["converged"]))
        V.one("ldpc.unsatisfied", sn, c["unsatisfied"])
        V.add("ldpc.llr", sn, c["llr"])
        V.one("ldpc.trace", sn, int("trace_hex" in c))


def read(build=BUILD, sets=None):
    """The values of every field over `sets` (default: every set in `build`)."""
    V = Values()
    sets = vector_sets(build) if sets is None else sets
    for name in sets:
        with open(os.path.join(build, name)) as f:
            d = json.load(f)
        sn = name[:-5]
        m = CHAIN_RE.match(name)
        if m:
            _chain(V, sn, m.group(3), d)
        else:
            _ldpc(V, sn, d)
        del d
    return V, sets


# --- the analysis -----------------------------------------------------------

@dataclass
class FieldResult:
    name: str
    desc: str
    per_set: dict          # set -> distinct values there
    distinct: int
    lo: object
    hi: object
    values: list           # up to 8 example values (all when few)
    status: str            # "ok", "constant", "absent", "not read" (no source set read)
    label: str             # the boundary class of a constant / absent field
    missing_bounds: list   # [(value, meaning)] never taken
    missing_domain: list   # enumerated values never seen
    excuse: str = ""       # "allowed: …" / "known gap: …" / ""


@dataclass
class Coverage:
    sets: list
    fields: dict           # name -> FieldResult
    shift_blind: dict      # status payload length -> [(word index, field of the word)]
    status_blocks: dict    # status payload length -> number of blocks
    gaps: dict = field(default_factory=dict)  # the known gaps applied

    def within_constant(self):
        """Fields of the Slots words that vary in the corpus but never inside
        one set: a value that lags by a frame (or a write) passes there."""
        return [f for f in self.fields.values()
                if f.status == "ok" and f.name.split(".")[0] in SLOTS_PARTS
                and all(n <= 1 for n in f.per_set.values())]

    def flagged(self):
        return [f for f in self.fields.values() if f.status in ("constant", "absent")]

    def unexcused(self):
        return [f for f in self.flagged() if not f.excuse]

    def stale_gaps(self):
        """Known gaps that the corpus now covers (remove them from KNOWN_GAPS)."""
        out = []
        for pat in self.gaps:
            hits = [f for n, f in self.fields.items() if fnmatch.fnmatchcase(n, pat)
                    and f.status != "not read"]
            if hits and all(f.status == "ok" for f in hits):
                out.append(pat)
        return out


def _fmt(v):
    if isinstance(v, int) and not isinstance(v, bool) and abs(v) >= 0x10000:
        return f"{v} (0x{v & ((1 << 64) - 1):X})"
    return str(v)


def _label(spec, vals):
    if not vals:
        return "never present"
    (v,) = vals
    if spec.flag:
        return "flag never set" if v == 0 else "flag always set"
    if spec.none is not None and v == spec.none:
        return "always none"
    if spec.reset is not None and v == spec.reset:
        return "always the reset value"
    if v == 0:
        return "always 0"
    if v is None:
        return "always None"
    return "constant"


def _status_word_names():
    """Field name of each status payload word for the base layout (no
    learned points); the constellation words follow their count."""
    n = (["status.peak"] * 2 + ["status.peak_pos"] + ["status.sum"] * 3
         + ["status.flags"] + ["status.offset"] + ["status.stray_markers"] * 2
         + ["status.missed_markers"] * 2 + ["status.carrier"] * 2 + ["status.agc"])
    n += [f"status.taps.{k}.{c}" for k in range(11) for c in ("re", "im")]
    for f in ("taps_from", "sums_frame", "n0", "llr_scale", "learned_from"):
        n += [f"status.{f}"] * 2
    return n


def _word_name(p, i):
    base = _status_word_names()
    if i < len(base):
        return base[i]
    j = len(base)
    for m in MODS:
        n = p[j]
        if i == j:
            return f"status.constellation.{m}.count"
        if i < j + 1 + 2 * n:
            return f"status.constellation.{m}.{'re' if (i - j - 1) % 2 == 0 else 'im'}"
        j += 1 + 2 * n
    k = i - j
    names = ["frames", "frames_undecoded", "tmcc_failed", "slots", "ldpc_failed", "bch_failed"]
    return f"status.{names[k // 2]}" if k < 12 else "status.iterations"


def _shift_blind(blocks):
    """Status words equal to the previous word in every block of a layout:
    a defect that sends word i - 1 for word i passes there."""
    out = {}
    for n, bl in sorted(blocks.items()):
        out[n] = [(i, _word_name(bl[0], i)) for i in range(1, n)
                  if all(p[i] == p[i - 1] for p in bl)]
    return out


def analyse(V, sets, allowed=None, gaps=None):
    allowed = ALLOWED if allowed is None else allowed
    gaps = KNOWN_GAPS if gaps is None else gaps
    kinds = {set_kind(s + ".json") for s in sets}
    fields = {}
    for name, sp in SPECS.items():
        per = V.v.get(name, {})
        union = set()
        for s in per.values():
            union |= s
        nums = sorted(x for x in union if isinstance(x, (int, float)) and not isinstance(x, bool))
        others = sorted((x for x in union if not isinstance(x, (int, float))), key=repr)
        read_any = any(k in kinds for k in sp.sources)
        if not read_any:
            status = "not read"
        elif not union:
            status = "absent"
        elif len(union) == 1:
            status = "constant"
        else:
            status = "ok"
        ex = (nums[:4] + nums[-4:] if len(nums) > 8 else nums) + others[:4]
        r = FieldResult(
            name=name, desc=sp.desc, per_set={s: len(v) for s, v in per.items()},
            distinct=len(union), lo=nums[0] if nums else None, hi=nums[-1] if nums else None,
            values=ex, status=status,
            label=_label(sp, union) if status in ("constant", "absent") else "",
            missing_bounds=[(v, m) for v, m in sp.bounds.items() if v not in union]
            if read_any else [],
            missing_domain=[v for v in sp.domain if v not in union] if read_any else [])
        if status in ("constant", "absent"):
            for table, word in ((allowed, "allowed"), (gaps, "known gap")):
                for pat, why in table.items():
                    if fnmatch.fnmatchcase(name, pat):
                        r.excuse = f"{word}: {why}"
                        break
                if r.excuse:
                    break
        fields[name] = r
    return Coverage(sets=sets, fields=fields, shift_blind=_shift_blind(V.blocks),
                    status_blocks={n: len(b) for n, b in V.blocks.items()}, gaps=gaps)


def coverage(build=BUILD, synthetic=False, sets=None, excuses=True):
    """Read and analyse the vector sets in `build` (`synthetic`: without the
    recording sets; `excuses`: apply ALLOWED and KNOWN_GAPS). Returns a
    `Coverage`."""
    if sets is None:
        sets = vector_sets(build, synthetic=synthetic)
    V, sets = read(build, sets)
    if excuses:
        return analyse(V, [s[:-5] for s in sets])
    return analyse(V, [s[:-5] for s in sets], allowed={}, gaps={})


# --- the allow-list and the known gaps ---------------------------------------

# Constants accepted by definition (fnmatch patterns, one reason each): the
# field cannot vary in a correct design, or the set is built so that it
# cannot. A field here is not checked by the vectors' values at all, so an
# entry needs a reason a reviewer can verify.
ALLOWED = {
    "header.version": "the Slots format version is 2 by definition (SLOTS_VERSION; the HDL "
                      "writes 2, the reader takes 1 too)",
    "header.flags": "the header flags are 0 in format versions 1 and 2",
    "header.seq_step": "one sequence number per block: always 1 on a stream the chain "
                       "sends whole (a gap is a defect)",
    "slot.w0_reserved": "slot w0 bits 14-15 are unused (0) in format version 1",
    "status.flags_reserved": "sync flags bits 2-15 are unused (0) in format version 1",
    "dummy.converged": "a dummy slot is not decoded: converged 0 (chain::out::blocks)",
    "dummy.bch_ok": "a dummy slot is not decoded: BCH ok 0",
    "dummy.bch": "a dummy slot is not decoded: BCH word 0xFFFF",
    "dummy.iterations": "a dummy slot is not decoded: 0 iterations",
    "dummy.bits_words": "a dummy slot carries no bits",
    "tmcc.lag": "the first image supports layout lag 2 only (LAYOUT_LAG; FEATURES says so)",
    "tmccst.lag": "LAYOUT_LAG is 2: the first image supports layout lag 2 only",
    "tmccst.known": "the known-zero LLR is +127 by definition (tmcc::KNOWN_ZERO)",
    "status.locked": "a status block is taken at a slot head, which the slot sync gives only "
                     "while locked; a loss of lock restarts the chain and drops the frame",
    "eq.sums.count": "one whole frame's training symbols: 120 x (24 + 32) = 6 720",
    "sums.count": "one whole frame's training symbols: 120 x (24 + 32) = 6 720 (as "
                  "eq.sums.count; a frame is summed only whole, with its layout)",
    "sums.frame_lag": "the sums block's frame is the frame summed by definition: the latch at "
                      "that frame's end, sent after its status block (0; a defect sending "
                      "another frame's sums shows in the sums themselves)",
    "sums.after_status": "a sums block follows its frame's status block by definition (the "
                         "model's order; the HDL's Finish sends it right after; the monitor "
                         "sums.follows_status checks it in every simulation)",
    "eq.taps_from": "chain-3's taps are the host's initial taps by construction (None); the "
                    "chain's own label is status.taps_from (a known gap)",
    "demap.n0_next": "chain-5 carries one frame's learning, so one value; it is a learned "
                     "value (39 982), not the reset",
    "demap.k_next": "chain-5 carries one frame's learning, so one value; it is a learned "
                    "value (107 423), not the reset 81 722",
    "chain.losses": "a property of chain-8's input (its one loss of 6 144 symbols), not a "
                    "value the HDL produces",
    "ldpc.offset": "the min-sum offset is fixed at 1 in the HDL decoder (HDL_OFFSET)",
}

# Real gaps of today's corpus (fnmatch patterns): fields the vector
# generators (`chainvectors.rs`, `ldpcvectors.rs`) should vary, each with
# the input that would. They are excused so the suite stays green until a
# generator changes; remove an entry once a set covers it (the test fails
# on a known gap that is covered). Every whole-chain set (6-10) decodes
# exactly one frame, its last whole one, so nothing a decoded frame
# teaches the next one reaches a status block.
KNOWN_GAPS = {
    "status.n0": "always the reset n0: needs a status block of the frame after a decoded "
                 "frame (the whole-chain sets one or more frames longer: frame D + 1 whole)",
    "status.llr_scale": "always the reset K (K follows n0): the same input as status.n0",
    "status.learned_from": "always none: the same input as status.n0",
    "status.constellation.*": "no status block carries learned points: the input of status.n0 "
                              "(frame D + 1 whole) gives 16APSK's 16; the other modulations "
                              "need their slots (a mixed-mode TMCC)",
    "status.taps_from": "the taps never change inside a set: needs frame D + 2 whole with "
                        "eq_every > 0 (taps solved at D are in use from D + EQ_APPLY_FRAMES)",
    "status.agc": "no set writes the AGC register (reset 0): needs Chain::set_agc in the "
                  "generator and the value in the set for the bench to write REG_AGC",
    "status.ldpc_failed": "no slot fails LDPC: needs a slot whose symbols are destroyed (e.g. "
                          "one slot replaced by noise) so the real decoder stops at 14",
    "status.bch_failed": "no slot fails BCH: needs 13 or more bit errors in one converged "
                         "slot (chain-10's injection beyond t = 12)",
    "status.tmcc_failed": "no TMCC fails: needs a TMCC word the code cannot correct (noise "
                          "on the TMCC symbols, or errors beyond its BCH t)",
    "status.missed_markers": "no frame marker missed: needs one frame head's FSync corrupted "
                             "while the lock holds",
    "status.stray_markers": "no stray frame marker: needs a !FSync head of another slot "
                            "turned into FSync polarity",
    "slot.converged": "every valid slot converges: the same input as status.ldpc_failed",
    "slot.bch_ok": "every converged slot's BCH succeeds: the same input as status.bch_failed",
    "slot.mode": "every slot is mode 0: needs a TMCC with several transmission modes",
    "slot.modulation": "every slot is 16APSK: needs pi/2-BPSK, QPSK, 8PSK and 32APSK slots "
                       "(a mixed-mode TMCC)",
    "slot.rate": "every slot is 93/120: needs the other ten rates",
    "dummy.modulation": "a dummy carries its mode's codes: one mode, one value (as slot.mode)",
    "dummy.rate": "a dummy carries its mode's codes: one mode, one value (as slot.rate)",
    "tmcc.bch": "no TMCC word needs a BCH correction: needs bit errors in the TMCC decoder's "
                "output (as chain-10 does for slots) or a noisier TMCC",
    "tmcc.iterations": "every TMCC word is a codeword at iteration 0: needs noise on the TMCC "
                       "symbols (lower Es/N0 there)",
    "chain.frames.tmcc_bch": "no TMCC BCH correction: the same input as tmcc.bch",
    "tmccst.ldpc_iterations": "chain-4's TMCC decodes at iteration 0 (as tmcc.iterations)",
    "tmccst.bch_corrected": "chain-4's TMCC has no BCH correction in its expected values (the "
                            "HDL test flips 7 bits itself)",
    "tmccst.layout.mode": "the layout has one mode: needs a mixed-mode TMCC",
    "tmccst.layout.modulation": "the layout is 16APSK only: needs a mixed-mode TMCC",
    "tmccst.layout.rate": "the layout is 93/120 only: needs a mixed-mode TMCC",
    "demap.slot.modulation": "chain-5's codewords are 16APSK only: needs a mixed-mode frame",
    "demap.slot.rate": "chain-5's codewords are 93/120 only",
    "demap.learned.*.count": "learned points only for 16APSK, the one modulation used",
    "demap.n0": "chain-5's frame is the first decoded one, demapped with the reset n0: needs "
                "the frame after it (n0 learned in force)",
    "demap.k": "always the reset K (K follows n0): the same input as demap.n0",
    "sync.period.peak_pos": "chain-1's slot position is fixed by its lead (1 000): needs a "
                            "set that relocks at another position (the whole-chain status "
                            "blocks have 977, 1 000 and 3 152)",
    "sync.carrier": "chain-1 ends one frame after the first frame head: one carrier update; "
                    "needs a longer stage-1 set (status.carrier varies end to end)",
}


# --- the report -------------------------------------------------------------

def _sets_of(f):
    return ", ".join(f"{s} ({n})" for s, n in f.per_set.items()) or "—"


def markdown(cov, all_fields=False):
    o = []
    o.append("# Value coverage of the chain and LDPC vector sets\n")
    o.append(f"{len(cov.sets)} sets: " + ", ".join(f"`{s}`" for s in cov.sets) + "\n")
    fl = cov.flagged()
    o.append(f"## Constant and absent fields ({len(fl)} of {len(cov.fields)}; "
             f"{len(cov.unexcused())} not excused)\n")
    o.append("| Field | Class | Value | Sets (distinct values) | Excuse |")
    o.append("|---|---|---|---|---|")
    for f in fl:
        v = _fmt(f.values[0]) if f.values else "—"
        o.append(f"| `{f.name}` | {f.label} | {v} | {_sets_of(f)} | {f.excuse or '**none**'} |")
    o.append("")
    o.append("## Boundary values never taken\n")
    o.append("| Field | Distinct | Range | Never |")
    o.append("|---|---|---|---|")
    for f in cov.fields.values():
        if f.missing_bounds and f.status != "not read":
            miss = "; ".join(f"{_fmt(v)} ({m})" for v, m in f.missing_bounds)
            o.append(f"| `{f.name}` | {f.distinct} | {_fmt(f.lo)} … {_fmt(f.hi)} | {miss} |")
    o.append("")
    o.append("## Enumerated values never seen\n")
    o.append("| Field | Seen | Never |")
    o.append("|---|---|---|")
    for f in cov.fields.values():
        if f.missing_domain and f.status != "not read":
            seen = sorted((v for v in SPECS[f.name].domain if v not in f.missing_domain), key=str)
            miss = f.missing_domain
            ms = ", ".join(map(str, miss)) if len(miss) <= 20 else f"{len(miss)} values"
            o.append(f"| `{f.name}` | {', '.join(map(str, seen)) or '—'} | {ms} |")
    o.append("")
    o.append("## Status words equal to the previous word in every block\n")
    o.append("A defect that sends word i − 1 for word i (the learned points, d53de60) "
             "passes on these.\n")
    for n, ws in cov.shift_blind.items():
        o.append(f"- payload of {n} words ({cov.status_blocks[n]} blocks): "
                 + (", ".join(f"{i} (`{w}`)" for i, w in ws) or "none"))
    if not cov.shift_blind:
        o.append("- no status block read")
    o.append("")
    wc = cov.within_constant()
    o.append("## Slots fields that never change inside a set\n")
    o.append("They vary between sets only: a value that lags by a frame or a write "
             "(taps_from, build 7) passes on every set.\n")
    o.append(", ".join(f"`{f.name}`" for f in wc) if wc else "none")
    o.append("")
    if all_fields:
        o.append("## Every field\n")
        o.append("| Field | Meaning | Distinct | Range / values | Sets (distinct values) |")
        o.append("|---|---|---|---|---|")
        for f in cov.fields.values():
            if f.status == "not read":
                continue
            vals = (f"{_fmt(f.lo)} … {_fmt(f.hi)}" if f.distinct > 8 and f.lo is not None
                    else ", ".join(_fmt(v) for v in f.values))
            o.append(f"| `{f.name}` | {f.desc} | {f.distinct} | {vals} | {_sets_of(f)} |")
        o.append("")
    return "\n".join(o)


def summary(cov):
    o = [f"{len(cov.sets)} sets, {len(cov.fields)} fields"]
    for f in cov.flagged():
        o.append(f"  {f.name:40s} {f.label:24s} {f.excuse.split(':')[0] or 'NOT EXCUSED'}")
    st = cov.stale_gaps()
    if st:
        o.append("known gaps now covered: " + ", ".join(st))
    return "\n".join(o)


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    build = args[0] if args else BUILD
    cov = coverage(build, synthetic="--synthetic" in argv, excuses="--no-excuses" not in argv)
    if "--markdown" in argv:
        print(markdown(cov, all_fields="--all-fields" in argv))
    else:
        print(summary(cov))
    return 1 if cov.unexcused() else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
