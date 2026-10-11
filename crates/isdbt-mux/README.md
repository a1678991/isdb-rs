# isdbt-mux

ARIB ISDB-T service multiplexer. A declarative TOML configuration (network,
services, elementary streams, events, broadcasters, per-table experiment
overrides) is turned into a constant-bit-rate transport stream that is paced
to the OFDM frame: every call yields exactly the `tsp_per_frame` packets of
each active hierarchical layer, ready for `isdbt_phy::Transmitter`. PSI/SI is
generated with the typed, lossless codecs of `isdb-ts` / `isdb-si`; MPEG-2
video and AAC ADTS files are packetised into PES with PTS/DTS and paced
through an H.222.0 T-STD buffer model; ADTS headers are checked against
ARIB STD-B32 Part 2 §5.2 and, by default, non-conforming input is refused
(nothing is rewritten). A `kind = "ts-program"` stream
rebroadcasts one program of a recorded, unscrambled transport stream as
a terrestrial service: its PES bytes go out unchanged at the
recording's own PCR pace, PTS/DTS rebased onto this multiplex, PSI/SI
generated here; the program, the stream roles (video, main/secondary
audio, captions, superimpose, data) and their output PIDs are read from
the recording, so no PID has to be written by hand. It is the generator half of the bench; `isdb-analyze` is
the independent checker.

## Spec scope

| Spec (edition) | Clauses / tables used |
|---|---|
| ARIB STD-B31 v2.2 | §3.2.1–3.2.2, Table 3-5 multiplex frame and model receiver (timing, via `isdbt_fec::MultiplexPattern`); §3.4 Table 3-3 TSPs per frame → layer capacities and the layouts that fit a rate (`capacity`) |
| ARIB STD-B31 v2.2 Part 2 (Attachment, Operational Guidelines) | §3.2.1, §3.2.3–3.2.4 / Tables 3-1, 3-3, 3-5 (layer of PAT/NIT/CAT and PMT, PCR packets per frame), Table 3-2 (layer robustness) |
| ITU-T H.222.0 (05/2006) | §2.4.2.2–2.4.2.6 T-STD (TBn, MBn, EBn, Bn), §2.4.2.2 eq. 2-4 (packet arrival times interpolated from the PCR: `pcrclock`, `relay`), §2.4.3 packets / adaptation field / PCR, §2.4.3.3 duplicate packets, §2.4.3.7 PES (PTS/DTS rewrite in place), §2.4.4 PAT/PMT/CAT, §2.7.2 PCR interval, §2.7.4 PTS spacing (loop seams), Table 2-34 stream_type |
| ARIB STD-B10 v5.13 | §5.1.1–5.1.4 (version_number, section packing, Table 5-2 repetition, "4KB in 32 ms"), §5.2 NIT/SDT/EIT p/f + schedule/TOT/TDT/BIT, §6.2 descriptors (network name, service list, service, short event, content, component, audio component, stream identifier, local time offset, hierarchical transmission, digital copy control, content availability, video decode control, TS information, terrestrial delivery system, partial reception, system management, broadcaster name, extended broadcaster), Annex C time coding, Annex F (copy-control user_defined), Annex N (network_id range), Appendix §1.4 EIT rules |
| ARIB STD-B32 v3.11 Part 1 | §5.1.1 (vbv_delay 0xFFFF, one frame per video PES), Attachment 2 §5.1 (PTS_DTS flags) |
| ARIB STD-B32 v3.11 Parts 2/3 | Part 2 §5.2.1–5.2.3 (MPEG-2 AAC ADTS header restrictions, enforced by `adts::b32`; channel_configuration mapping), Appendix 1 (ADTS CRC coverage, used for `SilentFrame`), Appendix 2 (AAC buffer sizes), Appendix 3 (adts_buffer_fullness meaning), Attachment Ch. 3 Table 1 (quality_indicator modes, derived); Part 3 §3.1 (stream_id), §3.10 (2) stream type identifier table |
| ARIB STD-B24 v6.4 | SI strings via `isdb-text` |

Not available locally and marked **[from knowledge]** in code and
SPEC-NOTES: ITU-T H.262 (video syntax), ISO/IEC 13818-7 (ADTS header field
widths, bit-reservoir scaling, the few fixed-width element fields
`SilentFrame` writes), ISO/IEC 11172-3 (the CRC-16 cited by B32 Part 2
Appendix 1), ARIB TR-B14 (terrestrial operational guidelines). The
raw_data_block walk and the ISO/IEC 13818-7 Annex A tables it needs are
not included (SPEC-NOTES "AAC ADTS: raw_data_block not parsed").

## Position in the stack

- Depends on: `isdb-ts` (packets, PES, sections, CC, PCR), `isdb-si` (SI
  tables, descriptors, `values`), `isdb-text` (string encoding),
  `isdbt-core` (`Params`, layers), `isdbt-fec` (`MultiplexPattern`, the
  §3.2 slot order). Dev-dependency: `isdbt-phy` (feeds the transmitter).
- Used by: `isdb-cli` — `isdb mux --config …` writes per-layer / full TS
  files, and `isdb tx --mux-config …` runs the multiplexer in-process
  (`crates/isdb-cli/src/mux.rs`, `input.rs`).

## Key types and entry points

| Item | Role |
|---|---|
| [`Config`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/config.rs) | `Config::from_toml` / `to_toml`; `deny_unknown_fields` everywhere. `config::EXAMPLE_TOML` = `examples/bench.toml` |
| [`Multiplexer`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/mux.rs) | `new(&cfg, &params, base_dir)`, `next_frame()` → `[Vec<Packet>; 3]`, `next_frame_full()` → (13-segment multiplex frame, per-layer packets), `slot_time(k, j)`, `stats()`, `tstd_violations()`, `warnings()`; also an endless `Iterator` |
| `layer_slices` | borrows a frame as `isdbt_phy::Transmitter::transmit_frame` takes it |
| [`TsWriter`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/output.rs) | per-layer TS files and/or the multiplex-frame TS (`-` = stdout) |
| `signalling` | typed receiver-facing values (video decode control 0xC8, EIT component types 0x50/0xC4, digital copy control 0xC1, remote control key) by spec-table name |
| `analysis::analyze` | measures a produced stream (section intervals, PCR, PES arrival vs DTS, CC); used by the tests and `isdb mux --report` |
| [`adts`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/adts/mod.rs) | ADTS header (`AdtsHeader`, every field), CRC-16 (`Crc16`), `buffer_channels` (from channel_configuration), `b32` (header `Rule`s with clause + quoted text, `check_frame` / `check_stream`, `conform` (pass or refuse), `cbr_fullness` / `stream_fullness`, `QualityMode`) |
| [`audio`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/audio.rs) | `AdtsEs::load(data, path, AdtsCompliance)` (`conform` / `warn` / `pass-through`), bit rate, derived quality mode; `SilentFrame` (valid silent ADTS frames of any size and header, crc_check computed from the known element layout, for tests) |
| `si`, `video`, `tstd`, `jst` | PSI/SI builder, H.262 parser, T-STD model, JST clock |
| [`capacity`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/capacity.rs) | `layer_bps(params, layer)` (TS rate from Table 3-3 × segments and the frame length), `suggest(params, layer, need_bps, n)` (layer layouts that carry a rate: the most robust code rate per layout, single 13-segment layer, shorter guard intervals), `over_capacity_message` (also used by `isdb tx --mux-ts`) |
| [`pcrclock`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/pcrclock.rs) | `PcrClock`: arrival time of every packet of a recorded TS interpolated from its PCR (H.222.0 eq. 2-4), wraps unwrapped, discontinuities bridged at the previous rate; each packet's `Anchor` maps the PTS/DTS it carries onto the same time line (also used by `isdb tx --mux-ts` pacing) |
| [`relay`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/relay.rs) | `kind = "ts-program"`: `probe` (program selection over PAT/PMT/SDT, ES selection, `classify` → `Role` and the PID plan, carried / dropped descriptors and PIDs, stream parameters, mean / peak rates, the T-STD model on the source timing, source EIT p/f and TOT; `Probe::report`), `select_source_program`, the expansion into service streams, the PCR-paced re-timing; `Multiplexer::relay_status`, `relay_stats`, `relays_finished`, `notes`; `MuxError::OverCapacity` (layer + need, for `--auto-layout`) |
| [`relay_template`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/src/relay_template.rs) | `RelayOptions`: the values of `examples/relay-ts.toml` filled from a few options (source, program / service name, channel, remote key, area code, name, copy control, at_eof, EIT from source, layer, clock) → `to_toml()` / `config()`; what `isdb … --relay FILE` runs and `--print-config` prints |

Example configurations: `examples/bench.toml` (live-broadcaster values, as
seen in a live terrestrial capture), `examples/bench-minimal.toml` (every
value derived from the media, copy-free) — both reference placeholder media
paths `media/video.m2v` and `media/audio.m2a` (relative to the TOML; point
them at your own MPEG-2 video ES and AAC ADTS files; no media is committed)
— and `examples/relay-ts.toml` (rebroadcast a recorded program on UHF ch44,
remote key 9; program and PIDs detected from the recording; its `source =
"recording.ts"` is a placeholder for your own recording). Transmission parameters are
not in the TOML: they come from `Params` (the `isdb` `--mode/--gi/--layer-*`
options). Every key, its default and its spec reference:
[docs/mux-config.md](https://github.com/a1678991/isdb-rs/blob/main/docs/mux-config.md).

```rust
use isdbt_mux::{Config, Multiplexer};
use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, TimeInterleave};

// Mode 3, GI 1/8, partial reception: A = 1 seg QPSK 2/3, B = 12 seg 64QAM 3/4.
let layer = |segments, modulation, code_rate| {
    Some(LayerConfig { segments, modulation, code_rate, time_interleave: TimeInterleave(2) })
};
let params = Params::new(
    Mode::M3,
    GuardInterval::G1_8,
    true,
    [layer(1, Modulation::Qpsk, CodeRate::R2_3), layer(12, Modulation::Qam64, CodeRate::R3_4), None],
)
.unwrap();

// One service whose only stream is declared in the PMT (no media file).
let cfg = Config::from_toml(r#"
[network]
network_id = 0x79EE
name = "ベンチ"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "ベンチHD"
provider = "ベンチ"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF

  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
"#)
.unwrap();

let mut mux = Multiplexer::new(&cfg, &params, std::path::Path::new(".")).unwrap();
let (full, layers) = mux.next_frame_full();
assert_eq!(full.len(), mux.pattern().len()); // multiplex frame, null slots included
for l in 0..3 {
    let want = params.layers()[l].map_or(0, |c| c.tsp_per_frame(params.mode()));
    assert_eq!(layers[l].len(), want); // exactly tsp_per_frame per layer
}
assert!(full.iter().all(|p| p[0] == 0x47));
```

## Design notes

- **Timing model.** Packet arrival = multiplex-frame slot: slot *j* of OFDM
  frame *k* starts at clock `k·samples_per_frame + 408·j` of 512/63 MHz,
  converted to 27 MHz by ×1701/512. PCR values and the PTS/DTS schedule are
  therefore consistent with the transmitted rate by construction.
- **Per-slot priority.** PCR packet at its fixed position (1/2/4 per frame in
  Mode 1/2/3, equal spacing, same positions every frame) → pending section
  packet (earliest deadline first, under the "4KB in 32 ms" window) → PES
  packet the T-STD can accept (earliest DTS first) → null packet.
- **Repetition.** Sections are regenerated at `interval × 0.9`
  (`interval_margin`) of the B10 Table 5-2 criterion; a content change bumps
  version_number. EIT p/f follows the stream clock.
- **Derived, not configured:** guard_interval / transmission_mode / frequency
  codes of 0xFA, partial reception descriptor, PCR count per frame, packets
  per layer, PMT/PAT layer placement, 0xC8 / component types from the media
  (unless set explicitly).
- **ADTS compliance** (`adts_compliance`, default `"conform"`): every
  frame header is checked against B32 Part 2 §5.2 (ID, layer,
  protection_absent, profile, sampling_frequency_index,
  channel_configuration, adts_buffer_fullness,
  number_of_raw_data_blocks_in_frame). `conform` sends conforming input
  byte-identically and refuses anything else with every rule, its clause,
  the reason and what the input needs. It cannot insert crc_check into
  protection_absent '1' input, and it does not rewrite adts_buffer_fullness
  or ID of protected input (the change would invalidate crc_check, whose
  recomputation needs a raw_data_block walk); for 0x7FF it reports the
  constant-rate fullness model instead. `warn` sends the file unchanged
  and reports; `pass-through` skips the checks. The EIT audio
  component descriptor's quality_indicator is `derived` from the measured
  rate per B32 Part 2 Attachment Ch. 3 Table 1 unless set
  (`audio_quality_indicator = "mode-1" | "mode-2" | "mode-3"`).
- **Validation at load** (`Config::validate`, `src/validate.rs`): typed
  signalling values are named variants (numbers rejected with an error naming
  them), remote_control_key_id 1..=12, and a raw descriptor that duplicates
  an active typed descriptor in the same loop is rejected. A PID on two
  layers is rejected (`src/mux.rs`).
- **`ts-program` relay** (`relay`, SPEC-NOTES "Rebroadcasting a recorded
  program"): at load the source is probed (30 s of PCR by default) and the
  entry expands into one stream per relayed ES (default selection: video,
  audio, PES private data; carousels 0x05/0x0B–0x0D only with
  `include_carousels`; `stream_types`, `component_tags`). The program is
  `program` / `service_id`, `service_name` (source SDT), or the only one
  (else an error listing the programs with names and streams). Each ES
  gets a role (`relay::classify`: stream_type; data_component_id 0x0008
  caption/superimpose split by component_tag range, data_component
  Timing, PES PTS presence; main audio = lowest component_tag) and, with
  `pid_map = "auto"` (default), the role's PID from the plan `pids = {
  video = 0x0111, audio = [0x0112, …], captions = 0x0130, superimpose =
  0x0138, data = 0x0140 }`; `pid_map = "keep"` or an explicit list
  checked against the source PMT. ES
  descriptors carried by type: stream_identifier, video_decode_control,
  data_component; hierarchy and unknown tags dropped (all listed in
  `Multiplexer::notes`). Conditional access (`ca = "keep"`, default): the
  source PMT's CA descriptors (program and ES loops), the ECM PIDs and the
  source CAT's CA_descriptors with the EMM PIDs present are relayed,
  CA_PIDs following the PID mapping; `ca = "drop"` relays none. Copy
  control: every 0xC1 / 0xDE of the source's PMT program and ES loops,
  SDT and EIT p/f is reported; `recording_control = "source"` takes PMT >
  ES > SDT > EIT and keeps the per-component ES-loop 0xC1, an explicit
  value is used as given; one note says `source copy control: …; relayed
  as: …`, and raw overrides of a relayed service's copy control are
  reported with what is sent. At run time a reader thread feeds the
  source; each packet's PCR-interpolated time + a constant (multiple of
  300 ticks, so PTS/DTS move by an exact 90 kHz constant) is its earliest
  slot (a due packet whose T-STD buffer would overflow waits up to 2 ms:
  output slot jitter); payload bytes stay the source's packet by packet. Cut-in at the
  first video PES starting with a sequence_header (leading B-picture PES of
  an open GOP dropped); loop with continuous timestamps (period ≥ arrival
  span, every PID's PTS span + one step, and the last decoding time capped
  at a 0.5 s PTS gap) or end at EOF. The rate check refuses a layer that
  cannot carry the rate the whole recording needs (`relay_rate`: every
  packet by its PES's decoding time, read once over the whole file, not
  just the probed start) + PCR/PSI/SI (`capacity::suggest` lists what
  fits); at run time, PES that leave after their decoding time raise an
  alert (`Multiplexer::take_alerts`).
- **Experiment escape hatches** on every table: `interval_ms`, `layer`,
  `version`, `add_descriptors`, `header`, `byte_patches` (not checked),
  `replace_sections` (sent byte-identical), `extra_sections`; plus
  `[[raw_table]]` streams.
- **Operational choices** (values not dictated by the local specs — PCR
  positions, SI layer, no section packing, descriptor order, copy-control
  defaults, etc.) are listed, numbered, at the end of
  [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/SPEC-NOTES.md), with which ones follow live practice and
  which a bisection on a Sony BDZ-series recorder showed to matter
  (copy_control_type `'00'` in 0xC1 user_defined is refused; `'01'`
  plays).

## Verification

| Test | What it proves | Data |
|---|---|---|
| `tests/mux.rs` `tables_conform_and_repeat` | 40 s of output: every PAT/CAT/PMT passes `isdb_ts::psi::check_*`, every SI section decodes typed, passes `isdb_si::conformance` and re-encodes byte-identically; every section repeats within B10 Table 5-2; CC continuous; version bump at an event change | synthetic media |
| `tests/mux.rs` `pcr_and_timestamps` | 4 PCRs/frame per service, ≤ 0.1 s, PCR deltas = slot distance × 408 clocks (±1 tick), equal spacing in the 13-segment TS and in the layer-A-only TS; DTS increasing, DTS ≤ PTS, every PES arrives before its DTS and ≤ 1 s early; no T-STD violation | synthetic media |
| `tests/mux.rs` other tests | EIT p/f switch at an event boundary; overrides (transplanted SDT byte-identical, added descriptors, header, byte patch with valid CRC); EIT schedule / TDT / PCR in the video PID / custom intervals; derived NIT values; hierarchical descriptor; `example_config_parses`; `feeds_transmitter` (3 frames into `isdbt_phy::Transmitter`) | synthetic media |
| `tests/relay_roles.rs` | permuted source PIDs, two audio streams (secondary first in the PMT), caption / superimpose / untagged ES with injected synchronous (0xBD, PTS) and asynchronous (0xBF) PES: roles and plan PIDs 0x0111/0x0112/0x0113/0x0130/0x0138/0x0139, carried 0xFD, PES bytes on the new PIDs, validation clean; three-program recording: ambiguity error listing number / SDT name / streams, selection by `program`, `service_id`, `service_name` (full-width), substring ambiguity, not found, only-program-with-packets; explicit `pid_map` errors (not an ES, not relayed, output collision), `"keep"`, per-role `pids`, config errors; the template's defaults = `examples/relay-ts.toml` | synthetic media |
| `tests/relay.rs` | a synthetic source of another network (CA descriptors in program and ES loops, ECM PID, CAT with an EMM PID, program and per-component copy control, a carousel stream; PCR and PTS wrap 2 s in; unscrambled) generated by this crate, relayed for 21 s (2+ loop passes): `isdb-analyze` validation without errors; no T-STD violation or late PES; ES bytes identical PES by PES across passes; the arrival→DTS distance equal to the source's within 5 ms (first pass) and never earlier / at most 0.5 s held after a seam; PTS continuous across seams and the wrap; CA descriptors, ECM and EMM relayed with `ca = "keep"` (CA_PIDs following the PID mapping) and absent with `"drop"`; carousel and foreign SI gone, stream_identifier carried, 0xC8 carried; source copy control found in PMT / ES / SDT / EIT (and 0xDE) with the PMT > ES > SDT > EIT precedence, kept with `"source"` (ES-loop 0xC1 too), reported with an explicit value; raw copy-control overrides reported with the values sent; source EIT p/f copied; `at_eof = "end"` stops; a 1-segment layer is refused with layouts that fit (`rate_check = "warn"` relays anyway); ts-program key validation; `examples/relay-ts.toml` parses | synthetic media |
| `tests/relay_tstd.rs` | a source multiplexed by this crate (≈ 11 Mbit/s video, MBn/EBn filled to their limits, its own T-STD clean) relayed: the probe finds the source clean and the relay output has no T-STD violation (MBn busy ≤ 1 s) and stays within 5 ms of the source pace | synthetic media |
| `tests/relay_rate.rs` | a source light for its first seconds (≈ 1.6 Mbit/s) and heavy after (≈ 11 Mbit/s), with a 1-s probe: the probe sees only the start, the whole-file profile the rest; a layer that carries the start but not the rest (12 seg 16QAM 1/2) is refused, and with `rate_check = "warn"` the relay leaves PES after their decoding time and raises an alert; a layer above the need relays every PES in time | synthetic media |
| `tests/signalling.rs` | both shipped examples re-decode to the named variants (live-broadcaster values for `bench.toml`, derived/copy-free for `bench-minimal.toml`); synthetic PMTs built with the isdb-ts/isdb-si encoders (three HD services with CA / access control / copy control `a4 42` / content availability in the program loop and stream identifier, video decode control `47`, data component 0x0008/0x000C in the ES loops; a data-only service; the one-seg service with copy control `a8 02`, H.264, data component 0x0012/0x000D) re-encode byte-identically and every descriptor decodes to its named isdb-si variant; raw duplicates rejected; remote-key range; old numeric forms rejected with variant names; maximum_bitrate / component loop encoding; examples round-trip through TOML | synthetic (built in the test) |
| `tests/adts.rs` `media_audio_gate_follows_header_report` | your own ADTS file: frame count = header report; `conform` passes it byte-identically exactly when no error-severity rule is violated, else refuses it ("crc_check cannot be inserted" for protection_absent '1') with a fullness model (≠ 0x7FF, ≤ the buffer); `warn` sends it unchanged; the quality mode is Table 1's for the file's own rate | `$ISDB_RS_MEDIA_DIR/audio.m2a` (SKIP when unset / absent) |
| `tests/adts.rs` `capture_all_frames_conform` | every frame of every unscrambled ADTS PID of a terrestrial capture (e.g. the one-seg audio, sent in the clear; scrambled PIDs do not parse as ADTS and are skipped) passes every header rule and `conform` byte-identically; fullness never 0x7FF | `$ISDB_RS_CAPTURE_TS` (SKIP when unset / absent) |
| `tests/media.rs` `media_ffprobe` | 10 s of `bench.toml` with your own media: no CC events, no T-STD violation, no late PES; ffprobe detects mpeg2video + aac with the expected duration and the width, height and sampling frequency the files declare; ffmpeg decode errors are printed | opt-in: `video.m2v` and `audio.m2a` in the directory named by `ISDB_RS_MEDIA_DIR`, and `ffprobe` — prints `SKIP` and passes when the variable is unset or a file or ffprobe is missing |
| unit tests in `src/` | capacity rates (Table 3-3: B 12 seg 64QAM 3/4 = 16.85, 7/8 = 19.66, 13 seg 7/8 = 21.30 Mbit/s at GI 1/8) and suggestions; PCR clock (eq. 2-4 interpolation, wrap, discontinuity at the previous rate, reference PID lock); PTS/DTS rewrite keeping prefix and marker bits; GOP flags after the 25-bit time_code; ADTS header round trip, CRC-16 check value, `SilentFrame` crc_check against an independent Appendix 1 computation, one frame per B32 Part 2 header rule (each violating exactly that rule), `conform` pass-through of conforming input and its refusals (protection_absent '1', protected input needing a header rewrite, re-encode cases), `warn`/`pass-through`, reservoir model (recurrence, VBR rate search, cap), Table 1 mode derivation; B10 Tables 6-62/6-63 codes, frequency codes, B31 Table 3-2 ranking, ADTS parsing, Annex C examples, IBBP timestamps, vbv_delay rewrite, copy-control user_defined, remote-key range, 0xC8 resolution, T-STD TB limit / underflow | — |

Synthetic media (`tests/common/mod.rs`): closed-GOP 1440×1080i MPEG-2
pictures (enough for access-unit, timestamp and buffer checks, not
decodable pictures) and 48 kHz stereo ADTS frames that are valid silent
AAC (`audio::SilentFrame`: a CPE with max_sfb 0 plus fill elements, with
crc_check and a constant-rate adts_buffer_fullness), so they pass the
default `conform` unchanged.

## Known limitations

- A PID carried on two layers (B31 Part 2 §3.2.2 duplicate packets for
  one-seg continuity) is not implemented; such configurations are rejected.
- Media: MPEG-2 video and AAC ADTS only; other streams (e.g. H.264 one-seg
  video) can only be `declared` in the PMT, without packets. Constant
  sampling frequency / channel configuration required for ADTS.
- H.262, ISO/IEC 13818-7 and TR-B14 are not available: the syntax and
  operational values taken from them are from knowledge (marked in
  SPEC-NOTES).
- ADTS: header-level checks only and no repair. Input without crc_check
  or with adts_buffer_fullness 0x7FF / ID '0' is refused by `conform`
  and must be re-encoded by an encoder that writes the ADTS crc_check and
  a constant-rate fullness itself. The raw_data_block rules (coupling
  channel, element order, PCE fields, SBR fill, LC tools, crc_check
  value) are not checked (no ISO/IEC 13818-7 tables; SPEC-NOTES "AAC
  ADTS: raw_data_block not parsed"). §5.2.3 (3) (a)–(c) (PCE timing on
  audio-mode switches) is not checked. Only digital terrestrial TV's
  sampling-frequency list is applied (not BS/CS or V-Low).
- T-STD: TBsys/Bsys for PSI not modelled; PES header bytes counted with the
  access unit (conservative); Main/Low-level MBn without the
  `VBVmax − vbv_buffer_size` term.
- Media looping offsets timestamps by the longest stream's duration; the
  shorter stream has a gap at the seam (up to one frame of the shorter
  stream, e.g. 17 ms of video).
- `ts-program`: a PES is sent only once complete (1.2 s look-ahead);
  audio PES that do not start on an ADTS frame (e.g. fixed 720-byte PES) make the
  first relayed audio PES start mid-frame (one decoder resync at the cut-in;
  the PES bytes are not trimmed) and the T-STD model treats each PES as one
  access unit at its PTS (approximate for frames straddling PES); ESCR is not
  rewritten (counted); the PCR_PID must be a dedicated PID; the source EIT
  p/f is copied once at start (no later updates); a loop seam leaves a PTS
  gap of up to one source lead (≤ 0.5 s + one frame).
- `ts-program` roles: the caption / superimpose split first uses the
  TR-B14/TR-B15 component_tag ranges 0x30–0x37 / 0x38–0x3F (not available
  locally, from knowledge; the local B24 text says the coding does not
  distinguish them), then the data_component Timing field and the PES PTS
  presence (B24). The default PID plan is an operational choice. Program
  selection and PES hints use only the probed part (default 30 s of PCR):
  a stream or SDT entry that first appears later is not seen. Elementary
  streams whose PMT entry changes mid-recording are not followed (the PMT
  is read once).
- Sections always start in a fresh packet (no packing); TR-B14 per-layer EIT
  PIDs are not applied by default.
- The timing model's real-receiver confirmation is still pending
  (SPEC-NOTES "Timing model").

## See also

- [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/SPEC-NOTES.md) — interpretations, receiver evidence and
  the numbered operational choices.
- Workspace [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
