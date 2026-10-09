# Multiplexer configuration reference (`arib mux` TOML)

`arib mux -c FILE.toml`, `arib tx --mux-config FILE.toml` and
`arib loopback --mux-config FILE.toml` read a declarative description of one
ISDB-T transport stream: the network, the services and their elementary
streams, the events, and options for every generated PSI/SI table. This page
lists every key the multiplexer accepts, with its type, default and the
specification clause it implements. It is written from the configuration
structs in [`crates/arib-mux/src/config.rs`](../crates/arib-mux/src/config.rs)
and [`signalling.rs`](../crates/arib-mux/src/signalling.rs); when this page
and the code disagree, the code wins.

- [General rules](#general-rules)
- [A minimal configuration](#a-minimal-configuration)
- [`[network]`](#network)
- [`[transport]`](#transport)
- [`[clock]`](#clock)
- [`[timing]`](#timing)
- [`[[service]]`](#service)
- [`[[service.stream]]`](#servicestream)
- [`[[service.event]]`](#serviceevent)
- [Typed signalling options](#typed-signalling-options)
- [Table options](#table-options)
- [`[[raw_table]]`](#raw_table)
- [Load-time checks](#load-time-checks)
- [Receiver-relevant settings](#receiver-relevant-settings)

## General rules

- **Transmission parameters are not in the TOML.** Mode, guard interval,
  partial reception and the layer layouts come from the same
  `--mode / --gi / --partial / --layer-a / --layer-b / --layer-c` options as
  `arib tx` (defaults: Mode 3, GI 1/8, partial reception, A = 1 segment QPSK
  2/3, B = 12 segments 64QAM 3/4). Everything ARIB derives from them is
  derived, not configured: the guard_interval and transmission_mode codes and
  the frequency list of the terrestrial delivery system descriptor (B10
  Tables 6-62/6-63), the partial reception descriptor, the number of PCR
  packets per frame and the packets per layer.
- **Unknown keys are errors.** Every table is parsed with
  `deny_unknown_fields`, so a misspelt key is reported with the list of keys
  that are valid at that place.
- **Integers** may be written in decimal or hex (`0x01F0`).
- **Hex strings** (descriptor bodies, sections) may contain spaces, `:` and
  `_`.
- **Paths** (`source`) are relative to the directory of the TOML file.
- **Times** are JST, `YYYY-MM-DDTHH:MM:SS` (a space instead of `T` is also
  accepted); durations are `HH:MM:SS`; time offsets `HH:MM`.
- **Receiver-facing values are named, not numbered.** Copy control, video
  decode control, component types and the audio quality indicator take the
  names of the B10 table entries. A number is rejected with an error that
  names the table and lists the valid names, for example:

  ```text
  digital_recording_control_data (B10 §6.2.23 Table 6-39) takes a named value,
  not a number; 2 = '10' is "copy-one-generation"; one of: "copy-free" ('00'),
  "provider-defined" ('01'), "copy-one-generation" ('10'), "copy-never" ('11')
  ```

Ready-made configurations:

| Source | What it is |
|---|---|
| `arib mux --print-config-example` | the annotated example, identical to [`crates/arib-mux/examples/bench.toml`](../crates/arib-mux/examples/bench.toml): a full-segment HD service with MPEG-2 video and AAC from files, plus a declared one-seg service, with the values live terrestrial broadcasters send |
| [`examples/bench-minimal.toml`](../crates/arib-mux/examples/bench-minimal.toml) | the same with every receiver-facing value derived from the media and copy-free |
| [`examples/relay-ts.toml`](../crates/arib-mux/examples/relay-ts.toml) | rebroadcast one program of a recorded TS (`kind = "ts-program"`) |
| `arib mux --relay FILE --print-config` | the relay configuration built for a given recording, with the chosen layout in a header comment |

The two `bench*.toml` examples reference the placeholder media paths
`media/video.m2v` and `media/audio.m2a`; no media is shipped. Point them at
your own MPEG-2 video elementary stream and AAC ADTS file, or replace the two
streams with `kind = "declared"` entries as in the next section.

## A minimal configuration

One HD service whose video is only declared in the PMT, so no media file is
needed. PAT, CAT, PMT, NIT, SDT, EIT present/following, TOT, BIT, PCR and null
packets are generated:

```toml
[network]
network_id = 0x7FE0
name = "テスト"
area_code = 0x8D5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x7FE0

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "テストHD"
provider = "テスト"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = { recording_control = "copy-free" }

  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "試験放送"
```

```sh
arib mux -c minimal.toml --seconds 5 --out-ts test.ts --report
arib ts validate test.ts
```

`arib ts validate` reports one warning for this file (`pid.missing`: PID
0x0111 is listed in the PMT but carries no packets), which is what a declared
stream is. See [getting-started.md](getting-started.md) and
[analysis.md](analysis.md) for what to do with the output.

## `[network]`

The NIT actual network (B10 Part 2 §5.2.4) and its terrestrial descriptors.
Required.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `network_id` | u16 | required | B10 §5.2.4; Annex N | Terrestrial TV range 0x7880–0x7FE8 (outside it the multiplexer warns). Also the default `original_network_id`. |
| `name` | string | required | network_name_descriptor 0x40, §6.2.11 | ARIB 8-unit coded by `arib-text`. |
| `area_code` | u16 (12 bits) | required | terrestrial_delivery_system_descriptor 0xFA, §6.2.31; Annex D Table D-2 | e.g. 0x5A5 Kanto wide area, 0x8D5 Kinki wide area. |
| `remote_control_key_id` | integer 1..=12 | required | TS_information_descriptor 0xCD, §6.2.42 Table 6-82 | B10 gives no range; arib-mux accepts the 12 one-touch keys. Receivers number the services key·10 + index (key 9 → 091, 092 …). See [receiver-relevant settings](#receiver-relevant-settings). |
| `ts_name` | string | `name` | 0xCD ts_name | |
| `channels` | array of u32 (13..62) | `[]` | 0xFA frequency, §6.2.31 | Japanese UHF channel numbers; each gives one frequency code, the centre 473 + 6·(ch − 13) + 1/7 MHz in units of 1/7 MHz (ch 27 → 3900). **Must equal the channel you transmit on.** |
| `frequencies` | array of u16 | `[]` | 0xFA frequency | Extra raw frequency codes (1/7 MHz units), appended after `channels`. |
| `additional_broadcasting_identification` | u8 | `0x01` | system_management_descriptor 0xFE, §6.2.21 Table 6-34 | "specified by the operation standard of service providers": an operational choice. |
| `system_management` | bool | `true` | 0xFE in the NIT network loop | |
| `transmission_types` | array of `{ info = u8, layer = "A"\|"B"\|"C" }` | `[]` | 0xCD transmission_type loops, §6.2.42 | transmission_type_info is "defined by the operational guidelines of service providers". The service_id list of each loop is derived from the services on that layer. Live broadcasters send `{ info = 0x0F, layer = "B" }, { info = 0xAF, layer = "A" }`. |
| `partial_reception_descriptor` | bool | `true` | partial_reception_descriptor 0xFB, §6.2.32 | Sent only with partial reception and at least one service on layer A; lists those services. |

Generated in the NIT TS loop: 0xFA (area, guard interval and mode codes
derived from the transmission parameters, frequencies), 0xCD, 0x41
service_list_descriptor (every service with its service_type) and 0xFB.

## `[transport]`

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `transport_stream_id` | u16 | required | H.222.0 Table 2-30; B10 §5.2.4 | PAT, NIT TS loop, SDT, EIT. |
| `original_network_id` | u16 | `network_id` | B10 §5.2.4 ("the network_id and the original_network_id shall take the same value") | |

## `[clock]`

The wall clock behind TOT/TDT and EIT "now". Optional.

| Key | Type | Default | Notes |
|---|---|---|---|
| `source` | `"system"` \| `"fixed"` | `"system"` | `system`: the system clock when the multiplexer starts; `fixed`: `start`. Either way it then advances with the stream clock (not the wall clock), so a file covers stream time. |
| `start` | time (JST) | none | Required for `fixed`. Use `fixed` for reproducible output. |
| `start_pcr` | u64 | `0` | PCR (27 MHz ticks) of the first multiplex slot. |

## `[timing]`

PCR, PES and section scheduling. Optional.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `av_delay_ms` | u32 | `700` | — | From the first multiplex slot to the first DTS. |
| `max_std_delay_ms` | u32 | `1000` | H.222.0 §2.4.2.6 ("less than or equal to one second") | Upper bound on the delay of any byte through the T-STD model. |
| `pcr_per_frame` | u32 | 1 / 2 / 4 in Mode 1 / 2 / 3 | B31 Part 2 §3.2.4 Table 3-5; H.222.0 §2.7.2 | PCR packets per OFDM frame per service, at fixed, equally spaced slots. Must divide the layer's TSPs per frame and keep the interval ≤ 100 ms. |
| `interval_margin` | f64 | `0.9` | — | Sections are scheduled at `interval_ms × margin`, so the measured interval stays inside the maximum. |
| `si_max_bytes` | u32 | `4096` | B10 §5.1.4 ("4KB in 32 ms") | Maximum bytes of 188-byte packets of one section PID in any `si_window_ms` window. |
| `si_window_ms` | u32 | `32` | B10 §5.1.4 | |

## `[[service]]`

One service (program). Repeat the table for each service.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `service_id` | u16 | required | B10 §5.2.6 (service_id = program_number) | |
| `service_type` | u8 | `0x01` | service_descriptor 0x48, Table 6-25 | 0x01 digital TV; one-seg services conventionally use 0xC0 (data service). |
| `provider` | string | `""` | 0x48 service_provider_name | |
| `name` | string | required | 0x48 service_name | |
| `layer` | `"A"` \| `"B"` \| `"C"` | required | — | Layer of the service's elementary streams; must be active. |
| `pmt_pid` | u16 | required | H.222.0 §2.4.4.8 | One-seg services conventionally use 0x1FC8–0x1FCF. |
| `pcr_pid` | u16 | `0x1FFF` (no PCR) | H.222.0 §2.4.4.9 | May equal an elementary PID (PCR in the video packets) except for a `ts-program` stream, whose packets keep their payload; a dedicated PID (e.g. 0x01FF) then carries adaptation-field-only PCR packets. |
| `running_status` | u8 | `0` | SDT running_status, Table 5-6 | 0 "undefined" is what the live terrestrial stations observed during development send; 4 = running. |
| `free_ca_mode` | bool | `false` | SDT / EIT free_CA_mode | |
| `eit_user_defined_flags` | u8 (3 bits) | `0x7` | SDT EIT_user_defined_flags | `'111'` = unused. |
| `eit_pf` | bool | `true` | B10 §5.2.7 | Generate EIT present/following for this service. |
| `eit_schedule` | bool | `true` | B10 §5.2.7 | Include the service in the EIT schedule when `[eit_schedule] enabled = true`. |
| `copy_control` | table | none | digital_copy_control_descriptor 0xC1, §6.2.23 | See [copy_control](#copy_control). Without it no 0xC1 is sent. |
| `content_availability` | table | none | content_availability_descriptor 0xDE, §6.2.45 | See [content_availability](#content_availability). |
| `program_descriptors` | array of `{ tag = u8, hex = "…" }` | `[]` | PMT program_info | Raw descriptors (body bytes after descriptor_length). |
| `sdt_descriptors` | array of `{ tag, hex }` | `[]` | SDT service loop | Raw, after the generated service_descriptor. |
| `pmt` | table | see [table options](#table-options) | | `[service.pmt]`: the PMT's options. |
| `stream` | array of tables | `[]` | | `[[service.stream]]`, below. |
| `event` | array of tables | `[]` | | `[[service.event]]`, below. |

## `[[service.stream]]`

One elementary stream of the enclosing service. `kind` selects what it is:

| `kind` | Meaning |
|---|---|
| `"mpeg2-video"` | An ITU-T H.262 \| ISO/IEC 13818-2 video elementary stream file, packetised into PES with PTS/DTS (one picture per PES, B32 Part 1 §5.1.1). Default stream_type 0x02. |
| `"aac-adts"` | An ISO/IEC 13818-7 AAC file in ADTS frames, checked against B32 Part 2 §5.2 (`adts_compliance`). Default stream_type 0x0F. |
| `"declared"` | Listed in the PMT only; no packets are sent. Needs `stream_type`. Use it for streams you cannot packetise (for example a one-seg H.264 stream, stream_type 0x1B) or to build a test multiplex without media. |
| `"ts-program"` | One program of a recorded transport stream, rebroadcast as this service: its PES bytes are sent unchanged at the recording's own PCR pace, PTS/DTS are rebased onto this multiplex, and all PSI/SI is generated here. See [ts-program keys](#ts-program-keys). |

Media files are looped; the timestamps continue across the seam.

### Common keys

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `kind` | see above | required | | |
| `pid` | u16 | required (not for `ts-program`) | H.222.0 Table 2-3 | 0x0010–0x1FFE. A PID used on two layers is rejected. |
| `stream_type` | u8 | 0x02 (`mpeg2-video`), 0x0F (`aac-adts`); required for `declared` | H.222.0 Table 2-34; B32 Part 3 §3.10 (2) | |
| `component_tag` | u8 | none | stream_identifier_descriptor 0x52, §6.2.16 | Also the component_tag of the EIT component descriptors. |
| `source` | path | none | | Media file; required for `mpeg2-video`, `aac-adts` and `ts-program`. |
| `layer` | `"A"` \| `"B"` \| `"C"` | the service's layer | | For hierarchical (high/low quality) services whose components sit on different layers. |
| `hierarchical` | `{ reference_pid = u16, high_quality = bool }` | none | hierarchical_transmission_descriptor 0xC0, §6.2.22 | `reference_pid`: the stream of the other quality level in the same service. `high_quality` (quality_level '1') defaults to the stream on the less robust layer (B31 Part 2 Table 3-2 ranking). |
| `descriptors` | array of `{ tag, hex }` | `[]` | PMT ES_info | Raw descriptors. |
| `eit_component` | bool | `true` | component_descriptor 0x50 §6.2.3 / audio_component_descriptor 0xC4 §6.2.26 | Generate the EIT component descriptor of this stream (video and audio streams with media). |

### `mpeg2-video` keys

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `video_decode_control` | table | `{ mode = "derived" }` | 0xC8, §6.2.30 | See [video_decode_control](#video_decode_control). |
| `component_type` | `"derived"` or a Table 6-5 name | `"derived"` | 0x50, Table 6-5 | See [component_type](#component_type). |
| `force_vbv_delay_ffff` | bool | `true` | B32 Part 1 §5.1.1 ("vbv_delay shall always be set to 0xFFFF") | Rewrites vbv_delay in every picture header. |
| `mark_broken_link` | bool | `true` | H.262 GOP header | Sets broken_link in the first GOP of an open GOP at each (re)start of the file. |

### `aac-adts` keys

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `frames_per_pes` | u32 | `1` | — | ADTS frames per PES packet. |
| `component_type` | `"derived"` or an audio table | `"derived"` | 0xC4, Table 6-43 | See [component_type](#component_type). |
| `audio_quality_indicator` | `"derived"` \| `"mode-1"` \| `"mode-2"` \| `"mode-3"` | `"derived"` | 0xC4 quality_indicator, Table 6-44; B32 Part 2 Attachment Ch. 3 Table 1 | See [audio_quality_indicator](#audio_quality_indicator). |
| `adts_compliance` | `"conform"` \| `"warn"` \| `"pass-through"` | `"conform"` | B32 Part 2 §5.2 | See [adts_compliance](#adts_compliance). `arib mux --adts-compliance` overrides it for every stream. |

### `declared` keys

Only the common keys apply. `component_type` is accepted but unused (a
declared stream has no media, so no EIT component descriptor is generated);
`video_decode_control = { mode = "explicit", … }` does emit a 0xC8.

### `ts-program` keys

A `ts-program` entry expands at load into one stream per relayed elementary
stream of the source program. Its `pid`, `stream_type`, `component_tag`,
`descriptors`, `hierarchical`, `video_decode_control` and `component_type`
come from the source PMT (giving them is an error). ES descriptors carried
over: stream_identifier (0x52), video_decode_control (0xC8) and
data_component (0xFD); CA, copy control, hierarchy and unknown descriptors,
all program-loop descriptors and the ECM/EMM PIDs are dropped (the start-up
report lists every drop). Background and rules:
[arib-mux README](../crates/arib-mux/README.md) and
[transmitting.md](transmitting.md#relaying-a-recording).

| Key | Type | Default | Notes |
|---|---|---|---|
| `source` | path | required | The recorded TS (188/192/204-byte packets). It must not be scrambled. |
| `program` (alias `service_id`) | u16 | the only program, or the only one whose streams carry packets | program_number in the source PAT. Otherwise the load fails with a list of the programs, their SDT names and streams. |
| `service_name` | string | none | Select by the source SDT service name: exact match first, then a unique substring; full-width ASCII and case folded (`TV12` finds `ＴＶ１２テスト`). Exclusive with `program`. |
| `stream_types` | array of u8 | `[0x01, 0x02, 0x1B, 0x24, 0x03, 0x04, 0x0F, 0x11, 0x06]` | Relay the elementary streams with these stream_types: video (MPEG-1/2, H.264, HEVC), audio (MPEG-1/2, AAC ADTS / LATM) and PES private data (captions, superimpose, data PES). |
| `component_tags` | array of u8 | all | Relay only streams whose stream_identifier_descriptor carries one of these tags. |
| `include_carousels` | bool | `false` | Also relay section streams (stream_type 0x05 private sections, 0x0B–0x0D DSM-CC data carousels), packet by packet. |
| `pid_map` | `"auto"` \| `"keep"` \| `[[source, output], …]` | `"auto"` | `auto`: output PID by stream role from `pids`. `keep`: the source PIDs. A list: explicit pairs; every source PID must be a relayed elementary stream of the program; output PIDs 0x0010–0x1FFE; unlisted relayed streams keep their source PID. |
| `pids` | `{ video, audio, captions, superimpose, data }`, each a PID or a list | `{ video = 0x0111, audio = 0x0112, captions = 0x0130, superimpose = 0x0138, data = 0x0140 }` | Only with `pid_map = "auto"`. Streams of a role, in component_tag order (then PMT order), take the role's PIDs in turn; extra streams take the next free PIDs after the last entry (a second audio stream gets 0x0113). The defaults are what terrestrial broadcasters use: an operational choice, not a spec value. Roles: main audio = lowest component_tag; captions and superimpose (both data_component_id 0x0008) are split by component_tag range, then by the data_component Timing field, then by PES PTS presence. |
| `at_eof` | `"loop"` \| `"end"` | `"loop"` | `loop`: restart at the first random access point with continuous timestamps. `end`: stop relaying; the multiplex then carries PSI/SI and nulls (`arib tx` ends when every relay has ended). |
| `eit_from_source` | bool | `false` | Take the EIT present/following events (name, text, genre, duration) from the source's EIT p/f for the program; the configured `[[service.event]]` entries are used when this is off or the source has none. (`--relay` sets it to `true`.) |
| `rate_check` | `"error"` \| `"warn"` | `"error"` | When the measured mean rate plus PCR/PSI/SI exceeds the layer capacity: refuse, listing layer layouts that fit, or warn and relay anyway (the stream then falls behind its PCR schedule). `--auto-layout` on the command line picks a layout instead. |
| `max_early_ms` | u32 | `0` | A packet may be sent up to this long before its PCR-derived time when the stream's T-STD model accepts it (smooths rate peaks into idle slots). 0 = strictly at the recording's pace. |
| `probe_seconds` | f64, 1..600 | `30` | Seconds of the source's PCR time measured at start for the rate check, the stream parameters, program selection and the source EIT. |

Service-level settings that matter for a relay: give the service a
dedicated `pcr_pid`, and use `copy_control = { recording_control = "source" }`
to keep the recording's own copy restriction (see
[copy_control](#copy_control)).

## `[[service.event]]`

Events of the service's EIT (present/following and, when enabled, schedule).
Events are sorted by start time and must not overlap.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `event_id` | u16 | required | B10 §5.2.7 | |
| `start` | time (JST) | none | EIT start_time (Annex C MJD/BCD) | Give `start` or `start_offset_s`, not both. |
| `start_offset_s` | i64 | none | | Seconds relative to the stream clock start (negative: already running). If neither is given: the previous event's end, or the clock start floored to the hour. |
| `duration` | `HH:MM:SS` | required | EIT duration | |
| `name` | string | required | short_event_descriptor 0x4D, §6.2.15 | Always generated (ISO 639 "jpn"). |
| `text` | string | `""` | 0x4D text | |
| `content` | array of u8 | `[]` | content_descriptor 0x54, §6.2.4 | Genre bytes `content_nibble_level_1 << 4 \| level_2` (e.g. `0x00` news). |
| `descriptors` | array of `{ tag, hex }` | `[]` | EIT event loop | Raw descriptors. |

## Typed signalling options

### copy_control

`copy_control` on a `[[service]]`: the digital_copy_control_descriptor
(0xC1, B10 §6.2.23 Table 6-38) with its placement.

```toml
copy_control = { recording_control = "copy-one-generation", copy_control_type = "serial-output-encoded", aps_control_data = "copy-free", maximum_bitrate_mbps = 16.5, in_pmt = true, in_sdt = true, in_eit = false }
```

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `recording_control` | name, see below | required | digital_recording_control_data, Table 6-39 | `"source"` is accepted only on a service with a `ts-program` stream. |
| `copy_control_type` | name, see below | `"serial-output-encoded"` ('01') | user_defined bits 3–2, Annex F Table F-2 | Keep '01': see [receiver-relevant settings](#receiver-relevant-settings). |
| `aps_control_data` | name, see below | `"copy-free"` ('00') | user_defined bits 1–0, Annex F Table F-4 | Only with `copy_control_type = "serial-output-encoded"`; with any other type the two bits are reserved_future_use and sent '11'. |
| `user_defined` | integer 0..=15 | none | §6.2.23 user_defined | Raw experiment override of the 4 bits; conflicts with `copy_control_type` / `aps_control_data`. |
| `maximum_bitrate_mbps` | number, 0 < x ≤ 63.75 | none (no maximum_bitrate) | §6.2.23 ("rolling up in each 1/4Mbps") | Sent as ⌈x × 4⌉ quarter-Mbit/s units (16.5 → 66). |
| `component_control` | array of tables | `[]` | Table 6-38 component loop | Each: `component_tag` (required), `recording_control` (required, no `"source"`), `copy_control_type`, `aps_control_data`, `user_defined`, `maximum_bitrate_mbps` as above. Rejected together with `in_pmt = true` (B10: "When this descriptor is transmitted by PMT, component control flag should always be '0'"); the component loop is sent only outside the PMT. |
| `in_pmt` | bool | `true` | | PMT program loop. |
| `in_eit` | bool | `true` | | Every EIT event of the service. |
| `in_sdt` | bool | `false` | | SDT service loop. |

The user_defined value is copy_control_type then aps_control_data, so the
default is 0b0100, the value live terrestrial broadcasters send. The Annex F
layout is informative in B10; the ARIB operational guidelines (TR-B14 /
TR-B15, not available to this project) are understood to make it normative.

| Field | Name (code) |
|---|---|
| `recording_control` (Table 6-39) | `"copy-free"` ('00'), `"provider-defined"` ('01'), `"copy-one-generation"` ('10'), `"copy-never"` ('11'), `"source"` (the relayed source program's value: its PMT program loop, else the most restrictive relayed ES loop; copy-free only if the source signals none) |
| `copy_control_type` (Table F-2) | `"undefined-00"` ('00'), `"serial-output-encoded"` ('01'), `"undefined-10"` ('10'), `"serial-output-unencoded"` ('11') |
| `aps_control_data` (Table F-4) | `"copy-free"` ('00'), `"pseudo-sync"` ('01'), `"pseudo-sync-2-line-burst"` ('10'), `"pseudo-sync-4-line-burst"` ('11') |

### content_availability

`content_availability` on a `[[service]]`: the content_availability_descriptor
(0xDE, B10 §6.2.45).

| Key | Type | Default |
|---|---|---|
| `copy_restriction_mode` | bool | `false` |
| `image_constraint_token` | bool | `true` |
| `retention_mode` | bool | `false` |
| `retention_state` | u8 (3 bits) | `7` |
| `encryption_mode` | bool | `false` |
| `in_pmt` | bool | `true` |
| `in_eit` | bool | `false` |

### video_decode_control

`video_decode_control` on a stream: the video_decode_control_descriptor
(0xC8, B10 §6.2.30 Table 6-59) in the PMT ES loop.

| Key | Type | Default | Notes |
|---|---|---|---|
| `mode` | `"derived"` \| `"explicit"` \| `"off"` | `"derived"` | `derived`: from the `mpeg2-video` sequence header; any field given replaces the derived value; a `declared` stream gets nothing. `explicit`: every field from the configuration (needs `video_encode_format` and `transfer_characteristics`); emitted for `mpeg2-video` and `declared` streams. `off`: no descriptor (takes no fields; admits a raw 0xC8). |
| `still_picture` | bool | `false` | still_picture_flag. |
| `sequence_end_code` | bool | derived: whether the ES contains a sequence_end_code; explicit: `false` | sequence_end_code_flag. |
| `video_encode_format` | name | derived from vertical_size and progressive_sequence | Table 6-60: `"1080p"` ('0000'), `"1080i"` ('0001'), `"720p"` ('0010'), `"480p"` ('0011'), `"480i"` ('0100'), `"240p"` ('0101'), `"120p"` ('0110'), `"2160p60"` ('0111'), `"180p"` ('1000'), `"2160p120"` ('1001'), `"4320p60"` ('1010'), `"4320p120"` ('1011'). |
| `transfer_characteristics` | name | `"bt709"` when derived | Table 6-60-1: `"bt709"` ('00'), `"pq"` ('01'), `"hlg"` ('10'), `"undefined"` ('11'). The derived value is the H.262 default (the sequence_display_extension is not parsed). |

Live HD services send the byte 0x47 (`mode = "explicit"`,
`sequence_end_code = true`, `video_encode_format = "1080i"`,
`transfer_characteristics = "undefined"`); the derived value for a 1080i
stream without sequence_end_code is 0x04. The old boolean form
(`video_decode_control = true`) is rejected with a message giving the new
form.

### component_type

`component_type` on a stream: the component_type of the EIT
component_descriptor (video, 0x50, Table 6-5, stream_content 0x1) or
audio_component_descriptor (audio, 0xC4, Table 6-43).

- `"derived"` (default). Video: from the sequence header (scan format and
  aspect_ratio_information; "16:9 without pan vectors" for 16:9). Audio: the
  audio mode from the ADTS channel_configuration, no dialog control, "none"
  for persons with disabilities.
- Video (`mpeg2-video`): a Table 6-5 name. Each resolution `480i`, `480p`,
  `720p`, `1080i`, `1080p`, `2160p`, `240p`, `180p` has the four forms
  `"<res>-4:3"`, `"<res>-16:9-with-pan-vectors"`,
  `"<res>-16:9-without-pan-vectors"` and `"<res>-over-16:9"`, plus
  `"4320p-16:9"`:

  | Name | Code | Name | Code |
  |---|---|---|---|
  | `480i-4:3` … `480i-over-16:9` | 0x01–0x04 | `1080i-4:3` … `1080i-over-16:9` | 0xB1–0xB4 |
  | `4320p-16:9` | 0x83 | `720p-4:3` … `720p-over-16:9` | 0xC1–0xC4 |
  | `2160p-4:3` … `2160p-over-16:9` | 0x91–0x94 | `240p-4:3` … `240p-over-16:9` | 0xD1–0xD4 |
  | `480p-4:3` … `480p-over-16:9` | 0xA1–0xA4 | `1080p-4:3` … `1080p-over-16:9` | 0xE1–0xE4 |
  | | | `180p-4:3` … `180p-over-16:9` | 0xF1–0xF4 |

  Within each group the codes run 4:3, 16:9 with pan vectors, 16:9 without
  pan vectors, over 16:9 (for 0xD2–0xD4 the B10 text prints "4:3", read as a
  typo). Live HD services send `"1080i-16:9-without-pan-vectors"` (0xB3).
- Audio (`aac-adts`): a table `{ audio_mode, dialog_control, for_disabilities }`;
  missing fields are derived / default.

  | Key | Values |
  |---|---|
  | `audio_mode` (b4–b0) | `"1/0"` mono, `"1/0+1/0"` dual mono, `"2/0"` stereo, `"2/1"`, `"3/0"`, `"2/2"`, `"3/1"`, `"3/2"`, `"3/2+LFE"` (5.1), `"3/3.1"`, `"2/0/0-2/0/2-0.1"`, `"5/2.1"`, `"3/2/2.1"`, `"2/0/0-3/0/2-0.1"`, `"0/2/0-3/0/2-0.1"`, `"2/0/0-3/2/3-0.2"`, `"3/3/3-5/2/3-3/0/0.2"` (codes 0b00001–0b10001 in this order) |
  | `dialog_control` (b7) | bool, default `false` |
  | `for_disabilities` (b6–b5) | `"none"` ('00', default), `"visually-impaired-commentary"` ('01'), `"hearing-impaired"` ('10') |

A video name on an audio stream, or an audio table on a video stream, is an
error; `ts-program` streams always use the derived value.

### audio_quality_indicator

quality_indicator of the audio component descriptor (0xC4, B10 Table 6-44:
"For detail, refer to ARIB STD-B32 Part 2"; the modes are defined in B32
Part 2 Attachment Chapter 3 Table 1).

- `"derived"` (default): from the stream as sent. Sampling frequency below
  32 kHz → Mode 3; bit rate per stereo pair ≥ 256 kbit/s → Mode 1;
  ≥ 144 kbit/s → Mode 2; otherwise Mode 3. A 192 kbit/s stereo stream
  derives Mode 2, the value live broadcasters send at that rate.
- `"mode-1"` ('01'), `"mode-2"` ('10'), `"mode-3"` ('11'): fixed. The Table 1
  bit rates are "for reference purposes", so an explicit mode always wins.

`arib ts validate` reports a quality_indicator that contradicts the measured
rate (`aac.quality_indicator`).

### adts_compliance

What `arib mux` does with an `aac-adts` file whose ADTS headers violate the
header restrictions of ARIB STD-B32 Part 2 §5.2 (ID, layer,
protection_absent '0', profile LC, sampling_frequency_index 0x3–0x8 for
terrestrial TV, channel_configuration, adts_buffer_fullness ≠ 0x7FF, one
raw_data_block per frame):

| Value | Behaviour |
|---|---|
| `"conform"` (default) | Send a file whose headers satisfy every rule byte-identically; refuse anything else, listing each rule with its clause, the reason and what the input needs. Nothing is rewritten: crc_check cannot be added to protection_absent '1' input, and a protected header cannot be changed without recomputing crc_check, which needs a raw_data_block parser that this project does not include. For adts_buffer_fullness 0x7FF the refusal also prints the constant-rate fullness model. |
| `"warn"` | Send the file unchanged and report every violation as a warning. |
| `"pass-through"` | Send the file unchanged without checking. |

Use an encoder that writes the ADTS crc_check and a constant-rate
adts_buffer_fullness itself (MPEG-2 AAC LC, 48 kHz, ≤ 5.1, CBR) to pass
`conform`.

### remote_control_key_id

`[network] remote_control_key_id`, an integer 1..=12 (0xCD, B10 §6.2.42).
B10 leaves the 8-bit key number to the operator ("Specified and operated by
the company", Part 1 Table 7-1); arib-mux restricts it to the 12 one-touch
keys of terrestrial receivers. For an out-of-range experiment, patch the NIT
bytes (`[nit] byte_patches`). Background:
[arib-mux SPEC-NOTES](../crates/arib-mux/SPEC-NOTES.md#remote-control-key-0xcd-remote_control_key_id).

## Table options

Every generated table has a section of the same options, for receiver
experiments: `[pat]`, `[cat]`, `[nit]`, `[sdt]`, `[eit]` (present/following),
`[eit_schedule]`, `[tot]`, `[tdt]`, `[bit]` at the top level, and
`[service.pmt]` inside each `[[service]]`. All keys are optional.

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `true`; `false` for `[eit_schedule]` and `[tdt]` | Transmit this table. B10 Table 5-2 needs TDT *or* TOT, so TOT is on and TDT off by default. |
| `interval_ms` | u32 | B10 Table 5-2, see below | Maximum repetition interval; sections are scheduled at `interval_ms × [timing] interval_margin`. |
| `layer` | `"A"` \| `"B"` \| `"C"` | see below | Layer carrying the table. |
| `pid` | u16 | B10 Table 5-1, see below | PID override. |
| `version` | u8 | `0` | Initial version_number; it is incremented when the content changes (EIT p/f at an event boundary, for example). |
| `add_descriptors` | array of `{ loop = "…", tag = u8, hex = "…", first = bool }` | `[]` | Raw descriptors added to a generated loop (appended, or inserted at the front with `first = true`). Loop selectors below. |
| `header` | table | `{}` | Header-field overrides applied to every generated section (CRC_32 recomputed): `table_id`, `section_syntax_indicator`, `private_indicator` (the bit after section_syntax_indicator), `reserved1` (2 bits before section_length), `table_id_extension`, `reserved2` (2 bits before version_number), `version_number` (fixes the version: no automatic increments), `current_next_indicator`, `section_number`, `last_section_number`. Short-form tables (TDT, TOT) use only the first four. |
| `byte_patches` | array of `{ section = usize, offset = usize, hex = "…", fix_crc = bool }` | `[]` | Overwrite bytes of the encoded section at `offset` (from the table_id byte); `section` = index in the table's generated list (all sections if absent); `fix_crc` (default `true`) recomputes CRC_32. Not checked: this is how to send values the typed options refuse. |
| `replace_sections` | array of hex strings | `[]` | Send these complete sections, byte-identical, instead of the generated ones (e.g. a section transplanted from a capture with `arib ts extract`). |
| `extra_sections` | array of hex strings | `[]` | Send these complete sections after the generated ones. |

Defaults per table:

| Table | Default PID (B10 Table 5-1) | Default interval (B10 Table 5-2) | Default layer | Extra keys |
|---|---|---|---|---|
| `[pat]` | 0x0000 | 100 ms | control layer | — |
| `[cat]` | 0x0001 | 1 s | control layer | `descriptors = [ { tag, hex } ]`: CA_descriptors (0x09, H.222.0 §2.6.16); default none (free to air) |
| `[service.pmt]` | the service's `pmt_pid` | 100 ms | see below | — |
| `[nit]` | 0x0010 | 10 s | control layer | — |
| `[sdt]` | 0x0011 | 2 s | control layer | — |
| `[eit]` | 0x0012 | 2 s | control layer | `present_running_status` (u8, default 4 "running", B10 Part 2 Annex Table 1-1), `following_running_status` (u8, default 1 "not running"; Table 1-2 does not allow "running") |
| `[eit_schedule]` | 0x0012 | 10 s | control layer | `reference = "today"` (default; segments counted from today 00:00 JST, Annex 1.4.2 method 1) or `"month"` (from the first of the month, method 2); any other string is currently treated as `"today"`. Tables 0x50 + 4-day blocks, 32 segments of 3 h, 8 sections each. |
| `[tot]` | 0x0014 | 30 s | control layer | `local_time_offset = [ { country_code = "JPN", country_region_id = 0, polarity = false, offset = "HH:MM", time_of_change = "<JST time>", next_offset = "HH:MM" } ]`: local_time_offset_descriptor 0x58 (§6.2.25); default none |
| `[tdt]` | 0x0014 | 30 s | control layer | — |
| `[bit]` | 0x0024 | 20 s | control layer | `broadcast_view_propriety` (bool, default `true`); `first_descriptors = [ { tag, hex } ]` (raw first-loop descriptors, e.g. SI_parameter_descriptor 0xD7); `broadcasters = [ { broadcaster_id = u8, name = "…", terrestrial = { terrestrial_broadcaster_id = u16, affiliation_ids = [u8], broadcasters = [[onid, broadcaster_id]] }, descriptors = [ { tag, hex } ] } ]`: `name` → broadcaster_name_descriptor 0xD8, `terrestrial` → extended_broadcaster_descriptor 0xCE (broadcaster_type 0x1) |

The **control layer** is the partial-reception layer (A) when partial
reception is on, otherwise the most robust active layer (B31 Part 2 §3.2.1
Table 3-1; robustness rank of Table 3-2). The B31 Part 2 rule covers
PAT/NIT/CAT; putting SDT/EIT/TOT/TDT/BIT on the same layer is an operational
choice (choice 2 in the [arib-mux operational choices](../crates/arib-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers)).
The ARIB per-layer EIT PIDs (0x0012 H-EIT, 0x0026/0x0027 M-/L-EIT) are not
applied by default; set `[eit] pid` / `layer` to use them.

The **PMT** goes on the partial-reception layer for a service on layer A with
partial reception, otherwise on the most robust layer carrying the service's
elementary streams (B31 Part 2 Table 3-3).

Loop selectors for `add_descriptors`:

| Table | Selectors |
|---|---|
| `[cat]`, `[tot]` | `main` |
| `[service.pmt]` | `program`, `es:<pid>` |
| `[nit]` | `network`, `ts` |
| `[sdt]` | `service` (every service), `service:<service_id>` |
| `[eit]`, `[eit_schedule]` | `event` (every event), `event:<service_id>`, `event:<service_id>:<event_id>` |
| `[bit]` | `first`, `broadcaster` (every broadcaster), `broadcaster:<id>` |
| `[pat]`, `[tdt]` | none (no descriptor loop) |

```toml
# Fragment: experiment knobs on the PMT of a service and on the NIT.
  [service.pmt]
  interval_ms = 100
  add_descriptors = [ { loop = "es:0x0112", tag = 0x7C, hex = "00" } ]

[nit]
version = 3

[sdt]
header = { version_number = 2 }
```

## `[[raw_table]]`

An additional stream of sections the multiplexer does not generate (for
example a table this project does not type, transplanted from a capture with
`arib ts extract`), each on its own PID and layer.

| Key | Type | Default | Notes |
|---|---|---|---|
| `pid` | u16 | required | |
| `layer` | `"A"` \| `"B"` \| `"C"` | required | Must be active. |
| `interval_ms` | u32 | required | All sections are sent, in order, every interval. |
| `sections` | array of hex strings | required | Each must be one complete section. |

The sections are sent as given; `arib ts validate` will still report a
table_id on a PID that B10 Table 5-1 does not allow.

## Load-time checks

`Config::from_toml` and the multiplexer reject a configuration, with a
message naming the table, service and stream, when:

- a key is unknown, or a named value is given as a number or an unknown
  name (the message lists the valid names and codes);
- `remote_control_key_id` is outside 1..=12;
- a raw descriptor (`descriptors`, `program_descriptors`, `sdt_descriptors`,
  event `descriptors`, broadcaster `descriptors`,
  `add_descriptors`) has the tag of a typed descriptor the same loop already
  generates. The message says how to switch the typed one off (for example
  `set copy_control.in_pmt = false`, `set video_decode_control = { mode = "off" }`,
  `set eit_component = false on stream 0x0111`). Component (0x50) and audio
  component (0xC4) descriptors collide only on the same component_tag.
  Descriptors that are always generated (0x40 network name, 0x48 service,
  0x4D short event, 0xFA terrestrial delivery system, 0xCD TS information,
  0x41 service list) can only be changed with `byte_patches` /
  `replace_sections`;
- `copy_control.component_control` is combined with `in_pmt = true`;
  `aps_control_data` is given with a `copy_control_type` other than
  `"serial-output-encoded"`; `user_defined` is given together with
  `copy_control_type` / `aps_control_data`; `maximum_bitrate_mbps` is outside
  (0, 63.75];
- `recording_control = "source"` is used on a service without a
  `ts-program` stream;
- `video_decode_control` is `explicit` without `video_encode_format` and
  `transfer_characteristics`, `off` with field values, or explicit on an
  `aac-adts` stream; `audio_quality_indicator` / `adts_compliance` are set on
  a stream that is not `aac-adts`; a `component_type` does not match the
  stream kind;
- a `ts-program`-only key (`program`, `service_name`, `stream_types`,
  `component_tags`, `include_carousels`, `pid_map`, `pids`, `at_eof`,
  `eit_from_source`, `rate_check`, `max_early_ms`, `probe_seconds`) is set on
  another kind; a `ts-program` entry has no `source`, sets `pid`,
  `stream_type`, `component_tag`, `descriptors`, `hierarchical`,
  `video_decode_control` or `component_type`, gives both `program` and
  `service_name`, has an invalid `pid_map` / `pids` (PID outside
  0x0010–0x1FFE, a source PID listed twice, a PID given to two roles), or
  `probe_seconds` outside 1..600;
- a PID is used twice, or on two layers (that would need the B31 Part 2
  §3.2.2 duplicate-packet mechanism, which is not implemented); an
  elementary PID is outside 0x0010–0x1FFE; a service, stream or table is on
  an inactive layer; a `declared` stream has no `stream_type`; a media
  stream has no `source`;
- `pcr_per_frame` does not divide the layer's TSPs per frame or gives a PCR
  interval above 100 ms; a `ts-program` service's `pcr_pid` is one of its
  relayed streams;
- events overlap, or an event gives both `start` and `start_offset_s`;
- an `add_descriptors` loop selector is unknown or does not apply to its
  table; a `replace_sections` / `extra_sections` entry is not exactly one
  complete section; a hex string has an odd number of digits.

## Receiver-relevant settings

These are not spec requirements but matter on real receivers (details and
evidence: [transmitting.md](transmitting.md#receiver-compatibility-notes)
and the
[arib-mux SPEC-NOTES](../crates/arib-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits)):

- **`[network] channels` must equal the channel you transmit on.** After a
  scan, receivers tune by the NIT frequency; a mismatch registers the service
  on a channel with no signal. (`--relay` takes it from `--channel`.)
- **Keep `copy_control_type = "serial-output-encoded"` ('01'),** the
  default. A Sony BDZ-series recorder refuses to play a service whose 0xC1
  user_defined carries copy_control_type '00' ("Unsupported signal");
  `arib ts validate` warns (`si.copy_control`) on '00'.
- **Choose a `remote_control_key_id` that no local station uses.** A key
  that collides with a station already registered on the receiver can make
  its scan merge or ignore the service.

See also: [getting-started.md](getting-started.md),
[analysis.md](analysis.md), [glossary.md](glossary.md),
[SPECS.md](../SPECS.md), and the
[arib-mux README](../crates/arib-mux/README.md).
