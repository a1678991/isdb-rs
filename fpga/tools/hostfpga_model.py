#!/usr/bin/env python3
"""A timed host-FPGA interaction model of the ISDB-S3 chain (`SlotsRx`) and
its host (issue #30 item 4).

A discrete-event simulation, standard library only and seeded, of what the
board's defects of builds 6-8 were made of: the order and the timing of
register accesses over iiod against the chain's frame clock. It has no
signal and computes no taps: it checks *when* values are read, whether a
multi-word read is one snapshot, whether a read returns the register it
asked for, and how long each host sequence waits.

What it simulates:

- **The FPGA** (`Fpga`): the chain's frame clock (33.05 ms, frame 0 a fixed
  time after START, frames 0 and 1 without a layout at lag 2), its register
  file as `top.py`'s docstring defines it (`REGISTERS`: which registers are
  constant, live, latched per frame, held), the command protocol on
  `gpio_out` / `gpio_in` (ADDR, HI, WRITE / WRITE_S, READ, NOP, CTRL), the
  training sums latched at each decoded frame's end, SUMS_CAPTURE's arm /
  latch / hold / release and its SEQ, TAPS / TAPS_FROM (in force from frame
  N + 2, or the next frame start), START, and the `Slots` stream: a decoded
  frame's slot blocks spread over the frame, its TMCC, status and (format 2)
  sums blocks a few slots into the next frame, cut into DMA buffers.
- **`direct_reg_access`** (`Device`): one cached register address per IIO
  device (the kernel's `cached_reg_addr`), set by every write to the
  attribute, an address alone or an address and a value; a read returns the
  register at the cached address. libiio 0.x reads a register as two iiod
  requests (the address written, then the value read), so another client's
  address write between them makes the read return the other register
  (#27, #29). Every value read carries the register it came from, so the
  model counts foreign values exactly (the ground truth the board's
  diagnostic double read approximated).
- **iiod clients** (`Client`): each request takes `iiod_s` of the board's
  processing and `wire_s` of network round trip and client turnaround; a
  write is applied at the end of its processing, a read samples at the start
  of its processing, so the window in which another client's address write
  hits a read is `wire_s` (from the address write's apply to the value
  read's sample). A local client on the board (`iio_attr` without `-u`) has
  its own short window.
- **Clients**: the host's slow loop (its own libiio context: the register
  path as `crates/isdb-radio/src/chain.rs` `SlowLoop::tick_at` does it, with
  the read discipline of each host generation: single reads (builds 6-8),
  the diagnostic double read of #27 / #29, or the verified reads of PR #31;
  or the stream path, `tick_stream`, with its fallback to the register path
  when the sums blocks stop while frames decode, `stream_stalled`), the
  host's capture context (the open,
  the per-buffer ADC overflow poll of builds 6-8, the counters at the end),
  the consumer of the `Slots` words (`ChainHealth`: the status and sums
  blocks), Tezuka's `api_controller.sh` polling 0x80000088 every 3.4-4.2 s
  and #29 E2's 5 Hz reader of 0x80000000.

Parameters (`Profile`, `Image`, `Host`, `Board`, `Signal`): every measured
number is a field with its source; change one and rerun. Two network
profiles: `BENCH` (this project's bench: 308 single reads in about 0.47 s,
build 7) and `ISSUE29` (the reporter's board: a 308-word set with the
diagnostic double read in 1.3-1.5 s). The exposed window `wire_s` of each is
fitted to one measurement and then checked against the others (fpga/README.md
"A timed host–FPGA model").

Run: `python3 -I fpga/tools/hostfpga_model.py` (every scenario, a few
seconds; scenario names to pick some, `--list`, `--seeds N --mean`, `--json`,
`--set SECTION.FIELD=VALUE` to change a parameter, `--sweep
SECTION.FIELD=V1,V2 --fields ...` to run several values). The tests are
`fpga/tests/test_hostfpga_model.py` (in `mise run fpga:test`).

What it does not model: the signal and the numerical solve (a set with a
foreign word is counted as solved from a wrong word; the host's sum checks
and gate, which catch most such sets, act on values the model does not
compute); n0 and the revert's quality rules (a written set is accepted once
ten status blocks show it); the decoder and its banks; iiod's server
contention between clients; Ethernet loss; TCP's buffering of the stream
beyond a fixed transfer time.
"""

from __future__ import annotations

import argparse
import dataclasses
import heapq
import json
import math
import random
import sys
from dataclasses import dataclass, field, replace

# --------------------------------------------------------------------------
# The register map: `fpga/isdb_fpga/chain/top.py`'s docstring and
# `isdb_frontend::control::chain` (fixed there from the HDL). Kinds:
#   const     never changes after configuration
#   config    written by the host, reads back what it wrote (or its effect)
#   live      changes on its own between any two reads
#   frame     a snapshot updated once per frame (stable within a frame)
#   latched   the training sums' latch: per frame, or held by SUMS_CAPTURE
#   version   identifies the latch (SUMS_FRAME, SUMS_CAPTURE's SEQ)
# --------------------------------------------------------------------------

GPIO_OUT = 0x800000BC  # cf-ad9361-lpc up_adc_gpio_out: the command words
GPIO_IN = 0x800000B8  # up_adc_gpio_in: the ID word or the READ's readback
ADC_STATUS = 0x80000088  # the ADC core's status (bit 2 overflow, W1C)
ADC_VERSION = 0x80000000  # the ADC core's VERSION (#29 E2: 0xA0300)
ADC_VERSION_VALUE = 0xA0300

NONE = 0xFFFFFFFF

VERSION, FEATURES, CARRIER, LAYOUT_LAG, INIT, STATUS, FRAME, SEQ = (
    0x400, 0x401, 0x402, 0x403, 0x404, 0x405, 0x406, 0x407)
N0, LLR_SCALE, LEARNED_FROM, CONST_MASK, MAX_ITER, AGC = 0x408, 0x409, 0x40A, 0x40B, 0x40C, 0x40D
TAPS, TAPS_FROM = 0x410, 0x41B
SUMS_FRAME, SUMS_COUNT, SUMS_CAPTURE = 0x420, 0x421, 0x422
COUNTERS = 0x430  # .. 0x437
TMCC_OVERRUNS, DECODER, RECLAIMED = 0x438, 0x439, 0x43A
SUMS_R, SUMS_P = 0x500, 0x610
FE_KP, FE_STATUS = 0x005, 0x010  # front end registers the open reads

N_TAPS = 11
R_ENTRIES = N_TAPS * (N_TAPS + 1) // 2  # 66
SUM_WORDS = 4 * (R_ENTRIES + N_TAPS)  # 308
SUM_REGS = [SUMS_R + i for i in range(4 * R_ENTRIES)] + [SUMS_P + i for i in range(4 * N_TAPS)]

# FEATURES bits (top.py)
F_TAPS, F_SUMS, F_SUMS_CAPTURE, F_DECODER_HEALTH, F_SUMS_SEQ, F_SUMS_STREAM = 1, 2, 16, 32, 64, 128
# SUMS_CAPTURE read bits
CAP_VALID, CAP_ARMED, CAP_NO_LAYOUT = 1, 2, 4
# the bits of SUMS_CAPTURE that identify a held capture (chain.rs
# HELD_EXTRA): SEQ, VALID and ARMED; NO_LAYOUT follows the layout while a
# capture holds and is not compared
CAP_HELD_BITS = 0xFF00 | CAP_VALID | CAP_ARMED
# chain STATUS bits
ST_LOCKED, ST_FRAMED, ST_DECODING = 1, 2, 4
# command ops (isdb_frontend::control::Op) and CTRL bits
OP_NOP, OP_ADDR, OP_HI, OP_WRITE, OP_READ, OP_CTRL, OP_WRITE_S = range(7)
CTRL_RUN, CTRL_START, CTRL_SYMBOLS, CTRL_SLOTS = 1, 2, 4, 8

# name, first address, words, kind, what the host does with it
REGISTERS = [
    ("VERSION", VERSION, 1, "const", "read at open"),
    ("FEATURES", FEATURES, 1, "const", "read at open and at the slow loop's first tick"),
    ("CARRIER", CARRIER, 1, "live", "read every register-path tick, reported only"),
    ("LAYOUT_LAG", LAYOUT_LAG, 1, "config", "written at open"),
    ("INIT", INIT, 1, "config", "written at open"),
    ("STATUS", STATUS, 1, "live", "read every register-path tick: the recovery"),
    ("FRAME", FRAME, 1, "frame", "read every register-path tick: the identity's TAPS_FROM"),
    ("SEQ", SEQ, 1, "live", "-"),
    ("N0", N0, 1, "frame", "-"),
    ("LLR_SCALE", LLR_SCALE, 1, "frame", "-"),
    ("LEARNED_FROM", LEARNED_FROM, 1, "frame", "-"),
    ("CONST_MASK", CONST_MASK, 1, "frame", "-"),
    ("MAX_ITER", MAX_ITER, 1, "config", "-"),
    ("AGC", AGC, 1, "config", "-"),
    ("TAPS", TAPS, N_TAPS, "config", "written by the slow loop (and at open)"),
    ("TAPS_FROM", TAPS_FROM, 1, "frame", "written after TAPS; read every register-path tick"),
    ("SUMS_FRAME", SUMS_FRAME, 1, "version", "read before and after the 308 sum words"),
    ("SUMS_COUNT", SUMS_COUNT, 1, "latched", "read with SUMS_FRAME"),
    ("SUMS_CAPTURE", SUMS_CAPTURE, 1, "version", "written to arm; read before and after the sum words"),
    ("COUNTERS", COUNTERS, 8, "live", "read at the end of a capture"),
    ("TMCC_OVERRUNS", TMCC_OVERRUNS, 1, "live", "-"),
    ("DECODER", DECODER, 1, "live", "read at open (a snapshot under 1 us old)"),
    ("RECLAIMED", RECLAIMED, 1, "live", "read at open"),
    ("SUMS_R", SUMS_R, 4 * R_ENTRIES, "latched", "read by the register path's slow loop"),
    ("SUMS_P", SUMS_P, 4 * N_TAPS, "latched", "read by the register path's slow loop"),
]

# The values the host combines from several words, and what makes the
# combination one value (`check_groups`). `stable`: what keeps the words
# still while they are read; `version`: the registers read before and after
# that must agree.
GROUPS = [
    dict(name="training sums", regs=["SUMS_FRAME", "SUMS_COUNT", "SUMS_R", "SUMS_P"],
         stable="latch", version=["SUMS_FRAME", "SUMS_CAPTURE"]),
    dict(name="COUNTERS iterations (low, high)", regs=["COUNTERS"], stable="live", version=[],
         words=2),
]

# Groups accepted without a snapshot or version, with the reason.
GROUPS_ALLOWED = {
    "COUNTERS iterations (low, high)":
        "a 64-bit live count read as two words at the end of a capture, no snapshot: torn only "
        "when the low word carries between the two reads, once per 2^32 iterations (about 8 days "
        "of decoding at 6 000 iterations/s), and the counters are a report, not an input",
}


# --------------------------------------------------------------------------
# Parameters
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Profile:
    """The network and iiod timing of one bench."""

    name: str
    # one iiod request (an attribute write or read), sent to answered
    request_s: float
    # of which the network round trip and the client's turnaround: from the
    # board's apply of a read's address write to its sample of the value
    # (a write applies at the end of iiod's processing, a read samples at
    # its start), the window in which another client's address write makes
    # the read return another register
    wire_s: float
    # each request's time scaled by U(1 - j, 1 + j)
    jitter: float = 0.2

    @property
    def iiod_s(self):
        """The board's processing of a request (not exposed)."""
        return self.request_s - self.wire_s


# This project's bench (BS7, a wired host): build 7's 308 single reads (four
# requests a word) took about 15 frames, its first write 0.90 s after the
# open: 1 232 requests in about 0.47 s, 0.38 ms each. `wire_s` fitted to the
# ADC overflow flags of builds 6 and 7 (27 and 8 in 30 s, none a lost block:
# the host's own poll misreading `gpio_in` / `gpio_out`; the model gives 25
# and 9 over 8 seeds), checked against build 9's one reread in 65 201 reads
# with `api_controller.sh` running (the model: 2.6 on average).
BENCH = Profile("bench", request_s=0.38e-3, wire_s=0.10e-3)
# The reporter's board (#27, #29): a 308-word set with the diagnostic double
# read (six requests a word) in 1.3-1.5 s: 0.75 ms a request. `wire_s`
# fitted to #29 E2 (a 5 Hz board-side reader of 0x80000000: 34 of 50 sets,
# 52 words), checked against #27 run B and #29 C, E1, E3.
ISSUE29 = Profile("issue29", request_s=0.75e-3, wire_s=0.36e-3)
# A host on the board itself (no network): the old HDL's transient 0.
LOCAL = Profile("local", request_s=16e-6, wire_s=4e-6)


@dataclass(frozen=True)
class Image:
    """The bitstream: what `SlotsRx` offers."""

    name: str
    version: int = 1  # the `Slots` format (2: the sums block)
    capture: bool = True  # FEATURES bit 4 SUMS_CAPTURE (build 7 on)
    seq: bool = True  # bit 6 SUMS_SEQ (build 9 on)
    stream: bool = False  # bit 7 SUMS_STREAM (format 2)
    decoder_health: bool = True  # bit 5
    # a sum READ shows 0 until the MAC unit is idle, up to 4 400 symbol
    # clocks (builds 1-8); else 3 symbol clocks, the readback keeping the
    # last value (build 9 on)
    old_sum_read: bool = False
    # a restart (START) releases a SUMS_CAPTURE hold and an armed capture
    # (dedbc14, with format 2)
    start_releases_hold: bool = True

    @property
    def features(self):
        f = F_TAPS | F_SUMS | (14 << 8) | (34 << 16)
        f |= F_SUMS_CAPTURE if self.capture else 0
        f |= F_DECODER_HEALTH if self.decoder_health else 0
        f |= F_SUMS_SEQ if self.seq else 0
        f |= F_SUMS_STREAM if self.stream else 0
        return f


BUILD6 = Image("build 6", capture=False, seq=False, decoder_health=False, old_sum_read=True,
               start_releases_hold=False)
BUILD7 = Image("build 7", seq=False, decoder_health=False, old_sum_read=True, start_releases_hold=False)
BUILD8 = Image("build 8", seq=False, decoder_health=False, old_sum_read=True, start_releases_hold=False)
BUILD9 = Image("build 9", start_releases_hold=False)
FORMAT2 = Image("format 2", version=2, stream=True)


@dataclass(frozen=True)
class Host:
    """The host binary: `crates/isdb-radio/src/chain.rs` and the Pluto
    backend's capture path, by generation."""

    name: str
    # 'single' (ADDR, READ, one gpio_in read: builds 6-8), 'diag' (two
    # gpio_in reads, the first used, a difference counted: #27 runs B, C and
    # #29), 'verified' (gpio_in until two reads in a row agree, at most
    # MAX_READS: PR #31)
    reads: str = "verified"
    # 'registers' or 'stream' (an image with SUMS_STREAM and a consumer)
    sums_source: str = "registers"
    # the capture context polls (and clears) the ADC overflow bit after
    # every DMA buffer (builds 6-8; off on the Slots path since PR #31)
    overflow_poll: bool = False
    # words per libiio buffer (262 144 then; 65 536 since #28)
    buffer_words: int = 65536
    # the consumer reports the status blocks to the slow loop (the revert
    # watch; build 9's host on)
    health: bool = True
    max_reads: int = 8
    poll_s: float = 0.200  # SlowLoopOptions::poll
    every_s: float = 2.0  # SlowLoopOptions::every
    recover_after_s: float = 1.5  # SlowLoopOptions::recover_after
    stream_stale_s: float = 0.5  # STREAM_STALE
    wake_s: float = 0.005  # spawn_slow_loop's sleep step
    revert_frames: int = 10  # Revert::frames
    revert_timeout_s: float = 5.0  # Revert::timeout
    # the stream path: decoded frames without a sums block before the sums
    # are read from the registers (SlowLoopOptions::sums_stall), and the
    # blocks between two ticks that end the fallback (STREAM_BACK_BLOCKS)
    sums_stall_frames: int = 90
    stream_back_blocks: int = 3
    # command words of the 11 taps and TAPS_FROM (ADDR, HI + WRITE per tap
    # whose imaginary part is negative, ADDR, WRITE_S)
    taps_words: int = 25
    # the front end's configuration at open (Encoder::configure)
    config_words: int = 400
    # a wrong word that moves its 48-bit entry by this much is a bad set
    # (R's diagonal is about 6.6e9: #27, a 2^32 error in p[k] moves tap k
    # by about 0.65); smaller errors move the taps by a few per cent
    bad_error: float = 1e9


HOST_BUILD8 = Host("build 6-8 host", reads="single", overflow_poll=True, buffer_words=262144,
                   health=False)
HOST_DIAG = replace(HOST_BUILD8, name="#27 diagnostic host", reads="diag")
HOST_VERIFIED = Host("build 9 host (verified reads)")
HOST_STREAM = Host("format 2 host (stream path)", sums_source="stream")


@dataclass(frozen=True)
class Board:
    """Other clients of the board's `cf-ad9361-lpc` `direct_reg_access`."""

    # Tezuka's api_controller.sh: reads 0x80000088 once a pass, a pass every
    # 3.4-4.2 s (#29)
    api_controller: bool = False
    api_period_s: tuple = (3.4, 4.2)
    # #29 E2's positive control: `iio_attr ... 0x80000000` at this rate
    version_reader_hz: float = 0.0
    # a local iio_attr's address write to its read
    local_window_s: float = 30e-6


@dataclass(frozen=True)
class Signal:
    """The chain's timing and the frames it decodes."""

    frame_s: float = 0.03305  # one ISDB-S3 frame, 120 slots
    slots: int = 120
    # START to frame 0's start (lock and the first frame head): fitted to
    # build 7, whose first write 0.90 s after the open (solved from frame 5)
    # was in force from frame 26: frame 0 began 0.041-0.074 s after START
    lock_s: float = 0.055
    layout_lag: int = 2
    # frames without a layout (TMCC failed): status blocks, nothing decoded
    no_layout: tuple = ()
    # `Slots` words of a decoded frame (120 slot blocks, 16APSK 93/120)
    decoded_words: int = 210000
    tmcc_words: int = 604
    status_words: int = 143  # 79 + 4 * 2^4 (16APSK learned)
    sums_words: int = 476
    # a frame's TMCC, status and sums blocks leave this many slots into the
    # next frame (the TMCC burst and decode); its slot blocks a slot late
    block_delay_slots: float = 4.5
    slot_delay_slots: float = 1.0
    # a DMA buffer's transfer to the host and the consumer's parse
    buffer_net_s: float = 1.5e-3
    consumer_s: float = 0.5e-3
    stream_start_s: float = 5e-3  # the open's end to the DMA running


@dataclass(frozen=True)
class Scenario:
    name: str
    profile: Profile
    image: Image
    host: Host
    board: Board = Board()
    signal: Signal = Signal()
    seconds: float = 30.0
    # SUMS_CAPTURE as a previous run left it: 'free' (every frame end
    # latches), 'armed' or 'held'
    initial_capture: str = "free"
    # another client writes SUMS_CAPTURE at these times (a second host)
    foreign_capture_at: tuple = ()
    # the chain restarts at these times (another client's START, or a lost
    # sync): a hold and an armed capture released
    restart_at: tuple = ()
    note: str = ""


# --------------------------------------------------------------------------
# The event loop: processes are generators yielding delays (seconds).
# --------------------------------------------------------------------------


class Sim:
    def __init__(self, seed):
        self.now = 0.0
        self.q = []
        self.n = 0
        self.rng = random.Random(seed)
        self.end = math.inf

    def at(self, t, fn, *args):
        heapq.heappush(self.q, (t, self.n, fn, args))
        self.n += 1

    def spawn(self, gen, delay=0.0):
        self.at(self.now + delay, self._step, gen)

    def _step(self, gen):
        # resume until the process yields past the next queued event (an
        # exact shortcut: no event can happen in between)
        while True:
            try:
                d = next(gen)
            except StopIteration:
                return
            t = self.now + d
            if (self.q and self.q[0][0] <= t) or t > self.end:
                self.at(t, self._step, gen)
                return
            self.now = t

    def run(self, until):
        self.end = until
        q = self.q
        while q and q[0][0] <= until:
            t, _, fn, args = heapq.heappop(q)
            self.now = t
            fn(*args)
        self.now = until


# --------------------------------------------------------------------------
# The FPGA: frame clock, register file, command protocol, `Slots` stream.
# --------------------------------------------------------------------------


def _base_sums():
    """A synthetic latch's 77 complex entries, shaped as the board's (R's
    diagonal real about 6.6e9 with an imaginary part of exactly 0, the rest
    small of both signs, p's centre large): about 28 % of the 308 words are
    0 (the imaginary diagonal, the high words of small positive entries), so
    a foreign 0 is invisible there, as on the board."""
    rng = random.Random(30)
    entries = []
    for r in range(N_TAPS):
        for c in range(r, N_TAPS):
            if r == c:
                entries.append((6.6e9, 0.0))
            else:
                s = 5e8 if c - r == 1 else 2e8
                entries.append((rng.gauss(0, s), rng.gauss(0, s)))
    for k in range(N_TAPS):
        entries.append((3.9e9, rng.gauss(0, 1e7)) if k == N_TAPS // 2 else
                       (rng.gauss(0, 2e8), rng.gauss(0, 2e8)))
    return entries


BASE_SUMS = _base_sums()


def _sext32(w):
    return w - (1 << 32) if w & 0x80000000 else w


def entry_error(i, got, true):
    """How far a wrong word `got` (for `true`) moves its 48-bit entry: word
    i of the 308 (re low, re high, im low, im high per entry)."""
    if i % 2 == 0:
        return abs(got - true)
    return abs(_sext32(got) - _sext32(true)) * (1 << 32)


def _words(v):
    v = int(round(v))
    lo = v & 0xFFFFFFFF
    hi = (v >> 32) & 0xFFFF
    hi = hi | 0xFFFF0000 if hi & 0x8000 else hi
    return lo, hi


class Fpga:
    def __init__(self, sim, sc, stats):
        self.sim, self.sc, self.st = sim, sc, stats
        self.sig, self.img = sc.signal, sc.image
        self.frame_s = self.sig.frame_s
        # the command decoder
        self.addr = 0
        self.staged = 0
        self.gpio_out = 0
        self.readback = 0x15DB4000 | (self.img.version << 8)  # the ID word
        self.readback_at = None  # (time the value appears, value): old HDL
        self.last_transient = False
        self.selected = False
        self.dma = False
        # the chain
        self.t0 = None  # frame 0's start (None: not started)
        self.gen = 0  # restart generation (pending frame events)
        self.last_frame = NONE
        self.last_decoded = False
        self.regs = {LAYOUT_LAG: 2, INIT: 0, MAX_ITER: 14, CARRIER: 0}
        self.taps_written = [0] * N_TAPS
        self.taps_from = NONE  # of the taps in force
        self.pending = None  # TAPS_FROM N written, not in force yet
        self.capture = sc.initial_capture  # 'free' | 'armed' | 'held'
        self.latched = NONE
        self.latch_seq = 0
        self.sums_cache = {}
        self.commits = []  # (frame in force from, TAPS_FROM, write time)
        self.taps_from_of = {}  # frame -> taps_from in force (status blocks)
        # the stream
        self.words = 0  # words into the DMA since it started
        self.next_boundary = 0
        self.buffer_index = 0
        self.markers = []  # (word position, kind, payload)
        self.on_buffer = None  # host callback(buffer markers)

    # -- the frame clock --
    def frame_start(self, k):
        return self.t0 + k * self.frame_s

    def frame_at(self, t):
        """The frame in progress at `t` (-1 before frame 0)."""
        if self.t0 is None or t < self.t0:
            return -1
        return int((t - self.t0) / self.frame_s)

    def decoded(self, k):
        return k >= self.sig.layout_lag and k not in self._no_layout

    def start(self):
        """CTRL START: the chain restarts (frame 0 after the lock), the
        taps last written in use with INIT bit 0, an armed set dropped."""
        now = self.sim.now
        self.gen += 1
        self.t0 = now + self.sig.lock_s
        self._no_layout = set()
        for a, b in self.sig.no_layout:
            self._no_layout.update(range(a, b + 1))
        self.last_frame = NONE
        self.last_decoded = False
        self.pending = None
        if self.regs.get(INIT, 0) & 1:
            self.taps_from = NONE
        if self.img.start_releases_hold:
            self.capture = "free"
        self.sim.at(self.frame_start(1), self._frame_end, 0, self.gen)

    def _frame_end(self, k, gen):
        """Frame k ends (and k + 1 starts)."""
        if gen != self.gen:
            return
        now = self.sim.now
        dec = self.decoded(k)
        latched = False
        if dec and self.capture in ("free", "armed"):
            self.latched = k
            self.latch_seq = (self.latch_seq + 1) & 0xFF
            latched = True
            if self.capture == "armed":
                self.capture = "held"
        self.last_frame = k
        self.last_decoded = dec
        self.taps_from_of[k] = self.taps_from
        # frame k + 1 starts: an armed TAPS_FROM N goes in force from N + 2
        if self.pending is not None and k + 1 >= self.pending[0] + 2:
            n, tw = self.pending
            self.taps_from = n
            self.pending = None
            self.commits.append((k + 1, n, tw))
        if self.selected and self.dma:
            self._emit_frame(k, dec, latched)
        self.sim.at(self.frame_start(k + 2), self._frame_end, k + 1, gen)

    # -- the stream --
    def _emit_frame(self, k, dec, latched):
        """Frame k ended: its blocks (and frame k + 1's slot flow) into the
        DMA. Frame k's slot blocks were added when frame k - 1 ended."""
        sig = self.sig
        slot = self.frame_s / sig.slots
        t_next = self.frame_start(k + 1)
        chunk_t = t_next + sig.block_delay_slots * slot
        flow0 = t_next + sig.slot_delay_slots * slot
        flow1 = self.frame_start(k + 2) + sig.slot_delay_slots * slot
        words = sig.tmcc_words + sig.status_words
        marks = [("status", dict(frame=k, decoded=dec, taps_from=self.taps_from_of[k]))]
        if latched and self.img.stream:
            words += sig.sums_words
            marks.append(("sums", dict(frame=k)))
            self.st.sums_emitted += 1
        rate = sig.decoded_words / self.frame_s if self.decoded(k + 1) else 0.0
        self.sim.at(flow0, self._flow, flow0, chunk_t, rate, self.gen)
        self.sim.at(chunk_t, self._chunk, words, marks, self.gen)
        self.sim.at(chunk_t, self._flow, chunk_t, flow1, rate, self.gen)

    def _flow(self, t0, t1, rate, gen):
        """Words at `rate` from t0 to t1 (scheduled at t0): the buffers they
        complete."""
        if gen != self.gen or rate == 0:
            return
        w = rate * (t1 - t0)
        start = self.words
        self.words += w
        while self.next_boundary <= self.words:
            t = t0 + (self.next_boundary - start) / rate
            self._buffer_at(max(t, t0), self.next_boundary)
            self.next_boundary += self.sc.host.buffer_words

    def _chunk(self, words, marks, gen):
        if gen != self.gen:
            return
        self.words += words
        for kind, m in marks:
            self.markers.append((self.words, kind, m))
        while self.next_boundary <= self.words:
            self._buffer_at(self.sim.now, self.next_boundary)
            self.next_boundary += self.sc.host.buffer_words

    def _buffer_at(self, t, boundary):
        if boundary == 0:
            return
        self.sim.at(t, self._buffer_done, boundary)

    def _buffer_done(self, boundary):
        out = []
        while self.markers and self.markers[0][0] <= boundary:
            out.append(self.markers.pop(0)[1:])
        self.buffer_index += 1
        self.st.buffers += 1
        if self.on_buffer:
            self.on_buffer(out)

    def start_dma(self):
        self.dma = True
        self.words = 0
        self.next_boundary = self.sc.host.buffer_words

    # -- registers --
    def sums_words(self, frame):
        w = self.sums_cache.get(frame)
        if w is None:
            rng = random.Random(frame * 7919 + 1)
            w = []
            for re, im in BASE_SUMS:
                g = 1 + 0.01 * rng.gauss(0, 1)
                w.extend(_words(re * g))
                w.extend(_words(im * g) if im else (0, 0))
            if len(self.sums_cache) > 64:
                self.sums_cache.clear()
            self.sums_cache[frame] = w
        return w

    def reg_value(self, a):
        """Register `a` now (what a READ latches into the readback)."""
        now = self.sim.now
        if a == VERSION:
            return self.img.version
        if a == FEATURES:
            return self.img.features
        if a == STATUS:
            if self.t0 is None or now < self.t0:
                return 0
            return ST_LOCKED | ST_FRAMED | (ST_DECODING if self.last_decoded else 0)
        if a == FRAME:
            return self.last_frame
        if a == CARRIER:
            # live: the carrier loop moves every 60 SSync steps
            return int(1e6 * math.sin(now * 3.0)) & 0xFFFFFFFF
        if a == TAPS_FROM:
            return self.taps_from
        if a == SUMS_FRAME:
            return self.latched
        if a == SUMS_COUNT:
            return 6720 if self.latched != NONE else 0
        if a == SUMS_CAPTURE:
            v = (self.latch_seq << 8) if self.img.seq else 0
            if self.capture == "held":
                v |= CAP_VALID
            if self.capture == "armed":
                v |= CAP_ARMED
            if self.img.seq and self.last_frame != NONE and not self.last_decoded:
                v |= CAP_NO_LAYOUT
            return v
        if SUMS_R <= a < SUMS_R + 4 * R_ENTRIES:
            return self.sums_words(self.latched)[a - SUMS_R] if self.latched != NONE else 0
        if SUMS_P <= a < SUMS_P + 4 * N_TAPS:
            return (self.sums_words(self.latched)[4 * R_ENTRIES + a - SUMS_P]
                    if self.latched != NONE else 0)
        if COUNTERS <= a < COUNTERS + 8:
            f = max(self.frame_at(now), 0)
            return (f * 96 * (3 if a == COUNTERS + 6 else 1)) & 0xFFFFFFFF
        if a == DECODER:
            return 3 << 24
        if a == FE_KP:
            return self.regs.get(FE_KP, 0)
        if a == FE_STATUS:
            return (1 | 2 | (4 if self.selected else 0)) if self.selected else 0
        return self.regs.get(a, 0)

    def gpio_in(self):
        self.last_transient = False
        if self.readback_at is not None:
            t, v = self.readback_at
            if self.sim.now >= t:
                self.readback = v
                self.readback_at = None
            else:
                # the old HDL's transient 0 (#27 item 4)
                self.st.transient_zero += 1
                self.last_transient = v != 0
                self.transient_value = v
                return 0
        return self.readback

    def is_sum(self, a):
        return SUMS_R <= a < SUMS_P + 4 * N_TAPS

    def command(self, word):
        """A command word on `gpio_out` (applied when iiod writes it)."""
        self.gpio_out = word
        op = (word >> 28) & 7
        arg = (word >> 1) & 0x07FFFFFF
        if op == OP_NOP:
            self.readback = 0x15DB4000 | (self.img.version << 8)
            self.readback_at = None
        elif op == OP_ADDR:
            self.addr = arg
        elif op == OP_HI:
            self.staged = arg & 0xFFFF
        elif op in (OP_WRITE, OP_WRITE_S):
            if op == OP_WRITE:
                v = self.staged << 16 | (arg & 0xFFFF)
            else:
                v = arg | 0xF8000000 if arg & (1 << 26) else arg
            self.write_reg(self.addr, v)
            self.addr += 1
        elif op == OP_READ:
            v = self.reg_value(self.addr)
            if self.img.old_sum_read and self.is_sum(self.addr):
                # the readback reads 0 until the MAC unit is idle
                busy = self.sim.rng.uniform(0, 4400 / 40e6)
                self.readback = 0
                self.readback_at = (self.sim.now + busy, v)
            else:
                self.readback = v
                self.readback_at = None
        elif op == OP_CTRL:
            if arg & CTRL_START:
                self.start()
            self.selected = bool(arg & CTRL_SLOTS) and bool(arg & CTRL_RUN)

    def write_reg(self, a, v):
        if TAPS <= a < TAPS + N_TAPS:
            self.taps_written[a - TAPS] = v
        elif a == TAPS_FROM:
            self.pending = (v, self.sim.now)
            self.st.taps_from_writes.append((self.sim.now, v, self.frame_at(self.sim.now)))
        elif a == SUMS_CAPTURE:
            self.capture = "armed"
        else:
            self.regs[a] = v

    def capture_write(self):
        """SUMS_CAPTURE written by another client (a second host)."""
        self.capture = "armed"


# --------------------------------------------------------------------------
# direct_reg_access and iiod clients
# --------------------------------------------------------------------------


class Device:
    """`cf-ad9361-lpc`'s debug register access: one cached address."""

    def __init__(self, sim, fpga, stats):
        self.sim, self.fpga, self.st = sim, fpga, stats
        self.cached = 0
        self.adc_status = 0  # 0 while the DMA runs; no real overflow modelled

    def value(self, addr):
        if addr == GPIO_IN:
            return self.fpga.gpio_in()
        if addr == GPIO_OUT:
            return self.fpga.gpio_out
        if addr == ADC_STATUS:
            return self.adc_status
        if addr == ADC_VERSION:
            return ADC_VERSION_VALUE
        return 0

    def set_addr(self, addr):
        self.cached = addr

    def write(self, addr, value):
        self.cached = addr
        if addr == GPIO_OUT:
            self.fpga.command(value)
        elif addr == ADC_STATUS:
            self.adc_status &= ~value

    def read(self, want):
        """(the value read, the true value of `want` now, foreign?)"""
        got = self.value(self.cached)
        if self.cached == want:
            return got, got, False
        return got, self.value(want), True


class Client:
    """One iiod connection (or a local libiio context on the board)."""

    def __init__(self, sim, dev, name, profile=None, local_window_s=None):
        self.sim, self.dev, self.name = sim, dev, name
        self.p = profile
        self.local = local_window_s
        self.lag = 0.0  # the end of the last request, not yet waited for
        self.requests = 0
        self.reads = 0
        self.foreign = 0  # reads that returned another register

    def _times(self):
        p = self.p
        j = 1 + p.jitter * (2 * self.sim.rng.random() - 1)
        return p.wire_s * j, p.iiod_s * j

    def flush(self):
        if self.lag:
            lag, self.lag = self.lag, 0.0
            yield lag

    def write(self, addr, value):
        if self.local is not None:
            self.dev.write(addr, value)
            return
            yield
        wire, srv = self._times()
        yield self.lag + wire / 2 + srv
        self.dev.write(addr, value)
        self.lag = wire / 2
        self.requests += 1

    def read(self, addr):
        """A register read as libiio 0.x does it: the address written, then
        the value read (two requests)."""
        self.reads += 1
        if self.local is not None:
            self.dev.set_addr(addr)
            yield self.local
            got, true, foreign = self.dev.read(addr)
        else:
            wire, srv = self._times()
            yield self.lag + wire / 2 + srv
            self.dev.set_addr(addr)
            wire2, srv2 = self._times()
            yield wire / 2 + wire2 / 2
            got, true, foreign = self.dev.read(addr)
            self.lag = srv2 + wire2 / 2
            self.requests += 2
        if foreign:
            self.foreign += 1
        return got, true, foreign


# --------------------------------------------------------------------------
# Statistics
# --------------------------------------------------------------------------


@dataclass
class Stats:
    # the slow loop
    ticks: int = 0
    open_end: float = 0.0
    sets_read: int = 0  # 308-word sets read
    sets_torn: int = 0  # the latch moved (SUMS_FRAME / SUMS_CAPTURE differ)
    sets_mixed: int = 0  # ground truth: words from more than one latch
    sets_foreign: int = 0  # ground truth: a gpio_in read returned another register
    sets_detected: int = 0  # diag: two reads of a word differed
    detected_words: int = 0
    sets_wrong: int = 0  # a set with an accepted word != the register's value
    solves: int = 0
    solved_mixed: int = 0
    solved_wrong: int = 0
    solved_bad: int = 0  # of which an entry moved by `Host.bad_error` or more
    # sets during which only SUMS_CAPTURE's NO_LAYOUT bit moved (accepted:
    # the held sums are one frame's; before the fix, discarded as torn)
    no_layout_moved: int = 0
    words_read: int = 0
    rereads: int = 0
    unsettled: int = 0
    reads_wrong: int = 0  # verified/single: an accepted value that is not the register's
    transient_zero: int = 0  # a sum READ's readback still 0 (old HDL)
    recoveries: int = 0
    unverified: int = 0
    # the stream path: stalls (sums_stall_frames decoded frames without a
    # sums block: warned, the sums then read from the registers), the
    # returns to the stream, and the sets read from the registers meanwhile
    sums_stalls: int = 0
    stream_returns: int = 0
    fallback_sets: int = 0
    stall_times: list = field(default_factory=list)
    first_write_t: float = None
    first_write_from: int = None
    first_write_frame: int = None  # the frame in progress at the write
    write_times: list = field(default_factory=list)
    set_times: list = field(default_factory=list)  # (start, end)
    taps_from_writes: list = field(default_factory=list)
    recover_times: list = field(default_factory=list)
    # exposed register reads (gpio_in / gpio_out), by phase
    exposed: dict = field(default_factory=lambda: {"open": 0, "loop": 0, "close": 0})
    # the capture context and the board's clients
    buffers: int = 0
    polls: int = 0
    overflow_flags: int = 0  # all false: no real overflow is modelled
    script_polls: int = 0
    script_misreads: int = 0
    # the stream
    status_blocks: int = 0
    sums_emitted: int = 0  # sums blocks the chain sent into the DMA
    sums_blocks: int = 0
    status_latency_max: float = 0.0  # frame end to the consumer, decoded frames
    sums_latency_max: float = 0.0
    status_gaps_max: float = 0.0  # between consecutive status blocks at the consumer


# --------------------------------------------------------------------------
# The host
# --------------------------------------------------------------------------


class Health:
    """`ChainHealth`: what the consumer has seen."""

    def __init__(self):
        self.last = None
        self.last_at = None
        self.cur_from = "unset"
        self.cur_frames = 0
        self.sums_frame = None
        self.sums_blocks = 0
        self.decoded_since_sums = 0  # HealthState::decoded_since_sums


class Model:
    """One scenario's run (`run`)."""

    def __init__(self, sc: Scenario, seed=1):
        self.sc = sc
        self.sim = Sim(seed)
        self.st = Stats()
        self.fpga = Fpga(self.sim, sc, self.st)
        self.dev = Device(self.sim, self.fpga, self.st)
        p = sc.profile
        self.cap = Client(self.sim, self.dev, "capture", p)  # the stream's context
        self.loop = Client(self.sim, self.dev, "slow loop", p)  # the slow loop's
        self.health = Health()
        self.toggle = False
        self.bit0 = 0
        self.stop = False
        self.loop_done = False
        self.phase = "open"
        self.queue = []  # buffers arrived at the capture thread
        self.cap_busy = False
        self.last_status_at = None
        self.fpga.on_buffer = self._buffer_done
        # the slow loop's state (chain.rs SlowLoop)
        self.capture = None
        self.armed = False
        self.armed_since = None
        self.no_layout_since = None
        self.last_recovery = None
        self.recovery_idle = False
        self.tried_good = False
        self.good = None
        self.good_from = None
        self.in_force = "identity"
        self.watch = None
        self.last_frame = None
        self.fallback = None  # the stream stalled: sums blocks counted at the last tick
        self.free_waited = False

    # -- command words (isdb_frontend::control::Encoder) --
    def word(self, op, arg):
        self.toggle = not self.toggle
        return (int(self.toggle) << 31) | (op << 28) | ((arg & 0x07FFFFFF) << 1) | self.bit0

    def cmd(self, c, op, arg=0):
        yield from c.write(GPIO_OUT, self.word(op, arg))

    def write_regs(self, c, addr, values):
        yield from self.cmd(c, OP_ADDR, addr)
        for v in values:
            if -(1 << 26) <= (v - (1 << 32) if v & 0x80000000 else v) < (1 << 26):
                yield from self.cmd(c, OP_WRITE_S, v & 0x07FFFFFF)
            else:
                yield from self.cmd(c, OP_HI, v >> 16)
                yield from self.cmd(c, OP_WRITE, v & 0xFFFF)

    def gpio_read(self, c):
        got, true, foreign = yield from c.read(GPIO_IN)
        self.st.exposed[self.phase] += 1
        return got, true, foreign

    def read_word(self, c, mode=None):
        """`gpio_in` after a READ, by the host's read discipline: (value
        used, the register's value, foreign seen?, detected?) or None when
        unsettled."""
        mode = mode or self.sc.host.reads
        if mode == "single":
            v, true, f = yield from self.gpio_read(c)
            return v, true, f, False
        if mode == "diag":
            v1, t1, f1 = yield from self.gpio_read(c)
            v2, t2, f2 = yield from self.gpio_read(c)
            return v1, t1, f1 or f2, v1 != v2
        # verified: until two reads in a row agree (chain.rs `settled`)
        reads = []
        v, true, f = yield from self.gpio_read(c)
        reads.append((v, true, f))
        foreign = f
        while len(reads) < self.sc.host.max_reads:
            v, true, f = yield from self.gpio_read(c)
            foreign |= f
            agree = reads[-1][0] == v
            reads.append((v, true, f))
            if agree:
                if len(reads) > 2:
                    self.st.rereads += 1
                return v, true, foreign, len(reads) > 2
        self.st.rereads += 1
        return None

    def read_regs(self, c, regs, record=None):
        """`ChainControl::read_regs_checked`: ADDR, READ, the word, for each;
        then NOP. Returns the values or None (a register never settled)."""
        out = []
        for a in regs:
            yield from self.cmd(c, OP_ADDR, a)
            yield from self.cmd(c, OP_READ)
            seq_at_read = self.fpga.latch_seq
            r = yield from self.read_word(c)
            if r is None:
                self.st.unsettled += 1
                yield from self.cmd(c, OP_NOP)
                return None
            v, true, foreign, detected = r
            if self.fpga.last_transient:
                true = self.fpga.transient_value  # the HDL's transient 0 accepted
            ok = v == true
            self.st.words_read += 1
            if not ok:
                self.st.reads_wrong += 1
            if record is not None:
                record.append((a, v, ok, foreign, detected, seq_at_read, true))
            out.append(v)
        yield from self.cmd(c, OP_NOP)
        return out

    # -- the open (ChainControl::open), on the capture context --
    def open_chain(self):
        c, h = self.cap, self.sc.host
        g = yield from self.read_word_raw(c, GPIO_OUT)
        self.bit0 = g & 1
        yield from self.cmd(c, OP_CTRL, 0)
        v = yield from self.read_regs(c, [VERSION, FEATURES])
        features = v[1] if v else 0
        if features & F_DECODER_HEALTH:
            yield from self.read_regs(c, [DECODER, RECLAIMED])
        # the front end's registers and banks (below 0x400)
        yield from self.cmd(c, OP_ADDR, 0x020)
        for _ in range(h.config_words - 1):
            yield from self.cmd(c, OP_WRITE_S, 0)
        self.fpga.regs[FE_KP] = 0x123
        yield from self.read_regs(c, [FE_KP])
        yield from self.write_regs(c, CARRIER, [0, 2])
        yield from self.write_regs(c, TAPS, [0x4000] + [0] * (N_TAPS - 1))
        yield from self.write_regs(c, INIT, [1])
        yield from self.cmd(c, OP_CTRL, CTRL_START)
        yield from self.cmd(c, OP_CTRL, CTRL_RUN | CTRL_SYMBOLS | CTRL_SLOTS)
        yield from self.read_regs(c, [FE_STATUS])
        yield from c.flush()
        self.st.open_end = self.sim.now
        self.phase = "loop"
        self.sim.spawn(self.slow_loop())
        self.sim.spawn(self.start_stream(), self.sc.signal.stream_start_s)

    def read_word_raw(self, c, addr):
        """`settled` on a core register (gpio_out at attach)."""
        prev = None
        for _ in range(self.sc.host.max_reads if self.sc.host.reads == "verified" else 1):
            got, true, foreign = yield from c.read(addr)
            self.st.exposed[self.phase] += 1
            if self.sc.host.reads != "verified" or got == prev:
                return got
            prev = got
        return prev

    def start_stream(self):
        self.fpga.start_dma()
        return
        yield

    # -- the capture thread and the consumer --
    def _buffer_done(self, marks):
        self.sim.at(self.sim.now + self.sc.signal.buffer_net_s, self._buffer_arrived, marks)

    def _buffer_arrived(self, marks):
        self.queue.append(marks)
        if not self.cap_busy:
            self.cap_busy = True
            self.sim.spawn(self.capture_thread())

    def capture_thread(self):
        h = self.sc.host
        while self.queue and not self.stop:
            marks = self.queue.pop(0)
            if h.overflow_poll:
                # adc_overflowed(): the status read, a W1C write when set
                self.st.polls += 1
                got, true, foreign = yield from self.cap.read(ADC_STATUS)
                if got & 4:
                    self.st.overflow_flags += 1
                    yield from self.cap.write(ADC_STATUS, 4)
                yield from self.cap.flush()
            self.sim.at(self.sim.now + self.sc.signal.consumer_s, self._consume, marks)
        self.cap_busy = False

    def _consume(self, marks):
        now = self.sim.now
        hl = self.health
        for kind, m in marks:
            if kind == "status":
                self.st.status_blocks += 1
                if self.last_status_at is not None:
                    self.st.status_gaps_max = max(self.st.status_gaps_max, now - self.last_status_at)
                self.last_status_at = now
                if m["decoded"]:
                    lat = now - self.fpga.frame_start(m["frame"] + 1)
                    self.st.status_latency_max = max(self.st.status_latency_max, lat)
                hl.last = m
                hl.last_at = now
                if hl.cur_from != m["taps_from"]:
                    hl.cur_from = m["taps_from"]
                    hl.cur_frames = 0
                hl.cur_frames += 1
                if m["decoded"]:
                    hl.decoded_since_sums += 1
            elif kind == "sums":
                self.st.sums_blocks += 1
                lat = now - self.fpga.frame_start(m["frame"] + 1)
                self.st.sums_latency_max = max(self.st.sums_latency_max, lat)
                hl.sums_frame = m["frame"]
                hl.sums_blocks += 1
                hl.decoded_since_sums = 0

    # -- the board's other clients --
    def api_controller(self):
        b = self.sc.board
        c = Client(self.sim, self.dev, "api_controller.sh", local_window_s=b.local_window_s)
        while True:
            yield self.sim.rng.uniform(*b.api_period_s)
            self.st.script_polls += 1
            got, true, foreign = yield from c.read(ADC_STATUS)
            if foreign:
                self.st.script_misreads += 1
            if got & 4 and self.fpga.dma:
                yield from c.write(ADC_STATUS, got)

    def version_reader(self):
        b = self.sc.board
        c = Client(self.sim, self.dev, "iio_attr 0x80000000", local_window_s=b.local_window_s)
        while True:
            yield 1.0 / b.version_reader_hz * self.sim.rng.uniform(0.9, 1.1)
            yield from c.read(ADC_VERSION)

    # -- the slow loop (chain.rs SlowLoop) --
    def wait(self, last):
        h = self.sc.host
        if last in ("Torn", "NoSums", "Capturing", "Watching"):
            return h.poll_s
        if self.no_layout_since is not None:
            return h.poll_s
        if self.st.solves == 0:
            return h.poll_s
        return h.every_s

    def sums_waiting(self):
        hl = self.health
        return (self.fallback is None and self.st.solves == 0 and self.watch is None
                and hl.sums_frame is not None and hl.sums_frame != self.last_frame)

    def slow_loop(self):
        h = self.sc.host
        stream = h.sums_source == "stream" and self.sc.image.stream
        c = self.loop
        yield from self.read_word_raw(c, GPIO_OUT)  # ChainControl::attach
        yield from c.flush()
        last = "NoSums"
        while True:
            due = self.sim.now + self.wait(last)
            step = h.wake_s
            while self.sim.now < due and not self.stop:
                if stream and self.sums_waiting():
                    break
                # sleeps of 5 ms, checking the stop flag (and on the stream
                # path before the first solve, a sums block waiting)
                yield step
            if self.stop:
                break
            self.st.ticks += 1
            if stream and not self.stream_stalled():
                last = yield from self.tick_stream()
            else:
                last = yield from self.tick_registers()
            yield from c.flush()
        self.loop_done = True

    def tick_registers(self):
        c, img = self.loop, self.sc.image
        if self.capture is None:
            f = yield from self.read_regs(c, [FEATURES])
            if f is None:
                return "Torn"
            self.capture = bool(f[0] & F_SUMS_CAPTURE)
        capture = self.capture
        regs = [SUMS_FRAME, SUMS_COUNT, CARRIER, TAPS_FROM, STATUS, FRAME, SUMS_CAPTURE]
        if capture:
            regs.insert(0, SUMS_CAPTURE)
        r = yield from self.read_regs(c, regs)
        if r is None:
            return "Torn"
        cap, v = (r[0], r[1:]) if capture else (None, r)
        frame, status, frame_now, held_before = v[0], v[4], v[5], v[6]
        t = yield from self.recover(status, cap, frame_now)
        if t:
            return t
        t = yield from self.judge()
        if t:
            return t
        if capture:
            if self.fallback is not None and not cap & (CAP_VALID | CAP_ARMED):
                # the stream's fallback, the latch released (a restart): a
                # poll for its sums blocks before a capture holds it again
                self.armed = False
                if not self.free_waited:
                    self.free_waited = True
                    return "Capturing"
            if not self.armed:
                self.free_waited = False
                yield from self.write_regs(c, SUMS_CAPTURE, [1])
                self.armed = True
                self.armed_since = self.sim.now
                return "Capturing"
            if not cap & CAP_VALID:
                return "Capturing"
            self.armed = False
        if frame == NONE:
            return "NoSums"
        if self.last_frame == frame:
            return "Same"
        rec = []
        t0 = self.sim.now
        words = yield from self.read_regs(c, SUM_REGS, record=rec)
        if words is None:
            self.armed = capture
            return "Torn"
        after = yield from self.read_regs(c, [SUMS_FRAME, SUMS_CAPTURE])
        if after is None:
            self.armed = capture
            return "Torn"
        yield from c.flush()
        self.st.set_times.append((t0, self.sim.now))
        self.st.sets_read += 1
        mixed = len({x[5] for x in rec}) > 1
        foreign = any(x[3] for x in rec)
        wrong = any(not x[2] for x in rec)
        detected = sum(1 for x in rec if x[4])
        error = max((entry_error(i, x[1], x[6]) for i, x in enumerate(rec) if not x[2]), default=0)
        self.st.sets_mixed += mixed
        self.st.sets_foreign += foreign
        self.st.sets_wrong += wrong
        self.st.sets_detected += detected > 0
        self.st.detected_words += detected
        if after[0] != frame or (after[1] ^ held_before) & CAP_HELD_BITS:
            self.st.sets_torn += 1
            return "Torn"
        if after[1] != held_before:
            # only SUMS_CAPTURE's NO_LAYOUT bit moved: the held set is one
            # snapshot, and kept
            self.st.no_layout_moved += 1
        if self.fallback is not None:
            self.st.fallback_sets += 1
        yield from self.solve_and_write(frame, mixed=mixed, wrong=wrong,
                                        bad=error >= self.sc.host.bad_error)
        return "Solved"

    def stream_stalled(self):
        """chain.rs `SlowLoop::stream_stalled`: on the stream path, whether
        this tick takes the register path (the sums blocks stopped while
        frames decode, and have not come back at the free latch's rate)."""
        h, hl = self.sc.host, self.health
        if self.fallback is None:
            stalled = hl.decoded_since_sums >= h.sums_stall_frames
            if stalled:
                self.st.sums_stalls += 1
                self.st.stall_times.append(self.sim.now)
        else:
            new = hl.sums_blocks - self.fallback
            stalled = not (new >= h.stream_back_blocks
                           and hl.decoded_since_sums < h.stream_back_blocks)
            if not stalled:
                self.st.stream_returns += 1
                self.armed = False
                self.armed_since = None
                self.free_waited = False
        self.fallback = hl.sums_blocks if stalled else None
        return stalled

    def tick_stream(self):
        h, hl = self.sc.host, self.health
        now = self.sim.now
        fresh = hl.last_at is not None and now - hl.last_at <= h.stream_stale_s
        last = hl.last if fresh else None
        status = 0
        if last is not None:
            status = ST_LOCKED | ST_FRAMED | (ST_DECODING if last["decoded"] else 0)
        frame_now = hl.last["frame"] if hl.last else NONE
        t = yield from self.recover(status, None, frame_now)
        if t:
            return t
        t = yield from self.judge()
        if t:
            return t
        if hl.sums_frame is None:
            return "NoSums"
        if self.last_frame == hl.sums_frame:
            return "Same"
        yield from self.solve_and_write(hl.sums_frame, mixed=False, wrong=False)
        return "Solved"

    def solve_and_write(self, frame, mixed, wrong, bad=False):
        # the checks, the solve and the gate take no register access and
        # (in the model) always pass: see the module docstring
        yield from self.write_taps(("solved", frame), frame)
        self.st.solves += 1
        self.st.solved_mixed += mixed
        self.st.solved_wrong += wrong
        self.st.solved_bad += bad
        self.last_frame = frame
        self.in_force = ("solved", frame)
        if self.st.first_write_t is None:
            self.st.first_write_t = self.sim.now - self.st.open_end
            self.st.first_write_from = frame
            self.st.first_write_frame = self.fpga.frame_at(self.sim.now)
        self.st.write_times.append(self.sim.now)
        w = dict(frm=frame, taps=("solved", frame), since=self.sim.now)
        if self.sc.host.health:
            self.watch = w
        else:
            self.accept(w)

    def write_taps(self, taps, frm):
        c = self.loop
        # ADDR TAPS, the 11 taps (HI + WRITE for a negative imaginary part),
        # ADDR TAPS_FROM, WRITE_S: `taps_words` command words in all
        n = self.sc.host.taps_words - 3
        yield from self.cmd(c, OP_ADDR, TAPS)
        for i in range(n):
            if i % 2 == 0:
                yield from self.cmd(c, OP_HI, 0xFF00 + i)
            else:
                yield from self.cmd(c, OP_WRITE, 0x4000)
        yield from self.cmd(c, OP_ADDR, TAPS_FROM)
        yield from self.cmd(c, OP_WRITE_S, frm & 0x07FFFFFF if frm != NONE else 0x07FFFFFF)
        if frm == NONE:
            # the identity committed from FRAME NONE: the model's frame is
            # not known yet; nothing to commit
            self.fpga.pending = None

    def accept(self, w):
        self.watch = None
        self.good = w["taps"]
        self.good_from = w["frm"]

    def judge(self):
        """The revert watch: the set written is accepted once ten status
        blocks show its TAPS_FROM (the quality rules are not modelled)."""
        if self.watch is None or not self.sc.host.health:
            return None
        yield from ()
        h, hl, w = self.sc.host, self.health, self.watch
        under = hl.cur_from == w["frm"]
        if not under or hl.cur_frames < h.revert_frames:
            if self.sim.now - w["since"] < h.revert_timeout_s:
                return "Watching"
            self.st.unverified += 1
        self.accept(w)
        return None

    def recover(self, status, cap, frame_now):
        """chain.rs `SlowLoop::recover`."""
        h, now = self.sc.host, self.sim.now
        if status & ST_DECODING:
            self.no_layout_since = None
        elif self.no_layout_since is None:
            self.no_layout_since = now
        if (cap is not None and cap & CAP_VALID) or not self.armed:
            self.armed_since = None
        if self.no_layout_since is None and self.armed_since is None:
            self.tried_good = False
            self.recovery_idle = False

        def long(t):
            return t is not None and now - t > h.recover_after_s

        if not (long(self.no_layout_since) or long(self.armed_since)) or (
                self.last_recovery is not None and now - self.last_recovery <= h.recover_after_s):
            return None
        watched = self.watch is not None
        self.watch = None
        if self.good is not None and self.good == self.in_force and not watched:
            self.tried_good = True
        if self.good is not None and not self.tried_good:
            self.tried_good = True
            target = (self.good, self.good_from)
        elif self.in_force != "identity":
            target = ("identity", frame_now)
        else:
            target = None
        self.last_recovery = now
        if target is None:
            self.recovery_idle = True
            return None
        yield from self.write_taps(target[0], target[1])
        self.in_force = target[0]
        self.st.recoveries += 1
        self.st.recover_times.append(now)
        return "Recovered"

    # -- the end of the capture --
    def close(self):
        """The end of the capture (the Pluto backend's `capture_slots`): the
        slow loop stopped and joined, then the counters and CTRL 0 on the
        capture's context (the command protocol is stateful: two contexts
        must not interleave commands)."""
        self.stop = True
        self.fpga.dma = False  # stream_words returns: the RX stream stopped first
        while not self.loop_done:
            yield 0.001
        self.phase = "close"
        yield from self.read_regs(self.cap, [COUNTERS + i for i in range(8)])
        yield from self.cmd(self.cap, OP_CTRL, 0)

    def run(self):
        sc = self.sc
        self.sim.spawn(self.open_chain())
        if sc.board.api_controller:
            self.sim.spawn(self.api_controller(), self.sim.rng.uniform(0, sc.board.api_period_s[1]))
        if sc.board.version_reader_hz:
            self.sim.spawn(self.version_reader(), self.sim.rng.uniform(0, 0.2))
        for t in sc.foreign_capture_at:
            self.sim.at(t, self.fpga.capture_write)
        for t in sc.restart_at:
            self.sim.at(t, self.fpga.start)
        self.sim.spawn(self.close(), sc.seconds)
        self.sim.run(sc.seconds + 3.0)
        return self.result()

    def result(self):
        st, f = self.st, self.fpga
        r = dataclasses.asdict(st)
        for k in ("write_times", "set_times", "taps_from_writes", "recover_times", "stall_times"):
            r.pop(k)
        # the first slow-loop set in force, from the status blocks' taps_from
        first = next(((k, n) for k, n, _ in f.commits if n != NONE), None)
        r["first_in_force_frame"] = first[0] if first else None
        r["first_in_force_from"] = first[1] if first else None
        sets = st.set_times
        r["set_s_mean"] = sum(b - a for a, b in sets) / len(sets) if sets else None
        r["write_period_s"] = ((st.write_times[-1] - st.write_times[0]) / (len(st.write_times) - 1)
                               if len(st.write_times) > 1 else None)
        r["last_solve_t"] = st.write_times[-1] - st.open_end if st.write_times else None
        # the longest stretch without a solve after the first (to the end)
        w = st.write_times + [self.sc.seconds]
        r["solve_gap_max"] = max((b - a for a, b in zip(w, w[1:])), default=None)
        r["recover_t"] = [t - st.open_end for t in st.recover_times]
        r["stall_t"] = [t - st.open_end for t in st.stall_times]
        r["solve_t"] = [t - st.open_end for t in st.write_times]
        r["scenario"] = self.sc.name
        r["seconds"] = self.sc.seconds
        r["request_s"] = self.sc.profile.request_s
        return r


def run(sc, seed=1):
    return Model(sc, seed).run()


# --------------------------------------------------------------------------
# The static check: multi-word values against the register map
# --------------------------------------------------------------------------


def set_read_s(profile, reads):
    """One 308-word set's duration by the read discipline (no foreign hit)."""
    per_word = {"single": 4, "diag": 6, "verified": 6}[reads]
    return SUM_WORDS * per_word * profile.request_s


def check_groups(image, profile, host, signal=Signal()):
    """Every value the host combines from several words must be one
    snapshot: its words held (or latched no faster than the host reads
    them) and a version read before and after. Returns the violations,
    each (group, reason)."""
    kinds = {n: k for n, _, _, k, _ in REGISTERS}
    out = []
    read_s = set_read_s(profile, host.reads)
    for g in GROUPS:
        if host.sums_source == "stream" and image.stream and g["name"] == "training sums":
            continue  # one block under one CRC: no register read
        if g["stable"] == "latch":
            held = image.capture
            stable_s = math.inf if held else signal.frame_s
            if read_s >= stable_s:
                out.append((g["name"], f"latched every frame ({signal.frame_s * 1e3:.2f} ms) and "
                            f"never held (no SUMS_CAPTURE): the host's read takes {read_s:.3f} s, "
                            f"{read_s / signal.frame_s:.1f} frames: every set torn"))
            missing = [v for v in g["version"] if kinds.get(v) != "version"]
            if missing:
                out.append((g["name"], f"no version register: {missing}"))
        elif g["stable"] == "live" and not g["version"]:
            if g["name"] not in GROUPS_ALLOWED:
                out.append((g["name"], "a live multi-word value without a snapshot or version"))
    return out


def liveness(sc, r):
    """No host sequence waits for ever: the problems of run `r` of `sc`
    (empty when none). Bounds, from the slow loop's own periods: the first
    taps within `poll + 2 frames + a set + the write` (plus the lock and the
    layout lag), the loop solving at least every `every + poll + a set +
    the revert watch` while frames are decoded (plus any stretch without a
    layout), and a stretch without a layout longer than `recover_after`
    ending in a recovery when known taps other than those in force exist.
    On the stream path both bounds also allow for a stall of the sums
    blocks (a held latch sends none): `sums_stall_frames` decoded frames,
    the tick that sees it and the register path's capture and set (whether
    a run needs it is `sums_stalls`, which no scenario without a foreign
    hold has)."""
    h, sig, p = sc.host, sc.signal, sc.profile
    stream = h.sums_source == "stream" and sc.image.stream
    set_s = 0.0 if stream else set_read_s(p, h.reads) * 1.3  # with rereads and tick reads
    stall_s = 0.0
    if stream:
        stall_s = (h.sums_stall_frames * sig.frame_s + h.every_s + 2 * h.poll_s + sig.frame_s
                   + set_read_s(p, h.reads) * 1.3)
    watch_s = (h.revert_frames + 3) * sig.frame_s if h.health else 0.0
    first_bound = (sig.lock_s + (sig.layout_lag + 2) * sig.frame_s + 2 * h.poll_s + set_s + stall_s
                   + 0.2)
    outage = sum((b - a + 1) * sig.frame_s for a, b in sig.no_layout)
    # one retry (a torn set read again) allowed
    gap_bound = h.every_s + 3 * h.poll_s + 2 * set_s + stall_s + watch_s + outage + 0.2
    out = []
    if r["first_write_t"] is None or r["first_write_t"] > first_bound:
        out.append(f"no taps written within {first_bound:.2f} s of the open "
                   f"(first: {r['first_write_t']}; frames in force from {r['first_in_force_frame']})")
    elif r["solve_gap_max"] is not None and r["solve_gap_max"] > gap_bound:
        out.append(f"no solve for {r['solve_gap_max']:.2f} s (bound {gap_bound:.2f} s) while frames "
                   f"were decoded")
    longest = max(((b - a + 1) * sig.frame_s for a, b in sig.no_layout), default=0.0)
    if longest > h.recover_after_s + h.poll_s + set_s and r["recoveries"] == 0:
        out.append(f"{longest:.2f} s without a layout ended in no recovery")
    return out


# Liveness gaps the model finds in today's host and HDL, with the reason:
# the test fails when one of them goes (update the list) or a new one comes.
KNOWN_GAPS = {
    "build6":
        "the reproduction of build 6: the latch moves every frame and the 308 reads take 15, "
        "every set is torn and the loop never writes taps",
}


def polled_live(image, host):
    """Registers the host polls on every tick that change on their own, and
    whether the host combines them with others (it must not, unversioned)."""
    if host.sums_source == "stream" and image.stream:
        return []
    kinds = {a: (n, k) for n, a, _, k, _ in REGISTERS}
    regs = [SUMS_FRAME, SUMS_COUNT, CARRIER, TAPS_FROM, STATUS, FRAME, SUMS_CAPTURE]
    return [(kinds[a][0], kinds[a][1]) for a in regs if kinds[a][1] in ("live", "frame")]


# --------------------------------------------------------------------------
# Scenarios: the board's runs
# --------------------------------------------------------------------------

SCENARIOS = {
    # (a) torn reads
    "build6": Scenario("build6", BENCH, BUILD6, HOST_BUILD8, seconds=30.0,
                       note="build 6 on the board (VERIFICATION.md): 46 sets read in 30 s, every one "
                            "torn, no taps; 27 ADC overflow flags, no block lost"),
    "build7": Scenario("build7", BENCH, BUILD7, HOST_BUILD8, seconds=30.0,
                       note="build 7: first write 0.90 s after the open, solved from frame 5, in force "
                            "from frame 26; 12 solves in 24 ticks, 0 torn; 8 overflow flags"),
    "build9": Scenario("build9", BENCH, BUILD9, HOST_VERIFIED, Board(api_controller=True), seconds=600.0,
                       note="build 9, api_controller.sh running, 600 s: 200 written in 400 ticks, "
                            "1 reread in 65 201 reads, 0 torn / unsettled; first set in force at "
                            "+33-34 frames (Build 9 residuals)"),
    "build9-quiet": Scenario("build9-quiet", BENCH, BUILD9, HOST_VERIFIED, seconds=300.0,
                             note="build 9, api_controller.sh stopped, 300 s: 102 solves, 0 rereads"),
    # (b) #27 / #29 on the reporter's board
    "issue27-B": Scenario("issue27-B", ISSUE29, BUILD8, HOST_DIAG, Board(api_controller=True), seconds=300.0,
                          note="#27 run B: the host's per-buffer overflow poll on (and the script): "
                               "47 of 50 sets with a differing word, 186 words"),
    "issue29-C": Scenario("issue29-C", ISSUE29, BUILD8, replace(HOST_DIAG, overflow_poll=False),
                          Board(api_controller=True), seconds=600.0,
                          note="#27 run C / #29 E3: poll off, script running: 7 of 146 and 7 of 158 sets"),
    "issue29-E1": Scenario("issue29-E1", ISSUE29, BUILD8, replace(HOST_DIAG, overflow_poll=False),
                           seconds=600.0, note="#29 E1: poll off, script stopped: 0 of 163 sets"),
    "issue29-E2": Scenario("issue29-E2", ISSUE29, BUILD8, replace(HOST_DIAG, overflow_poll=False),
                           Board(version_reader_hz=5.0), seconds=180.0,
                           note="#29 E2: a 5 Hz reader of 0x80000000: 34 of 50 sets, 52 words, all "
                                "0xA0300 (the fit of ISSUE29.wire_s)"),
    "issue27-A": Scenario("issue27-A", ISSUE29, BUILD8, HOST_BUILD8, Board(api_controller=True),
                          seconds=600.0,
                          note="#27 run A: build 8's host as released (single reads, the poll): "
                               "231 overflow flags with no block lost; 13 bad sets written, 30 rejected "
                               "of 190 solves"),
    "issue27-B-verified": Scenario("issue27-B-verified", ISSUE29, BUILD8,
                                   replace(HOST_VERIFIED, overflow_poll=True, buffer_words=262144),
                                   Board(api_controller=True), seconds=300.0,
                                   note="PR #31's verified reads under run B's load: rereads, no "
                                        "wrong word accepted"),
    "issue29-C-verified": Scenario("issue29-C-verified", ISSUE29, BUILD8, HOST_VERIFIED,
                                   Board(api_controller=True), seconds=600.0,
                                   note="PR #31 under run C's load"),
    # (c) the stream path
    "stream": Scenario("stream", BENCH, FORMAT2, HOST_STREAM, Board(api_controller=True), seconds=30.0,
                       note="format 2: plan 'Sums in the stream' expects the first taps in force "
                            "from frame 4 (5 when the write lands after its start)"),
    "stream-issue29": Scenario("stream-issue29", ISSUE29, FORMAT2, HOST_STREAM, Board(api_controller=True),
                               seconds=30.0, note="the stream path on the slower network"),
    # liveness
    "nolayout-registers": Scenario("nolayout-registers", BENCH, BUILD9, HOST_VERIFIED,
                                   signal=Signal(no_layout=((300, 420),)), seconds=30.0,
                                   note="TMCC lost for 121 frames (4 s): an armed capture waits, the "
                                        "recovery writes the identity, the loop solves again after"),
    "nolayout-stream": Scenario("nolayout-stream", BENCH, FORMAT2, HOST_STREAM,
                                signal=Signal(no_layout=((300, 420),)), seconds=30.0,
                                note="the same on the stream path"),
    "stream-held-prefix": Scenario("stream-held-prefix", BENCH, replace(FORMAT2, start_releases_hold=False),
                                   HOST_STREAM, initial_capture="held", seconds=30.0,
                                   note="a register-reading run left a capture held; before dedbc14 "
                                        "START kept it: no sums block, no taps, no recovery"),
    "stream-held": Scenario("stream-held", BENCH, FORMAT2, HOST_STREAM, initial_capture="held",
                            seconds=30.0, note="the same with dedbc14 (START releases the hold)"),
    "stream-foreign-capture": Scenario("stream-foreign-capture", BENCH, FORMAT2, HOST_STREAM,
                                       foreign_capture_at=(10.0,), restart_at=(20.0,), seconds=30.0,
                                       note="another client arms SUMS_CAPTURE at 10 s: the sums "
                                            "blocks stop; after 90 decoded frames the host warns "
                                            "and reads the sums from the registers; a restart at "
                                            "20 s frees the latch and the host goes back to the "
                                            "stream"),
    # the HDL's latent transient 0 (#27 item 4)
    "local-old-hdl": Scenario("local-old-hdl", LOCAL, BUILD8, replace(HOST_VERIFIED, health=False),
                              seconds=5.0, note="a host on the board, the old sum READ (0 for up to "
                                                "4 400 clocks)"),
    "local-new-hdl": Scenario("local-new-hdl", LOCAL, BUILD9, replace(HOST_VERIFIED, health=False),
                              seconds=5.0, note="the same with build 9's 3-clock READ"),
}

# What the board measured, per scenario (for the report)
MEASURED = {
    "build6": "46 sets, all torn; no write; 27 overflow flags",
    "build7": "first write 0.90 s (from frame 5), in force from 26; 12 solves / 24 ticks; 8 flags",
    "build9": "200 written / 400 ticks; 1 reread in 65 201 reads; in force at +33-34",
    "build9-quiet": "102 solves; 0 rereads",
    "issue27-B": "47 / 50 sets with a differing word (186 words)",
    "issue29-C": "7 / 146 sets (run C), 7 / 158 (E3)",
    "issue29-E1": "0 / 163 sets",
    "issue29-E2": "34 / 50 sets, 52 words",
    "issue27-A": "231 overflow flags; 13 bad written + 30 rejected of 190 solves",
    "issue27-B-verified": "(not run on the board)",
    "issue29-C-verified": "(not run on the board)",
    "stream": "expected: taps_from 2 in force from frame 4 or 5 (no image yet)",
    "stream-issue29": "-",
    "nolayout-registers": "-",
    "nolayout-stream": "-",
    "stream-held-prefix": "-",
    "stream-held": "-",
    "stream-foreign-capture": "-",
    "local-old-hdl": "(the HDL race, not observed over the network)",
    "local-new-hdl": "-",
}


def override(sc, assignments):
    """`sc` with `section.field=value` assignments (`profile`, `image`,
    `host`, `board`, `signal`, or a scenario field): a measured number
    changed from the command line."""
    import ast

    for a in assignments:
        key, _, val = a.partition("=")
        sect, _, name = key.partition(".")
        try:
            v = ast.literal_eval(val)  # numbers, tuples, booleans
        except (ValueError, SyntaxError):
            v = val  # a string
        if name:
            sc = replace(sc, **{sect: replace(getattr(sc, sect), **{name: v})})
        else:
            sc = replace(sc, **{sect: v})
    return sc


def mean_summary(runs):
    """Per numeric key, the mean over the seeds' summaries."""
    out = {}
    for k in runs[0]:
        vals = [r.get(k) for r in runs]
        if all(isinstance(v, (int, float)) for v in vals):
            out[k] = round(sum(vals) / len(vals), 4)
    return out


def summarise(r):
    keys = ["sets_read", "sets_torn", "sets_foreign", "sets_detected", "detected_words", "sets_wrong",
            "solves", "solved_wrong", "solved_bad", "solved_mixed", "no_layout_moved", "ticks",
            "words_read", "rereads", "unsettled",
            "reads_wrong", "overflow_flags", "first_write_t", "first_write_from", "first_in_force_frame",
            "set_s_mean", "recoveries", "sums_blocks", "status_latency_max", "sums_latency_max",
            "sums_stalls", "stream_returns", "fallback_sets"]
    out = {}
    for k in keys:
        v = r.get(k)
        if v is None:
            continue
        out[k] = round(v, 4) if isinstance(v, float) else v
    out["exposed"] = r["exposed"]
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("scenario", nargs="*", help="scenario names (default: all)")
    ap.add_argument("--seeds", type=int, default=1, help="seeds per scenario (1..N)")
    ap.add_argument("--mean", action="store_true", help="print the mean over the seeds only")
    ap.add_argument("--set", action="append", default=[], metavar="SECTION.FIELD=VALUE",
                    help="change a parameter, e.g. profile.wire_s=50e-6 or host.buffer_words=262144")
    ap.add_argument("--sweep", metavar="SECTION.FIELD=V1,V2,...",
                    help="run the scenarios at each value, print --fields (seed means)")
    ap.add_argument("--fields", default="sets_detected,overflow_flags,rereads",
                    help="the summary fields --sweep prints")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--list", action="store_true")
    a = ap.parse_args(argv)
    if a.list:
        for n, s in SCENARIOS.items():
            print(f"{n:24} {s.note}")
        return 0
    names = a.scenario or list(SCENARIOS)
    if a.sweep:
        # one parameter over several values, the mean of the seeds each
        key, _, vals = a.sweep.partition("=")
        fields = a.fields.split(",")
        print("value".ljust(12) + "".join(f"{n}:{f}".rjust(28) for n in names for f in fields))
        for v in vals.split(","):
            row = v.ljust(12)
            for n in names:
                sc = override(SCENARIOS[n], a.set + [f"{key}={v}"])
                m = mean_summary([summarise(run(sc, s)) for s in range(1, a.seeds + 1)])
                row += "".join(str(m.get(f, "-")).rjust(28) for f in fields)
            print(row)
        return 0
    results = {}
    for n in names:
        sc = override(SCENARIOS[n], a.set)
        runs = [run(sc, seed) for seed in range(1, a.seeds + 1)]
        results[n] = [summarise(r) for r in runs]
        if a.mean:
            results[n] = [mean_summary(results[n])]
        if not a.json:
            print(f"== {n}: {sc.note}")
            print(f"   measured: {MEASURED.get(n, '-')}")
            for s in results[n]:
                print("   model:   " + ", ".join(f"{k} {v}" for k, v in s.items()))
    if a.json:
        json.dump(results, sys.stdout, indent=1)
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
