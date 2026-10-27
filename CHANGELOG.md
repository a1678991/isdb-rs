# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
All crates of the workspace share one version (`[workspace.package]`).

## [Unreleased]

Initial feature set, implemented from the published specifications
(ITU-T H.222.0 | ISO/IEC 13818-1, ARIB STD-B10, STD-B24, STD-B31,
STD-B32; editions in `SPECS.md`). ISDB-T is implemented end to end;
ISDB-S (ITU-R BO.1408-1, MIC Ordinance No. 87 of 2011) is received to
transport streams; ISDB-S3 (ARIB STD-B44) to TLV streams.

### Added

- `isdb-cli`: the left-hand-circular BS and 110° CS channels: `BS2`..`BS24`
  (even) and `ND1`..`ND23` (odd) are accepted by `--freq` / `--channel`
  and tuned at the IF of the left-hand LNB LO, 9.505 GHz (MIC Ordinance
  No. 95 of 2011, Art. 18–19; IF 2224–3224 MHz), e.g. `--freq BS14`
  (NHK BS8K) at 2471.82 MHz; the right-hand channels keep the 10.678 GHz
  LO. Labels read ` (BS14 IF, left-hand)`. Receiving them needs a feed
  that carries the left-hand IF (the lab's cable feed does not).
  `isdb capture` records `isdb:lnb_lo_hz` and `isdb:polarisation`
  (`right` / `left`) next to `isdb:channel_rf_hz` for a satellite
  channel (`isdb` SigMF extension 0.3.0; older recordings, without them,
  read as before). `tools/tlv-nit-polarisation.py` lists the frequency
  and polarisation of each satellite delivery system descriptor in a TLV
  stream file.

- `isdb-mmt` (new): the content of ISDB-S3 TLV streams per ARIB STD-B60:
  MMT-SI descriptors (16-bit tags, the length width of each descriptor's
  structure), the MPT with its assets and locations, PLT, MH-EIT, MH-SDT,
  MH-TOT, MH-BIT, TLV-NIT and AMT (CRC-32 checked, repeats not decoded
  again), divided and aggregated control messages, MPU payloads and MFUs
  reassembled per asset with packet-loss and MPU-continuity counters,
  NTP, and elementary-stream framing (HEVC Annex B, LOAS, caption MFUs).
  MMT-SI text is UTF-8. Scrambled payloads are counted from their clear
  8-byte header and never descrambled.
- `isdb-cli`: `isdb tlv inspect` decodes the tables (network, services,
  events, packages and assets, time; `--json`), `isdb tlv extract
  --out-dir` writes each asset's elementary stream (`--service`,
  `--asset`, `--write-scrambled` for the encrypted payloads as they are),
  `isdb tlv validate` checks the structure. All three stream the file.
- `isdb-tlv`: `tlv::FileReader`, a TLV stream file read a chunk at a
  time.
- `isdb-cli`: `isdb rx --system isdbs|isdbs3 --radio …` receives
  continuously through the radio's FPGA symbol front end when it offers
  one (`--offload auto|fpga|cpu`, default auto): acquisition on a short IQ
  probe, then the front end's `Symbols` words decoded as one stream, cut
  on frame (ISDB-S3) or superframe (ISDB-S) boundaries, with losses and
  lost locks reported as discontinuities. `--symbols FILE` decodes a words
  file (`isdb symbols --out`). Live from a PlutoSky R2: 30 s of gapless TS
  from BS1 (every packet RS-clean, 0 continuity errors) and BS7's TLV
  streams with one reported loss.
- `isdbs3-phy`: `rx::Rx::frame_starts` and `RxOptions::slot_offset` (a
  synchronisation hint for streams cut on a frame start);
  `frontend::acquire`. `isdbs-phy`: `frontend::acquire`.
- `isdb-frontend`: format 4, the arithmetic of an FPGA that takes one
  sample per clock (the PlutoSky R2 clocks its sample path at the sample
  rate): loop delay 24 symbols (was 6), the timing detector's banks `h`
  and `g` non-zero on their centre 8 taps (`TED_TAPS`, enforced by
  `Params::validate`). Same words as format 3; `MIN_ARITH_VERSION` 4, so a
  format-2/3 FPGA is refused. Examples `tedspan` and `loopdiag`.
- `fpga`: the full-rate symbol front end (a sample every clock, fully
  pipelined, 112 DSP48E1) and the PlutoSky R2 image that closes timing
  with Vivado 2026.1 (`fpga/tezuka/`, `isdb_symbols.xdc`; the image leaves
  out Tezuka's DVB-S2 transmitter). `isdb_top.bin` builds; not yet run on
  the board.
- `isdb-radio`: the Pluto symbol path refuses to run when the AD9361 core
  is in r1_mode (the clock case the bitstream is not timed for).
- `isdb-frontend`: AVX2 and AVX-512 builds of the fast front end's dot
  products, chosen at run time (`simd::Isa`; AVX2 by default, AVX-512
  with `ISDB_SIMD=avx512` and Rust ≥ 1.89, `ISDB_SIMD=baseline` for the
  portable path; `FastFrontend::with_isa`). Same output on every
  instruction set (tested per set, with fault injection). ISDB-S3 front
  end on one core 0.83× → 1.19× real time, ISDB-S 1.40×. The crate's one
  `unsafe` is the call into the chosen `#[target_feature]` loop after
  detecting its features (`simd::run`), named in the workspace
  `Cargo.toml`.

- `isdb-frontend`: `Symbols` format version 3. The block trailer is a
  CRC-32C (two words: CRC-32/ISCSI of the 32-bit sequence number and
  the payload, little endian) instead of a CRC-16, and a version-3
  block is accepted on it alone: no framing check, so neither the
  intact block before a loss is dropped nor delivery held back. Versions
  1 and 2 still read (CRC-16 and framing check); `Packer::with_version`
  writes them. Slicing-by-8 CRC-32C in safe Rust (2.8 GB/s per core).
  The FPGA packer computes it one word per clock (+279 LUT, +17 FF);
  the host drives front ends of version 2 or 3. `examples/crcspeed`.
- `tools/sigmf-repeat.py`: repeat a SigMF recording into a long
  multi-block one, for pipeline throughput measurements.
- `isdb-frontend`: `fast::FastFrontend`, the bit-exact front end 2–2.4×
  faster (linear delay line, fused banks, 16-bit products in `i32` on
  split coefficients, an `i64` kernel while a larger input is in the
  line), equal to `fixed::Frontend` symbol for symbol (property tests,
  recordings); the receivers run it, the FPGA vectors stay made with the
  reference. `process_each` hands symbols to a closure.
- `isdb-frontend` `s3sync::measure_at`; the slot search on every core,
  bit for bit as one thread.
- `isdbs3-phy`: `main_signal::decode_frames` (every codeword of several
  frames in one pool), `sync::synchronise_at`, `frontend::symbols_with_prior`
  (reuse a block's carrier and trim after checking them on 0.05 s);
  `rx::receive` prepares frames in parallel: 5.85 → 0.60 s per 0.83 s
  block, output unchanged.
- `isdbs-phy`: `frontend::symbols_with_prior`, the trim candidates run at
  once (7.6 → 0.65 s), frame acquisition and derotation on every core,
  `par` (ordered parallel map, first-maximum search).
- `isdb-cli`: `isdb rx --system isdbs` / `isdbs3` read or capture, run the
  front end and receive at once, up to three blocks' front ends at a time
  (`satpipe`); the summary's `[rx] time:` line and `time` in `--json`.
  ISDB-S3 recordings decode 3–5× faster, ISDB-S 5–8×, with the same
  output on single-block recordings; live from a PlutoSky the decoder
  keeps up with the radio's captures.
- `tools/rx-compare.sh`: decode recordings with two builds and compare
  the outputs byte for byte.

- `isdb-fec`: `ldpc::fixed`, a fixed-point layered offset min-sum
  decoder for the quasi-cyclic IRA codes, decoding 16 codewords at once
  in SIMD lanes (refilled as each stops) with the same bits and
  iterations as its one-lane scalar form; `simd`, its kernels, written
  for auto-vectorisation on stable compilers, or on `std::simd` when the
  build script finds a matching nightly. ISDB-S3 LDPC: 166 → 4 678
  codewords/s per thread, 11.4× real time on 32 threads.
- `isdb rx --system isdbs3 --ldpc fixed|scalar|float`: the main signal's
  LDPC decoder (fixed by default).
- `isdb-fec`: `bch` (binary BCH over GF(2¹⁶), t = 12, any shortening:
  ARIB STD-B44 Table 3.4-1), `ldpc` (quasi-cyclic IRA LDPC from a
  374-bit-group address table: encoder and layered normalised min-sum
  decoder) and `prbs::B44Prbs` (the ISDB-S3 slot, TMCC and pilot energy
  dispersals, output from the last stage).
- `isdbs3-core` (new): ISDB-S3 parameters from ARIB STD-B44 v2.1: frame
  and slot geometry, the eleven code rates and their LDPC codes (address
  tables transcribed by `tools/b44-ldpc-tables.py`), π/2-BPSK to 32APSK
  constellations with γ, the bit interleaver, energy dispersal, the TMCC
  information codec and signal coding, slot allocation and codeword
  placement. Readings and their confirmation on live signals in its
  SPEC-NOTES.
- `isdbs3-phy` (new): ISDB-S3 receive chain after the symbol front end:
  slot and frame sync, carrier phase per slot, an 11-tap least-squares
  equaliser trained on each frame's sync and pilot symbols, TMCC
  decoding, the pilot-learned constellation and the main signal (max-log
  demapping, de-interleaving, LDPC, dispersal, BCH) to each slot's data;
  `rx::receive`.
- `isdb-tlv` (new): TLV packets reassembled from a stream's slots with the
  head-TLV indication, IPv4 / IPv6 / header-compressed IP, MMTP headers
  (with the B60 scramble-information extension), MMT-SI messages and
  table ids, TLV-SI sections by CRC, per-stream statistics.
- `isdb-cli`: `isdb rx --system isdbs3` (and `auto` from a recording's
  survey) writes one TLV stream file per stream id (`{tlv_id}` in
  `--out`, or `--ts-id`), offline or block-wise from a radio; `isdb iq
  s3tmcc` (ISDB-S3 TMCC of a recording); `isdb tlv inspect` (content of
  a TLV stream file).

- `isdbs-core` (new): ISDB-S parameters from ITU-R BO.1408-1 and MIC
  Ordinance No. 87 of 2011 annexed tables 45–51: frame geometry (192-symbol
  header, 192 × (203 + 4)), W1/W2/W3 and their coded tails, the seven
  modulation-code combinations with slot units and puncturing, slot runs,
  the 384-bit TMCC information codec with validation and the TMCC signal
  of a superframe (RS(64,48), energy dispersal), the burst bits.
- `isdbs-phy` (new): ISDB-S reception after the symbol front end: frame
  sync on the frame's 808 known symbols with slip detection, burst-aided
  forward-backward carrier loop, TMCC decoding, and the main signal to TS
  packets (`demod`: BPSK / punctured QPSK / TC8PSK soft demapping with
  set-partition metrics, one Viterbi decoder per frame from the W2/W3
  state to the next W1; `ts`: de-interleaving, energy dispersal, sync-byte
  restoration, RS(204,188), TS_ID per packet, superframes decoded in
  parallel). Examples `mapprobe` and `throughput`.
- `isdbs-core::mainsig`: the slots sent (dummy slots after the valid ones
  of each unit), the superframe energy dispersal, the depth-8 inter-frame
  interleaver of BO.1408-1 Fig. 6, a superframe's RS codewords and their
  transmit-side inverse.
- `isdb-fec`: `ViterbiDecoder::with_state` (known start state),
  `push_known` (known trailing bits end the trellis) and `push_metrics`
  (arbitrary branch metrics, for trellis-coded 8PSK).
- `isdb-cli`: `isdb rx --system auto|isdbt|isdbs` receives ISDB-S from a
  SigMF recording or a radio, one TS per TS_ID (`--out` with `{ts_id}`,
  `--ts-id`), per-TS RS counters and JSON; from a radio it decodes one
  contiguous capture after another (`--rate`, `--bw`, `--block-seconds`).
  `tools/isdbs-ts-acceptance.sh` receives a capture directory and checks
  each TS. 19 of the 21 lab transponders give RS-clean TS (311 520
  packets, 0 uncorrectable, 0 continuity errors; VERIFICATION.md).
- `isdb-cli`: `isdb iq tmcc REC [--summary|--json]` decodes the frames and
  the TMCC of an ISDB-S recording: combinations, slots and TS_IDs per
  superframe, slips, pilot and main MER. `tools/isdbs-tmcc-acceptance.sh`
  runs it over a capture directory. The TMCC decodes on every superframe
  of the 9 BS and 12 110° CS transponders recorded from a cable feed
  (VERIFICATION.md).
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
  `i16`) is the DAC format of its `QueuedSink`. A TV received its
  `isdb tx` output from a PlutoSky R2's TX2 over a conducted link
  (VERIFICATION.md "Real receivers").
- `isdb-cli`: the `pluto` feature (default on) forwarding `isdb-radio/pluto`.
- `isdb-iq`: IQ recordings independent of the broadcast system — sample
  formats (`cs8`, `cs16` with its full scale, `cf32`), SigMF 1.2 metadata
  with an optional `isdb` extension (full scale, LO and bandwidth read
  back, gain, channel, capture method, gap marker between capture
  segments and the samples discarded around a loss), `Recording` (opened by data, `.sigmf-meta` or base name), and
  a single-carrier survey: power spectrum and band edges, the |z|²
  symbol-rate line (aliases above fs/2 included), z², z⁴, z⁸ and
  π/2-BPSK carrier lines, and the system (ISDB-S, ISDB-S3) whose symbol
  rate matches.
- `isdb-iio`: `Context::capture`, one buffer refilled once (contiguous
  samples without a stream thread, returned as the device's raw bytes);
  `Context::start_rx_raw`, an RX stream of raw bytes for any channel
  layout that waits for its consumer instead of dropping; register access
  (`Device::reg_read` / `reg_write`).
- `isdb-radio`: raw captures — `Backend::describe_capture`,
  `capture_limit` and `capture`, which writes the samples to an
  `io::Write` as they arrive and returns a `Capture` (`CaptureFormat`
  cs8 or cs16 with its full scale, samples written, `CaptureGap`s, rate /
  LO / bandwidth / gain read back, device identity, method, host time);
  `hackrf` cannot capture. The `pluto` backend captures through one
  libiio buffer (`recorder=iio`, up to 63 MiB), Tezuka's maia-sdr
  recorder over HTTP (`recorder=maia`, its 128 MiB buffer, modes
  `IQ16bit` / `IQ12bit` / `IQ8bit`) or a stream of libiio buffers
  (`recorder=stream`) whose losses it finds from the ADC core's overflow
  bit; `bits=8` selects 8-bit samples (Tezuka's packing through libiio).
- `isdb-cli`: `isdb capture` (receive only: one contiguous block per
  segment, or a stream, at any rate the radio supports; cs16 or cs8 +
  SigMF metadata; `--samples` or `--seconds`, `--segments` and losses
  inside a stream as SigMF segments with gap markers, `--discard` to test
  a link without writing, progress every 5 s, Ctrl-C ends a stream and
  keeps what it wrote, `--dry-run`, `--survey`, no overwrite without
  `--overwrite`) and `isdb iq survey` (text or `--json`). `--freq` of
  `capture` takes channel names: `T<n>` (UHF), `BS<n>` and `ND<n>` (BS
  and 110° CS at their satellite IF).
- `isdb-frontend`: single-carrier receive front end for ISDB-S and
  ISDB-S3 (IQ to symbols at one per symbol):
  - a bit-exact fixed-point model of format version 2: 32-bit NCO mixer,
    64-phase polyphase root-raised-cosine matched filter with its
    derivative and an output filter (the matched filter a trim later),
    non-data-aided timing recovery on the first two with a PI loop applied
    six symbols late, AGC and n-bit output on the third;
  - a floating-point reference;
  - the `Symbols` boundary: 16-bit words in blocks with sync, sequence
    number, gain, period, header check and a CRC-16 trailer, and a
    `Reader` that reports every loss;
  - `Design::for_system` with the settings measured on the satellite IF
    (32 taps, 6 bits for both systems) and `Design::trim`;
  - ring / PSK MER and SQNR metrics;
  - `s3sync`: ISDB-S3 slot synchronisation on the STD-B44 sync words, the
    SNR on those known symbols, and `best_trim`, the trim search that the
    host runs before configuring the front end;
  - the FPGA's register map and command words over the AD9361 core's
    general purpose registers (`control`);
  - JSON test vectors for the HDL.
- `fpga/`: the same front end in Amaranth HDL (`SymbolsRx`: core,
  command decoder on `up_adc_gpio_out`, ID word on `up_adc_gpio_in`,
  output selection before the RX DMA's packer), format 2 with the
  output bank. It matches the model bit for bit in simulation, trimmed
  and untrimmed, and Yosys estimates 64 DSP48E1 and 3.9k LUT for 32 taps.
  `mise run fpga:test`, `fpga:estimate` and `fpga:verilog`. Not yet built
  into a bitstream.
- `isdb-radio`: `Offload` (stages a radio runs itself), probed by
  `Backend::offloads` without changing the device, and
  `Backend::capture_symbols`. The Pluto backend reads the front end's ID
  word, configures it through the command register (checked by a
  readback), streams the 16-bit words and switches back to IQ afterwards.
- `isdb-cli`: `isdb symbols` (receive only). It uses the radio's FPGA
  front end when its bitstream has one (`--offload auto|fpga|cpu`,
  `--probe`). Otherwise it runs the CPU front end on one contiguous IQ
  capture (40 Msps and 56 MHz analog bandwidth by default). Also `isdb iq
  symbols`: the CPU front end on a recording, or `--read` a words file
  (blocks, losses, ring MER).
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

- `isdbs-phy`, `isdbs3-phy`: `clippy::cast_possible_truncation` is on at
  crate level and in every example and integration test
  (`docs/plans/type-safety.md` §2), with three `#[allow]`s per crate, each
  on a one-line private helper: the 12-bit input quantiser (rounded and
  clamped before the cast), the f64 → f32 soft metric handed to the
  Viterbi / LDPC decoder, and the f64 → usize sample count (no fallible
  conversion in std). Both `frontend` modules gain `samples_in(seconds,
  fs)`; `isdbs_phy::frontend::to_input` is now public, so the examples
  stop repeating the quantiser. Recording segment lengths in the examples
  and tests go through `usize::try_from`. Behaviour is unchanged.
- `isdb-si`: `clippy::cast_possible_truncation` is on at crate level
  (`docs/plans/type-safety.md` §2, item H3), with no `#[allow]`: the 149
  narrowing casts are gone. Bit fields go through the width-typed
  `BitReader::read_u8` / `read_u16` / `read_u32` / `read_usize`; the
  descriptor and DSM-CC `descriptor_length`, the DDB block count and the
  Annex C MJD / time-of-day arithmetic narrow through `try_from`; byte
  extraction (`MjdTime::from_u40`, the BCD digit pairs, the DII / DDB
  header fields that carry the low bits of a wider value) uses
  `to_be_bytes`. `CodedValue` gains the provided constant `MASK`.
  Behaviour is unchanged.
- `clippy::cast_possible_truncation` is on (`#![warn]`) in `isdb-ts` and
  `isdb-mmt` (`docs/plans/type-safety.md` §2), with no new `#[allow]`:
  the codec field reads go through `BitReader::read_u8/u16/u32/usize`,
  the length fields of `Descriptor::encode`, `PesPacket::encode` and
  `packetize_sections` are `try_from` into the `Error::Overflow` they
  already returned, and `isdb_mmt::es::loas_frame` takes its 13-bit
  length from `u16::to_be_bytes`. No public signature changed.
- `clippy::cast_possible_truncation` is on (`#![warn]`) in `isdbt-mux`
  (`docs/plans/type-safety.md` §2), with 8 `#[allow]`s (float → integer
  tick and packet counts, two clamped values, one bounded PES length).
  Section builders report a value that does not fit its wire field
  instead of wrapping it: the BML resource list (`resourceCount`, a
  `resourceNameLength` over 127 bytes), `moduleSize` / `component_size`
  and the EIT schedule section numbers are errors; a `service:` / `event:`
  / `broadcaster:` loop selector whose number exceeds the identifier
  width selects nothing. `SilentFrame::stereo_48k` and `build` panic
  (documented) on a `len` past the 13-bit `aac_frame_length`.
- `clippy::cast_possible_truncation` is on (crate-level `#![warn]`) in
  `isdbt-ofdm` and `isdbt-phy` (`docs/plans/type-safety.md` §2). The
  TMCC, AC and IIP decoders read narrow fields through width-typed
  `get_u8` / `get_u16` readers, the IIP encoder's loop and length bytes
  are `try_from` into `Error::BroadcastTs`, and the remaining narrowings
  (DSP quantisation, `f64` sample positions, carrier-table indices) sit
  under 19 `#[allow]`s in library code, each with its invariant. No
  behaviour change; no public signature changed.
- `isdb-frontend`: `clippy::cast_possible_truncation` is on (crate-level
  `#![warn]` in `lib.rs`, every example and test; `docs/plans/type-safety.md`
  §3 H3). Of its 130 casts, 54 are restructured (the `Symbols` words
  through `to_le_bytes`, the filter phase as a masked `u8`, index
  windows through `usize::try_from`, the float-to-12-bit input through the
  new `isdb_frontend::input_sample`), 14 are `try_from` in examples and
  tests, and 62 are DSP quantisation under 31 `#[allow]`s, each with its
  invariant (the loop's clamps in `fixed`, shared with `fast` through
  `pub(crate)` helpers, the bounded `Params::design` quantities, the f64
  sample positions of the measurement tools). The FPGA vectors are
  bit-identical.
- The reserved payloads of the lossless coded enums are canonical by
  construction (`docs/plans/type-safety.md` §3 H1): `Reserved(u8)` /
  `UserDefined(u8)` become `Reserved(ReservedCode)` /
  `UserDefined(UserDefinedCode)`, a private-field newtype per crate with
  `get()` (`const`), `Display`, `Binary` and `LowerHex`, whose only
  constructor is `pub(crate)` and called by `from_code` after it has
  excluded every assigned and unused code. A hand-built `Reserved(0xF6)`
  that was `!=` `Reserved(0x06)` yet encoded the same, or a
  `Reserved(0b111)` spelled where `Unused` is canonical, can no longer be
  written: `==` is wire equality on every value, not only on decoded ones.
  Affects `isdb_ts::packet::TransportScramblingControl`,
  `isdbt_ofdm::tmcc::{ModulationCode, RateCode, InterleaveCode,
  SegmentsCode}`, `isdbs_core::CombinationWord`,
  `isdbs3_core::{ModulationCode, RateCode, StreamType}`,
  `isdb_tlv::mmtp::PayloadType` and `isdb_mmt::mpu::FragmentType`.
  `code()` no longer masks (the payload is canonical); `from_code` still
  does. A `compile_fail` doctest per crate proves the constructor is
  private, and an exhaustive test per enum that `from_code(c) ==
  from_code(c & mask)` and `from_code(c).code() == c & mask` over every
  `u8`. Callers that matched `Reserved(c)` read `c.get()`; tests and
  READMEs that spelled a reserved value use `from_code`.
- `isdb-ts`: the continuity_counter is a value type, `ContinuityCounter`,
  instead of a `u8`: 4 bits by construction (`ContinuityCounter::new(u8)
  -> Option<_>`, `get()`, `ZERO`, `MAX`), with the H.222.0 §2.4.3.3
  arithmetic on it (`next()` wraps 15 → 0, `is_next_of()` is "differs by
  a positive value of one" modulo 16) and `Display` as the decimal value.
  `Header.continuity_counter`, `Header::new`, `Packet::with_payload` /
  `with_adaptation` / `pcr_only`, `cc::Counter::new` / `next` / `peek`
  and the `expected` / `got` / `previous` payloads of `cc::Continuity`
  and `cc::CcEventKind` take it, so the `& 0x0F` masks and `+ 1` of the
  checker (`Tracker`), the transmit `Counter`, `packetize`, the muxer's
  relay (`last_cc`) and `isdbt-phy`'s IIP writer are gone; `Counter::new(0)`
  is `Counter::default()`. Decoders build it with
  `ContinuityCounter::from_wire` (a mask on the 4 bits already read, so
  the codec stays lossless; `prop_packet_roundtrip` covers 0..=15). The
  `isdbt_phy::broadcast_ts::Iip.continuity_counter` field follows, and
  `Iip::to_packet` / `from_packet` build and read the TS header through
  `isdb_ts::packet::Header`. In the same change the raw-byte readers of
  transport_scrambling_control and adaptation_field_control outside
  `isdb-ts` (`p[3] >> 6`, `(p[3] >> 4) & 3` in `isdb-analyze`'s scan,
  `isdb-cli`'s packet source, the muxer's relay, PCR clock and rate
  probe) go through `Header::decode` / `packet::View` and the Table 2-4 /
  2-5 enums (`is_scrambled()`, `has_payload()`); the relay policy's
  refusal of scrambled input and its messages are unchanged. Item H2 of
  `docs/plans/type-safety.md`; `tools/newtype-wrap.py` did the literal
  sites.
- `clippy::cast_possible_truncation` is on (`#![warn]`) in `isdbt-core`,
  `isdb-hackrf`, `isdb-iio`, `isdb-text`, `isdb-radio` and `isdb-tlv`
  (`docs/plans/type-safety.md` §2): the remaining narrowing casts there
  are restructured to produce the narrow type, made fallible, or carry an
  `#[allow]` with the invariant. `isdb_tlv::tlv::Packet::encode` returns
  `isdb_ts::Error::Overflow` for data longer than the 16-bit `data_length`
  field and replaces `Packet::bytes`, which wrapped the length silently
  (`isdb rx` for ISDB-S3 propagates the error).
- `isdbt-core`: `LayerConfig.segments` is `Segments`, a `1..=13` newtype
  of ARIB STD-B31 Table 3-31 ('0001'–'1101'; Table 3-2 Ns = 13), instead
  of `usize`. `Segments::new(n)` is the only constructor (`None` outside
  the range; `const`), with `get()` (the `u8` count / code), `count()`
  (`usize` for carrier and TSP arithmetic), `MAX` (13), `ALL` and
  `Display`. `Params::new` no longer checks for an empty layer and
  `ParamError::EmptyLayer` is gone (the value cannot be built); the
  sum-to-13 and partial-reception checks stay.
  `ofdm_params::bit_delay_adjustment_bits` takes `Segments`. In
  `isdbt-ofdm`, `tmcc::SegmentsCode::Segments` carries a `Segments`
  (not a `u8`), so `TmccLayer::from_config` has no cast and
  `from_code` goes through `Segments::new`; the CLI layer spec refuses
  a count outside 1..=13 at the parser. Item G (segments) of
  `docs/plans/type-safety.md`.
- `isdb-ts`: the PID is a value type, `Pid`, instead of a `u16`: 13 bits
  by construction (`Pid::new(u16) -> Option<Pid>`, `TryFrom<u16>`,
  `get()`), with H.222.0 Table 2-3 on it (`Pid::PAT`, `CAT`, `TSDT`,
  `IPMP`, `NULL`; `is_reserved()` for 0x0000–0x000F and 0x1FFF,
  `is_assignable()` for 0x0010–0x1FFE, `may_carry_pcr()` for the table's
  NOTE) and `Display` / `Debug` as `0x1fff`. Every `pid` field, parameter
  and map key of the crate takes it: `Header.pid`, `PatEntry.pid`,
  `PmtStream.elementary_pid`, `Pmt.pcr_pid`, `Packet::with_payload` /
  `with_adaptation` / `pcr_only`, `SectionAssembler::new`,
  `SectionDemux::add_pid` / `has_pid`, `PesAssembler::new`,
  `pes::packetize`, `packetize_sections`, `CcEvent.pid`, `Stats.pids`.
  The `packet::PID_*` constants are gone (use the `Pid` constants);
  `BitReader::pid()` reads the 13-bit field. Decoders build it with
  `Pid::from_wire` (a mask on the 13 bits already read, so the codec stays
  lossless; `prop_pid_round_trip` covers 0..=0x1FFF). Behind the new
  `serde` feature it serialises as the integer and deserialises with the
  range check, so a PID above 8191 in an `isdbt-mux` TOML now fails at
  load, naming the key, instead of at encode time; the TOML and the
  `isdb ts` JSON reports are byte-identical. `isdb-si` follows
  (`AnyTable::decode`, `conformance::check`, `table_allowed_on_pid`,
  `ConditionalAccess.ca_pid`, `AccessControl.pid`,
  `HierarchicalTransmission.reference_pid`), as do `isdb-analyze`,
  `isdbt-mux` (`Config` PID keys, `PidMap`, `PidList`, `si::pid`
  constants, `Probe`, `RelayedEs`), `isdbt-phy` (`Iip.pid`, `IIP_PID`;
  `BroadcastTsWriter::with_iip_pid` is infallible) and `isdb-mmt`
  (`Location::Mpeg2Ts`). Item G of `docs/plans/type-safety.md`;
  `tools/newtype-wrap.py` did the literal sites.

- Workspace: `clippy::cast_lossless` is enabled (`warn`, so an error in
  CI). The 632 `x as T` casts where `T: From<X>` (`u8`/`u16`/`u32` →
  wider integers, `bool` → integer, `f32`/`u32`/`i32` → `f64`) are written
  `T::from(x)`, so every `as` left is a lossy one. No behaviour change.

- `isdb-ts`: `Header.transport_scrambling_control` and
  `Header.adaptation_field_control` are lossless coded enums of H.222.0
  Tables 2-4 and 2-5 instead of raw `u8` codes:
  `TransportScramblingControl { NotScrambled, UserDefined(u8) }` (the
  user-defined codes '01'–'11' keep their value) and
  `AdaptationFieldControl { Reserved, PayloadOnly, AdaptationOnly,
  AdaptationAndPayload }`, each with a total `from_code()` (masked to the
  2-bit field) / `code()` pair and `Display` as two binary digits.
  `has_payload()` / `has_adaptation_field()` live on
  `AdaptationFieldControl` (the `Header` methods forward to them),
  `TransportScramblingControl::is_scrambled()` replaces `!= 0`,
  `Header::new` takes the enum and the `packet::afc` constants are gone.
  Decoding stays bit-exact (`prop_*` round trips unchanged). Item A of
  `docs/plans/type-safety.md`.
- `isdbs-core`: the TMCC combination word is a lossless coded enum of
  ITU-R BO.1408-1 Fig. 8 / Table 1, `CombinationWord { Assigned(Combination),
  Dummy, Reserved(u8) }` (`from_word()` total over the 4-bit code,
  `word()` its inverse), instead of `CombinationEntry { word: u8 }`.
  `TmccInfo::validate` and `slot_combinations` match on the variants
  (no table lookup, no `DUMMY_WORD` comparison); `TmccError::ReservedWord`
  is still what `validate` returns for `Reserved(_)`, and the codec stays
  bit-exact (`prop_tmcc_info_lossless` unchanged). `CombinationEntry::DUMMY`
  is the §6.1.2 unused entry (1111, zero slots); `CombinationEntry::combination`
  is `const`. `isdb iq tmcc --json` prints the same `word` number as
  before. Second step of `docs/plans/type-safety.md` (item C).
- `isdbt-ofdm`: the four fields of `tmcc::TmccLayer` are lossless coded
  enums of ARIB STD-B31 Tables 3-28..3-31 (`ModulationCode`, `RateCode`,
  `InterleaveCode`, `SegmentsCode`: `Assigned(..)` over the `isdbt-core`
  type, `Reserved(code)`, `Unused`; total `from_code()` / `code()`)
  instead of raw `u8` codes. A received TMCC word still re-encodes bit
  for bit (reserved codes keep their value), and `TmccLayer::config()`
  (was `to_config`) matches variants, not integers: the unused layer is
  `None`, a reserved code is `Error::Tmcc` as before. Table 3-30 '100'
  ("not used", terrestrial sound) is `InterleaveCode::Reserved(0b100)`.
  Second item of `docs/plans/type-safety.md`.
- `isdbs3-core`: the TMCC code fields of ARIB STD-B44 §3.11 are lossless
  coded enums instead of raw integers: `Mode.modulation_code:
  ModulationCode { Assigned(Modulation), Unused, Reserved(u8) }` (Table
  3.11-1), `Mode.rate_code: RateCode { Assigned(CodeRate), Unused,
  Reserved(u8) }` (Table 3.11-2; `1010` still reads as 105/120 per
  SPEC-NOTES "TMCC code-rate table") and `TmccInfo.stream_type:
  [StreamType; 16]` with `StreamType { Ts, Tlv, None, Reserved(u8) }`
  (Table 3.11-4), each with a total `from_code()` (masked to the field
  width) and its inverse `code()`, so the TMCC word still round-trips
  bit-exactly. `Mode::modulation()` / `rate()` / `is_used()` are
  `const` pattern matches; the `STREAM_TS` / `STREAM_TLV` constants are
  gone; `isdbs3_phy::rx::RxSlot.stream_type` is a `StreamType`. The
  `isdb iq s3tmcc` / `isdb rx` JSON keeps the numeric codes. Item D of
  `docs/plans/type-safety.md`.
- `isdbt-core`: `TimeInterleave` is a closed enum of the four TMCC codes
  of ARIB STD-B31 Table 3-12 (`K0`..`K3`, `code()` / `from_code()`,
  `ALL`) instead of `TimeInterleave(pub u8)`. `length(mode)` is
  infallible and `const`; `Params::new` no longer checks the index and
  `ParamError::TimeInterleaveIndex` is gone; the reserved codes
  '100'–'111' are refused in `from_code` alone (TMCC decoder, CLI layer
  spec). `Mode::number` / `Mode::scale` are `const fn`. First step of
  the type-safety workflow in `docs/plans/type-safety.md`;
  `tools/type-census.sh` counts what is left.
- `isdb-tlv`, `isdb-mmt`: the coded fields of the MMTP and MPU headers
  are lossless coded enums of their ARIB STD-B60 §6 tables instead of
  `u8`: `mmtp::Header { fec_type: FecType, payload_type: PayloadType }`
  (Tables 6-5, 6-6; the loose consts `mmtp::MPU` / `mmtp::MESSAGES` are
  gone, use `PayloadType::Mpu` / `PayloadType::Messages`) and
  `mpu::PayloadHeader { fragment_type: FragmentType, fragmentation:
  FragmentationIndicator }` (Tables 6-2, 6-3). Each has `from_code()`
  (total, masked to the field width) and `code()`, names every assigned
  code and keeps the reserved ones in `Reserved(u8)`, so headers still
  re-encode bit-exactly; the MFU reassembler matches on
  `FragmentationIndicator::{Complete, Head, Middle, Last}` instead of
  integers. `StreamStats::mmtp` keeps a `PayloadType`. Item E of the
  type-safety workflow.
- `isdb-fec`: the fixed-point LDPC iteration (every check node, then the
  syndrome) runs in a function of its own, 14 % more codewords per thread
  (4 615 → 5 380 on live ISDB-S3 codewords), same results. Runtime SIMD
  dispatch was measured and not adopted (needs `unsafe` or gains under
  6 %; VERIFICATION.md "Satellite receive speed").
- `isdb-frontend` format 2 (`Symbols` words keep their layout; the
  reader accepts versions 1 and 2): the front end's timing loop locks a
  fixed fraction of a symbol away from the best sampling instant on the
  lab's satellite signals (0.15–0.18 on ISDB-S3, 5–6 dB of SNR; 0.05–0.10
  on ISDB-S), so the output now comes from a separate filter bank shifted
  by a trim the host measures on known symbols. `isdb symbols` and `isdb
  iq symbols` measure and apply it (`--trim auto|T`, default auto) and
  record it in the sidecar; ISDB-S3 output goes from 5 to 6 bits; the
  ISDB-S3 report gives the sync-symbol Es/N0. With the trim: ISDB-S3
  sync-symbol Es/N0 8.8–9.5 → 14.6–14.7 dB on BS7/11/17; ISDB-S burst
  MER +1.3 dB (median) on 21 transponders, TS still RS-clean.
  `isdbs_phy::frontend::symbols` measures the trim too (`Trim::Auto`).
- `isdb-cli`: `isdb iq s3sync`, the ISDB-S3 slot sync and sync-symbol
  Es/N0 of a recording (untrimmed and trimmed) or of a `Symbols` words
  file.

- `isdb rx`: a SigMF recording given to `--iq` supplies its format and
  must be at 512/63 MHz (others are refused with a pointer to `isdb iq
  survey` / `isdb iq tmcc` instead of being misread); `--iq-format` is now
  optional (default cf32, or the recording's). When a file holds no
  ISDB-T frame, `rx` surveys it and names the single-carrier system found.
- `isdb-fec` (new): the RS(255,239) shortened codes (now any length, for
  RS(204,188) and the ISDB-S TMCC's RS(64,48)), the K = 7 171/133
  convolutional code and Viterbi decoder, and the X¹⁵+X¹⁴+1 PRBS moved
  out of `isdbt-fec`, joined by the ISDB-S burst PRBS X⁹+X⁴+1.
  `isdbt-fec::{rs, conv, energy}` keep their public items and delegate;
  every ISDB-T test is unchanged and Viterbi throughput is the same.
- `isdb-cli`: channels are written `T<n>` (terrestrial UHF, T13–T62),
  `BS<n>` (odd 1–23) and `ND<n>` (even 2–24), parsed case-insensitively.
  `--channel` of `tx` / `rx` takes these names, and a bare number still
  means a terrestrial channel (`--channel 44` = T44). Labels and messages
  print ` (T27)`, ` (BS1 IF)`, ` (ND2 IF)` (formerly ` (UHF ch27)`,
  ` (BS-1 IF)`); `isdb capture` writes `isdb:channel` as `BS1` / `ND2` /
  `T27`. The `chN` / `uhfN` / `bs-N` / `nd-N` spellings are no longer
  accepted. `tx` / `rx` refuse satellite channels (ISDB-T only).
- `isdb-cli`: the UHF channel table moved from `phyargs` to the new
  `bands` module (`channel_hz` → `bands::uhf_hz`), next to the BS and
  110° CS satellite IF tables; `tx` / `rx` behave as before.
- `isdb-radio`: the `pluto` device arguments are parsed per use (TX, RX,
  capture): `recorder` is a capture argument, `buffer` a streaming one.
- `.gitignore`: `*.cs16`, `*.sigmf-data` and `*.sigmf-meta` recordings.

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

- `isdbs3-phy` / `isdb-cli`: streamed ISDB-S3 reception lost every frame
  ≈ 30 s after the FPGA front end was configured. The front end has no
  carrier loop and the carrier drifted (−97 → −830 Hz in 30 s); at
  ≈ 908 Hz the two SSync neighbours that tell the frame marker from
  !FSync cancel. `RxOptions::carrier_hz` / `Rx::residual_hz`: the
  streaming receiver tracks the residual and removes it; a loss now costs
  about one frame. The block-wise path is unchanged.
- `isdb-radio`: the FPGA symbol stream overflowed the device's DMA every
  10.2 s at 51 MB/s (iiod near a full Pluto core, four kernel buffers).
  New device argument `kbuffers` (default 16 × 1 Mi words); `buffer` also
  applies to the symbol stream. The cause is the board's TCP congestion
  control: Tezuka sets BBR, whose PROBE_RTT phase pauses the sender for
  ≥ 200 ms every 10 s (5 of 5 BBR runs lost blocks at the same stream
  position, 2 of 2 `cubic` runs lost none); 16 buffers (660 ms) absorb
  it.
- `isdb-cli`: the streaming receiver's queue is bounded (four chunks);
  beyond it whole blocks are dropped and reported (`blocks_dropped`)
  instead of memory growing while the decoder is behind.
- `isdbs3-phy` / `isdb-cli`: the ISDB-S3 carrier at 40 Msps. The survey's
  strongest line was an alias on two of seven live recordings (sync
  Es/N0 −3 / −8 dB against +9 dB for the carrier): `isdb symbols` took it,
  and `pick_carrier` tried only the first four candidates while the
  carrier was the 4th of 5. Every candidate is now tried, and `isdb
  symbols` uses the same choice; the FPGA path of `isdb rx` also refuses a
  lock below 4 dB and probes again.
- `isdb-frontend`: the `Symbols` reader could deliver a damaged block.
  A loss glued block 1's intact header to later payload, and a payload
  word that landed in the trailer position equalled the CRC-16 of that
  spliced payload (property-test seed `bits = 5, cut_at = 208, cut_len =
  282`). A block is now accepted only when it is also framed: the next
  block's header starts right after its trailer, or the stream ends
  there. Cost: the intact block just before a loss is counted as lost
  too, and delivery lags by at most one block. No format change; the
  FPGA vectors are unchanged.
- `isdbs3-phy`: the carrier choice took a candidate 48 kHz off on a live
  BS11 block (its sync Es/N0 0.6 dB higher, no TMCC decoded): candidates
  whose carrier phase turns more than 0.5 rad between SSync slots now
  rank below the others.
- `isdb-cli`: the live-feed stall test no longer fails when the rest of
  the workspace tests load every core.

- `isdbs3-phy`: the ISDB-S3 carrier was the survey's strongest line, and
  an unrelated line in one of the ±R/2 windows could be stronger than the
  π/2-BPSK line: a live block then decoded no TMCC at all. The carrier is
  now the candidate the front end locks on best.
- `isdb-analyze`: scrambled packets (transport_scrambling_control ≠ 00)
  were parsed as sections, PES and ADTS, giving hundreds of false CRC,
  pointer and PES-start errors on scrambled services; their payloads are
  now left alone (an all-scrambled elementary stream is an Info finding).
  Mandatory tables, CAT and PMTs missing from a capture shorter than
  their repetition interval are Info instead of Error.
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
