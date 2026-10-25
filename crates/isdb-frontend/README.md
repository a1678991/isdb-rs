# isdb-frontend

Single-carrier receive front end for ISDB-S and ISDB-S3: IQ samples at
the radio's rate in, symbols at one sample per symbol out (the `Iq` →
`Symbols` stage of the receive chain). The stage exists twice with the
same output bit for bit: here, on the CPU, and in the FPGA of a PlutoSky
R2 ([`fpga/`](../../fpga/README.md)). The radio sends symbols instead
of samples when its link cannot carry the samples. This crate is the
reference: the fixed-point model is the specification the FPGA follows,
and it writes the test vectors the FPGA must reproduce.

Per input sample (the `fixed` module documents every step; format
version 2):

1. **Mixer**: 32-bit NCO, 1024-entry cosine table, removes the carrier
   offset the host measured (`isdb_iq::survey`).
2. **Three polyphase filter banks** over the same delay line: the matched
   filter `h` (root-raised cosine under a Kaiser window), its derivative
   `g`, and the output filter `k` (the matched filter evaluated a trim
   later inside the same window, [below](#the-timing-trim)); 64 phases of
   32 taps, 18-bit coefficients.
3. **Timing recovery**: non-data-aided maximum-likelihood detector
   `Re{z·conj(dz)}` on `h` and `g`, which needs no carrier lock. It feeds
   a PI loop on the symbol period (Q8.24 samples) that applies each
   symbol's error six symbols later (the latency the FPGA pipeline is
   allowed).
4. **AGC** on the output symbols `zk` (gain Q6.10, same delay).
5. **Output**: n-bit I and Q (2..8) of `zk`, framed into the `Symbols`
   words.

The front end does not recover the carrier phase: its symbols rotate
slowly with the residual offset. Carrier recovery runs on the host after
the boundary, where the frame structure (TMCC, bursts, pilots) is
available.

## Design choices (measured 2026-10-20)

All on the lab's satellite IF (PlutoSky R2, Tezuka v0.3.24, 8-bit
captures at 40 Msps with 56 MHz analog bandwidth; details in
[VERIFICATION.md](../../VERIFICATION.md#symbol-front-end-iq-to-symbols-on-the-cpu-and-in-the-fpga)).

| Question | Measurement | Choice |
|---|---|---|
| Why not stream IQ? | iiod streams 8-bit IQ loss-free for 5 min up to 36 Msps (71.9 MB/s) with Tezuka's services stopped, and drops at 80 MB/s; a raw TCP send from the board manages 59.8 MB/s (one stream) to 69.5 MB/s (two, 89 % CPU) | IQ for ISDB-S needs ≥ 38.96 Msps ((1 + α)·R), ≥ 78 MB/s in 8 bits: above the ceiling. At 36 Msps it fits only by aliasing the roll-off (−0.9 dB MER) and with no margin. Send symbols instead |
| Sample rate | BS1 8PSK MER 14.9 dB at 40 Msps and 56 MHz bandwidth, against 15.2 dB at 57.72 / 61.44 Msps; 13.9 dB with 40 MHz and 13.0 dB with 32 MHz analog bandwidth | 40 Msps (≥ (1 + α)·R for both systems), analog bandwidth 56 MHz |
| Taps | ISDB-S MER at 8 / 12 / 16 / 24 / 32 taps: 13.75 / 14.33 / 14.65 / 14.86 / 14.94 dB. ISDB-S3 (2026-10-25, trimmed, sync-symbol Es/N0 on BS7): 32 / 48 / 64 taps 14.96 / 15.16 / 15.15 dB | 32 (64 DSP48E1 in the FPGA with the output bank) |
| Timing loop bandwidth | ISDB-S locks at 2·10⁻⁴ R. ISDB-S3 (α = 0.03: the detector's slope shrinks with the roll-off) wanders at 2·10⁻⁴ R with 32 taps (−265 ppm) and locks at 2·10⁻⁵…10⁻⁶ R (+0.68 ppm, the board's clock offset, as on ISDB-S) | 2·10⁻⁴ R for ISDB-S, 5·10⁻⁶ R for ISDB-S3 (`Design::for_system`) |
| Fixed point | Fixed-point and floating-point outputs within 0.03 dB (ring MER, 8PSK MER) on every recording | the model as specified |
| Output width | MER change by the n-bit quantiser (no clipping): ISDB-S 4 / 5 / 6 bits −1.1 / −0.32 / −0.07 dB. ISDB-S3, measured after the trim on BS7 / 11 / 17 (2026-10-25): 16APSK ring MER 4 / 5 / 6 / 7 bits −0.6 / −0.16…−0.19 / −0.04 / −0.01 dB, sync-symbol Es/N0 −1.0 / −0.27 / −0.07 / −0.02 dB; nothing reaches full scale. (The earlier "5 bits" for ISDB-S3 was measured through the untrimmed filter, whose 9 dB hid the quantiser's cost) | 6 bits for both: ISDB-S 43.4 MB/s, ISDB-S3 51.0 MB/s, 62 % and 73 % of the measured 70 MB/s ceiling |
| Timing trim | The loop locks 0.15–0.18 T off the best instant on ISDB-S3, 0.05–0.10 T on ISDB-S ([below](#the-timing-trim)) | output bank `k` with a trim the host measures (format 2) |

## The timing trim

Measured on BS7, BS11 and BS17 (ISDB-S3, 16APSK) at 61.44 Msps and 56 MHz
analog bandwidth, 2026-10-25, with the sync-symbol Es/N0 of
[`s3sync`](src/s3sync.rs) (24 known π/2-BPSK symbols per 9 296-symbol
modulation slot):

| Where the SNR goes | Measurement (example) | Es/N0 |
|---|---|---|
| In-band SNR from the spectrum (2 MHz gap floor between transponders, approximate) | `isdb-iq` band power | ≈ 17 dB |
| Data-aided timing per slot, 128-tap filter | `s3budget` `slot` | 15.4 / 14.7 / 14.7 dB |
| Same filter, the loop's instant | `s3budget` `slot` | 9.5 / 9.6 / 9.0 dB |
| The loop's timing error | `s3budget` `slot` | −0.153 / −0.165 / −0.181 T, ±0.02 T rms, steady over 0.55 s and across sample rates (40 / 61.44 Msps), formats and analog bandwidths (32 / 49 / 56 MHz) |
| Filter length at ideal timing: 32 / 64 / 128 taps | `s3budget` `fir` | 14.39 / 14.73 / 14.80 dB (BS7) |
| Neighbouring transponders (brick-wall channel filter ahead) | `s3budget` `ideal`, `pre` | no change |
| Loop bandwidth 10⁻⁶ … 2·10⁻⁵ R, jitter 0.004–0.03 T | `s3budget` `loop` | within 0.1 dB |
| Fixed point against floating point | `s3budget` `fixed`, `float` | within 0.3 dB |

The detector is unbiased on clean synthetic signals, random QPSK and
ISDB-S3-structured streams with 8PSK or 16APSK data alike (lock offset
within ±0.05 T, `examples/tedbias.rs`), so the offset comes from the
received pulse, not from the detector or the arithmetic. Which part of
the chain distorts it (satellite transponder, cable feed or the radio's
analog filter) is not settled; it is the same on all three ISDB-S3
transponders and has the same sign, smaller in symbols, on ISDB-S
(−0.05 … −0.10 T on 21 transponders, where α = 0.35 makes the pulse
less sensitive).

Format 2 therefore evaluates the output a fixed **trim** away from where
the loop locks: the output bank `k` is the matched filter at τ + trim
inside the same window, while the loop keeps `h` and `g`. The trim
cannot be applied to the instant where `z` and `dz` are computed: the
loop would follow it. The host measures it before a run on known
symbols the front end cannot see: `s3sync::best_trim` (ISDB-S3 sync
words) and `isdbs_phy::frontend::measure_trim` (ISDB-S bursts), on
0.1–0.15 s of IQ. The optimum is flat within ±0.03 T (under 0.5 dB).

Result over whole recordings, 32 taps, 8-bit output (`isdb iq s3sync`):

| | Trim | Untrimmed | Trimmed |
|---|---|---|---|
| BS7 | −0.168 T | 9.31 dB | 14.65 dB |
| BS11 | −0.165 T | 9.46 dB | 14.67 dB |
| BS17 | −0.180 T | 8.83 dB | 14.58 dB |

The rest of the gap to the per-slot ideal (0.7–1.2 dB) and to the
spectrum's estimate is intersymbol interference a fixed filter cannot
remove: an equaliser on the host, after the boundary, where the pilots
(STD-B44 Annex A) are available. On ISDB-S the trim raises the burst
MER by 0.1–2.3 dB (median 1.3 dB) on all 21 transponders; their TMCC
and TS stay RS-clean.

## The fast implementation

`fast::FastFrontend` computes exactly the reference's integers, only
differently: the newest `taps` mixer outputs kept contiguous (no modulo
per tap), the three banks of a phase fused, and the dot products as
16×16-bit products summed in `i32` on coefficients split as
`hi·2⁹ + lo` (both `i16`), which LLVM vectorises (`pmaddwd` at the
x86-64 baseline, `smlal` on aarch64). The sums fit `i32` while the last
`taps` inputs stay within 13 bits (|x| ≤ 4 095; the radios deliver 12):
a mixer output is then at most 8 190 in magnitude and a phase's
coefficient magnitudes sum to at most 2¹⁷ − 1, so each partial sum stays
below 64 × 512 × 8 190 < 2²⁸. While a larger input is in the delay line
an `i64` kernel on the unsplit coefficients is used, so the equality
holds for every `i16` input. The padded tap count is a const generic
chosen once per `process` call.

On a 0.826 s ISDB-S3 block at 40 Msps (27.9 M symbols, 32 taps): the
reference 2.2 s, the fast one 0.93–1.05 s (`process`), about 0.9 s with
`-C target-cpu=native`; one thread, the timing loop being sequential
(`examples/fespeed.rs`; `examples/feprof.rs` for `valgrind
--tool=callgrind`). Equality: `tests/fast.rs` (property tests over
designs, widths, trims, rates, amplitudes to full-range `i16` and any
chunking; opt-in recordings through `ISDB_RS_ISDBS3_IQ` /
`ISDB_RS_ISDBS_IQ`). The FPGA vectors stay made with the reference.

## Spec scope

The front end implements no broadcast specification: the symbol rates
and roll-offs come from `isdb_iq::survey::KNOWN_SYSTEMS`, which quotes
them from the transmission standards, and the arithmetic and the
`Symbols` format are this project's own definitions (format version 2).
[`s3sync`](src/s3sync.rs) reads ARIB STD-B44 v2.1 Fig. 3.3-1 (slot
layout, FSync / !FSync / SSync) and §3.8 (π/2-shift BPSK) to measure
SNR; it decodes nothing, so the crate has no SPEC-NOTES. One reading
there is an observation only: on the lab's signals one odd slot in 60
(once per 120-slot frame) has the opposite sign to the other odd slots;
which of FSync and !FSync that is depends on the bit order and
reference phase assumed, and a TMCC decoder will settle it.

## The `Symbols` boundary, versions 1 and 2

A stream of 16-bit little-endian words in blocks (the same layout in
both versions; version 2 is the arithmetic with the output bank, and a
reader accepts both):

- **Header** (9 words): sync words `0x1ACF 0xFC1D`; format word (version,
  bits − 1, log2 of the block length); 32-bit sequence number; gain;
  32-bit period; check word.
- **Payload**: L = 4096 symbols, `I | Q << bits`, packed least
  significant bit first.
- **Trailer**: a CRC-16 (0x1021) of the payload, seeded with the
  sequence number.

The `Reader` accepts a block only when its trailer matches and the
block is framed: the next block's header starts right after its trailer
(or the stream ends exactly there). The CRC alone let a splice through
on a 16-bit coincidence: a property-test seed glued an intact header to
later payload with a matching word in the trailer position
(`examples/formatcut.rs`); the framing check rejects it, because a
spliced block ends at an arbitrary point. So every loss is found, of
any size and wherever it falls, and no damaged block is delivered. The
price is one block per loss: the intact block just before a loss is not
framed either, so it is counted as lost with the damaged one. Delivery
lags the input by at most one block. Sequence numbers count the missing
blocks, and the blocks around a loss stay in order. The `format` module
has the details.

## Position in the stack

- Depends on: `isdb-iq` (the known systems), `num-complex`, `serde_json`,
  `thiserror`.
- Used by:
  - `isdb-radio`: `Offload::Symbols`, `Backend::capture_symbols`, and
    the Pluto's discovery and configuration through `control`;
  - `isdb-cli`: `isdb symbols`, `isdb iq symbols`;
  - `fpga/`: the test vectors;
  - `isdbs-phy` and `isdbs3-phy`, the ISDB-S / ISDB-S3 receivers,
    through `fast::FastFrontend`.

## Key types and entry points

| Item | Role |
|---|---|
| `params::Design`, `Design::for_system`, `Params::design` | physical settings (rates, roll-off, taps, loop bandwidth, bits, trim) to the integer parameters (the FPGA's registers and banks) |
| `fixed::Frontend` (`push`, `process`) → `Symbol` | the bit-exact model; `Symbol` carries every internal the FPGA test compares |
| `fast::FastFrontend` (`push`, `process`, `process_each`) | the same `Symbol`s, 2–2.4× faster: what the receivers run (below) |
| `float::FloatFrontend` | the floating-point reference |
| `format::Packer`, `format::Reader` → `Event::{Block, Loss, Truncated}` | the `Symbols` words and their reader |
| `control::{Encoder, Identity, reg}` | the FPGA's command words (through the AD9361 core's `up_adc_gpio_out` / `up_adc_gpio_in`), register map and ID word |
| `metrics::{ring_stats, psk_mer, sqnr_db}` | MER without frame sync, SQNR |
| `s3sync::{measure, measure_at, best_trim, derotated_data}` | ISDB-S3 slot sync (the search on every core, bit for bit as one thread) and sync-symbol Es/N0, or the same at a known offset; the trim search; data symbols for ring statistics |
| `vectors::document` | the JSON test vectors for `fpga/` |

## Tests

- **Unit, spec and property tests:**
  - header layout (versions 1 and 2), CRC-16 check value, command word
    layout;
  - the output bank without trim equals `h`, and a trim changes only
    the output, not the loop's first symbols;
  - the slot sync measures the SNR of a synthetic stream within 1 dB;
  - `prop_pack_unpack_round_trip`;
  - `prop_any_loss_is_reported_and_the_rest_kept`: any cut anywhere,
    every untouched block delivered;
  - mixer and timing checks.
- **Opt-in recordings** (`tests/captures.rs`, `ISDB_RS_ISDBS_IQ`,
  `ISDB_RS_ISDBS3_IQ`):
  - timing locked within 5 ppm of the survey;
  - fixed point within 0.1 dB of floating point;
  - ISDB-S 8PSK MER above 10 dB;
  - the words read back to the same symbols;
  - ISDB-S3: the trim measured on 0.1 s raises the sync-symbol Es/N0 by
    more than 3 dB and above 13 dB, and 6 bits lose less than 0.15 dB
    of it.
- **The FPGA side:** `mise run fpga:test` writes vectors with
  `examples/vectors.rs` and simulates the HDL against them.
- **Analysis:** `examples/analyze.rs` prints timing lock, MER and the
  output-width table for a recording; `examples/s3budget.rs` is the
  loss budget above, `examples/s3trim.rs` the trim end to end with the
  output-width study, `examples/tedbias.rs` the detector on synthetic
  signals; `isdbs-phy`'s `examples/trimscan.rs` the trim on ISDB-S.
