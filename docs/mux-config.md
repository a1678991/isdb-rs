# Multiplexer configuration reference (`isdb mux` TOML)

`isdb mux -c FILE.toml`, `isdb tx --mux-config FILE.toml` and
`isdb loopback --mux-config FILE.toml` read a declarative description of one
ISDB-T transport stream: the network, the services and their elementary
streams, the events, and options for every generated PSI/SI table. This page
lists every key the multiplexer accepts, with its type, default and the
specification clause it implements. It is written from the configuration
structs in [`crates/isdbt-mux/src/config.rs`](../crates/isdbt-mux/src/config.rs)
and [`signalling.rs`](../crates/isdbt-mux/src/signalling.rs); when this page
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
- [`[service.hybridcast]`](#servicehybridcast)
- [Typed signalling options](#typed-signalling-options)
- [Table options](#table-options)
- [`[[raw_table]]`](#raw_table)
- [Load-time checks](#load-time-checks)
- [Receiver-relevant settings](#receiver-relevant-settings)

## General rules

- **Transmission parameters are not in the TOML.** Mode, guard interval,
  partial reception and the layer layouts come from the same
  `--mode / --gi / --partial / --layer-a / --layer-b / --layer-c` options as
  `isdb tx` (defaults: Mode 3, GI 1/8, partial reception, A = 1 segment QPSK
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
| `isdb mux --print-config-example` | the annotated example, identical to [`crates/isdbt-mux/examples/bench.toml`](../crates/isdbt-mux/examples/bench.toml): a full-segment HD service with MPEG-2 video and AAC from files, plus a declared one-seg service, with the values live terrestrial broadcasters send |
| [`examples/bench-minimal.toml`](../crates/isdbt-mux/examples/bench-minimal.toml) | the same with every receiver-facing value derived from the media and copy-free |
| [`examples/relay-ts.toml`](../crates/isdbt-mux/examples/relay-ts.toml) | rebroadcast one program of a recorded TS (`kind = "ts-program"`) |
| [`examples/bml-data.toml`](../crates/isdbt-mux/examples/bml-data.toml) | BML data broadcasting: the files of [`examples/bml/`](../crates/isdbt-mux/examples/bml/) as a data carousel (`kind = "bml-carousel"`): a menu page (`startup.bml`, picture in the upper-right quarter, menu column, text strip, up/down + Enter and the four colour keys) and an information page (`info.bml`, four tabs, Back / colour keys), written in UTF-8 and transcoded (`source_encoding = "utf-8"`); the pages use the idioms of live stations (a focused `div` with `onkeydown`, `document.currentEvent.keyCode` 1/2/18/19/21–24, `normalStyle`, `firstChild.data`, `browser.Ureg`, `browser.launchDocument(uri, "cut")`) |
| [`examples/hybridcast.toml`](../crates/isdbt-mux/examples/hybridcast.toml) | start a Hybridcast HTML5 application from a URL (`[service.hybridcast]`, AIT in the carousel with a generated launcher) |
| `isdb mux --relay FILE --print-config` | the relay configuration built for a given recording, with the chosen layout in a header comment |

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
network_id = 0x79EE
name = "テスト"
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
isdb mux -c minimal.toml --seconds 5 --out-ts test.ts --report
isdb ts validate test.ts
```

`isdb ts validate` reports one warning for this file (`pid.missing`: PID
0x0111 is listed in the PMT but carries no packets), which is what a declared
stream is. See [getting-started.md](getting-started.md) and
[analysis.md](analysis.md) for what to do with the output.

Types in the tables below are TOML integers of the given width; `PID` is a
13-bit packet identifier (H.222.0 Table 2-3), 0x0000–0x1FFF, and a value
outside that range is refused when the file is loaded.

## `[network]`

The NIT actual network (B10 Part 2 §5.2.4) and its terrestrial descriptors.
Required.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `network_id` | u16 | required | B10 §5.2.4; Annex N | Terrestrial TV range 0x7880–0x7FE8 (outside it the multiplexer warns); the allocation inside the range (ARIB TR-B14) belongs to real broadcasters, so choose an ID not used where your receiver is. The examples use the placeholder 0x79EE. Also the default `original_network_id`. |
| `name` | string | required | network_name_descriptor 0x40, §6.2.11 | ARIB 8-unit coded by `isdb-text`. |
| `area_code` | u16 (12 bits) | required | terrestrial_delivery_system_descriptor 0xFA, §6.2.31; Annex D Table D-2 | A code from Annex D Table D-2 (e.g. 0x5A5); set the area your receiver expects. |
| `remote_control_key_id` | integer 1..=12 | required | TS_information_descriptor 0xCD, §6.2.42 Table 6-82 | B10 gives no range; isdbt-mux accepts the 12 one-touch keys. Receivers number the services key·10 + index (key 9 → 091, 092 …). See [receiver-relevant settings](#receiver-relevant-settings). |
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
| `pmt_pid` | PID | required | H.222.0 §2.4.4.8 | One-seg services conventionally use 0x1FC8–0x1FCF. |
| `pcr_pid` | PID | `0x1FFF` (no PCR) | H.222.0 §2.4.4.9 | May equal an elementary PID (PCR in the video packets) except for a `ts-program` stream, whose packets keep their payload; a dedicated PID (e.g. 0x01FF) then carries adaptation-field-only PCR packets. |
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
| `hybridcast` | table | none | B24 Vol. 4; IPTVFJ STD-0010/0013 | `[service.hybridcast]`: the AIT naming the Hybridcast application to start. See [`[service.hybridcast]`](#servicehybridcast). |
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
| `"bml-carousel"` | BML data broadcasting: the files of a directory as a DSM-CC data carousel (ARIB STD-B24 Vol. 3 Ch. 6), signalled in the PMT (stream_type 0x0D, stream_identifier, data_component_descriptor with additional_arib_bxml_info, B24 Vol. 2 §9.3.2) and in the EIT (data_content_descriptor, §9.3.3). See [bml-carousel keys](#bml-carousel-keys). |

Media files are looped; the timestamps continue across the seam.

### Common keys

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `kind` | see above | required | | |
| `pid` | PID | required (not for `ts-program`) | H.222.0 Table 2-3 | 0x0010–0x1FFE. A PID used on two layers is rejected. |
| `stream_type` | u8 | 0x02 (`mpeg2-video`), 0x0F (`aac-adts`); required for `declared` | H.222.0 Table 2-34; B32 Part 3 §3.10 (2) | |
| `component_tag` | u8 | none (`bml-carousel`: 0x40) | stream_identifier_descriptor 0x52, §6.2.16 | Also the component_tag of the EIT component descriptors. A `bml-carousel` always gets one; 0x40 is the entry-component convention of terrestrial data broadcasting (TR-B14, from knowledge). |
| `source` | path | none | | Media file; required for `mpeg2-video`, `aac-adts` and `ts-program`. For `bml-carousel`: the directory of the BML files. |
| `layer` | `"A"` \| `"B"` \| `"C"` | the service's layer | | For hierarchical (high/low quality) services whose components sit on different layers. |
| `hierarchical` | `{ reference_pid = PID, high_quality = bool }` | none | hierarchical_transmission_descriptor 0xC0, §6.2.22 | `reference_pid`: the stream of the other quality level in the same service. `high_quality` (quality_level '1') defaults to the stream on the less robust layer (B31 Part 2 Table 3-2 ranking). |
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
| `adts_compliance` | `"conform"` \| `"warn"` \| `"pass-through"` | `"conform"` | B32 Part 2 §5.2 | See [adts_compliance](#adts_compliance). `isdb mux --adts-compliance` overrides it for every stream. |

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
data_component (0xFD); conditional_access (0x09) with `ca = "keep"`;
per-component digital_copy_control (0xC1) when the service keeps the
source's copy control (`recording_control = "source"`); hierarchy and
unknown descriptors are dropped. Program-loop descriptors: the CA
descriptors with `ca = "keep"`; copy control is regenerated from the
typed `copy_control` (see below); the rest is dropped. The start-up report
lists every drop, the relayed ECM / EMM PIDs and every copy-control
descriptor (0xC1, 0xDE) found in the source's PMT, SDT and EIT p/f, and
prints one line `source copy control: …; relayed as: …`. Background and rules:
[isdbt-mux README](../crates/isdbt-mux/README.md) and
[transmitting.md](transmitting.md#relaying-a-recording).

| Key | Type | Default | Notes |
|---|---|---|---|
| `source` | path | required | The recorded TS (188/192/204-byte packets). It must not be scrambled. |
| `program` (alias `service_id`) | u16 | the only program, or the only one whose streams carry packets | program_number in the source PAT. Otherwise the load fails with a list of the programs, their SDT names and streams. |
| `service_name` | string | none | Select by the source SDT service name: exact match first, then a unique substring; full-width ASCII and case folded (`TV12` finds `ＴＶ１２テスト`). Exclusive with `program`. |
| `stream_types` | array of u8 | `[0x01, 0x02, 0x1B, 0x24, 0x03, 0x04, 0x0F, 0x11, 0x06]` | Relay the elementary streams with these stream_types: video (MPEG-1/2, H.264, HEVC), audio (MPEG-1/2, AAC ADTS / LATM) and PES private data (captions, superimpose, data PES). |
| `component_tags` | array of u8 | all | Relay only streams whose stream_identifier_descriptor carries one of these tags. |
| `include_carousels` | bool | `false` | Also relay section streams (stream_type 0x05 private sections, 0x0B–0x0D DSM-CC data carousels), packet by packet. |
| `pid_map` | `"auto"` \| `"keep"` \| `[[source, output], …]` | `"auto"` | `auto`: output PID by stream role from `pids`; a relayed ECM / EMM PID keeps its source PID unless the plan or a relayed stream uses it (then the next free one). `keep`: the source PIDs. A list: explicit pairs; every source PID must be a relayed elementary stream or a relayed ECM / EMM PID of the program; output PIDs 0x0010–0x1FFE; unlisted relayed PIDs keep their source PID. CA_PIDs in the generated descriptors follow the mapping. |
| `pids` | `{ video, audio, captions, superimpose, data }`, each a PID or a list | `{ video = 0x0111, audio = 0x0112, captions = 0x0130, superimpose = 0x0138, data = 0x0140 }` | Only with `pid_map = "auto"`. Streams of a role, in component_tag order (then PMT order), take the role's PIDs in turn; extra streams take the next free PIDs after the last entry (a second audio stream gets 0x0113). The defaults are what terrestrial broadcasters use: an operational choice, not a spec value. Roles: main audio = lowest component_tag; captions and superimpose (both data_component_id 0x0008) are split by component_tag range, then by the data_component Timing field, then by PES PTS presence. |
| `at_eof` | `"loop"` \| `"end"` | `"loop"` | `loop`: restart at the first random access point with continuous timestamps. `end`: stop relaying; the multiplex then carries PSI/SI and nulls (`isdb tx` ends when every relay has ended). |
| `eit_from_source` | bool | `false` | Take the EIT present/following events (name, text, genre, duration) from the source's EIT p/f for the program; the configured `[[service.event]]` entries are used when this is off or the source has none. (`--relay` sets it to `true`.) |
| `rate_check` | `"error"` \| `"warn"` | `"error"` | When the rate the whole source file needs (every relayed packet by its PES's decoding time; the file is read once at start) plus PCR/PSI/SI exceeds the layer capacity: refuse, listing layer layouts that fit, or warn and relay anyway (the stream then falls behind its PCR schedule). `--auto-layout` on the command line picks a layout instead. |
| `ca` | `"keep"` \| `"drop"` | `"keep"` | The source's conditional-access signalling. `keep`: the source PMT's conditional_access_descriptors (program and ES loops) go into the generated PMT, the ECM PIDs they name are relayed packet by packet, and the source CAT's CA_descriptors go into the generated CAT, with the EMM PIDs present in the source relayed. The payload stays unscrambled (transport_scrambling_control '00'; scrambled input is refused), so receivers see CA signalling with a clear payload. `drop`: none of it. CLI: `--ca keep\|drop`. |
| `max_early_ms` | u32 | `0` | A packet may be sent up to this long before its PCR-derived time when the stream's T-STD model accepts it (smooths rate peaks into idle slots). 0 = strictly at the recording's pace. |
| `probe_seconds` | f64, 1..600 | `30` | Seconds of the source's PCR time measured at start for the stream parameters, program selection, the source EIT and the probe report's rates (the rate check reads the whole file). |

### `bml-carousel` keys

A `bml-carousel` stream sends the files of `source` as one DSM-CC data
carousel: a DownloadInfoIndication (DII, table_id 0x3B) listing the
modules and the DownloadDataBlocks (DDB, 0x3C) of each module, on the
stream's PID (B24 Vol. 3 Ch. 6, Table 6-24). By default every file goes
into module 0x0000 as a `multipart/mixed` entity (B24 Vol. 2 §9.1.2 (2):
each body-part with `Content-Type`, `Content-Location` and
`Content-Length`) whose first body-part is the
`application/X-arib-resourceList` (§9.1.2.3); `startup.bml`, the document
a receiver launches first (§9.2.2), must be among the files unless a
[Hybridcast launcher](#servicehybridcast) generates it. Resources are
addressed as `arib-dc://…/<component_tag>/<moduleId>/<file>`. Media
types come from the file extension (B24 Vol. 2 Table 9-6, written as
live carousels write them): `bml` → `text/X-arib-bml;charset="euc-jp"`,
`css`, `js`/`es`/`ecm` (`text/X-arib-ecmascript;charset="euc-jp"`),
`jpg`/`jpeg`, `png` (`image/X-arib-png`), `mng`, `aif`/`aiff`, `aac`,
`clt`/`clut`, `btable`/`btb`, `drcs`, `ait`; anything else needs
`bml.types`. BML documents are EUC-JP (ASCII is a subset); the files are
sent as they are unless `bml.source_encoding = "utf-8"`, which transcodes
the text files from UTF-8 (the shipped example is written that way). A terrestrial document starts with
`<?xml version="1.0" encoding="EUC-JP"?>`, `<!DOCTYPE bml PUBLIC
"+//ARIB STD-B24:1999//DTD BML Document//JA"
"http://www.arib.or.jp/B24/DTD/bml_1_1.dtd">` and
`<?bml bml-version="3.0"?>` (the forms a live broadcast sends; see
[`examples/bml/startup.bml`](../crates/isdbt-mux/examples/bml/startup.bml),
whose body also keeps the broadcast video and audio on screen with
`video/X-arib-mpeg2` / `audio/X-arib-mpeg2-aac` objects). The PID's rate is limited to `bml.bitrate_kbps`; the whole carousel
repeats within `bml.cycle_ms` when the rate allows, with a DII at least
every `bml.dii_interval_ms`. A module is limited to 256 DDBs (≈ 1 MB at
the default block size: the DDB section_number is 8 bits). The start-up
report lists the modules, their resources and the cycle time. While
`isdb tx` runs, editing a file in the directory updates the carousel on
air (`watch`): the changed module's version and the DII transaction
number advance, and a page that subscribes to `ModuleUpdated` (as the
examples do) reloads itself.

Receivers may refuse to start their BML browser for a network they do not
know (see [receiver-relevant settings](#receiver-relevant-settings)); the
following fragment is the form that opened on a Sony recorder: a local
station's recording re-emitted with its own identifiers on its own
channel, the carousel replacing the station's (`pid_map = "keep"` keeps
the other PIDs; the carousel takes the station's entry PID and tag):

```toml
[network]
network_id = 0x7FD3            # the station's, from its NIT
name = "近畿広域３"
area_code = 0x8D5
remote_control_key_id = 6
ts_name = "ＡＢＣテレビ"
channels = [15]                # the station's own channel; shielded bench only
transmission_types = [ { info = 0x0F, layer = "B" }, { info = 0xAF, layer = "A" } ]

[transport]
transport_stream_id = 0x7FD3

[[service]]
service_id = 0x0818
name = "ＡＢＣテレビ１"
layer = "B"
pmt_pid = 0x0818
pcr_pid = 0x0100
copy_control = { recording_control = "source" }

  [[service.stream]]
  kind = "ts-program"
  source = "recording.ts"
  program = 0x0818
  pid_map = "keep"
  eit_from_source = true

  [[service.stream]]
  kind = "bml-carousel"
  pid = 0x1100
  component_tag = 0x40
  source = "bml"

[bit]
broadcasters = [ { broadcaster_id = 0xFF, terrestrial = { terrestrial_broadcaster_id = 0x7FD3, affiliation_ids = [5] } } ]
```

Options go in `[service.stream.bml]` (or `bml = { … }`); all are optional.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `data_component_id` | u16 | `0x000C` | B10 Annex J Table J-1 | 0x000C terrestrial A profile, 0x000D C profile (one-seg), 0x0007 BS/CS BML. Used in the PMT 0xFD and EIT 0xC7. |
| `data_event_id` | u8 (4 bits) | `0` | B24 Vol. 3 Fig. 6-1 | Bits 31–28 of the downloadId. |
| `pmt_data_event_id` | u8 (4 bits) | `0xF` | Vol. 3 Annex C.1 | data_event_id of the PMT carousel info; all ones = the DIIs of any data_event_id are valid **[live]**. |
| `download_id` | u32 (28 bits) | `0x0FFFFFFF` | Vol. 3 §6.2.1 | Bits 27–0 of the downloadId **[live]**. |
| `transaction_number` | u32 (30 bits) | `0` | Vol. 3 Fig. 6-2 | DII version (transaction_id bits 29–0; bits 31–30 are '10'). The DII section's version_number is 0 (Table 6-24). |
| `module_version` | u8 | `0` | Vol. 3 Table 6-1 | moduleVersion of every module (DDB version_number = its low 5 bits). |
| `block_size` | u16, 1..=4066 | `4066` | Vol. 3 §6.2.1 | Bytes per DDB; 4066 fills the 4096-byte section (TR-B14 practice, from knowledge; the arithmetic is the spec's). |
| `entry_point` | bool | `true` | Vol. 2 Table 9-12 entry_point_flag | This component carries the first document (module 0x0000 / startup.bml). |
| `auto_start` | bool | `true` | auto_start_flag | Invoke it immediately. |
| `document_resolution` | name | `"960x540"` | Table 9-12 document_resolution | `"mixed"` ('0000'), `"1920x1080"`, `"1280x720"`, `"960x540"`, `"720x480-16:9"`, `"720x480-4:3"`, `"320x240"`, `"unspecified"` ('1111'). |
| `bml_version` | `"major.minor"` | `"3.0"` | Table 9-12/9-13 bml_major/minor_version | default_version_flag 0 with the numbers in the PMT and EIT info; `"3.0"` is what terrestrial broadcasts send **[live]** and what the `<?bml bml-version?>` of their documents says. `""` = default_version_flag 1. |
| `control_descriptor` | bool | `false` | Vol. 2 §9.3.5 Table 9-15 | Also put the version in a DII Control descriptor on the BML modules. Live carousels send none. |
| `independent` | bool | `true` | independent_flag | **[live]** |
| `style_for_tv` | bool | `true` | style_for_tv_flag | **[live]** |
| `event_section` | bool | `false` | event_section_flag | Event messages in this component (none are sent). |
| `ondemand_retrieval` | bool | `true` | ondemand_retrieval_flag | **[live]** |
| `reserved_future_use` | u8 (4 bits) | `0xF` | Table 9-12 | The reserved bits after style_for_tv_flag ('1111' per B24). TR-B14 is understood to carry Hybridcast's start_priority in them (position not in the local texts): an experiment knob. |
| `return_to_entry` | bool | the descriptor on an entry component | Vol. 2 §9.3.4 arib_bxml_privatedata_descriptor (0xF0) | The descriptor with return_to_entry_flag 1 in the DII private area (**[live]** on the entry carousel); `false` = none. |
| `dii_interval_ms` | u32 | `500` | — | DIIs are interleaved with the DDBs at this spacing at `bitrate_kbps`. Live carousels repeat the DII every 0.5–0.6 s. |
| `cycle_ms` | u32 | `500` | — | Maximum time between two carousel starts; the cycle restarts right away when the rate makes it longer. |
| `bitrate_kbps` | u32 | `1000` | B10 §5.1.4 (SI rule, applied by choice) | Packets of the PID per `[timing] si_window_ms` are capped at this rate. |
| `eit_data_content` | bool | `true` | B10 §6.2.28; Vol. 2 §9.3.3 | Generate the EIT data_content_descriptor (entry component, every carousel of the service with its component_size). Only with `entry_point`. |
| `types` | table `{ ext = "media type" }` | `{}` | Vol. 2 Table 9-6 | Media types by extension, added to the built-in list. Types outside Table 9-6 cannot appear in a resource list (`resource_list = false` on that module). |
| `watch` | bool | `true` | Vol. 3 §6.2.1 (transaction_id), §6.3.1 (moduleVersion) | Watch the source files while on air: a change (size or modification time) seen in two checks one second apart rebuilds the carousel; changed modules get moduleVersion + 1, the DII a new transaction number, and the next cycle carries them, so receivers raise `ModuleUpdated` (the example pages reload themselves on it). A file that fails to load (unknown type, bad UTF-8) is reported and the carousel on air is kept. |
| `source_encoding` | `"euc-jp"` \| `"utf-8"` | `"euc-jp"` | Vol. 2 §5.2.1 | `"utf-8"`: every `text/*` resource (BML, CSS, ECMAScript) is transcoded to EUC-JP on load and an `encoding="UTF-8"` XML declaration becomes `EUC-JP`; a character outside JIS X 0208 is an error naming it. `"euc-jp"`: sent as they are. |
| `modules` | array of tables | one module 0x0000 with every file | Vol. 2 §9.1.2 | Each: `id` (u16), `files` (paths relative to `source`), `multipart` (bool, default false: one file alone is sent as the module itself, "direct mapping", with Type and Name descriptors), `resource_list` (bool, default true), `name` (Name descriptor; default none: the module is addressed by its 4-digit hex id). |

## `[service.hybridcast]`

One AIT (ARIB STD-B24 Vol. 4 Ch. 5, application_type 0x0010) with one
HTML5 application: the receiver fetches `url` and starts it
(application_control_code AUTOSTART). The AIT holds a transport protocol
descriptor (protocol_id 0x0003 HTTP/HTTPS, URL_base), an application
descriptor (profile, version, service_bound, visibility, priority), a
simple application location descriptor (initial_path) and an application
boundary and permission descriptor (IPTVFJ STD-0013 §5.2, §8.2.4.2.17.1;
B24 Vol. 4 Annex A Fig. A-2 shows this set). Two carriages:

- `carriage = "carousel"` (default): the AIT section is a resource of a
  module of the service's `bml-carousel` (STD-0013 Phase 1: "the
  section-format AIT fetched from a data carousel", §8.2.3.10; B24 Vol. 4
  §7.2). The service needs a `bml-carousel` stream. Unless its files have
  a `startup.bml`, a launcher is generated that calls
  `browser.startAITControlledApp(organization_id, application_id,
  "arib-dc://-1.-1.-1/<component_tag>/<module_id>/<resource>")` on load
  (STD-0013 §6.1.5 / §8.1.3.3). The module and resource names follow the
  STD-0013 §6.1.3 example (`/40/9FFF/app1.ait`); the convention TR-B14
  fixes for the receiver's constantly monitored AIT is not in the local
  texts, so a BML of your own should name the AIT explicitly as the
  launcher does.
- `carriage = "es"`: the AIT on its own PID (stream_type 0x05) with a
  data_component_descriptor carrying ait_identifier_info (B24 Vol. 4 §7.3;
  STD-0010 §7.1.3 "Section transmission of AIT"). B24 v6.4 leaves the
  data_component_id "[T B D]", so it must be given.

What a receiver does with an AIT (which organization_id values it accepts,
TLS requirements of the application server, the start priority against
the BML content) is the IPTV Forum's and the maker's policy
(STD-0013 §8), not this file's.

| Key | Type | Default | Spec | Notes |
|---|---|---|---|---|
| `url` | string | required | B24 Vol. 4 §5.3.2 URL_base, §5.3.3 initial_path; STD-0010 §7.4.1 | `http://` or `https://`, ASCII. Split at the last `/`: URL_base up to and including it, initial_path the rest. |
| `organization_id` | u16, ≠ 0 | required | Vol. 4 Table 5-2; STD-0013 §5.2.1 | "assigned by the IPTV Forum"; 0 "cannot be entered in an AIT". |
| `application_id` | u32 | required | Table 5-2 | Unique within the organization. |
| `application_type` | u16 | `0x0010` | Vol. 4 §5.1 | HTML5. |
| `control_code` | `"autostart"` \| `"present"` \| `"kill"` | `"autostart"` | Vol. 4 §5.1; STD-0013 §5.2.3 | `"prefetch"` is refused (STD-0013: "shall not be used"). |
| `profile` | u16 | `0x0002` | STD-0013 §5.2.2 | application_profile. |
| `profile_version` | `"major.minor.micro"` | `"1.1.1"` | STD-0013 §5.2.2 | |
| `service_bound` | bool | `true` | Vol. 4 §5.3.1 | The application ends on a service change. |
| `visibility` | `"visible"` \| `"applications-only"` \| `"invisible"` | `"visible"` | Vol. 4 §5.3.1 | '11', '01', '00'. |
| `priority` | u8 | `1` | application_priority | |
| `permission_bitmap` | u16, upper 3 bits '000' | `0x1FFF` | Vol. 4 §5.3.4; STD-0013 §8.2.4.2.17.1 | 0x1FFF every permission, 0 none; '001' layouts (per-function bits) are accepted as given. |
| `boundaries` | array of strings | the origin of `url` | STD-0013 §6.1.3.1 | managed_URL entries: `scheme://domain[/path]`. |
| `autostart_priority` | u8 | none | Vol. 4 §5.3.5 | Adds the autostart_priority_descriptor. |
| `ait_version` | u8 (5 bits) | `0` | Vol. 4 §5.1 | version_number of the AIT (and of ait_identifier_info). |
| `carriage` | `"carousel"` \| `"es"` | `"carousel"` | see above | |
| `module_id` | u16 | `0x9FFF` | STD-0013 §6.1.3 example | carousel: the AIT module (multipart, no resource list). Must not be a `bml.modules` id. |
| `resource` | string | `"app1.ait"` | same | carousel: resource name of the AIT (media type `application/X-arib-ait`, a name the texts do not fix). |
| `launcher` | bool | `true` | STD-0013 §6.1.5 | carousel: generate `startup.bml` when the files have none. |
| `launcher_prologue` | string | the live terrestrial form | Vol. 2 §5.2, §10.2 | XML declaration + DOCTYPE + `<?bml bml-version?>` of the launcher. Default `+//ARIB STD-B24:1999//DTD BML Document//JA` / `bml_1_1.dtd`, bml-version 3.0 **[live]**. |
| `pid` | PID | none | | es: required. |
| `data_component_id` | u16 | none | Vol. 4 §7.3 | es: required (not in the local texts). |
| `interval_ms` | u32 | `1000` | — | es: AIT repetition. |
| `layer` | `"A"` \| `"B"` \| `"C"` | the service's | | es: layer of the AIT PID. |
| `application_priority` | u8 (2 bits) | `0` | Vol. 4 Table 7-1 | es: application_priority of ait_identifier_info. |

```toml
[[service]]
# …
  [service.hybridcast]
  url = "https://example.com/hybridcast/app1/index.html"
  organization_id = 1
  application_id = 1

  [[service.stream]]
  kind = "bml-carousel"
  pid = 0x0140
  component_tag = 0x40
  # source = "bml"   # your BML files; without them only the launcher and the AIT are sent
```

Service-level settings that matter for a relay: give the service a
dedicated `pcr_pid`, and use `copy_control = { recording_control = "source" }`
to keep the recording's own copy restriction (see
[copy_control](#copy_control)); an explicit value is used as given and the
source's value is still reported. Raw descriptors, `add_descriptors`,
`byte_patches`, `replace_sections` or `extra_sections` that can set the
copy control of a relayed service are allowed; the start-up report names
them with the copy control actually sent. `free_ca_mode` is not taken
from the source (the report notes a source SDT with free_CA_mode 1).

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
| `recording_control` (Table 6-39) | `"copy-free"` ('00'), `"provider-defined"` ('01'), `"copy-one-generation"` ('10'), `"copy-never"` ('11'), `"source"` (the relayed source program's value: its PMT program loop, else the most restrictive relayed ES loop, else its SDT, else its EIT p/f; copy-free only if the source signals none; the relayed streams keep their per-component ES-loop 0xC1) |
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

`isdb ts validate` reports a quality_indicator that contradicts the measured
rate (`aac.quality_indicator`).

### adts_compliance

What `isdb mux` does with an `aac-adts` file whose ADTS headers violate the
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
the company", Part 1 Table 7-1); isdbt-mux restricts it to the 12 one-touch
keys of terrestrial receivers. For an out-of-range experiment, patch the NIT
bytes (`[nit] byte_patches`). Background:
[isdbt-mux SPEC-NOTES](../crates/isdbt-mux/SPEC-NOTES.md#remote-control-key-0xcd-remote_control_key_id).

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
| `pid` | PID | B10 Table 5-1, see below | PID override. |
| `version` | u8 | `0` | Initial version_number; it is incremented when the content changes (EIT p/f at an event boundary, for example). |
| `add_descriptors` | array of `{ loop = "…", tag = u8, hex = "…", first = bool }` | `[]` | Raw descriptors added to a generated loop (appended, or inserted at the front with `first = true`). Loop selectors below. |
| `header` | table | `{}` | Header-field overrides applied to every generated section (CRC_32 recomputed): `table_id`, `section_syntax_indicator`, `private_indicator` (the bit after section_syntax_indicator), `reserved1` (2 bits before section_length), `table_id_extension`, `reserved2` (2 bits before version_number), `version_number` (fixes the version: no automatic increments), `current_next_indicator`, `section_number`, `last_section_number`. Short-form tables (TDT, TOT) use only the first four. |
| `byte_patches` | array of `{ section = usize, offset = usize, hex = "…", fix_crc = bool }` | `[]` | Overwrite bytes of the encoded section at `offset` (from the table_id byte); `section` = index in the table's generated list (all sections if absent); `fix_crc` (default `true`) recomputes CRC_32. Not checked: this is how to send values the typed options refuse. |
| `replace_sections` | array of hex strings | `[]` | Send these complete sections, byte-identical, instead of the generated ones (e.g. a section transplanted from a capture with `isdb ts extract`). |
| `extra_sections` | array of hex strings | `[]` | Send these complete sections after the generated ones. |

Defaults per table:

| Table | Default PID (B10 Table 5-1) | Default interval (B10 Table 5-2) | Default layer | Extra keys |
|---|---|---|---|---|
| `[pat]` | 0x0000 | 100 ms | control layer | — |
| `[cat]` | 0x0001 | 1 s | control layer | `descriptors = [ { tag, hex } ]`: CA_descriptors (0x09, H.222.0 §2.6.16); default none (free to air). A `ts-program` with `ca = "keep"` adds the source CAT's CA_descriptors. |
| `[service.pmt]` | the service's `pmt_pid` | 100 ms | see below | — |
| `[nit]` | 0x0010 | 10 s | control layer | — |
| `[sdt]` | 0x0011 | 2 s | control layer | — |
| `[eit]` | 0x0012 | 2 s | control layer | `present_running_status` (u8, default 4 "running", B10 Part 2 Annex Table 1-1), `following_running_status` (u8, default 1 "not running"; Table 1-2 does not allow "running") |
| `[eit_schedule]` | 0x0012 | 10 s | control layer | `reference = "today"` (default; segments counted from today 00:00 JST, Annex 1.4.2 method 1) or `"month"` (from the first of the month, method 2); any other value is a configuration error. Tables 0x50 + 4-day blocks, 32 segments of 3 h, 8 sections each. |
| `[tot]` | 0x0014 | 30 s | control layer | `local_time_offset = [ { country_code = "JPN", country_region_id = 0, polarity = false, offset = "HH:MM", time_of_change = "<JST time>", next_offset = "HH:MM" } ]`: local_time_offset_descriptor 0x58 (§6.2.25); default none |
| `[tdt]` | 0x0014 | 30 s | control layer | — |
| `[bit]` | 0x0024 | 20 s | control layer | `broadcast_view_propriety` (bool, default `true`); `first_descriptors = [ { tag, hex } ]` (raw first-loop descriptors, e.g. SI_parameter_descriptor 0xD7); `broadcasters = [ { broadcaster_id = u8, name = "…", terrestrial = { terrestrial_broadcaster_id = u16, affiliation_ids = [u8], broadcasters = [[onid, broadcaster_id]] }, descriptors = [ { tag, hex } ] } ]`: `name` → broadcaster_name_descriptor 0xD8, `terrestrial` → extended_broadcaster_descriptor 0xCE (broadcaster_type 0x1) |

The **control layer** is the partial-reception layer (A) when partial
reception is on, otherwise the most robust active layer (B31 Part 2 §3.2.1
Table 3-1; robustness rank of Table 3-2). The B31 Part 2 rule covers
PAT/NIT/CAT; putting SDT/EIT/TOT/TDT/BIT on the same layer is an operational
choice (choice 2 in the [isdbt-mux operational choices](../crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers)).
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
`isdb ts extract`), each on its own PID and layer.

| Key | Type | Default | Notes |
|---|---|---|---|
| `pid` | PID | required | |
| `layer` | `"A"` \| `"B"` \| `"C"` | required | Must be active. |
| `interval_ms` | u32 | required | All sections are sent, in order, every interval. |
| `sections` | array of hex strings | required | Each must be one complete section. |

The sections are sent as given; `isdb ts validate` will still report a
table_id on a PID that B10 Table 5-1 does not allow.

## Load-time checks

`Config::from_toml` and the multiplexer reject a configuration, with a
message that starts with the TOML key of the value (indices count the
entries of the file from 0: `config: service[0].stream[1].bml.block_size:
5000 outside 1..=4066 (…)`), when:

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
  0x41 service list; 0x52 and 0xFD of a `bml-carousel`) can only be changed
  with `byte_patches` / `replace_sections`;
- a `bml-carousel` has no `source` and no `modules` (and no Hybridcast
  launcher), `startup.bml` is missing from module 0x0000 with
  `entry_point = true`, a file has no media type (the message lists the
  extensions), a resource name is not a B24 Vol. 2 §9.2.1.2 name, a module
  id is listed twice, a module needs more than 256 DDBs, `block_size` is
  outside 1..=4066, `data_event_id` / `download_id` / `transaction_number`
  / `reserved_future_use` exceed their widths, `document_resolution` is
  reserved, `stream_type` is not 0x0B / 0x0D, `component_type` is given,
  or `bml` options are set on another kind;
- `[service.hybridcast]`: `organization_id` is 0, `url` is not an ASCII
  `http://` / `https://` URL, `control_code` is `"prefetch"` or a number,
  `permission_bitmap` has upper bits set, `boundaries` entries lack a
  scheme, `carriage = "carousel"` without a `bml-carousel` stream (or with
  `pid` / `data_component_id`), `module_id` equal to a bml module,
  `carriage = "es"` without `pid` or `data_component_id` or with a PID of a
  stream;
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
[isdbt-mux SPEC-NOTES](../crates/isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits)):

- **`[network] channels` must equal the channel you transmit on.** After a
  scan, receivers tune by the NIT frequency; a mismatch registers the service
  on a channel with no signal. (`--relay` takes it from `--channel`.)
- **Keep `copy_control_type = "serial-output-encoded"` ('01'),** the
  default. A Sony BDZ-series recorder refuses to play a service whose 0xC1
  user_defined carries copy_control_type '00' ("Unsupported signal");
  `isdb ts validate` warns (`si.copy_control`) on '00'.
- **Choose a `remote_control_key_id` that no local station uses.** A key
  that collides with a station already registered on the receiver can make
  its scan merge or ignore the service.
- **Data broadcasting only under a registered broadcaster's identity.** A
  Sony BDZ-ZW1500 shows the "d" mark for any service with a
  `bml-carousel` but starts its BML browser only when the network_id, TS
  and service IDs and the frequency match a station in its channel
  memory. The shipped examples (placeholder network 0x79EE) therefore
  show the mark and nothing more on it. To see the page, re-emit a
  recording of a local station with that station's identifiers on its own
  channel and add the `bml-carousel` to that service (details in
  [transmitting.md](transmitting.md#receiver-compatibility-notes)).

See also: [getting-started.md](getting-started.md),
[analysis.md](analysis.md), [glossary.md](glossary.md),
[SPECS.md](../SPECS.md), and the
[isdbt-mux README](../crates/isdbt-mux/README.md).
