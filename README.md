# isdb-rs — ARIB ISDB-T transport and transmission in Rust

> **Legal and safety notice.** Radiating on broadcast (UHF TV) frequencies
> requires a licence — in Japan under the Radio Act (電波法), elsewhere under
> local law. The transmit path (`arib tx --hackrf`) is intended for shielded
> or conducted lab setups only: coax with attenuators straight into the
> receiver, or a shielded enclosure; it radiates only with `--rf-enable`.
> This project does **not** descramble: it has no B-CAS / conditional-access
> support and refuses scrambled input (transport_scrambling_control ≠ '00').
> You are responsible for having the rights to any content you transmit or
> relay. The software is provided under the Apache License 2.0, without
> warranty (see [LICENSE](LICENSE)).

A from-the-spec implementation of the ISDB-T chain: MPEG-2 TS / ARIB SI
generation and analysis, and the ARIB STD-B31 modulator/demodulator, so both
directions (TX: services → IQ → HackRF; RX: IQ → TS → analysis) run on one
verifiable library. Only ISDB-T is implemented today; the name leaves room
for ISDB-S / ISDB-S3.

## Ground truth

The **specifications are the only ground truth**. Other implementations
(e.g. GNU Radio gr-isdbt) are *not* references: they are prior art whose
behaviour may be wrong, and they are used only as independent oracles, run
as separate programs in differential tests once the spec implementation
exists (a disagreement is resolved by reading the spec, never by copying the
oracle). No code or tables of other implementations are included.

| Spec | Scope here | Local text extraction (not distributed) |
|---|---|---|
| ITU-T H.222.0 (05/2006) \| ISO/IEC 13818-1 | TS packets, adaptation field, PCR, PES, sections, PAT/PMT/CAT, CRC_32 | `specs/ITU-T-H.222.0-2006.txt` |
| ARIB STD-B10 v5.13 | SI tables (NIT, SDT, EIT, TOT/TDT, BIT, …), descriptors, time coding | `specs/ARIB-STD-B10-v5.13.txt` |
| ARIB STD-B24 v6.4 Vol.1 Part 2 Ch.7 | 8-unit character coding of SI strings | `specs/ARIB-STD-B24-v6.4-vol1.txt` |
| ARIB STD-B32 v3.11 Part 1 | video/audio coding constraints, stream types | `specs/ARIB-STD-B32-v3.11-part1.txt` |
| ARIB STD-B32 v3.11 Part 2 | MPEG-2 AAC restrictions, audio quality_indicator | `specs/ARIB-STD-B32-v3.11-part2.txt` |
| ARIB STD-B32 v3.11 Part 3 | PES stream_id, PMT stream type identifier table | `specs/ARIB-STD-B32-v3.11-part3.txt` |
| ARIB STD-B31 v2.2 | ISDB-T transmission: remux, FEC, interleaving, mapping, OFDM frame, TMCC/AC | `specs/ARIB-STD-B31-v2.2.txt` |
| ARIB STD-B21 v5.12 | receiver behaviour (scan, SI use) | `specs/ARIB-STD-B21-v5.12.txt` |

The specifications are not part of this repository (they are copyrighted
by their publishers). Download the PDFs from ARIB / ITU (URLs in
[`SPECS.md`](SPECS.md)) and, if you want greppable text, extract them into
the gitignored `specs/` directory with `pdftotext -layout <pdf>
specs/<name>.txt`. Clause/table numbers in code comments refer to these
editions.

Interpretations of ambiguous or garbled spec text are recorded, with the
quoted text, the reasoning and the confirmation status, in each crate's
`crates/<crate>/SPEC-NOTES.md`. The top-level [`SPEC-NOTES.md`](SPEC-NOTES.md)
is an index of those files plus the decisions that span crates (frame
alignment).

## Crates

Each crate has a README (spec scope, position in the stack, entry points
with a doctested example, verification, known limitations).

| Crate | Spec | Scope |
|---|---|---|
| [`arib-ts`](crates/arib-ts/README.md) | H.222.0 | bit I/O, CRC_32, TS packet / adaptation field / PCR, PES, section assembly and packetisation, PAT/PMT/CAT, TS reader |
| [`arib-text`](crates/arib-text/README.md) | B24 Vol.1 Pt.2 Ch.7 | ARIB 8-unit string decode/encode |
| [`arib-si`](crates/arib-si/README.md) | B10 | SI tables, descriptors, MJD/BCD time; lossless typed codecs plus conformance checks |
| [`arib-analyze`](crates/arib-analyze/README.md) | H.222.0, B10 | TS inspection, spec validation with clause citations, table/field diff of two captures, experiment editing |
| [`arib-mux`](crates/arib-mux/README.md) | H.222.0, B10, B31 §3.2 and Part 2 §3.2, B32 Pt.1 | service multiplexer: PSI/SI generation, MPEG-2 video / AAC packetisation, rebroadcast of a recorded program (`ts-program`), CBR output paced to the OFDM frame, layer capacities |
| [`isdbt-core`](crates/isdbt-core/README.md) | B31 §2–3 tables | modes, guard intervals, layer parameters, derived rates |
| [`isdbt-fec`](crates/isdbt-fec/README.md) | B31 §3.2–3.8 | TS re-multiplex (model receiver), RS(204,188), energy dispersal, delay adjustment, byte interleaving, convolutional code / Viterbi |
| [`isdbt-ofdm`](crates/isdbt-ofdm/README.md) | B31 §3.9–3.16 | bit interleaving and mapping/demapping, layer combining, time/frequency interleaving, OFDM frame (SP/CP/TMCC/AC), IFFT/GI, receiver synchronisation and equalisation |
| [`isdbt-phy`](crates/isdbt-phy/README.md) | B31 | TX/RX pipelines: TS packets ↔ complex baseband, IQ file formats |
| [`hackrf`](crates/hackrf/README.md) | — | minimal safe wrapper over the system libhackrf for TX/RX streaming (FFI) |
| [`arib-cli`](crates/arib-cli/README.md) | — | the `arib` binary: `tx`, `rx`, `loopback`, `mux`, `ts` |

[`SPECS.md`](SPECS.md) is the inventory of the specifications and of which
clauses each crate covers.

## Using it

```
cargo build --release -p arib-cli      # → target/release/arib
```

Commands, options and the end-to-end bench recipe (mux → loopback → HackRF
TX → RX → validate/diff against a capture) are in
[`crates/arib-cli/README.md`](crates/arib-cli/README.md).

## Architecture

```text
                       ┌──────────── arib-ts / arib-text / arib-si ────────────┐
                       │  lossless codecs: packets, PES, sections, SI, strings │
                       └───────▲──────────────────────────────────────────▲────┘
                               │ builds with                 decodes with │
TX                             │                                          │
 TOML + media ─► arib-mux::Multiplexer::next_frame()                      │
   (or per-layer TS files / PID split / synthetic, in arib-cli)           │
        │  [Vec<Packet>; 3]: exactly tsp_per_frame 188-byte TSPs per layer│
        ▼                                                                 │
 isdbt-phy::Transmitter::transmit_frame   (one call = one OFDM frame)     │
        │  isdbt-fec::LayerEncoder  §3.3–3.8                              │
        │    RS(204,188) → energy dispersal → delay adj. → byte interleave│
        │    → convolutional code + puncturing          ⇒ coded bits (u8) │
        │  isdbt-ofdm::Modulator    §3.9–3.16                             │
        │    delay adj. + bit interleave + mapping → layer combining      │
        │    → time / frequency interleave → SP/CP/TMCC/AC frame          │
        │    → IFFT + guard interval                   ⇒ C32 @ 512/63 MHz │
        ▼                                                                 │
 IQ file (cf32 / cs8 via isdbt-phy::iq)  or  hackrf::TxStream (cs8)       │
                                                                          │
RX                                                                        │
 IQ file  or  hackrf::RxStream                                            │
        │                                                                 │
 isdbt-phy::Receiver / PipelinedReceiver::push(samples)                   │
        │  isdbt-ofdm::Demodulator: sync, CFO, channel estimation,        │
        │    TMCC decode → Params, inverse chain   ⇒ DemodFrame (f32 LLRs)│
        │  isdbt-phy::FrameDecoder → isdbt-fec::LayerDecoder:             │
        │    depuncture + Viterbi → byte deinterleave → descramble        │
        │    → RS decode                           ⇒ per-layer TSPs       │
        │  isdbt-fec::MultiplexPattern::reproduce_ts  (§3.2.2 model       │
        │    receiver: multiplex frame with null TSPs in unused slots)    │
        ▼                                                                 │
 RxFrame { tx_frame, valid, layers, ts, tmcc, metrics, rs }               │
        ▼                                                                 │
 TS file ─► arib-analyze (scan → inspect / validate / diff / edit) ───────┘
```

`isdbt-core` (parameters and spec tables) is used by every PHY crate;
`arib-cli` wires all of the above into the `arib` binary (threads, pacing,
radio control, reports).

**Frame and latency contract.** Everything is clocked by the OFDM frame
(204 symbols). Per frame and active layer, the multiplexer delivers and the
transmitter consumes exactly `tsp_per_frame` TSPs (Table 3-3 × segments;
the §3.2 multiplex frame for the 13-segment TS). The modulator holds one
frame (Part 2 Fig. 3-8 one-frame buffer, the 2-symbol bit-interleave
advance): the call that receives input frame `k+1` emits on-air frame `k`,
so the first emitted frame is on-air frame −1. The demodulator outputs the
LLRs of data frame `m − L` with on-air frame `m`, where
`L = isdbt_ofdm::latency::latency_frames` (the largest Table 3-12 delay of
the active layers; layers with less time interleaving are aligned by a
receiver FIFO), and the FEC pair adds `isdbt_fec::layer::LATENCY_FRAMES` = 1
(§3.6 delay adjustment + byte interleaving), so `RxFrame::tx_frame =
frame_number − L − 1` and `valid` turns true once every deinterleaver is
primed. Energy dispersal and puncturing restart at every OFDM frame. On the
TX side, `Transmitter::preroll` discards `1 + L + 1` frames so the first
emitted frame is ordinary OFDM, `lead_frames` (2) null frames let a receiver
that locks one frame late still output every packet, and `flush_frames`
(`1 + L + 1 + 1`) null frames after the data push the last packets through
the interleavers and the receiver's channel-estimation look-ahead. Details:
[SPEC-NOTES.md](SPEC-NOTES.md) (cross-crate decision) and the
`isdbt_ofdm::latency` module docs.

## How it is verified

Every layer must be verifiable without trusting the implementation itself:

1. **Spec vectors** — values printed in the specs (check values, tables,
   PRBS initial states, constellation points, frame lengths, TSP counts) are
   asserted directly in tests that cite the table.
2. **Lossless round trips** — decoders keep every bit (reserved bits
   included); `encode(decode(x)) == x` is property-tested (proptest) and run
   over synthetic multiplexes and (optionally) over every section of a real
   capture (during development, all 144 distinct SI sections of a live
   terrestrial capture decoded and re-encoded byte-identically).
3. **Inverse pairs** — each TX stage has its RX inverse tested as an
   identity (interleavers, mappers, codes), and the whole PHY is tested as
   TX → channel impairments → RX, byte-exact.
4. **Real-world data** — live ISDB-T transport-stream captures must
   parse with zero errors (optional tests, pointed at your own capture with
   environment variables; see below); consumer receivers (a USB tuner and a
   Blu-ray recorder, see the receiver compatibility notes in
   [`crates/arib-mux/SPEC-NOTES.md`](crates/arib-mux/SPEC-NOTES.md)) must
   accept our TX over a conducted link. (No off-air ISDB-T IQ capture has
   been tested yet; see the open questions in
   [`VERIFICATION.md`](VERIFICATION.md).)
5. **Independent oracles** — gr-isdbt (TX↔RX cross-decoding) and ffprobe
   (media) are run differentially as external programs. They detect
   disagreement; the spec decides who is right.

The gr-isdbt differential results (both directions byte-exact in 8
configurations, and the gr-isdbt deviations from B31 found on the way) are in
[`VERIFICATION.md`](VERIFICATION.md).

Conformance of field *values* (reserved bits = '1', allowed table_id per
PID, repetition rates) lives in validators, separately from the lossless
codecs, so captures that violate the spec can still be decoded and edited.

## Development

Toolchain: Rust ≥ 1.88, edition 2024 (`Cargo.toml`). The dev profile
builds the `isdbt-*` crates and all dependencies with `opt-level = 3`
(`Cargo.toml`): unoptimised, the modulator reaches only ≈ 0.2× real time
and a HackRF fed by `cargo run -p arib-cli -- tx …` underruns ≈ 75 % of its
USB buffers (details in `crates/arib-cli/README.md`). The `hackrf` crate
links the system `libhackrf` dynamically (`crates/hackrf/build.rs`; Arch
package `hackrf`, Debian/Ubuntu `libhackrf-dev`), so the library must be
installed to link `arib` and the `hackrf` tests/examples even when no radio
is attached; no test opens a device.

```text
cargo build --workspace                       # libraries, examples, the arib binary
cargo build --release -p arib-cli             # → target/release/arib
cargo test --workspace                        # all tests ([profile.test] is opt-level 2)
cargo test --workspace --release              # same, faster PHY loopbacks
cargo test --workspace --doc                  # doctests, incl. every crate README's code blocks
cargo clippy --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Heavy tests are `#[ignore]`d and run explicitly, in release:

```text
cargo test --release -p isdbt-ofdm --test loopback -- --ignored --nocapture   # stress (long echo, seeds) + real-time factor
cargo test --release -p isdbt-fec viterbi_throughput -- --ignored --nocapture
cargo test --release -p arib-cli --test cli -- --ignored mux_ts_producer_throughput --nocapture   # producer ≥ 2× real time
```

**Tests that use real data (optional).** Captures, recordings and media
are never committed. The tests that use them read a path from an
environment variable and skip themselves (printing `SKIP`) when it is unset
or the file is missing, so a fresh checkout passes with no data:

| Variable | Points at | Tests |
|---|---|---|
| `ISDB_RS_CAPTURE_TS` | an unscrambled terrestrial ISDB-T capture (188-byte TS) | `arib-ts` `tests/capture.rs`, `arib-si` `tests/capture.rs`, `arib-text` `capture_si_strings`, `arib-analyze` `tests/real_data.rs`, `arib-mux` `tests/adts.rs` `capture_all_frames_conform` |
| `ISDB_RS_OUR_TS` | a TS produced by `arib mux` (compared with the capture) | `arib-analyze` `ours_vs_capture`, `transplant_nit_between_captures_is_crc_and_cc_clean` |
| `ISDB_RS_RELAY_TS` | an unscrambled recording you may relay, for `ts-program` | `arib-cli` `relay_recording`, `relay_recording_without_toml` |
| `ISDB_RS_MEDIA_DIR` | a directory with bench media (`v63p.m2v` MPEG-2 video, `a.m2a` ADTS AAC) | `arib-mux` `tests/media.rs` (also needs `ffprobe`) and `tests/adts.rs` `bench_media_refused_by_conform`, `arib-cli` `loopback_with_in_process_multiplexer` (media part), `bench_media_aac_conforms_to_b32_part2` |

The exact variable set per test is documented in each crate's README.
Fixtures that are committed are synthetic: generated by this project's own
encoders.

**gr-isdbt oracle.** `oracle/` holds the differential test against GNU
Radio gr-isdbt (an external GPL-3.0 program that is *not* distributed here;
the scripts import it at run time): `grisdbt_tx.py` / `grisdbt_rx.py` (run
inside a container with GNU Radio 3.10 and gr-isdbt), `ts_compare.py`,
`iq_compare.py`, `run_matrix.py` (the whole matrix) and `rust_tx3/` (a
stand-alone crate, not a workspace member: a three-layer `tx_file` and the
rotation-numbering check `rx_rot`). They drive the `isdbt-phy` examples
`tx_file` / `rx_file`. The container is expected to have this repository
mounted at `/work` (name `arib-sdr` by default, `ISDB_RS_ORACLE_CONTAINER`);
output goes to `target/oracle-matrix/` (`ISDB_RS_ORACLE_OUT`).
Reproduction commands and results: [VERIFICATION.md](VERIFICATION.md).

## Conventions

- Every public item's doc comment names its clause/table, e.g.
  `/// ARIB STD-B10 §5.2.6, Table 5-5`.
- Spec-vector tests are named `spec_<table>_…`; property tests `prop_…`.
- No `unsafe` outside FFI crates; no panics on malformed input in decoders
  (return `arib_ts::Error`).
- Soft bits are `f32` LLRs, `ln P(b=0)/P(b=1)` (positive ⇒ 0).

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](LICENSE); see
also [NOTICE](NOTICE)). Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this work shall be
licensed as above, without any additional terms or conditions.
