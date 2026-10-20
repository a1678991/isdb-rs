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
- `isdb-radio`: device-neutral SDR layer between `isdbt-phy` and a radio —
  the `IqSink` / `IqSource` traits, `DeviceSpec`
  (`NAME[:key=value,...]`), `RadioConfig`, `TxOptions` (digital backoff,
  checked to 0–40 dB, and transmit-queue sizing) and `RxOptions`
  (receive-queue length in seconds), the `Backend` trait with a registry
  of compiled-in backends (an unknown or left-out backend is reported
  with the `isdb-radio` feature enabling it and the `isdb-cli` feature
  forwarding it), the `Quantizer` for integer sample formats, the
  `QueuedSink` transmit path shared by backends with integer samples
  (`RadioTx`), and an in-memory sink and radio for tests (feature
  `mock`). The HackRF backend (feature `hackrf`, default on) moved here
  from `isdb-cli`; it validates its device arguments, gains, the
  1 MHz–6 GHz / 2–20 Msps range and the baseband filter request without
  opening the device.
- `isdb-iio`: minimal safe wrapper over the system libiio (0.x API) —
  contexts by URI (USB, Ethernet, local), device, channel and debug
  attributes, and TX/RX streams of interleaved `i16` on their own threads
  with bounded queues, underrun/overrun counters, a drain on TX end and a
  stop that cancels a blocked buffer only as a fallback.
- `isdb-radio`: the `pluto` backend (feature `pluto`, default on) for
  AD936x radios with ADI's Pluto firmware (ADALM-Pluto, PlutoSky R2, …)
  over libiio, e.g. `--radio pluto:uri=ip:192.0.2.30` over Ethernet. Device
  arguments `uri`, `channel` (TX1/RX1 or TX2/RX2), `buffer` and, on TX,
  `loopback` (digital TX→RX loopback with the TX LO powered down, for
  tests without radiating); TX gain `att` (attenuation, default the
  maximum 89.75 dB), RX gains `agc` and `gain`. The programmed LO, the
  sample rate the clock chain runs at (512/63 MHz: −0.02 ppm) and the RF
  bandwidth are read back and reported; a rate more than 10 ppm off is
  refused. Opening the TX sets the maximum attenuation and switches the
  DDS off first; closing it restores the maximum attenuation and powers
  the TX LO down. The `Cs12` sample format (12-bit, left-aligned in
  `i16`) is the DAC format of its `QueuedSink`.
- `isdb-cli`: the `pluto` feature (default on) forwarding `isdb-radio/pluto`.
- `isdb-cli`: the `isdb` binary — `tx` (to an IQ file or a radio; radiates
  only with `--rf-enable`), `rx`, `loopback` (software channel with
  impairments), `mux` (including `--relay` of a recording) and `ts`
  (inspect, validate, diff, edit).
- `isdb-cli`: `isdb completions SHELL` prints a shell completion script
  (bash, zsh; also fish, elvish, PowerShell) generated with
  `clap_complete` from the binary's own command line.
- `isdb tx` playback keys (when stdin is a terminal; `--no-keys`,
  `--seek-step S`): space pauses / resumes, ←/→ seek, ↑/↓ change the play
  rate (0.25–16×) of `--relay` and `ts-program` relays. The engine is
  `isdbt_mux::playback` with `Multiplexer::enable_playback_control`,
  `pause`, `resume`, `seek`, `set_rate` and `playback_status`: PSI/SI and
  PCR continue in every state, fast play skips video pictures and audio
  frames within the source's own rate, slow play repeats audio frames, and
  seeks use a 0.5 s index from the whole-file scan. Uncontrolled relay
  output is unchanged (byte-identical).
- `isdb tx --iq-in FILE [--iq-in-format cf32|cs8]`: replay a baseband
  file to a radio or file; the receiver finds its parameters and frame
  start, and the playback keys pause (null frames with the same
  parameters) and seek it.
- `isdbt-phy`: `broadcast_ts` — the 204-byte broadcasting TS of ARIB
  STD-B31 Part 2 §5.5 (studio-to-transmitter interface): lossless codecs
  of `ISDB-T_information` in the dummy bytes of each TSP (Table 5-6), the
  optional shortened RS(204,196) parity, and the IIP (Table 5-8) with
  `modulation_control_configuration_information` (Table 5-10, carrying the
  TMCC information) and `network_synchronization_information` (Table
  5-12, SFN delay control); `BroadcastTsWriter` wraps multiplex frames
  with one IIP per frame, `BroadcastTsReader` delimits frames, splits them
  by layer and recovers the transmission parameters from the IIP, so a
  `Transmitter` can be configured from the stream. `isdbt-phy` now
  depends on `isdb-ts` (CRC_32).
- `isdb-si`: `dsmcc` (DSM-CC data carousel DII / DDB sections and the DII
  module descriptors, ARIB STD-B24 Vol. 3 Ch. 6), `ait` (the Application
  Information Table and its descriptors, B24 Vol. 4 Ch. 5, tags per B10
  Part 1 Table 5-5) and `bxml` (additional_arib_bxml_info,
  arib_bxml_info, ait_identifier_info, the BML Control descriptor data;
  B24 Vol. 2 §9.3, Vol. 3 Annex C, Vol. 4 Table 7-1), all lossless, with
  the `DocumentResolution`, `ApplicationControlCode` and
  `ApplicationVisibility` value tables.
- `isdbt-mux`: BML data broadcasting — `[[service.stream]] kind =
  "bml-carousel"` sends the files of a directory as a data carousel
  (module 0x0000 as a `multipart/mixed` entity with the
  `application/X-arib-resourceList`, or explicit `modules` with direct
  mapping), signalled with stream_type 0x0D, a stream_identifier
  (component_tag 0x40) and a data component descriptor carrying
  additional_arib_bxml_info in the PMT and a data content descriptor in
  the EIT; DII / cycle / rate options, `source_encoding = "utf-8"`
  (BML / CSS / script sources transcoded to EUC-JP on load) and `watch`
  (files edited while on air are re-sent with moduleVersion + 1 and a new
  DII transaction, so pages subscribed to `ModuleUpdated` reload) under
  `[service.stream.bml]` (`docs/mux-config.md`). The example pages
  (`examples/bml/startup.bml`, `info.bml`) show the usual layout — picture
  in the upper-right quarter, menu, text strip — with remote-key
  navigation (arrows, Enter, Back, the four colour keys) and a page
  transition, modelled on live stations' documents. The layouts and the default operational values
  (bml version 3.0, data_event_id 0xF in the PMT, the multipart boundary
  and media type spellings, DII repetition) follow a live terrestrial
  recording. Example `examples/bml-data.toml` with
  `examples/bml/startup.bml` (the terrestrial document prologue, the
  broadcast video / audio kept on screen). Shown on a Sony BDZ-ZW1500
  under a local station's identity (`docs/transmitting.md`).
- `isdbt-mux`: Hybridcast — `[service.hybridcast]` with `url`,
  `organization_id` and `application_id` builds an AIT (application_type
  0x0010, AUTOSTART, profile 0x0002 / 1.1.1, HTTP transport protocol,
  simple application location, boundary and permission descriptors per
  IPTVFJ STD-0013) and carries it as a resource of the service's
  bml-carousel (module 0x9FFF / `app1.ait`, with a generated `startup.bml`
  launcher calling `startAITControlledApp` unless the files have one) or,
  with `carriage = "es"`, on its own PID with ait_identifier_info in the
  PMT. Example `examples/hybridcast.toml`.
- Verification: spec-vector tests, property-tested lossless round trips,
  TX→RX loopbacks, opt-in tests on real captures (`ISDB_RS_*` environment
  variables) and a differential test against gr-isdbt (`oracle/`,
  `VERIFICATION.md`).
- Repository harness: pinned toolchain, rustfmt/clippy/cargo-deny
  configuration, git hooks (lefthook, commitlint), mise tasks as the single
  check entry point, CI workflows (lint, test, doc, cargo-deny, MSRV) and
  Renovate.
- `oracle/Dockerfile`: a reproducible gr-isdbt oracle container (Ubuntu
  24.04, GNU Radio 3.10, gr-isdbt built from upstream at a pinned commit),
  and `oracle/README.md` with the licensing of the oracle scripts.
- `README.ja.md`: a Japanese translation of the root README, linked from
  the first line of `README.md` (the documents it links to stay in
  English).

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
- `isdb-cli`: HackRF support is the optional `hackrf` feature (default on),
  which forwards to `isdb-radio/hackrf`; `isdb-cli` no longer depends on
  `isdb-hackrf` directly and contains no device code.
  `cargo build -p isdb-cli --no-default-features` builds without libhackrf
  (all file-based commands work). CI has a `no-hackrf` job, which also
  checks `isdb-radio` without any backend.
- `isdb-hackrf`: minimum libhackrf 2022.09.1; older libraries are refused
  at open.
- Neutral default identifiers in examples, the relay template and tests:
  network_id / transport_stream_id 0x79EE (placeholder; choose an ID not
  used where your receiver is), area_code 0x5A5.
- **Breaking:**
  - `isdb tx` / `isdb rx` radio options, with no aliases for the old
    spellings: `--hackrf` is now `--radio SPEC` with SPEC
    `NAME[:key=value,...]` (`--radio hackrf`); `--serial S` is now the
    device argument `--radio hackrf:serial=S`; `--txvga N`, `--amp`,
    `--lna N` and `--vga N` are now the repeatable
    `--gain NAME=VALUE` (TX `txvga=0..47`, `amp=on|off`; RX `lna=0..40`
    in 8 dB steps, `vga=0..62` in 2 dB steps, `amp=on|off`), validated by
    the backend. `--bb-filter-hz` is an analog bandwidth request that the
    backend rounds to a supported value. The `--dry-run` text starts with
    `<backend> TX settings:` (` (UHF chN)` with `--channel`), and the
    tuning line no longer repeats the channel. `--backoff-db`, `--rf-enable`,
    `--dry-run`, `--prefill-frames`, `--channel`, `--freq`,
    `--freq-unrestricted` and `--freq-corr-ppm` are unchanged.
  - `isdb tx --dry-run` / `isdb rx` with `--radio` in a build without the
    backend (e.g. `--no-default-features`) fail at the backend lookup and
    no longer print the settings first.
  - The device's frequency range (HackRF 1 MHz–6 GHz) is checked by the
    backend on the programmed, `--freq-corr-ppm`-corrected frequency
    instead of the nominal one. `--bb-filter-hz` above 28 MHz is refused
    by the HackRF backend (libhackrf would program 0 Hz).
  - `isdb rx --json`: the `input` string of a radio reception is
    `radio SPEC[ (UHF chN)], centre … MHz, programmed … Hz; sample rate …`
    instead of `HackRF …`.
  - The header of a relay `--print-config` suggests
    `isdb tx --mux-config THIS.toml … --radio SPEC --channel N --rf-enable`
    (no `--hackrf`, no `--txvga 20` hint).
  - The radio TX status lines count `buffers` instead of `usb buffers`, and
    the `--dry-run` refusal reads `dry run, not transmitting: use
    --rf-enable instead of --dry-run …`.
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
  - `isdbt_mux::relay::Probe` has a `rate` field (the whole-file
    `relay_rate::RateProfile`), and the `ts-program` rate check uses its
    `required_bps` instead of the probe's `mean_bps`: a layer accepted
    before may now be refused (it could not carry the recording).
- Repository harness: mise is the single task runner. The Makefile is
  replaced by mise tasks (`mise.toml`) with the same names and commands
  (`mise run check`, `mise run lint`, `mise run test`, …; `mise tasks`
  replaces `make help`), and the git hooks and CI call them. The `CARGO`
  override of the Makefile is gone.

### Fixed

- `ts-program` relay: the rate check and `--auto-layout` use the rate the
  whole recording needs (`relay_rate`: the smallest constant rate that
  delivers every relayed packet by its PES's decoding time, read once over
  the whole file) instead of the mean over the probed first 30 s. A
  recording that started quieter than it went on was put on a layer below
  its sustained rate; the relay then fell up to seconds behind its PCR
  schedule and receivers dropped its late audio after a few minutes while
  the video kept playing. The probe report adds a `whole file:` line, and
  the 1-s peak warning is gone (the need covers the peaks).
- `ts-program` relay: PES that leave after their decoding time raise a
  run-time warning (`Multiplexer::take_alerts`, printed by `isdb tx`,
  `loopback` and `mux`; at most once a minute per relay), not only a
  counter in the status line.
- `isdb-ts`: `StreamReader` no longer zero-fills a 64 KiB scratch buffer
  for every packet; reading is about ten times faster.
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

- `isdb tx` / `isdb rx` flags `--serial`, `--txvga`, `--lna`, `--vga` and
  `--amp` (and `--hackrf`): use `--radio hackrf:serial=S` and
  `--gain NAME=VALUE` (see Changed).
- Insertion of AAC crc_check into unprotected ADTS input. Computing the
  CRC needs a raw_data_block parser written from ISO/IEC 13818-7, which
  the project does not have, so the default `adts_compliance = "conform"`
  refuses such input with the reason, and `"warn"` sends it unchanged.
  crc_check values of protected input are neither verified nor rewritten.

[Unreleased]: https://github.com/a1678991/isdb-rs/commits/main
