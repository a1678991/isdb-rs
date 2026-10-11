# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
All crates of the workspace share one version (`[workspace.package]`).

## [Unreleased]

Initial feature set, implemented from the published specifications
(ITU-T H.222.0 | ISO/IEC 13818-1, ARIB STD-B10, STD-B24, STD-B31,
STD-B32; editions in `SPECS.md`). Only ISDB-T is implemented.

### Added

- `isdb-ts`: MPEG-2 transport stream layer — bit I/O, CRC_32, TS packets,
  adaptation field and PCR, PES, section assembly and packetisation,
  PAT/PMT/CAT, a TS reader for 188/192/204-byte framings.
- `isdb-text`: ARIB STD-B24 8-unit character coding, decode and encode.
- `isdb-si`: ARIB STD-B10 service information tables and descriptors with
  lossless typed codecs (reserved bits kept), MJD/BCD time and separate
  conformance checks.
- `isdb-analyze`: TS inspection, spec validation with clause citations,
  table/field diff of two streams and experiment editing.
- `isdbt-mux`: service multiplexer — PSI/SI generation, MPEG-2 video and
  AAC (ADTS, STD-B32 Part 2 rules) packetisation, rebroadcast of a
  recorded program (`ts-program`), CBR output paced to the OFDM frame,
  layer capacities.
- `isdbt-core`: ISDB-T transmission parameters (modes, guard intervals,
  layer parameters, derived rates; STD-B31 tables).
- `isdbt-fec`: STD-B31 §3.2–3.8 channel coding — TS re-multiplex, RS(204,188),
  energy dispersal, delay adjustment, byte interleaving, convolutional
  code with puncturing and a Viterbi decoder.
- `isdbt-ofdm`: STD-B31 §3.9–3.16 — bit interleaving and mapping,
  layer combining, time/frequency interleaving, OFDM frame (SP/CP/TMCC/AC),
  IFFT/guard interval, and receiver synchronisation and equalisation.
- `isdbt-phy`: TX/RX pipelines between TS packets and complex baseband,
  IQ file formats (cf32/cs8).
- `isdb-hackrf`: minimal safe wrapper over the system libhackrf for TX/RX
  streaming.
- `isdb-cli`: the `isdb` binary — `tx` (to an IQ file or a HackRF; radiates
  only with `--rf-enable`), `rx`, `loopback` (software channel with
  impairments), `mux` (including `--relay` of a recording) and `ts`
  (inspect, validate, diff, edit).
- Verification: spec-vector tests, property-tested lossless round trips,
  TX→RX loopbacks, opt-in tests on real captures (`ISDB_RS_*` environment
  variables) and a differential test against gr-isdbt (`oracle/`,
  `VERIFICATION.md`).
- Repository harness: pinned toolchain, rustfmt/clippy/cargo-deny
  configuration, git hooks (lefthook, commitlint), a Makefile as the single
  check entry point, CI workflows (lint, test, doc, cargo-deny, MSRV) and
  Renovate.
- `oracle/Dockerfile`: a reproducible gr-isdbt oracle container (Ubuntu
  24.04, GNU Radio 3.10, gr-isdbt built from upstream at a pinned commit),
  and `oracle/README.md` with the licensing of the oracle scripts.

### Changed

- Crates and binary renamed: `arib-ts` → `isdb-ts`, `arib-text` →
  `isdb-text`, `arib-si` → `isdb-si`, `arib-analyze` → `isdb-analyze`,
  `arib-mux` → `isdbt-mux`, `hackrf` → `isdb-hackrf`, `arib-cli` →
  `isdb-cli`; the binary `arib` is now `isdb`. The `isdbt-*` PHY crates keep
  their names. The default oracle container name is now `isdb-oracle`
  (`ISDB_RS_ORACLE_CONTAINER` still overrides it).
- `ts-program` relay, conditional access: input must be unscrambled
  (transport_scrambling_control '00') and scrambled input is refused; the
  tool never descrambles. The source's CA signalling is now kept by default
  (`ca = "keep"`, CLI `--ca keep`): the PMT's conditional_access_descriptors
  (program and relayed ES loops) and the ECM PIDs, and the CAT's
  CA_descriptors with the EMM PIDs present in the source, following the PID
  mapping. `ca = "drop"` (`--ca drop`) removes them on explicit request.
- `ts-program` relay, copy control: the source value is detected from the
  PMT program loop, the relayed ES loops, the SDT and EIT p/f (0xC1 and
  0xDE; precedence PMT > ES > SDT > EIT, per-component ES-loop 0xC1 kept)
  and preserved by default. An explicit value is used as given, and an
  `info` line prints the source value next to what is relayed. Raw
  descriptors, byte patches or section replacements that can set a relayed
  service's copy control are reported with the value actually sent.
- `isdb tx`/`rx`: `--freq` and `--channel` are limited to 470–710 MHz (UHF
  channels 13–52) unless `--freq-unrestricted` is given.
- `isdb-cli`: HackRF support is the optional `hackrf` feature (default on);
  `cargo build -p isdb-cli --no-default-features` builds without libhackrf
  (all file-based commands work). CI has a `no-hackrf` job.
- `isdb-hackrf`: minimum libhackrf 2022.09.1; older libraries are refused
  at open.
- Neutral default identifiers in examples, the relay template and tests:
  network_id / transport_stream_id 0x79EE (placeholder; choose an ID not
  used where your receiver is), area_code 0x5A5.
- **Breaking:**
  - `isdbt_core::Params` and `LayerConfig` fields are private, with
    accessors (construction is validated); `ParamError` is an enum.
  - `isdbt_phy::PipelinedReceiver::new` returns a `Result`.
  - Public error enums and growing configuration enums are
    `#[non_exhaustive]`.
  - `isdbt_mux::relay::Probe::source_copy_control` is a
    `SourceCopyControl` with findings (no `Option`);
    `relay::expanded_stream` takes `keep_copy_control`; stream
    configurations have a `ca` field; `[eit_schedule] reference` is a
    validated enum (`"today"` | `"month"`).

### Fixed

- `ts-program` relay: a due packet whose T-STD buffer would overflow now
  waits up to 2 ms (`relay::RETIME_HOLD`), so output slot jitter no longer
  overflows MBn for sources multiplexed without buffer headroom.
- `ts-program` relay: read errors on the recording are reported instead of
  being treated as end of file.
- `isdb-hackrf`: the callback context is kept alive when
  `hackrf_stop_tx`/`hackrf_stop_rx` fails, closing a possible
  use-after-free.
- `isdb ts validate --expect-services` parses its argument without
  panicking.

### Removed

- Insertion of AAC crc_check into unprotected ADTS input. Computing the
  CRC needs a raw_data_block parser written from ISO/IEC 13818-7, which
  the project does not have, so the default `adts_compliance = "conform"`
  refuses such input with the reason, and `"warn"` sends it unchanged.
  crc_check values of protected input are neither verified nor rewritten.

[Unreleased]: https://github.com/a1678991/isdb-rs/commits/main
