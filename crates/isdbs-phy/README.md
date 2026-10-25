# isdbs-phy

ISDB-S receive chain after the symbol front end: frame and superframe
synchronisation, carrier recovery on the known symbols, TMCC decoding, and
the main signal of every superframe to TS packets per TS_ID. Symbols come
from `isdb-frontend` (shared with ISDB-S3, and the same bits as the
PlutoSky's FPGA front end); frame layout, codes, the interleaver and the
energy dispersal from `isdbs-core`.

## Stages

1. **Frame sync** (`sync`): each candidate start is correlated with the
   frame's 808 known BPSK symbols (the coded W1 and W2/W3 tails and the 192
   bursts), coherently within each group and non-coherently between
   groups. Then each frame is checked exactly 39 936 symbols after the
   previous one; a missing frame counts as a slip and restarts the search,
   which also reveals symbols lost upstream. The W2 / W3 tail marks
   frame 1 of each superframe.
2. **Carrier** (`carrier`): the phase of the W1 tail, the W2/W3 tail and
   each burst, tracked by a second-order loop over the pilot sequence run
   forward and backward (zero-lag), interpolated linearly per symbol. The
   loop's frequency starts from the mean pilot-to-pilot rotation. The
   phase ambiguity is fixed by W1 (BPSK "0" at 0 rad).
3. **TMCC** (`tmcc`): the 192 derotated header symbols, soft BPSK, Viterbi
   (X then Y), the 12 header bytes and their sync words; the 8 TMCC
   blocks of a superframe go to `isdbs_core::tmcc::decode_blocks`
   (energy dispersal, RS(64,48)).
4. **Frames** (`rx::receive`): per frame the 38 976 main symbols,
   derotated and scaled to unit amplitude on the bursts, the pilot MER and
   the header check; per superframe the decoded `TmccInfo`.
5. **Main signal → bytes** (`demod::decode_frame`): run by run of the
   TMCC's slot layout, soft demapping and one K = 7 Viterbi decoder per
   frame, started in the state the header's W2/W3 left and ended on the
   next frame's W1 (both known). BPSK r = 1/2: X then Y on successive
   symbols. QPSK: the (P1, P0) pairs of the punctured code, P1 on I, the
   puncturing restarted at each run. TC8PSK r = 2/3: B1 sent uncoded as
   C2, B0 coded into C1 = Y (133₈) and C0 = X (171₈); the branch metric of
   each (C1, C0) is the better of its two antipodal points (|Re z·p̄|),
   and C2 is decided afterwards in the subset of the decoded (C1, C0).
   Bytes MSB first, alternately B1 and B0 for TC8PSK.
6. **Superframe → TS** (`ts::decode_superframe`, `ts::decode_all`): the
   8 frames' bytes (203 per sent slot) de-interleaved per slot number,
   the superframe energy dispersal removed, the sync byte 0x47 restored,
   RS(204,188), each packet tagged with its slot's TS_ID from the TMCC;
   uncorrectable words keep transport_error_indicator = 1. A superframe
   whose own TMCC failed decodes under the previous one. Superframes are
   decoded in parallel on every core.

## Design choices (measured on the lab recordings, 2026-10-20)

| Question | Measurement | Choice |
|---|---|---|
| Sync detector | The W1 tail alone (20 symbols, differential): 0.99 on frames of strong transponders, but random data reaches 0.75–0.84 somewhere in every frame length, and on ND22 (MER ≈ 1 dB) frames score 0.01–0.6. The 808-symbol frame metric: frames ≥ 0.81 on every transponder, random data ≤ 0.55 | frame metric, acquire 0.65, track 0.62 |
| Carrier | Linear interpolation of measured pilot phases with smoothing: biased at frame ends with a plain average (fixed with a local line fit); a forward-backward loop over the pilot correlations gives the same MER on strong signals and cannot slip on a single bad pilot | loop, bandwidth 0.05 of the pilot rate |
| Burst data | all 30 720 burst symbols of 40 frames match BPSK of the PRBS alone | all-zero burst data (SPEC-NOTES of `isdbs-core`) |

## Boundaries

`rx::Frame::main`: the 38 976 main-signal symbols of a frame in
transmission order, derotated (BPSK "0" on the positive real axis, QPSK
and 8PSK on the grid of annexed table 46) and scaled to unit amplitude.
`Frame::index` gives the frame's position in its superframe (0 = the W2
frame), so the inter-frame de-interleaver can align. The slot layout of
the superframe comes from its `TmccInfo` through `isdbs_core::slots::runs`.
Each run is a contiguous range of main symbols with one combination, and
the puncturing restarts at its start.

`ts::Packet`: 188 bytes, the TS_ID, the frame and slot it was sent in, and
the RS outcome. `isdb rx --system isdbs` writes them per TS_ID; slots whose
relative TS ID points at an unused (FFFFh) entry belong to no TS.

## Constellation readings (confirmed by RS(204,188) on the lab recordings)

`demod::Mapping` keeps the alternatives selectable for `examples/mapprobe`,
which counts RS successes under each:

| Reading | Alternatives | Result |
|---|---|---|
| TC8PSK C1/C0 | C1 = 171₈ (X) or 133₈ (Y) | only C1 = Y decodes: BS3 19 968 of 19 968 packets RS-clean, C1 = X 0. BO.1408-1 Fig. 15 labels the points (W, Y, X) |
| QPSK P1 | on I (annexed table 46 text) or on Q | only P1 on I decodes: ND2 288 of 288 packets RS-clean |
| Spectrum | as received or mirrored | as received (mirrored decodes nothing) |

## Throughput (32-thread host, release build)

| Stage | BS1, 1.68 s at 40 Msps | Notes |
|---|---|---|
| survey | 0.29 s | |
| trim search (11 candidates, each a front end and a receive over 0.15 s) | 0.65 s | candidates at once (were one after another: 7.6 s) |
| front end (`isdb-frontend` `FastFrontend`, one thread) | 2.0 s | 0.84× real time, the sequential stream |
| frame sync, carrier, derotation | 0.17 s | acquisition scan and frames shared out between threads |
| main signal → TS (every core) | 0.40 s | |

Real time needs 28.86 Msym/s. The front end is one sequential stream
per block (the bit-exact model's timing loop); `isdb rx` overlaps it
with the receiver and runs several blocks' front ends at once
(VERIFICATION.md "Satellite receive speed"; `examples/chaintime`,
`examples/throughput`).
## Position in the stack

- Depends on: `isdb-frontend`, `isdb-iq` (survey), `isdb-fec`,
  `isdbs-core`, `num-complex`.
- Used by: `isdb-cli` (`isdb iq tmcc`, `isdb rx --system isdbs`).

## Tests

- Unit tests on a synthetic signal (`testsig`: header, bursts and 8PSK
  data with carrier offset and noise): frames found at their exact
  starts, W2 every 8 frames; 1024 dropped symbols reported as one slip
  with the following frames re-found; bursts derotated onto their BPSK
  points under a carrier offset; TMCC decoded on every superframe, main
  MER as the noise predicts.
- Opt-in recordings (`tests/captures.rs`, `ISDB_RS_ISDBS_TMCC_IQ`, a
  `:`-separated list): no slips, RS(64,48) on every superframe, valid and
  unchanging TMCC, 39 936-symbol spacing across each recording.
- `demod` unit tests: annexed table 46 points and set partitioning,
  the encoder state after W2/W3, `prop_frame_round_trip` (every
  combination mix encodes with `encode_frame` and decodes back exactly),
  TC8PSK at 12 dB SNR (few byte errors, the reverse C1/C0 reading fails).
- Opt-in recordings, main signal (`ISDB_RS_ISDBS_TS_IQ`): no
  uncorrectable RS word, sync byte on every packet, continuity counters
  without a gap per TS_ID and PID.
- Development tools (examples): `syncstat` (sync-metric statistics on and
  off the frame grid), `spacing` (acquisition-only frame spacing),
  `offsets` (metric at ±2 symbols and the derotated burst projection),
  `mapprobe` (RS success under each constellation reading), `throughput`
  (time per stage).
