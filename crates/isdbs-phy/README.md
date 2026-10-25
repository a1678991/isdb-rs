# isdbs-phy

ISDB-S receive chain after the symbol front end: frame and superframe
synchronisation, carrier recovery on the known symbols, TMCC decoding, and
the derotated main signal of every frame. Symbols come from
`isdb-frontend` (shared with ISDB-S3, and the same bits as the PlutoSky's
FPGA front end); frame layout and codes from `isdbs-core`. Main-signal
demapping, the inner code, de-interleaving, RS and TS output are Phase 3
of the ISDB-S plan.

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

## Design choices (measured on the lab recordings, 2026-10-20)

| Question | Measurement | Choice |
|---|---|---|
| Sync detector | The W1 tail alone (20 symbols, differential): 0.99 on frames of strong transponders, but random data reaches 0.75–0.84 somewhere in every frame length, and on ND22 (MER ≈ 1 dB) frames score 0.01–0.6. The 808-symbol frame metric: frames ≥ 0.81 on every transponder, random data ≤ 0.55 | frame metric, acquire 0.65, track 0.62 |
| Carrier | Linear interpolation of measured pilot phases with smoothing: biased at frame ends with a plain average (fixed with a local line fit); a forward-backward loop over the pilot correlations gives the same MER on strong signals and cannot slip on a single bad pilot | loop, bandwidth 0.05 of the pilot rate |
| Burst data | all 30 720 burst symbols of 40 frames match BPSK of the PRBS alone | all-zero burst data (SPEC-NOTES of `isdbs-core`) |

## The boundary to Phase 3

`rx::Frame::main`: the 38 976 main-signal symbols of a frame in
transmission order, derotated (BPSK "0" on the positive real axis, QPSK
and 8PSK on the grid of annexed table 46) and scaled to unit amplitude.
`Frame::index` gives the frame's position in its superframe (0 = the W2
frame), so the inter-frame de-interleaver can align. The slot layout of
the superframe comes from its `TmccInfo` through `isdbs_core::slots::runs`.
Each run is a contiguous range of main symbols with one combination, and
the puncturing restarts at its start.

## Position in the stack

- Depends on: `isdb-frontend`, `isdb-iq` (survey), `isdb-fec`,
  `isdbs-core`, `num-complex`.
- Used by: `isdb-cli` (`isdb iq tmcc`).

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
- Development tools (examples): `syncstat` (sync-metric statistics on and
  off the frame grid), `spacing` (acquisition-only frame spacing),
  `offsets` (metric at ±2 symbols and the derotated burst projection).
