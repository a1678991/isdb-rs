# Plan: the streamed ISDB-S3 receiver pipelined (2026-11-01)

Goal: raise the speed and the CPU use of one `isdb rx --symbols` process
(a `Symbols` words file, or a radio's FPGA front end, to TLV) without
changing a byte of its output. Design and figures from PR #23; the
measurements are the contributor's, on a 4-core i5-4278U, and are
recorded in VERIFICATION.md ("Satellite receive speed").

## Background

Decoding 3.9 s of BS-14 symbols on 4 cores took 22.2 s of wall time for
63.5 s of CPU (286 %); by Amdahl's split about 8 s of it was sequential
and 55 CPU-seconds parallel, which on a 32-core host predicts 0.39× real
time at 20 % use, what a 32-vCPU VM showed. The cause is the shape of
`symstream::run` → `rxs3::decode` → `rx::receive`: for every 24-frame
chunk (about 0.8 s of signal)

1. sequential: derotation, slot sync (`synchronise_at`, twice), the
   TMCC and the equaliser's training;
2. parallel: equalisation (`eq::apply`) and each frame's preparation
   (`prepare`);
3. parallel: demapping and LDPC (`decode_frames`);
4. sequential: TLV assembly and writes;

with a fork-join between chunks, so every other core waits through the
sequential parts.

## Goals and acceptance

- The TLV output is byte-identical to the previous binary on reference
  files (a 1-minute and a 10-minute BS-14 `Symbols` recording), and the
  summary lines (frames, TMCC, slots, LDPC/BCH failures, MER, TLV
  packets, lost slots) match except for the timings.
- The live path (`--radio … --offload`) keeps its behaviour: the same
  `run_stream`, so chunk order, loss handling and Ctrl-C mean what they
  did.
- Memory grows by at most about 1 GB (one more chunk in flight). f64 to
  f32 would change the output and is out of scope.

## Stage 0: measure

`rxs3::Stats` gains per-stage sums, reported after the `time:` line as
`stages: sync and preparation, LDPC, TLV`. The CLI times the two halves
of the receiver around their calls; the library's result stays a pure
function of its input.

## Stage 2: pipeline

### The receiver split (`isdbs3-phy`)

- `rx::receive_front(z, opt) -> Front`: today's `receive` from the sync
  to the frames' `ready` / `order`. `Front` owns what it holds (the
  equalised symbols each frame needs are the `ms` / `pil` that
  `prepare` cut out, so nothing borrows `z`). `frame_starts`,
  `residual_hz`, `sync_snr_db`, `sync_snr_eq_db` and the taps are
  decided here.
- `rx::receive_back(front, opt) -> Rx`: the constellation points, the
  noise estimate, `decode_frames` and the `Rx`.
- `receive(z, opt) = receive_back(receive_front(z, opt), opt)`; the
  other callers (`isdb rx --iq`, examples, tests) are unchanged.

### The stream side (`isdb-cli` `rxs3::run_stream`)

- The decode closure splits in two.
  - Front (the thread that runs `symstream::run`): `receive_front`,
    then the cut `upto`, `aligned` and the tracked carrier are decided
    on the spot, as before, and `used` returned. The `Front` and the
    cut go to the back over a depth-1 `sync_channel`.
  - Back (the calling thread): `receive_back`, the counters, TLV
    assembly and writes, in chunk order.
- The end of a contiguous stretch (a loss, a lost lock, the end of the
  input) is an `End` message on the same channel, so the back closes
  the assemblers in order.
- The status line reads the counters the back updates through a
  `Mutex`; `residual_hz`, `sync_snr_db` and `tracked_hz` are written by
  the front.
- Failures: an error in the back (a write failure) drops the receiver,
  the front's next send fails and `run` ends with an error; the back's
  error is the one reported. A panic in the back is re-raised. Ctrl-C
  stops the front's loop as before and the back drains what was sent.
- Memory: one more chunk in flight (a `Front` is about 0.4 GB on
  24-frame chunks; at most three exist at once).
- The front re-acquires (no slot hint) on the chunk after a continued
  chunk whose frames decoded no TMCC: a stream that locked onto an
  alias of the residual carrier (`sync::carrier_hz`) would otherwise
  track it for ever.

### Out of scope

- A shared LDPC thread pool across chunks (decide on stage 2's result).
- Splitting one file across processes inside the CLI; `tools/split-rx.sh`
  does it outside, now that a slice acquires mid-recording.

## Tests

- Unit: `receive(z)` and `receive_back(receive_front(z))` give the same
  `Rx` on synthetic symbols; the opt-in `ISDB_RS_ISDBS3_WORDS` test does
  the same on a recording's first 25 frames.
- Integration: the two reference files' TLV compared with `cmp`, the
  summary lines compared.
- `cargo test` (isdbs3-phy, isdb-cli) and clippy pass.
- Speed: wall time and CPU use before and after on the 4-core host.
