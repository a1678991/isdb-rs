"""A model of the LDPC decoder's codeword queue over frames (ldpc.py,
"The codeword queue"): K banks, each held by a codeword from its `begin`
to the end of its output stream; one engine decoding the committed
codewords in commit order; the results leaving in commit order, a dropped
codeword's as an overrun record. It answers the question the iteration
limit raises: with a limit L and K banks, which frames' iteration counts
fit without an overrun.

Timing (cycles of the decoder's clock f):
- the frame is F = 33.05 ms; slot codeword s (0 .. 119) loads across its
  slot, [s F / 120, (s + 1) F / 120): `begin` at the slot's start (the
  interleaver spreads a codeword over the whole slot), `start` at its end;
- the TMCC codeword, `tmcc`:
  "staged" (the default, the receive chain's choice): the TMCC stage
  buffers its soft bits across the frame and loads them into a bank as a
  burst after the frame's end: `begin` at the frame's end, `start`
  burst cycles later (31 680 writes sharing the input port with the next
  frame's slot LLRs, ≈ 0.81 of the cycles at 200 MHz for 120 slots of
  44 880 at 30.26 frames/s: burst ≈ 31 680 / 0.19 ≈ 167k cycles);
  "interleaved": its LLRs arrive with the slots', so it holds a bank
  from the frame's start to its end;
- decoding a codeword takes fill + (iterations + 1) passes of the rate's
  cycles per pass (`ldpcsched.gaps`) + a drain; a codeword at the limit
  without converging takes limit + 1 passes; its result then waits for
  the output stream (n / 16 words) of the codeword before it, and its
  bank is freed when its own stream ends; the engine is held until its
  result can leave (ldpc.py's DRAIN).

`finish_hz` (the chain's symbol clock, 40 MHz; None: the decoder's own
rate, the first model): the stream leaves at one 16-bit word per symbol
clock (the bridge's FIFO into the chain's finish stage, which takes one
per clock), and the next codeword's stream waits until the finish stage
has sent the slot's block (`FINISH_FORMAT` symbol clocks: the BCH check,
the 3 header words and 2 clocks per payload word for 93/120, up to three
dummy blocks), so a bank is held about 7 200 symbol clocks longer than
the decoder's own output time. The TMCC burst then also runs at the
chain's pace: four values per beat on the port cycles the demapper leaves
(tmcc.py), 18.5 % of the symbol clock at the front end's 33.76 Msymbol/s
(`CHAIN_FREE`): 1.07 ms, 3.9 slots. `result_fifo`: the chain's result
FIFO (chain/results.py) takes the next stream while the finish stage
formats the slot block (no gap), as long as the stage keeps up on average.
"""

import math

from . import ldpcmodel, ldpcsched
from .ldpc import DECIDE, WRITE_LATENCY  # the HDL's pipeline (synth/ldpc_latency.py)

FRAME_S = 33.05e-3
SLOTS = 120
TMCC_SENT = 31_680
DRAIN = 40  # the write pipeline's drain and the result's hand-over, about
FINISH_FORMAT = 20 + 3 + 2 * 2174 + 9  # finish.py, a 93/120 slot block
BURST_LANES = 4  # tmcc.py: TMCC values per port beat
CHAIN_FREE = 1 - 33.7561e6 / 40e6 * 8976 / 9296  # the port cycles the demapper leaves


def passes_cycles(rate, s_units=34, tables=None):
    """Cycles per pass of a rate at S units (the schedule's steady state)."""
    tables = tables or ldpcmodel.load_tables()
    lay = ldpcsched.Layout(tables[rate], ldpcmodel.N // ldpcmodel.GROUP, s_units)
    return ldpcsched.gaps(lay, WRITE_LATENCY, DECIDE)[1]


def measured_frame(seed=1, long_at=(17, 61, 103), long_iter=16):
    """120 slot codewords' iteration counts like the lab signal's (mean
    2.1 with the long ones: mostly 2, many 1, some 3, a few 4), with codewords that run to
    `long_iter` at the slots `long_at` (isolated)."""
    rng = ldpcmodel.Rng(seed)
    out = []
    for _ in range(SLOTS):
        r = rng.bits() % 100
        out.append(1 if r < 32 else 2 if r < 90 else 3 if r < 99 else 4)
    for s in long_at:
        out[s] = long_iter
    return out


def run(frames, banks=3, limit=16, rate=93, s_units=34, clock_hz=200e6, tmcc="staged",
        tmcc_iter=2, cycles=None, tmcc_cycles=None, finish_hz=None, result_fifo=False):
    """Simulate `frames` (a list of frames, each a list of 120 slot
    codewords' iteration counts; a count above `limit` means not
    converged at the limit). Returns {"records": [(frame, "slot" s |
    "tmcc", "decoded" | "overrun")] in result order, "overruns",
    "max_busy" (the engine's longest stretch), "passes" (mean per decoded
    codeword)}."""
    F = FRAME_S * clock_hz
    T = F / SLOTS
    n = ldpcmodel.N
    out_cycles = math.ceil(n / 16) + 8
    out_gap = 0.0
    if finish_hz:
        out_cycles = (math.ceil(n / 16) + 8) * clock_hz / finish_hz
        out_gap = 0.0 if result_fifo else FINISH_FORMAT * clock_hz / finish_hz
    cpp = cycles or passes_cycles(rate, s_units)
    cpp_t = tmcc_cycles or passes_cycles(61, s_units)
    fill_t = math.ceil(ldpcmodel.TMCC_SHORT[0] / s_units) + math.ceil(
        (ldpcmodel.GROUP * 61 - sum(ldpcmodel.TMCC_SHORT)) / s_units)
    burst = TMCC_SENT / (1 - SLOTS * n * (1 / FRAME_S) / clock_hz)
    if finish_hz:
        burst = TMCC_SENT / BURST_LANES / (CHAIN_FREE * finish_hz) * clock_hz
    cws = []  # (begin, commit, decode cycles, label)
    for fi, frame in enumerate(frames):
        t0 = fi * F
        for s, it in enumerate(frame):
            p = min(it, limit) + 1
            cws.append((t0 + s * T, t0 + (s + 1) * T, p * cpp + DRAIN, (fi, f"slot {s}")))
        p = min(tmcc_iter, limit) + 1
        dec = fill_t + p * cpp_t + DRAIN
        if tmcc == "staged":
            cws.append((t0 + F, t0 + F + burst, dec, (fi, "tmcc")))
        else:
            cws.append((t0, t0 + F, dec, (fi, "tmcc")))
    # bank allocation in begin order, results in commit order
    by_begin = sorted(range(len(cws)), key=lambda i: cws[i][0])
    by_commit = sorted(range(len(cws)), key=lambda i: cws[i][1])

    held = []  # (free time or None, codeword) of the banks in use
    dropped = set()
    free_at = {}
    engine = 0.0
    out_free = 0.0
    done = 0  # commit-order index up to which free times are known

    def settle(t):
        """Schedule, in commit order, the accepted codewords committed by t."""
        nonlocal engine, out_free, done
        while done < len(by_commit) and cws[by_commit[done]][1] <= t:
            i = by_commit[done]
            done += 1
            if i in dropped:
                continue
            start = max(cws[i][1], engine)
            end = start + cws[i][2]
            leave = max(end, out_free)
            free_at[i] = leave + out_cycles
            out_free = leave + out_cycles + out_gap
            engine = leave

    busy = 0.0
    for i in by_begin:
        t = cws[i][0]
        settle(t)
        in_use = [j for j in held if free_at.get(j, math.inf) > t]
        held = in_use
        if len(held) >= banks:
            dropped.add(i)
        else:
            held.append(i)
    settle(math.inf)
    for i in by_commit:
        if i not in dropped:
            busy = max(busy, cws[i][2])
    records = [(cws[i][3][0], cws[i][3][1], "overrun" if i in dropped else "decoded")
               for i in by_commit]
    decoded = [i for i in by_commit if i not in dropped]

    return {"records": records, "overruns": len(dropped), "max_busy": busy,
            "passes": sum((cws[i][2] - DRAIN) / cpp for i in decoded) / max(len(decoded), 1),
            "frame_cycles": F, "slot_cycles": T, "cycles_per_pass": cpp}


def budget(rate=93, s_units=34, clock_hz=200e6):
    """The frame-average passes per codeword: F / (121 x cycles per pass)."""
    return FRAME_S * clock_hz / ((SLOTS + 1) * passes_cycles(rate, s_units))
