# isdbs-phy

ISDB-S receive and transmit chains on the symbol side of the front end.
Receive: frame and superframe synchronisation, carrier recovery on the
known symbols, TMCC decoding, and the main signal of every superframe to
TS packets per TS_ID. Transmit: TS packets per TS to the symbols of a
superframe, the exact inverse. Symbols come from `isdb-frontend` (shared
with ISDB-S3, and the same bits as the PlutoSky's FPGA front end); frame
layout, codes, the interleaver and the energy dispersal from `isdbs-core`.

## Receive stages

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

## Transmit (`tx`)

`tx::Transmitter::new(info)` takes a `TmccInfo` (built from a slot
layout by `isdbs_core::TmccInfo::assign`, or decoded) and precomputes the
superframe's 8 headers and the sent-slot table; `superframe(&[&[Packet]])`
takes, per relative TS, exactly `packets_per_superframe(r)` packets (its
sent slots per frame × 8; too few or too many is
`TxError::PacketCount`, the caller pads with `tx::NULL_PACKET`) and
`null_superframe()` fills every slot with null packets. The stages, in
transmit order:

1. **Slots**: frame by frame, each sent slot (dummy slots are not sent)
   takes the next packet of the TS its relative TS ID names.
2. **RS(204,188)** (`isdb_fec::RS_204_188`) on each packet.
3. **Energy dispersal, depth-8 interleaver, sync bytes and dummy slots
   dropped** (`isdbs_core::mainsig::frame_bytes`): 203 bytes per sent slot
   per frame.
4. **Inner code and mapping** (`demod::encode_frame_with_state`, the
   inverse of `decode_frame`), per frame: run by run, BPSK r = 1/2,
   punctured QPSK, TC8PSK r = 2/3, the encoder starting in the state the
   header's W2/W3 leaves; `Mapping::default()` (the readings settled on
   air) unless `with_mapping` says otherwise.
5. **Frame** (`tx::frame_symbols`, annexed table 45): the 12 header bytes
   of `isdbs_core::tmcc::headers` (W1, the frame's TMCC block with
   RS(64,48) and dispersal, W2/W3) inner-coded to 192 BPSK symbols, X then
   Y; the 192 bursts; the 38 976 main symbols. The inner coder runs on
   from the previous frame's main signal into the header (only the first
   12 header symbols depend on it; W1 flushes it).

The boundary is `tx::Superframe`: `symbols`, the 319 488
(`SUPERFRAME_SYMBOLS`) unit-modulus symbols in transmission order at one
sample per symbol (pulse shaping is the front end's), and `placed`, the
(frame, slot, TS_ID) of each packet in the order and with the fields of
the `ts::Packet`s a receiver returns. Superframes are independent (the
TMCC, the dispersal and the interleaver each span one), so the caller
can stop and start at any superframe.

**Codes.** `superframe_codes` / `null_superframe_codes` return the same
superframe as `tx::SuperframeCodes`: one 4-bit TX boundary code per
symbol (`isdb_frontend::txformat`, 8 + k for the point e^{jkπ/4} of
annexed table 46; BPSK 0 → 8, 1 → 12), which the FPGA's transmit block or
`isdb_frontend::shape::fixed::Interpolator` shapes. The codes come from
the bits, not from the points: `demod::encode_frame_with_state` and
`encode_frame_codes_with_state`, `frame_symbols` and `frame_codes` are one
encoder each, written over a crate-private `Point` (the `f64` point or
the code, `LabelCodes::TABLE_46`), so code n stands for symbol n by
construction (`qpsk_point`'s 1/√2 and `psk8_point`'s cos π/4 differ by
an ulp, so recovering codes from points would need a nearest-point
search). A conjugated or swapped `Mapping` relabels the codes the same
way. `Transmitter::new` refuses a dummy combination entry (1111) with
slots (`TxError::DummySlots`, BO.1408-1 §6.1.2: the slots it would cover
carry no symbol, 0 + 0j, which no label represents); `TmccInfo::assign`
never builds one.

## Diagnostics

Diagnostics are `tracing` events, never printed; the crate installs no
subscriber (the `isdb` binary does). `info`: the acquisition (carrier,
trim, burst MER), frame sync acquired and lost (`start`, `metric`), the
first TMCC of a buffer and every TMCC change (the slot assignment, order
of change). `debug`: the survey, a reused or rejected prior acquisition,
a frame whose sync words did not decode, each superframe's TMCC and
RS(204,188) outcome (`uncorrectable`, `corrected_bytes`, `mer_db`) and
the buffer's totals. Nothing is emitted per symbol or per packet.

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

`tx::Superframe` (transmit side): a superframe's symbols and the
(frame, slot, TS_ID) of every packet, in `ts::Packet` order (see
[Transmit](#transmit-tx)); `tx::SuperframeCodes` the same with TX
boundary codes.

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
- Used by: `isdb-cli` (`isdb iq tmcc`, `isdb rx --system isdbs`; `tx`
  in `isdb tx` and `isdb loopback --system isdbs`).

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
  TC8PSK at 12 dB SNR (few byte errors, the reverse C1/C0 reading fails),
  `codes_are_the_points` (the codes' points and `psk8_point` of their
  labels equal the symbols within 10⁻¹² on every mix, a frame with unused
  symbols included, under all eight mappings).
- `tx` unit tests: `frame_symbols_follows_table_45` (the coded header,
  the bursts and the main symbols where `frame::*_position` puts them,
  each symbol once; the encoder left in the W2/W3 state),
  `slot_tables` (TSs, slots and packets per superframe of the BS and CS
  layouts), `null_superframe_decodes_to_null_packets` (received
  noiselessly: the TMCC sent, null packets RS-clean where `placed` says),
  `codes_are_the_symbols` (`superframe_codes` against `superframe` over
  three superframes and five mappings), `errors` (every `TxError`, the
  dummy entry with slots first or after a real one).
- Symbol-domain loopback (`tests/loopback.rs`): one null superframe and
  three of distinct packets per TS through `Transmitter`, a carrier
  offset of 2·10⁻⁴ cycle per symbol, a phase and AWGN, after 1 111
  noise-only symbols, then `receive` and `ts::decode_all`: every packet
  back byte-exact per TS_ID and in order, none uncorrectable, every
  superframe's TMCC the one sent, the receiver's (frame, slot, TS_ID) the
  transmitter's `placed`. Layouts: the BS layout (TC8PSK 2/3 × 48, three
  TSs) and TC8PSK 2/3 × 44 + BPSK 1/2 × 4 (two TSs) at 12 dB; the CS
  layout (QPSK 3/4 × 48, one TS), QPSK 7/8 × 40 + QPSK 1/2 × 8 (three
  TSs) and QPSK 5/6 × 30 + QPSK 2/3 × 18 (two TSs) at 8 dB. Each layout
  runs twice: from the symbols, and from the points of a twin
  transmitter's `superframe_codes` (every code checked against its
  symbol through `psk8_point` of its label first), with the same
  outcome.
- Sample-domain loopback (`tests/loopback.rs`
  `sample_domain_bs_layout_at_40_msps`): the BS layout, one null
  superframe, three of distinct packets and one null frame through
  `Transmitter`, `isdb_frontend::shape::Shaper` at 40 Msps (RMS 0.3 of
  full scale) and `frontend::symbols` (the survey's carrier, the trim
  searched on the bursts), then `receive` and `ts::decode_all`: the three
  data superframes byte-exact per TS_ID, none uncorrectable, the trim
  found within ±0.05 symbol of 0 (measured: 0.000, main MER 44.6 dB). The
  front end's loops settle in the first symbols of the lead superframe,
  so its first slots (17 of 384) are uncorrectable: the reason a
  transmission starts with a null superframe.
- Sample-domain loopback through the FPGA's arithmetic
  (`sample_domain_fixed_interpolator_bs_layout_at_40_msps`): the same
  stream as codes (`superframe_codes`), eight to a DMA word, through
  `isdb_frontend::shape::fixed::Interpolator` (ISDB-S at 40 Msps, 10 dB
  backoff), its 12-bit integers into `frontend::symbols`: the data
  superframes byte-exact per TS_ID, none uncorrectable, no stall, the trim
  within ±0.05 (measured: 0.000, main MER 44.5 dB, the realised symbol
  rate 28 859 999.2 Bd of the Q0.24 step).
- `examples/txvectors.rs`: the FPGA transmit block's vectors with real
  codes (`fpga/build/tx-5-isdbs.json`: the first 4 000 codes of a BS data
  superframe after a null one).
- Opt-in recordings, main signal (`ISDB_RS_ISDBS_TS_IQ`): no
  uncorrectable RS word, sync byte on every packet, continuity counters
  without a gap per TS_ID and PID.
- `testsig::Synth` builds its frames with `tx::frame_symbols` (random
  8PSK main symbols, the inner coder running through the headers).
- Development tools (examples): `syncstat` (sync-metric statistics on and
  off the frame grid), `spacing` (acquisition-only frame spacing),
  `offsets` (metric at ±2 symbols and the derotated burst projection),
  `mapprobe` (RS success under each constellation reading), `throughput`
  (time per stage).
