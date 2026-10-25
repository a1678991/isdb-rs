# isdb-frontend

Single-carrier receive front end for ISDB-S and ISDB-S3: IQ samples at
the radio's rate in, symbols at one sample per symbol out (the `Iq` →
`Symbols` stage of the receive chain). The stage exists twice with the
same output bit for bit: here, on the CPU, and in the FPGA of a PlutoSky
R2 ([`fpga/`](../../fpga/README.md)). The radio sends symbols instead
of samples when its link cannot carry the samples. This crate is the
reference: the fixed-point model is the specification the FPGA follows,
and it writes the test vectors the FPGA must reproduce.

Per input sample (the `fixed` module documents every step):

1. **Mixer**: 32-bit NCO, 1024-entry cosine table, removes the carrier
   offset the host measured (`isdb_iq::survey`).
2. **Polyphase matched filter and its derivative**: root-raised cosine
   under a Kaiser window, 64 phases of 32 taps (18-bit coefficients).
3. **Timing recovery**: non-data-aided maximum-likelihood detector
   `Re{z·conj(dz)}`, which needs no carrier lock. It feeds a PI loop on
   the symbol period (Q8.24 samples) that applies each symbol's error six
   symbols later (the latency the FPGA pipeline is allowed).
4. **AGC** on the symbols (gain Q6.10, same delay).
5. **Output**: n-bit I and Q (2..8), framed into the `Symbols` words.

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
| Taps | ISDB-S MER at 8 / 12 / 16 / 24 / 32 taps: 13.75 / 14.33 / 14.65 / 14.86 / 14.94 dB; ISDB-S3 ring MER within 0.1 dB from 24 to 64 taps | 32 (46 DSP48E1 in the FPGA) |
| Timing loop bandwidth | ISDB-S locks at 2·10⁻⁴ R. ISDB-S3 (α = 0.03: the detector's slope shrinks with the roll-off) wanders at 2·10⁻⁴ R with 32 taps (−265 ppm) and locks at 2·10⁻⁵…10⁻⁶ R (+0.68 ppm, the board's clock offset, as on ISDB-S) | 2·10⁻⁴ R for ISDB-S, 5·10⁻⁶ R for ISDB-S3 (`Design::for_system`) |
| Fixed point | Fixed-point and floating-point outputs within 0.03 dB (ring MER, 8PSK MER) on every recording | the model as specified |
| Output width | MER change by the n-bit quantiser (no clipping): ISDB-S 4 / 5 / 6 bits −1.1 / −0.32 / −0.07 dB; ISDB-S3 (ring MER) −0.45 / −0.07 / −0.01 dB | 6 bits for ISDB-S (43.4 MB/s), 5 for ISDB-S3 (42.4 MB/s): about 60 % of the measured ceiling |

## Spec scope

No broadcast specification is implemented here. The symbol rates and
roll-offs come from `isdb_iq::survey::KNOWN_SYSTEMS`, which quotes them
from the transmission standards. The arithmetic and the `Symbols` format
are this project's own definitions (format version 1), so the crate has
no SPEC-NOTES.

## The `Symbols` boundary, version 1

A stream of 16-bit little-endian words in blocks:

- **Header** (9 words): sync words `0x1ACF 0xFC1D`; format word (version,
  bits − 1, log2 of the block length); 32-bit sequence number; gain;
  32-bit period; check word.
- **Payload**: L = 4096 symbols, `I | Q << bits`, packed least
  significant bit first.
- **Trailer**: a CRC-16 (0x1021) of the payload, seeded with the
  sequence number.

The `Reader` accepts a block only when its trailer matches, so every
loss is found, of any size and wherever it falls. Sequence numbers count
the missing blocks, and the blocks around a loss stay in order. The
`format` module has the details.

## Position in the stack

- Depends on: `isdb-iq` (the known systems), `num-complex`, `serde_json`,
  `thiserror`.
- Used by:
  - `isdb-radio`: `Offload::Symbols`, `Backend::capture_symbols`, and
    the Pluto's discovery and configuration through `control`;
  - `isdb-cli`: `isdb symbols`, `isdb iq symbols`;
  - `fpga/`: the test vectors;
  - the planned ISDB-S / ISDB-S3 receivers, which start from its
    symbols.

## Key types and entry points

| Item | Role |
|---|---|
| `params::Design`, `Design::for_system`, `Params::design` | physical settings (rates, roll-off, taps, loop bandwidth, bits) to the integer parameters (the FPGA's registers) |
| `fixed::Frontend` (`push`, `process`) → `Symbol` | the bit-exact model; `Symbol` carries every internal the FPGA test compares |
| `float::FloatFrontend` | the floating-point reference |
| `format::Packer`, `format::Reader` → `Event::{Block, Loss, Truncated}` | the `Symbols` words and their reader |
| `control::{Encoder, Identity, reg}` | the FPGA's command words (through the AD9361 core's `up_adc_gpio_out` / `up_adc_gpio_in`), register map and ID word |
| `metrics::{ring_stats, psk_mer, sqnr_db}` | MER without frame sync, SQNR |
| `vectors::document` | the JSON test vectors for `fpga/` |

## Tests

- **Unit, spec and property tests:**
  - header layout, CRC-16 check value, command word layout;
  - `prop_pack_unpack_round_trip`;
  - `prop_any_loss_is_reported_and_the_rest_kept`: any cut anywhere,
    every untouched block delivered;
  - mixer and timing checks.
- **Opt-in recordings** (`tests/captures.rs`, `ISDB_RS_ISDBS_IQ`,
  `ISDB_RS_ISDBS3_IQ`):
  - timing locked within 5 ppm of the survey;
  - fixed point within 0.1 dB of floating point;
  - ISDB-S 8PSK MER above 10 dB;
  - the words read back to the same symbols.
- **The FPGA side:** `mise run fpga:test` writes vectors with
  `examples/vectors.rs` and simulates the HDL against them.
- **Analysis:** `examples/analyze.rs` prints timing lock, MER and the
  output-width table for a recording (the measurements above).
