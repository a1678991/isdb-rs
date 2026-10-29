"""A plain Python model of the transmit block's arithmetic and of its DMA
side, and a writer of test vectors in the shape the Rust model
(`isdb_frontend::shape::fixed`, the reference) writes them, so the HDL
simulation runs on either. It was the HDL's stand-in reference before the
Rust vectors existed; it reproduces them exactly (banks, samples,
counters, command words: tests TxModelTest), writes two more sets, and
gives the expected samples of a restart (`run(..., restart_at=)`), which
the Rust vectors do not cover. The contract: tx.py's module
documentation, docs/plans/isdbs-fpga-tx.md §2.

python -m isdb_fpga.txmodel OUT.json [--preset isdbs|stress] [--samples N] [--seed S]

Presets: `isdbs` = ISDB-S at 40 Msps (step 0xB8B439), the bank at 10 dB
backoff, DMA stalls of 1 to 6 requests; `stress` = a step of 2**24 - 1 (a
symbol on all samples but one, the fastest the prefetch must keep up
with), SHIFT two below the bank's so that peaks saturate, a stall at the
very first word and the DMA running dry before the end.

Vectors JSON (format 1):
  {"format": 1, "design": {...},
   "params": {"step", "shift", "phases", "span", "a": [[2S] x P], "b", "id_word"},
   "gpio": [command words of a full configuration from Encoder(0): STEP,
            SHIFT, the banks' near halves, CTRL START, CTRL RUN|SELECT],
   "words": [u32: (q_word << 16) | i_word, ...],
   "stalls": [[word_index, n], ...]  (sorted; before word i is delivered,
              n requests find no word, and the next request asks for the
              same word; requests after the last word find none),
   "samples": [{"n", "frac" (before the update), "q", "line" (2S codes,
                newest first), "sum_i", "sum_q", "out_i", "out_q",
                "wrap": 0/1, "code": entered code or null}, ...],
   "counters": {"consumed", "missed", "first_miss", "samples"}}
"""

import json
import math
import os
import random
import sys

from .tx import (A_BASE, B_BASE, CTRL_RUN, CTRL_SELECT, CTRL_START, FRAC_BITS, OP_ADDR,
                 OP_CTRL, OP_HI, OP_WRITE, OP_WRITE_S, SI, SQ, STEP, check_bank, id_word)

SHAPER_PHASES = 1024  # the f32 Shaper's rows, whose gain G the bank takes


def rrc(t, a):
    """`isdb_frontend::tables::rrc`: the root-raised-cosine pulse."""
    if abs(t) < 1e-12:
        return 1 - a + 4 * a / math.pi
    x = 4 * a * t
    if abs(abs(x) - 1) < 1e-9:
        b = math.pi / (4 * a)
        return a / math.sqrt(2) * ((1 + 2 / math.pi) * math.sin(b) + (1 - 2 / math.pi) * math.cos(b))
    return (math.sin(math.pi * t * (1 - a)) + x * math.cos(math.pi * t * (1 + a))) / (
        math.pi * t * (1 - x * x))


def rnd(x):
    """Round half away from zero (Rust's f64::round)."""
    return int(math.copysign(math.floor(abs(x) + 0.5), x))


def bank(phases, span, roll_off=0.35, backoff_db=10.0):
    """(a, b, shift): rows q at (q + 1/2)/P, a = round(G*p*D*2**s),
    b = round(G*p*D*2**s/sqrt2), s the largest with max|a| <= 2**17 - 1;
    G the Shaper's gain (mean row energy 1), D = 2048*10**(-backoff/20).
    One half is computed and mirrored, so the symmetry is exact."""
    w = 2 * span

    def pulse(x):
        return rrc(x, roll_off) if abs(x) < span else 0.0

    energy = sum(pulse(q / SHAPER_PHASES + t - span) ** 2
                 for q in range(SHAPER_PHASES) for t in range(w)) / SHAPER_PHASES
    k = energy ** -0.5 * 2048 * 10 ** (-backoff_db / 20)
    half = [[k * pulse((q + 0.5) / phases + t - span) for t in range(span)] for q in range(phases)]
    peak = max(abs(c) for row in half for c in row)
    s = 0
    while rnd(peak * (1 << (s + 1))) <= (1 << 17) - 1:
        s += 1
    a = [[0] * w for _ in range(phases)]
    b = [[0] * w for _ in range(phases)]
    for q in range(phases):
        for t in range(span):
            va = rnd(half[q][t] * (1 << s))
            vb = rnd(half[q][t] * (1 << s) / math.sqrt(2))
            a[q][t] = a[phases - 1 - q][w - 1 - t] = va
            b[q][t] = b[phases - 1 - q][w - 1 - t] = vb
    return a, b, s


def round_shift(v, s):
    """`isdb_frontend::fixed::round_shift`: add half, arithmetic shift."""
    return v if s == 0 else (v + (1 << (s - 1))) >> s


class Dma:
    """The DMA side as the block sees it: one response per request, in
    order: a word, or none (underflow) while a stall lasts or after the
    last word."""

    def __init__(self, words, stalls):
        self.words = list(words)
        self.left = {}
        for i, n in stalls:
            self.left[i] = self.left.get(i, 0) + n
        self.next = 0

    def request(self):
        """The next response: the word, or None."""
        if self.left.get(self.next, 0) > 0:
            self.left[self.next] -= 1
            return None
        if self.next >= len(self.words):
            return None
        w = self.words[self.next]
        self.next += 1
        return w


def run(params, words, stalls, n, restart_at=None):
    """The block's output for n samples after START: (samples, counters)
    in the vectors' layout. With `restart_at` = r, the block first makes r
    samples, then gets START (frac, the line, the counters and the rest of
    the current word dropped; the DMA's queue as it is) and the n samples
    returned are those after it."""
    step, shift = params["step"], params["shift"]
    a, b = params["a"], params["b"]
    phases, span = params["phases"], params["span"]
    pb = phases.bit_length() - 1
    w = 2 * span
    dma = Dma(words, stalls)
    codes, miss = [], False
    for part, count in ((0, restart_at), (1, n)):
        if count is None:
            continue
        frac, line = 0, [0] * w
        codes, miss = [], False
        consumed = missed = 0
        first_miss = None
        out = []
        for i in range(count):
            q = frac >> (FRAC_BITS - pb)
            si = sq = 0
            for t, c in enumerate(line):
                if c & 8:
                    k = c & 7
                    v = b[q][t] if k & 1 else a[q][t]
                    si += SI[k] * v
                    sq += SQ[k] * v
            rec = {
                "n": i, "frac": frac, "q": q, "line": list(line), "sum_i": si, "sum_q": sq,
                "out_i": max(-2048, min(2047, round_shift(si, shift))),
                "out_q": max(-2048, min(2047, round_shift(sq, shift))),
                "wrap": 0, "code": None,
            }
            frac += step
            if frac >> FRAC_BITS:
                frac -= 1 << FRAC_BITS
                if not codes:
                    word = dma.request()
                    miss = word is None
                    codes = [0] * 8 if miss else [(word >> (4 * j)) & 15 for j in range(8)]
                code = codes.pop(0)
                if miss:  # one count per symbol as it enters
                    missed += 1
                    if first_miss is None:
                        first_miss = consumed
                else:
                    consumed += 1
                line = [code] + line[:-1]
                rec["wrap"], rec["code"] = 1, code
            out.append(rec)
    counters = {"consumed": consumed, "missed": missed,
                "first_miss": first_miss or 0, "samples": n}
    return out, counters


class Encoder:
    """The host's command words (`isdb_frontend::control::Encoder`'s
    protocol): toggle bit 31, op in 30..28, argument in 27..1, bit 0 kept."""

    def __init__(self, current=0):
        self.toggle = current >> 31 & 1
        self.staged = None
        self.bit0 = current & 1

    def word(self, op, arg):
        self.toggle ^= 1
        return self.toggle << 31 | op << 28 | (arg & 0x07FFFFFF) << 1 | self.bit0

    def write(self, addr, values):
        out = [self.word(OP_ADDR, addr)]
        for v in values:
            v &= 0xFFFFFFFF
            sv = v - (1 << 32) if v >> 31 else v
            if -(1 << 26) <= sv < 1 << 26:
                out.append(self.word(OP_WRITE_S, v & 0x07FFFFFF))
                continue
            hi = v >> 16
            if self.staged != hi:
                out.append(self.word(OP_HI, hi))
                self.staged = hi
            out.append(self.word(OP_WRITE, v & 0xFFFF))
        return out

    def configure_tx(self, params, banks=True):
        """`Encoder::configure_tx`'s words: STEP and SHIFT, the banks
        (each row's near half, t < S, which is the whole symmetric bank),
        CTRL START, CTRL RUN|SELECT."""
        out = self.write(STEP, [params["step"], params["shift"]])
        if banks:
            span = params["span"]
            for base, bk in ((A_BASE, params["a"]), (B_BASE, params["b"])):
                for q, row in enumerate(bk):
                    out += self.write(base + q * 32, row[:span])
        out.append(self.word(OP_CTRL, CTRL_START))
        out.append(self.word(OP_CTRL, CTRL_RUN | CTRL_SELECT))
        return out


def random_codes(rng, n):
    """Mostly points (8 + k), some zero symbols and reserved codes."""
    out = []
    for _ in range(n):
        r = rng.random()
        out.append(rng.randrange(8) + 8 if r < 0.9 else (0 if r < 0.95 else rng.randrange(1, 8)))
    return out


def pack(codes):
    """8 codes per DMA word, code 0 in bits 3..0 of the I word (low half)."""
    return [sum(c << (4 * j) for j, c in enumerate(codes[i:i + 8])) for i in range(0, len(codes), 8)]


PRESETS = {
    # ISDB-S at 40 Msps; stalls of 1 to 6 requests, two of them adjacent
    "isdbs": dict(sample_rate=40e6, symbol_rate=28.86e6, shift_delta=0, words=300,
                  stalls=[[3, 1], [40, 2], [41, 1], [97, 6], [180, 3]]),
    # a symbol on nearly every sample, saturating peaks, a stall at word 0,
    # the DMA dry after 340 words (the samples need about 375)
    "stress": dict(step=(1 << FRAC_BITS) - 1, shift_delta=-2, words=340,
                   stalls=[[0, 2], [17, 1], [200, 4]]),
}


def vectors(preset="isdbs", n=3000, seed=1, phases=512, span=12, roll_off=0.35, backoff_db=10.0):
    cfg = PRESETS[preset]
    rng = random.Random(seed)
    a, b, s = bank(phases, span, roll_off, backoff_db)
    if "step" in cfg:
        step = cfg["step"]
        design = {"preset": preset, "step": step}
    else:
        step = rnd((1 << FRAC_BITS) * cfg["symbol_rate"] / cfg["sample_rate"])
        design = {"preset": preset, "sample_rate": cfg["sample_rate"],
                  "symbol_rate": cfg["symbol_rate"]}
    design.update(roll_off=roll_off, span=span, phases=phases, backoff_db=backoff_db,
                  model="fpga/isdb_fpga/txmodel.py")
    params = {"step": step, "shift": s + cfg["shift_delta"], "phases": phases, "span": span,
              "a": a, "b": b, "id_word": id_word(phases, span)}
    check_bank("a", a, phases, span)
    check_bank("b", b, phases, span)
    words = pack(random_codes(rng, 8 * cfg["words"]))
    stalls = cfg["stalls"]
    samples, counters = run(params, words, stalls, n)
    return {"format": 1, "design": design, "params": params,
            "gpio": Encoder(0).configure_tx(params), "words": words, "stalls": stalls,
            "samples": samples, "counters": counters}


def main(argv):
    path = argv[1]
    preset = argv[argv.index("--preset") + 1] if "--preset" in argv else "isdbs"
    n = int(argv[argv.index("--samples") + 1]) if "--samples" in argv else 3000
    seed = int(argv[argv.index("--seed") + 1]) if "--seed" in argv else 1
    doc = vectors(preset, n, seed)
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as f:
        json.dump(doc, f)
    c = doc["counters"]
    sat = sum(1 for r in doc["samples"] if {r["out_i"], r["out_q"]} & {2047, -2048})
    print(f"{path}: {preset}, {n} samples, step {doc['params']['step']:#x}, shift "
          f"{doc['params']['shift']}, {len(doc['words'])} words, {len(doc['gpio'])} commands; "
          f"consumed {c['consumed']}, missed {c['missed']}, first miss {c['first_miss']}, "
          f"{sat} saturated samples")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
