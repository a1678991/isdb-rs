# isdbt-mux: spec interpretations and operational choices

Ground truth: the local texts in `specs/` (H.222.0 05/2006, ARIB STD-B10
v5.13, B31 v2.2, B32 v3.11 Parts 1–3, B21 v5.12, B24). Not available
locally: ITU-T H.262 (MPEG-2 video), ISO/IEC 13818-7 (AAC/ADTS), ARIB
TR-B14 (terrestrial operational guidelines). Everything taken from the
unavailable documents is marked **[from knowledge]**; everything that is a
free choice is listed under **Operational choices** at the end so it can be
tested on real receivers. The receivers tested so far are a MyGica S270 USB
tuner and a Sony BDZ-series Blu-ray recorder ("the recorder" below); see
"Receiver compatibility notes".

"Bench" below means the conducted test set-up (HackRF → coax →
attenuators → receiver, `docs/transmitting.md` in the workspace) and the
media used on it; the "live capture" is a terrestrial broadcast capture
recorded during development. Neither is distributed; the tests that use
such data read it from the `ISDB_RS_*` environment variables
(`docs/test-data.md`).

Format per entry: spec + clause, quoted text, interpretation, confirmation.

---

## Timing model: packet arrival = multiplex-frame slot
- Spec: B31 §3.2.1–3.2.2 (model receiver, Fig. 3-4); H.222.0 §2.4.2.2 eq. 2-1…2-5.
- Text (B31 §3.2.1): "The arrangement of transmission TSPs within a multiplex
  frame is determined in advance to ensure that it is identical to that of the
  TSs that will be reproduced by the model receiver". H.222.0: "The value
  encoded in the PCR field indicates the time t(i), where i is the index of the
  byte containing the last bit of the program_clock_reference_base field."
- Interpretation: the TS whose T-STD timing we define is the one a 13-segment
  receiver reproduces: one TSP per 408 clocks of 512/63 MHz in slot order of
  `isdbt_fec::MultiplexPattern`. Slot *j* of frame *k* starts at clock
  `k·samples_per_frame + 408·j`; 27 MHz ticks = clocks × 1701/512 (27·63/512).
  The PCR byte (byte 10: 4 header + 1 AF length + 1 flags + 33-bit base ending
  in byte 10) is taken 10 bytes × 2 clocks later (204-byte TSP per 408 clocks).
  A constant offset (S4 delay, receiver latency) is irrelevant to PCR/PTS
  consistency and is not modelled.
- Confirmation: test `pcr_and_timestamps` checks every PCR difference against
  the slot distance × 408 clocks (±1 tick), independently of the generator's
  formula. Status: confirmed (internal); pending real-receiver check.

## PCR packets per frame at fixed positions
- Spec: B31 Part 2 §3.2.4, Table 3-5; H.222.0 §2.7.2.
- Text: "Mode 3 — For the duration of a single multiplex frame, four PCR
  packets must be multiplexed per service at the same intervals"; Mode 1: one
  "and the multiplexing position must remain constant for all multiplex
  frames"; Mode 2: two. H.222.0: "t(i) − t(i′) ≤ 0.1 s".
- Interpretation: every service with a PCR_PID gets `pcr_per_frame` (default
  1/2/4 = Mode 1/2/3) PCR packets at layer-local packet indices
  `offset + q·N/pcr_per_frame` (N = TSPs per frame of the layer), the same in
  every frame; applied to all layers, not only the partial-reception layer.
  Frame length/4 = 231.3/4 = 57.8 ms (Mode 3, GI 1/8) ≤ 100 ms. Configurations giving
  > 100 ms or a non-dividing count are rejected.
- Confirmation: test checks equal slot spacing n/4 in the 13-segment TS and
  N/4 in the layer-A-only sequence (one-seg receiver view). Status: confirmed
  for the bench pattern.
- PCR_PID can be a dedicated PID (adaptation-field-only packets, AFC '10',
  continuity_counter not incremented per §2.4.3.3) or an elementary PID (PCR
  in the adaptation field of the next PES packet, or an AF-only packet if no
  PES data is ready). On a PCR-carrying ES PID the random_access_indicator is
  only set in packets with a PCR (§2.4.3.5: "In the PCR_PID the
  random_access_indicator may only be set to '1' in Transport Stream packet
  containing the PCR fields").

## Layers of PSI/SI
- Spec: B31 Part 2 §3.2.1 Table 3-1, §3.2.3 Table 3-3, Table 3-2 (robustness).
- Text: "Broadcasting with partial reception: (1) Multiplexed into the layer
  for partial reception"; "*1: CAT is required for partial reception";
  PMT: "Partial-reception service — Transmitted with the hierarchical layer
  for partial reception"; with a hierarchical transmission descriptor "PMT
  should be transmitted at the robustest layer among those transmitting
  ESs"; otherwise "with one of the hierarchical layers transmitting ESs".
- Interpretation: PAT/CAT/NIT default to layer A with partial reception, else
  the robustest active layer (Table 3-2 rank). PMT: layer A for a layer-A
  service; otherwise the robustest layer carrying the service's streams.
  All overridable per table (`layer = ...`).
- A PID is never put on two layers: B31 Part 2 §3.2.2 would require
  duplicate packets ("duplicate packets must be used") so that one-seg
  receivers see a continuous continuity_counter; not implemented (gap), the
  configuration is rejected.

## Repetition intervals
- Spec: B10 §5.1.3 Table 5-2.
- Text: PAT/PMT "Once or more/100m sec.", CAT "/1 sec.", NIT actual "/10
  sec.", SDT actual "/2 sec.", EIT p/f actual "/2 sec.", EIT schedule (8 days)
  "/10 sec.", TDT/TOT "/30 sec.", BIT "/20 sec."; "The value specified as
  sending frequency in table 5-2 is only a criterion of operation and is not
  the standard value."
- Interpretation: the configured value is the maximum gap between the starts
  of two transmissions of a section; transmissions are scheduled at
  `interval × interval_margin` (0.9) after the previous start and pending
  section packets are served earliest-deadline-first, so the measured gap
  stays below the criterion. Test `tables_conform_and_repeat` measures it on
  40 s of output for every (PID, table_id, extension, section).

## "4KB in 32 ms" per PID
- Spec: B10 §5.1.4.
- Text: "TS packets of the same PID is transmitted within the range of
  4KB±100% in 32msec each. The rule of "4KB in 32msec" is a detailed
  specification of 1Mbit per 1 sec. for every PID."
- Interpretation: conservative reading — no section PID carries more than
  ⌊4096/188⌋ = 21 packets in any 32 ms window (configurable `si_max_bytes`,
  `si_window_ms`); applied to every section PID. The "multi-sectional
  availability … 4KB at maximum" sentence (continuous allocation of a
  sub_table) is not separately enforced. Test checks the sliding window.

## Section packing
- Spec: B10 §5.1.2; H.222.0 §2.4.4.
- Interpretation: every section starts in a new packet (pointer_field 0, rest
  stuffed with 0xFF) — allowed by "Sections may start at the beginning of the
  payload … but this is not a requirement". Operational choice.

## Version numbers
- Spec: B10 §5.1.1 d), §5.2.7 version_number.
- Text: "The version_number shall be incremented by 1 when a change in the
  information carried within the sub_table occurs."
- Interpretation: each table starts at the configured `version`; at every
  repetition the sections are regenerated and compared with the previous
  ones; any change increments the version (mod 32). TOT/TDT carry none.
  A header override `version_number` pins it.

## EIT present/following
- Spec: B10 §5.2.7; Appendix (Guideline for the operation method of SI) §1.4.1.
- Text: "two sections per service for an EIT Present/Following with the
  section_number 0x00 reserved for the description of the present event and
  section_number 0x01 for the following event"; "when there is no present
  event … an empty section 0"; "if no following event exists, an empty
  section 1 … shall be transmitted"; following "running — Not allowed";
  segment_last_section_number: "For sub_tables which are not segmented, this
  field shall set to the same value as the last_section_number field";
  last_table_id: "If only one table is used this is set to the table_id of
  this table".
- Interpretation: last_section_number = segment_last_section_number = 1,
  last_table_id = 0x4E; present/following computed from the stream clock at
  each repetition; defaults running_status 4 (present) / 1 (following).
  Test `eit_present_following` checks the switch at an event boundary.

## EIT schedule
- Spec: Appendix §1.4.2.1 a)–m).
- Interpretation: method 1 (t0 = last JST midnight) by default, method 2
  (`reference = "month"`) optional; table_id 0x50 + ⌊3-h segment/32⌋; one
  section per segment unless the events exceed ~4000 bytes (then up to 8);
  segment_last_section_number = s0 + n − 1; empty segments up to the last
  used one are sent as one empty section (rule g); last_section_number = last
  section of the sub_table; last_table_id = last used table; events already
  ended are dropped (rule m). EIT_schedule_flag in the SDT follows.

## NIT
- Spec: B10 §5.2.4, §6.2.11, §6.2.14, §6.2.21 (Tables 6-34/35/36), §6.2.31
  (Tables 6-62/6-63), §6.2.32, §6.2.42; Annex N.
- Derived values: guard_interval code from Params (1/32 '00', 1/16 '01', 1/8
  '10', 1/4 '11'); transmission_mode from Params (Mode 1 '00', 2 '01', 3
  '10'); frequency "unit shall be 1/7MHz": channel *ch* → (473 + 6(ch−13))·7 + 1
  **[the UHF channel plan (centre = 473 + 6(ch−13) + 1/7 MHz) is from the Radio
  Act ordinance, not in the local specs; same formula as `isdb-cli`
  `phyargs::channel_hz`]**; partial_reception_descriptor lists the services on
  layer A when partial reception is on; system_management_id = broadcasting_flag
  '00' (broadcasting) | broadcasting_identifier '000011' ("digital terrestrial
  television broadcasting") | additional_broadcasting_identification.
- Placement (not stated by B10 for terrestrial; operational): network loop =
  network_name, system_management; TS loop = terrestrial_delivery_system,
  TS_information, service_list, partial_reception.
- network_id outside 0x7880–0x7FE8 (Annex N, terrestrial TV) gives a warning.

## SDT, BIT, TOT, CAT
- SDT (§5.2.6): one section, services in service_id order, service
  descriptor mandatory (Table 6-1, checked by `isdb_si::conformance`).
- BIT (§5.2.13): broadcaster loop from config (broadcaster_name 0xD8,
  extended_broadcaster 0xCE type 0x1); first loop raw (SI_parameter_descriptor
  content is TR-B14 territory).
- TOT (§5.2.9): JST_time = clock start + elapsed stream time at the
  transmission start, truncated to the second; optional local time offset
  descriptor. TDT optional (Table 5-2 *2: "Transmitting either TDT or TOT is
  mandatory").
- CAT (H.222.0 §2.4.4.6): CA descriptors from config (empty by default; B31
  Part 2 Table 3-1 note "CAT is required for partial reception", so always on).

## PMT descriptors derived from the media
- video_decode_control_descriptor (B10 §6.2.30 Tables 6-59/6-60/6-60-1),
  typed option `video_decode_control = { mode = "derived" | "explicit" |
  "off", still_picture, sequence_end_code, video_encode_format,
  transfer_characteristics }`. `derived` (default) takes: still_picture_flag
  0 (coded moving video, not an "MPEG-I picture" still component);
  sequence_end_code_flag = the file contains sequence_end_code (0x000001B7);
  video_encode_format from vertical_size + progressive_sequence (1080/1088
  lines → 1080/P or 1080/I, 720p → 720/P, 480 → 480/P or 480/I, 240p/120p/
  180p); transfer_characteristics '00' "bt709" (VUI 1/11/14: the H.262 default
  without sequence_display_extension **[H.262 from knowledge]**). Fields given
  with `derived` override the derived ones; `explicit` needs
  video_encode_format and transfer_characteristics (flags default 0).
- Live HD services send 0x47 = still 0, sequence_end_code 1, 1080/I, '11'
  ("Undefined" in Table 6-60-1 — the bits that pre-v5 editions and Part 1
  Fig. 6.2-32 still call reserved_future_use, i.e. '11' by §3.1). Expressible
  as `{ mode = "explicit", still_picture = false, sequence_end_code = true,
  video_encode_format = "1080i", transfer_characteristics = "undefined" }`
  (bench.toml).
- stream_identifier_descriptor (§6.2.16) when component_tag is configured.
- hierarchical_transmission_descriptor (§6.2.22) when configured; quality_level
  derived: the stream on the less robust layer (B31 Part 2 Table 3-2) is
  "high quality" ('1').
- stream_type (H.222.0 Table 2-34): 0x02 "ITU-T Rec. H.262 | ISO/IEC 13818-2
  Video", 0x0F "ISO/IEC 13818-7 Audio with ADTS transport syntax". B32 Part 3
  §3.10 (2) PMT, note 6 "The stream type id shall be used to identify
  broadcast program element type and its assignment shall be shown below",
  "Table: Stream type identifier" (Table No.16, Notification; printed
  p. 28): "0x02 Video stream prescribed in ITU-T Rec. H.262 or constrained
  parameter video stream defined in ISO/IEC 11172-2", "0x0F Audio stream
  defined in ISO/IEC 13818-7"; H.264 one-seg video would be "0x1B AVC video
  stream prescribed in ITU-T Rec. H.264" (`declared` streams only).
  Agrees with the implementation.

## EIT component descriptors derived from the media
- component_descriptor (§6.2.3, Table 6-5): stream_content 0x1; typed option
  `component_type = "derived" | "<Table 6-5 name>"`, e.g.
  `"1080i-16:9-without-pan-vectors"` (0xB3). `derived`: row from vertical
  size/scan, column from aspect_ratio_information (2 → 4:3 ('x1'), 3 →
  16:9 coded "without pan vectors" ('x3'), 4 → ">16:9" ('x4')
  **[H.262 Table 6-3 from knowledge]**). The choice "without pan vectors" is
  ours (the ES's pan-scan signalling is not inspected).
- Table 6-5 rows 0x1/0xD2–0xD4 are printed "Video 240p, 4:3 aspect ratio,
  with pan vectors / without pan vector / > 16:9" (text and PDF p.142); every
  other row (and the H.264 rows 0x5/0xD2–0xD4) has 16:9 there, so the
  variants are named `240p-16:9-with-pan-vectors` etc.; the code is
  unaffected. Status: interpretation (typo).
- A 1440×1080i file that declares aspect_ratio_information 2 (4:3) gives
  0xB1 `1080i-4:3` under `derived`; live HD services send 0xB3.
- audio_component_descriptor (§6.2.26): stream_content 0x2 ("For an audio
  stream whose coding method isn't specified, it is set to "0x2""),
  component_type (typed: `"derived"` or `{ audio_mode = "2/0",
  dialog_control = false, for_disabilities = "none" }`, Table 6-43 b7 / b6–b5
  / b4–b0) = audio mode from channel_configuration (Table 6-43: 1 →
  1/0, 2 → 2/0, 3 → 3/0, 4 → 3/1, 5 → 3/2, 6 → 3/2+LFE), sampling_rate
  (Table 6-45), simulcast_group_tag 0xFF, main_component_flag 1,
  quality_indicator typed `audio_quality_indicator = "derived" | "mode-1"
  | "mode-2" | "mode-3"` (default `derived`, see below).
- Channel mapping: B32 Part 2 §5.2.3 (2) (printed p. 23), "Coding mode
  stipulated as default in the AAC Standard": channel_configuration "1 mono
  (1/0)", "2 stereo (2/0)", "3 3/0", "4 3/1", "5 3/2", "6 3/2+LFE (3/2.1)";
  2/1, 2/2 and dual mono (1/0+1/0) use channel_configuration 0 with a PCE.
  Agrees with `AdtsEs::audio_mode`; channel_configuration 0 gives no
  derived audio mode (no PCE parsing), so `derived` emits no audio component
  descriptor for it.
- quality_indicator: B32 Part 2 Attachment (Operational Guidelines)
  Chapter 3 "Audio Quality Indication", Table 1 (printed p. 74; table
  checked against the PDF page): "00 Reserve"; "01 Mode 1 — Audio quality
  equivalent to B mode available in BS analog television broadcasting",
  remarks "Reference bitrate by ALS lossless audio coding system and AAC
  audio coding system: 192 to 256 kbps/stereo or more (Note 2)"; "10 Mode
  2 — Audio quality (Note 3) other than mode 1 that is not classified in
  mode 3", Note 3 "Audio quality for broadcasting given in ITU-R —
  144 kbps/stereo or more"; "11 Mode 3 — Mode with limited audio quality
  compared to modes 1 and 2", remarks "broadcasters will choose whether to
  offer this mode at their own discretion based on agreed upon rules. (ex.
  Sampling frequency below 32 kHz)". Text (p. 73): "The quality indicator
  shall be transmitted in accordance with the quality of an AAC coding
  stream that meets the audio quality criteria"; the bitrates are "shown
  for reference purposes" and "advances in encoder technology … will
  ensure that specified audio quality criteria are met at lower bitrates
  than the reference values"; for Mode 3 "no quantitative guidelines are
  established". (B10 Table 6-44 still refers to "ARIB STD-B32 Part 2
  Appendix 2", the old location of this chapter.)
- Interpretation (`QualityMode::derive`, the `derived` default): rate per
  stereo pair = ADTS stream rate (whole frames, as sent) × 2 / coded
  channels (SCE 1, CPE 2, LFE 0); sampling frequency < 32 kHz → Mode 3;
  ≥ 256 kbit/s per pair → Mode 1; ≥ 144 kbit/s → Mode 2; otherwise Mode
  3. **Ambiguity:** "192 to 256 kbps/stereo or more" is a range, not a
  threshold (Note 2: 192 kbit/s is "barely distinguishable from the
  original"; 256 is 1.5 × the broadcast rate); the derivation takes the
  upper end so that `derived` never claims Mode 1 for a stream the table
  does not unambiguously place there, and it never claims a higher mode
  than an explicit setting would. The rate per stereo pair for mono and
  multichannel is our normalisation (the table only speaks of stereo).
  The analyzer (`isdb ts validate`, `aac.quality_indicator`) warns only on
  clear contradictions: Mode 1 below 192 kbit/s per pair, Mode 2 below 144,
  Mode 1/2 below 32 kHz. Implicit SBR (×2 output rate) is not visible in
  the ADTS header; at sampling_frequency_index 0x6–0x8, where §5.2.3
  (4)(a) permits it, a contradiction that disappears at 2 × fs is
  reported as information, not as a warning. Coded channels come from
  channel_configuration (6 → 5; 0 / 7 undetermined, counted as 2).
- Results: an AAC test file of 188.4 kbit/s stereo 48 kHz (sent with
  `warn`) → Mode 2 ('10'), the value the EIT of live full-seg services
  carries (SI only; the audio itself was not examined); the live one-seg
  EIT carries no audio component descriptor. Status: interpretation.

## MPEG-2 video access units and timestamps [H.262 from knowledge]
- Access unit per H.222.0 §2.1.1 (local): the AU starts at the first of
  sequence_header / group_start / picture_start preceding a picture;
  sequence_end_code belongs to the last AU. Field pictures are paired into one
  AU (B32 Part 1 Attachment 2 §5.1 "one frame or one field pair"; §5.1.1
  "Video PES shall consist of video data of a single frame").
- H.262 syntax used (§6.2.2.1 sequence_header, §6.2.2.3 sequence_extension,
  §6.2.2.6 GOP header, §6.2.3 picture_header, §6.2.3.1
  picture_coding_extension) and display duration (§6.3.10: frame picture 2
  fields + repeat_first_field; progressive_sequence: 1 + rff·(1 + tff) frames).
- PTS: display order inside each GOP from temporal_reference, cumulated
  durations, plus one frame period of reorder delay. DTS: B-picture DTS = PTS;
  I/P-picture DTS = PTS of the previous I/P picture in decode order (first: 0).
  This keeps DTS strictly increasing and DTS ≤ PTS (checked by tests). PTS_DTS
  flags per B32 Part 1 Attachment 2 §5.1: '11' when PTS ≠ DTS, '10' when equal.
- B32 Part 1 §5.1.1 "vbv_delay shall always be set to 0xFFFF": the picture
  headers' vbv_delay is rewritten to 0xFFFF (`force_vbv_delay_ffff`, default on).
- Open GOP at the file start/loop seam: broken_link is set in the first GOP
  header when closed_gop = 0 (`mark_broken_link`), so decoders drop the leading
  B-pictures that would reference the previous loop's last anchor.
  group_of_pictures_header is time_code (25 bits: drop_frame_flag, hours
  5, minutes 6, marker, seconds 6, pictures 6), then closed_gop,
  broken_link **[H.262 §6.2.2.6 from knowledge]**, i.e. byte 3 after the
  start code holds the last time_code bit (0x80), closed_gop (0x40) and
  broken_link (0x20) (`video::GOP_CLOSED` / `GOP_BROKEN_LINK`, test
  `gop_flags_follow_the_25_bit_time_code`). A receiver check of an
  open-GOP loop seam is pending.
- PES_packet_length 0 is used only for AUs over 65 535 bytes (H.222.0
  §2.4.3.7 allows it for video in TS; B32 Part 3 §3.1 note 3: "the value is
  ‘0’ when the payload of PES packet is video elementary stream, and the
  packet length is not specified"). data_alignment_indicator = 1.

## AAC ADTS: B32 Part 2 §5.2 restrictions, CRC and adts_buffer_fullness
Code: `src/adts/` (`mod.rs` header, CRC-16, `buffer_channels`; `b32.rs`
header rules / pass-or-refuse gate / fullness model / quality modes),
`src/audio.rs` (`AdtsEs::load`, `AdtsCompliance`, `SilentFrame`).
Header-level only — see "AAC ADTS: raw_data_block not parsed" below.

**What comes from ISO/IEC 13818-7 (not available locally) — [from
knowledge], kept minimal:** the adts_fixed_header / adts_variable_header
field order and widths (syncword 12, ID 1, layer 2, protection_absent 1,
profile 2, sampling_frequency_index 4, private_bit 1,
channel_configuration 3, original_copy 1, home 1;
copyright_identification_bit 1, copyright_identification_start 1,
aac_frame_length 13 incl. headers, adts_buffer_fullness 11,
number_of_raw_data_blocks_in_frame 2; crc_check 16 after the headers when
protection_absent = 0 and one raw_data_block); ID '1' = MPEG-2, layer
'00'; the sampling_frequency_index table; the 1024 samples per
raw_data_block; the bit-reservoir recurrence and the /(32·NCC) scaling of
adts_buffer_fullness; and, for `SilentFrame` only, the syntax of the few
elements it writes (an SCE / CPE whose individual_channel_streams have
max_sfb 0, FIL with EXT_FILL, END — fixed-width fields, no Huffman code).

### The restrictions (B32 Part 2, text and PDF pp. 21–24 checked)
| Clause | Quoted text | Rule (`b32::Rule`) | `conform` |
|---|---|---|---|
| §5.2.1 | "Bitstream format: AAC Audio Data Transport Stream (ADTS)"; "Profile: Low Complexity (LC) profile" | `profile` (header only; the LC tool set inside raw_data_block is not checked) | refuse |
| §5.2.1 | "Max. number of coded channels: 5.1 channels (Note) per ADTS" | `channel_configuration` (7 = 7.1) | refuse |
| §5.2.2 (1) | "protection_absent ‘0’ (CRC error check is always presented)" | `protection_absent` | refuse: crc_check cannot be inserted |
| §5.2.2 (1) | "profile 1 (LC profile)" | `profile` | refuse |
| §5.2.2 (1) | "sampling_frequency_index 0x0 (96kHz) (Note 1), 0x3 (48kHz), 0x4 (44.1kHz), 0x5 (32kHz), 0x6 (24kHz) (Note 2), 0x7 (22.05kHz) (Note 2), 0x8 (16kHz) (note 2)"; Note 1 "0x0 … only for V-Low multimedia broadcasting"; Note 2 "0x6 to 0x8 … are not used for BS/wide band CS" | `sampling_frequency_index`: terrestrial TV = 0x3–0x8 | refuse |
| §5.2.2 (2) | "adts_buffer_fullness: Use of 0x7FF (indicating variable rate) is not permitted." | `adts_buffer_fullness` | refuse (a rewrite would invalidate crc_check); the fullness model is reported |
| §5.2.2 (2) | "number_of_raw_data_blocks_in_frame: 0 (number of raw_data_blocks per frame = 1)" | `number_of_raw_data_blocks_in_frame` | refuse |
| §4.1 / 13818-7 | ADTS "consists of … audio coded information defined in ISO/IEC 13818-7" (ID '1', layer '00' from knowledge) | `id`, `layer` | refuse (an ID rewrite would invalidate crc_check) |
| §5.2.2 (2), Description 1 (3), Appendix 3 (1)(a) | constant average bit rate; 6 144 bits per channel | `constant_rate` (only when the fullness model is computed) | reported with the refusal |

Not checked (they need the raw_data_block contents): §5.2.2 (3) coupling
channel elements; §5.2.3 (2) element order per channel_configuration;
§5.2.3 (3) (d) PCE fields; §5.2.3 (4) (a) SBR fill payloads only at
0x6–0x8; the LC tool set (prediction, gain control, PNS …); the value of
crc_check (Appendix 1). Also not checked: §5.2.3 (3) (a)–(c) (PCE timing
when the audio mode switches, PCE every < 550 ms when downmix
coefficients are sent — stream evolution); §5.2.1 "Max. bitrate:
Compliant to ISO/IEC 13818-7" (13818-7 limit not available); §5.2.4
(downmix, receiver side).

Policy per `aac-adts` stream, `adts_compliance`: `"conform"` (default)
sends a file whose headers satisfy every rule **byte-identically** and
refuses anything else, listing every rule with clause and frame count, the
reason, the fullness model when adts_buffer_fullness is 0x7FF, and what
the input needs (MPEG-2 AAC LC, 48 kHz, ≤ 5.1, constant bit rate, encoded
by an encoder that writes the ADTS crc_check and a constant-rate
adts_buffer_fullness itself). It never rewrites anything:

| Input | `conform` |
|---|---|
| every header rule satisfied | pass, byte-identical |
| protection_absent '1' (any frame) | refuse: crc_check cannot be inserted (needs the element boundaries) |
| protected, adts_buffer_fullness 0x7FF or ID '0' | refuse: the header change would invalidate crc_check, which cannot be recomputed |
| profile / sampling_frequency_index / channel_configuration 7 / layer / several raw_data_blocks | refuse: needs re-encoding |

`"warn"` sends the frames unchanged and reports every violation (plus the
fullness model) as multiplexer warnings; `"pass-through"` sends them
unchanged without checks (reproducing old streams on receivers).
`isdb mux --adts-compliance` overrides it for every stream.

### CRC (B32 Part 2 Appendix 1)
- Text (printed p. 81, PDF page checked), paraphrased: adts_error_check()
  is the CRC of ISO/IEC 11172-3 §2.4.3.1 over, in order of appearance,
  (1) every header bit, (2) the first 192 bits of each SCE, CPE, CCE and
  LFE element, (3) the first 128 bits of the second
  individual_channel_stream of a CPE, (4) all of any PCE or data element.
  The appendix walks a CPE through it: the CPE from its start padded to
  192 bits, then its second ICS from its start padded to 128 bits.
  Verbatim only the padding rule: "For any element where the specified
  protection length of 128 or 192 bits exceeds its actual length, the
  element is zero padded".
- Consequence: crc_check covers every header bit, so rewriting any header
  field (adts_buffer_fullness, ID, aac_frame_length) of a protected frame
  invalidates it, and computing it for an arbitrary frame needs the
  element boundaries, i.e. a raw_data_block walk. This crate therefore
  computes crc_check only for frames whose layout it knows by
  construction (`SilentFrame`).
- CRC algorithm [ISO/IEC 11172-3 not local; from knowledge]: generator
  x¹⁶+x¹⁵+x²+1 (0x8005), register preset 0xFFFF, MSB first, no final
  inversion (unit test: check value 0xAEE7 over "123456789", the
  catalogued CRC-16/CMS parameter set).
- Interpretation: "headers" = the 56 bits of adts_fixed_header +
  adts_variable_header (crc_check excluded); an element's protected bits
  start **after its 3-bit id_syn_ele** (at element_instance_tag); fill
  elements are not protected. This reading was settled during
  development: of 16 combinations tried, only it reproduced the
  transmitted crc_check of every frame of the one-seg audio of a live
  capture (249 frames; one-seg is sent in the clear; omitting the zero
  padding failed exactly the frames with a short element), and libfdk-aac (which verifies ADTS CRCs; FFmpeg's
  native decoder does not) accepted frames written under it and rejected
  single-byte corruptions. That check needs a raw_data_block parser, which
  the workspace does not include (see "AAC ADTS: raw_data_block not
  parsed"), so no in-tree test re-checks the reading against third-party
  data.
- `SilentFrame` (protection_absent '0' by default): crc_check over the 56
  header bits, then the SCE (from element_instance_tag, 4 + 22 bits) zero
  padded to 192, or the CPE (4 + 1 + 2 × 22 bits) zero padded to 192 and
  its second ICS (22 bits) zero padded to 128; FIL / END not covered.
  Unit test `silent_frame_crc_matches_appendix_1` recomputes it with an
  independent CRC loop from the field widths written out in the test.

### adts_buffer_fullness (constant-rate model)
- Text: B32 Part 2 §5.2.2 (2) (above); Description 1 (3): "To maintain
  the average bitrate fixed, 0x7FF (indicating variable bitrate) is
  prohibited for use as adts_buffer_fullness value"; Appendix 3 (1)(a):
  ISO/IEC 13818-7:2003 "stipulates that adts_buffer_fullness is the
  'amount of remained equivalent buffer per channel (6,144 bits per
  channel)'"; (1)(b) "there is no LFE component in the decoder buffer".
- Model [recurrence and scaling from knowledge of 13818-7]: constant rate
  R bits/frame; reservoir after frame n: r[n] = min(r[n−1] + R − bits[n],
  6144·NCC − R), bits = the whole ADTS frame; r ≥ 0 (decoder buffer never
  underflows); adts_buffer_fullness = ⌊r[n] / (32·NCC)⌋; NCC = SCE + 2·CPE
  (LFE excluded).
- Confirmed on live data: the live one-seg encoder follows exactly this
  recurrence with R = 2048 bits (48 kbit/s at 24 kHz), prediction error
  within the truncation (−0.75…0 fullness units over 249 frames); its
  maximum 160 = (12288 − 2048)/64 is the model's cap. The header-only recurrence check runs in
  `capture_all_frames_conform` when `ISDB_RS_CAPTURE_TS` names a capture.
- Choice of R: the smallest integer R ≥ the mean frame length for which r
  never goes negative on the periodic trajectory (start full, repeat the
  pass until it repeats, so the values continue across the media loop).
  A constant-rate stream (the live encoder) gets R = its rate and nothing
  clamped. An ABR test file (an FFmpeg encode) is **not** constant-rate at
  its mean: in the file examined, at its mean of 4034.1 bits/frame it needs a 13 588-bit
  reservoir, more than the 8 254 bits the buffer allows, so the model uses
  R = 4072 bits/frame (190.9 kbit/s, 0.9 % above the mean, computed on
  the frame lengths with crc_check); a constant-rate encoder at R would
  have spent 17 792 bits per pass (≈ 4.7 bytes/frame) on fill when its
  reservoir was full. The fullness values describe the reservoir of the
  R-rate encoder, which never underflows and never exceeds the buffer:
  values 1..=128, never 0x7FF. A stream with a single frame larger than
  6144·NCC bits has no model (`constant_rate`). Status: model confirmed
  on live data; the R > mean choice for VBR input is an interpretation.
- Use: the model is **informative only**, because the raw_data_block is
  not parsed (see the next section). `b32::stream_fullness` computes it
  from the headers (frame lengths as they would be with crc_check; NCC
  from channel_configuration via `adts::buffer_channels`: 1–5 → 1–5,
  6 (5.1) → 5 because the LFE has no buffer; 0 needs a PCE and 7 is
  beyond 5.1 → no model). `conform` reports it in its refusal and
  `"warn"` in its warnings when adts_buffer_fullness is 0x7FF; it is
  never written into the stream, because changing adts_buffer_fullness
  of a protected frame invalidates its crc_check.
  `SilentFrame::stereo_48k` writes the model's value for a stream of
  identical frames, ⌊(6144·NCC − 8·len) / (32·NCC)⌋.
- Buffer sizes for the T-STD are unchanged: H.222.0 §2.4.2.3 restated by
  B32 Part 2 Appendix 2 (printed p. 83): Rxn 2 000 000 bit/s / BSn 3 584
  bytes for 1–2 channels, 5 529 600 / 8 976 for 3–8.

### Encoder output and live reference
- FFmpeg's `aac` encoder output (ID 1, LC, 48 kHz, channel_configuration
  2) violates §5.2.2 (1) protection_absent and §5.2.2 (2)
  adts_buffer_fullness in every frame (the header rules see nothing
  else). `conform` refuses it; `warn` sends it unchanged (a recorder
  played it unchanged).
- Live capture: the one-seg audio (249 frames, 24 kHz, 48 kbit/s CBR,
  crc_check, fullness 85..=160) violates none of the header rules and
  passes `conform` byte-identically (test `capture_all_frames_conform`).
- Frames per PES configurable (default 1); PTS of the first frame; every
  audio PES gets random_access_indicator (an audio frame start is an
  elementary stream access point, H.222.0 §2.4.3.5).

## AAC ADTS: raw_data_block not parsed
- **Scope**: only the 56-bit ADTS header is parsed and checked. The
  raw_data_block (element list, individual_channel_stream, section / scale
  factor / spectral data with their Huffman codes) is not parsed, so the
  crate cannot verify crc_check values of input frames, cannot insert
  crc_check into protection_absent '1' input, cannot rewrite a protected
  header (adts_buffer_fullness, ID) without invalidating its crc_check,
  and does not check the B32 Part 2 rules that read the raw_data_block
  (coupling channel, element order, PCE fields, SBR fill, LC tool set).
- **Why**: parsing needs the ISO/IEC 13818-7 Annex A Huffman codebooks and
  scalefactor-band (swb_offset) tables. ARIB STD-B32 does not reproduce
  them and ISO/IEC 13818-7 is not available to the project, so there is
  no independent source for them. Copying them from another
  implementation (FFmpeg, libfdk-aac, faad2, …) is not acceptable: the
  specifications are the only ground truth, and those sources are under
  other licences.
- **Adding it** needs ISO/IEC 13818-7 (2006 or later; sold by ISO or a
  national body) obtained legally, and the Annex A tables and the
  raw_data_block syntax transcribed independently from that text.
- **Consequences**:
  - `adts_compliance = "conform"` passes header-conforming input
    byte-identically and refuses protection_absent '1' input and
    protected input that would need a header rewrite (adts_buffer_fullness
    0x7FF, ID '0'). The fullness model is computed and reported, never
    written. `"warn"` sends unchanged and reports; `"pass-through"` sends
    unchanged unchecked.
  - The B32 Part 2 checks are header-level only, in the multiplexer and
    in the analyzer (`isdb ts validate` `aac.*`; crc_check presence is
    counted, its value is not verified).
  - Coded channels (for the quality mode and the fullness model) come
    from channel_configuration; implicit SBR is not detected (the analyzer
    treats a quality_indicator contradiction that disappears at 2 × fs as
    information when sampling_frequency_index is 0x6–0x8).
  - `SilentFrame` computes its own crc_check from its known layout and
    defaults to protection_absent '0' with a constant-rate fullness, so
    synthetic streams pass `conform` without a rewrite.

## T-STD pacing
- Spec: H.222.0 §2.4.2.3–2.4.2.6.
- Model per ES: TBn 512 bytes leaking at Rxn (video 1.2 × Rmax; ADTS 2 Mbit/s
  for 1–2 channels, table in §2.4.2.3); video MBn (High/High-1440:
  BSmux + BSoh = 0.004·Rmax + Rmax/750; Main/Low: same, i.e. without the
  "VBVmax − vbv_buffer_size" term — conservative) drained at
  Rbx = min(1.05·Res, Rmax) (High levels) or Rmax into EBn = vbv_buffer_size;
  audio Bn = 3584 bytes (1–2 ch). AUs removed at DTS (video) / PTS (audio).
  PES header bytes are counted with their AU (conservative).
- Rmax per level **[H.262 Table 8-13 from knowledge]**: High 80, High-1440 60,
  Main 15, Low 4 Mbit/s.
- Scheduler: a PES packet is sent only if TBn and the next buffer can take it,
  MBn + EBn stays ≤ EBSn (so MBn always drains), MBn has been non-empty for
  less than 0.5 s ("MBn … shall empty at least once every second"), and the
  AU's decoding time is within `max_std_delay_ms − 50 ms` ("tdn(j) – t(i) ≤
  1 second"). Among eligible streams, earliest DTS first.
- Tests: no TB/MB/B overflow or underflow, TB/MB busy ≤ 1 s, every PES fully
  arrived before its DTS and its first byte ≤ 1 s before it.
- Not modelled: TBsys/Bsys for PSI (bursts are at most a few packets).
- Underflow tolerance: an access unit underflows when less than its bytes
  minus half a byte has reached EBn/Bn at its decoding time (buffer
  contents are whole bytes; the f64 running sums accumulate ≈ 0.01 byte of
  rounding after 4 GB, which a tighter tolerance reported as an
  "underflow" in a long relay).

## Rebroadcasting a recorded program (`kind = "ts-program"`, `src/relay.rs`)
- Purpose: a recorded program of another network (its own NIT/SDT,
  possibly CA descriptors and an ECM PID, copy control) cannot be
  registered by a terrestrial receiver when relayed as is (the recorder
  registers a TS from its terrestrial NIT). Only the program's elementary
  streams (and, with `ca = "keep"`, its ECM / EMM PIDs) are taken; every
  PSI/SI table is this multiplexer's (terrestrial NIT/SDT/EIT/TOT/BIT, PMT
  with typed copy control, CAT from the configuration plus the source's
  CA_descriptors). The input must be unscrambled.
- Selection (H.222.0 §2.4.4.8 PMT; Table 2-34 / B32 Part 3 §3.10 (2)
  stream types): default stream_types 0x01/0x02 (MPEG-1/2 video), 0x1B
  (H.264), 0x24 (HEVC), 0x03/0x04 (MPEG audio), 0x0F (AAC ADTS), 0x11 (AAC
  LATM), 0x06 (PES private data: ARIB captions / superimpose / data PES);
  section streams 0x05 / 0x0B–0x0D (data carousels) only with
  `include_carousels` (relayed packet by packet, payload unchanged).
  Scrambled packets of a relayed PID (transport_scrambling_control ≠
  '00') make the probe refuse the source.
- Conditional access (`ca`, owner decision; `relay::SourceCa`): with
  `"keep"` (default) the source PMT's conditional_access_descriptors
  (H.222.0 §2.6.16 Table 2-60; program loop and the relayed streams' ES
  loops) are carried as bytes into the generated PMT, the ECM PIDs they
  name (CA_PID ≠ 0x1FFF) are relayed packet by packet like a carousel,
  and the source CAT's CA_descriptors (§2.4.4.6) go into the generated CAT
  (after any configured `[cat] descriptors`), with each EMM PID relayed
  when it has packets in the probed part (otherwise the descriptor is
  carried as is and the report says so). CA_PIDs follow the PID mapping:
  `"keep"` / unlisted keep the source PID; with `"auto"` a CA PID that the
  plan or a relayed stream already uses moves to the next free PID; an
  explicit `pid_map` may list ECM / EMM PIDs. The CA_PID field is
  rewritten in place (3 reserved bits and private data kept). The payload
  stays unscrambled, so receivers see CA signalling with a clear payload;
  `free_CA_mode` is not derived (the report notes a source SDT
  free_CA_mode 1). `"drop"`: no CA descriptors, ECM or EMM (each listed
  as dropped with the reason). Status: confirmed on the synthetic source
  (`tests/relay.rs`); receiver behaviour pending real-world check.
- Program-loop descriptors: the CA descriptors as above; copy control 0xC1
  and content availability 0xDE are generated from the typed
  configuration; the others are dropped (listed in the report).
- Copy control (owner decision; `relay::SourceCopyControl`): the probe
  collects every digital_copy_control_descriptor 0xC1 (B10 §6.2.23) and
  content_availability_descriptor 0xDE (§6.2.45) of the source program in
  its PMT program loop, the relayed streams' ES loops, its SDT service
  loop (§5.2.6) and its EIT present/following events (§5.2.7), first
  version of each; all are listed in the report. With
  `copy_control.recording_control = "source"` (the default of
  `examples/relay-ts.toml` and of `isdb … --relay`) the generated 0xC1
  takes the source's digital_recording_control_data (Table 6-39) with the
  precedence PMT program loop ("digital copy control information is
  specified for the whole event") > relayed ES loops (most restrictive,
  largest code) > SDT > EIT present > EIT following, else '00' copy-free
  when the source signals none; the relayed streams keep their
  per-component ES-loop 0xC1 bytes. An explicit value (`--copy`, a Table
  6-39 name in the TOML) is used as given — relaxing included — and the
  ES-loop 0xC1 are then dropped. Either way one note prints the source
  value and what is sent, e.g. `source copy control: copy-one-generation
  (PMT); relayed as: copy-free (explicit)`. 0xDE is reported, not carried
  (configure `content_availability` to send one). Raw configuration that
  can set a relayed service's copy control (raw 0xC1/0xDE descriptors,
  `add_descriptors`, `byte_patches`, `replace_sections`, `extra_sections`
  of its PMT or of the SDT/EIT) is allowed; a note names it with the copy
  control decoded from the generated sections. user_defined still
  defaults to copy_control_type '01' (see "Digital copy control
  descriptor" and the receiver compatibility notes); maximum_bitrate is
  not carried over.
- ES descriptors are decoded with `isdb_si::Descriptor` and carried over
  by type when they describe the stream itself: stream_identifier 0x52
  (B10 §6.2.16; its component_tag becomes the stream's typed
  `component_tag`, also used by the EIT component descriptors),
  video_decode_control 0xC8 (§6.2.30; when absent for MPEG-2 video it is
  derived from the sequence header as for media files), data_component
  0xFD (§6.2.20; tells the receiver the caption / superimpose coding,
  e.g. data_component_id 0x0008). CA 0x09 and copy control 0xC1 as above.
  Dropped: hierarchical_transmission 0xC0 (refers to source PIDs and
  layers), access_control 0xF6, unknown tags. The probe report lists
  both.
- Re-timing (H.222.0 §2.4.2.2): the source has no slot clock, so every
  packet's arrival time is interpolated between the PCRs of the program's
  PCR_PID (eq. 2-4, `pcrclock::PcrClock`). A recording of a partial TS has
  lost the other services' packets, so the interpolation spreads the
  program's packets evenly over each PCR interval (tens of ms) — an
  approximation of their original positions, exact in the mean.
  Discontinuities (discontinuity_indicator, or a PCR step outside
  0 < Δ ≤ 0.7 s) continue the time line at the previous interval's rate;
  PCR/PTS wrap (2^33) is unwrapped. Mux time = source time + `base`
  (+ one loop period per pass); `base` is a multiple of 300 ticks, so the
  PTS/DTS shift is an exact 90 kHz constant: each PES keeps the source's
  arrival → decode distance, i.e. the T-STD behaviour the source
  multiplexer produced. The new PCR is the multiplex frame clock (PCR PID
  of the service, dedicated: relayed packets keep their payload, so a PCR
  cannot be added to them). A packet is eligible from its time on (TBn
  permitting) and leaves in the first free slot of its layer; with spare
  capacity the delay is a few slots. A due packet whose T-STD buffer would
  overflow waits up to 2 ms past its time (`relay::RETIME_HOLD`): the
  output slots are not the source's, so relayed packets arrive 0–1 ms
  later than in the source, unevenly, and consecutive packets can move up
  to a few slots closer. A source multiplexed with no MBn/EBn headroom —
  such as a TS made by `isdb mux`, whose scheduler admits a packet whenever
  it just fits — then overflowed MBn by one or two packets at hundreds of
  points (and kept MBn busy > 1 s) although the probe found the source
  clean: the probe and the output check run the same model, the
  difference was this jitter. The hold removes it (`tests/relay_tstd.rs`;
  a TS made from `examples/bench.toml` with ffmpeg test media: 2326 MBn
  overflows → 0, at most 2 ms behind the source pace).
- PES bytes intact: every source packet of a relayed PID becomes one
  output packet with the same payload bytes; only the 33 PTS/DTS bits of
  the PES header are rewritten (prefix and marker bits kept, Table 2-21);
  PID renumbered by `pid_map`; continuity_counter renumbered; adaptation
  field rebuilt as stuffing (+ random_access_indicator when the source set
  it and the field has room), so source PCR/OPCR/splice/private data are
  not carried. Source duplicates (§2.4.3.3: same CC and bytes) are dropped;
  a CC break, transport_error_indicator or scrambled packet drops the PES
  under assembly. A PES is sent only once complete; the source is read and
  timed 1.2 s ahead (§2.4.2.6 bounds a byte's T-STD delay to 1 s, so a
  conformant PES spans less).
- Cut-in and loop (operational): a pass starts at the first video PES whose
  ES begins with a sequence_header (an access point); if its GOP is open,
  the following B-picture PES (which reference the unsent previous GOP) are
  dropped whole until the first I/P. Other PIDs start at their first PES
  start after that point. Audio PES need not start on an ADTS frame (e.g.
  fixed 720-byte PES, data_alignment_indicator '0', frames straddling PES), so
  the first relayed audio PES may begin mid-frame — one decoder resync at
  the cut-in (ffmpeg: "Number of bands … exceeds limit" once), not
  trimmed. A pass ends with each PID's last complete PES. `at_eof =
  "loop"`: the next pass is the same file shifted by one period P ≥
  arrival span (+1 ms), ≥ every PID's PTS span + its PTS step, and ≥ the
  pass's last decoding time (so the decoder buffers are empty when the
  next pass's head, multiplexed at the file start for the source's buffer
  state, arrives) — the last term capped so no PID's PTS gap exceeds
  0.5 s (H.222.0 §2.7.4: PTS at most 0.7 s apart). Within the first 2 s of
  a later pass a due packet whose T-STD buffer would overflow is held
  until it fits or until its PES must leave to complete by its decoding
  time (100 ms + twice its remaining bytes at the layer rate). Elsewhere
  the source timing is kept, apart from the 2 ms re-timing hold above.
- T-STD of the source: the probe runs the same model on the source's own
  timing and reports it. A recording whose audio arrives early enough to
  hold more than H.222.0 §2.4.2.3's BSn = 3584 bytes for ≤ 2 channels
  ("BSn = BSmux + BSdec + BSoh = 3584 bytes") overflows the model itself;
  the relayed stream then reports overflows too (the 2 ms hold does not
  re-time it to hide them). The model is approximate for such audio: each
  PES is one access unit removed at its PTS.
- Rate check: need = the rate the relayed packets need over the *whole*
  file (`relay_rate`) + the layer's PCR packets + its PSI/SI (each section
  stream's packets per repetition interval); above the layer's Table 3-3
  capacity → configuration error listing `capacity::suggest` layouts
  (`rate_check = "warn"` relays anyway, falling behind). The whole-file
  need is the smallest constant rate at which the relayed packets, served
  first-in first-out in source order (as the relay sends them), each
  arrive by the decoding time (DTS, else PTS) of their PES + 2 ms (the
  relay's slot jitter); a packet with no decoding time (ECM / EMM,
  carousels, PES without PTS) by 1 s after its source time. It is computed
  on 1 ms bins of source PCR time, by bisection to 1 kbit/s. The probe's
  mean is not used: a recording that starts with a quieter part (the tail
  of the previous programme) measured 16.94 Mbit/s over its first 30 s and
  18.8–18.9 Mbit/s for minutes after; sized from the probe, the relay fell
  up to 1.7 s behind on B 12 seg 64QAM 5/6 (18.72 Mbit/s) and its audio,
  whose arrival→decode lead (0.11–0.19 s) is the shortest, left after its
  PTS, which a receiver drops; its whole-file need (18.86 + 0.04 Mbit/s)
  picks 7/8 = 19.66. The scan reads the file once per process (memoised by
  path, size and modification time). At run time, a relay whose PES leave
  after their decoding time raises an alert
  (`Multiplexer::take_alerts`, at most one per relay per minute of stream
  time).
- EIT (`eit_from_source`): the source EIT p/f actual (table 0x4E,
  service_id = program) present and following events: event_id, duration,
  short_event name/text (decoded by `isdb-text`, re-encoded by the SI
  builder; characters it cannot encode are dropped) and content genres. The
  present event is placed so that the relay start is as far into it as the
  source's first TOT/TDT (corrected to the cut-in by PCR time) was; the
  following at the source's distance. Copied once at start; component
  descriptors are derived from the relayed streams as for media files.

### Program selection, stream roles and the PID plan (`relay::classify`, `pid_map = "auto"`)
- Program (`relay::select_source_program`, also used by `probe`): the
  probe reads the PAT, every PMT it lists, the SDT actual (0x42,
  service_descriptor names) and EIT p/f of all services. `program` (alias
  `service_id`: B10 §5.2.6 "The service_id is the same as the
  program_number in the corresponding program_map_section") must be in
  the PAT; `service_name` matches the SDT name exactly, else as a unique
  substring, after folding full-width ASCII (U+FF01–U+FF5E) and case and
  dropping white space (so `TV12` finds `ＴＶ１２テスト`; half-width
  katakana is not folded). Neither given: the PAT's only program, or the
  only one whose elementary streams carry packets in the probed part (a
  recording of a partial TS keeps a PAT entry per service but packets of
  one). Otherwise — and when a selection matches nothing or several — the
  error lists every program: number, SDT name and service_type, PMT PID,
  ES summary by role, ES packet count. [Operational.]
- Roles (`relay::classify`, first matching rule):
  1. stream_type (H.222.0 Table 2-34; B32 Part 3 §3.10 (2) "Table: Stream
     type identifier"): 0x01/0x02/0x1B/0x24 video, 0x03/0x04/0x0F/0x11
     audio, 0x05/0x0B–0x0D section streams (carousels); other types than
     0x06 → "other" (relayed only when listed in `stream_types`).
  2. stream_type 0x06 with a data_component_descriptor (B10 §6.2.20
     Table 6-32) whose data_component_id is 0x0008: ARIB caption /
     superimpose coding — B24 Vol. 1 Part 3 §9.6 footnote 1 "The
     data_component_id of caption and superimpose coding scheme specified
     by ARIB shall be 0x0008"; B10 Annex J Table J-1 (informative) lists
     0x0008 as "ARIB-Subtitle & teletext coding". Any other
     data_component_id, or none: data PES (relayed by default, as before).
  3. Caption versus superimpose. B24 Vol. 1 Part 3 Ch. 4: "Among service
     to display characters overlapping on video of television
     broadcasting, service related to contents of video is called caption
     and all others is called superimpose. When transmitting and coding,
     these are not classified, and both of them are called caption
     generally." The coding therefore does not say which is which; the
     rules, in order:
     (a) component_tag 0x30–0x37 caption, 0x38–0x3F superimpose — the
         component_tag assignment of the terrestrial / BS operational
         guidelines (ARIB TR-B14 / TR-B15). **Not available locally: from
         knowledge**, consistent with a recording used during development
         (0x30 on the synchronous-PES stream, 0x38 on the asynchronous one).
     (b) the Timing field (2 LSBs) of additional_arib_caption_info in the
         data_component_descriptor (B24 Part 3 §9.6.1 Table 9-15, values
         Table 9-16: '00' asynchronous, '01' program synchronous, '10'
         time synchronous): '01' → caption; '00' and '10' → superimpose.
         Interpretation: caption is "related to contents of video", i.e.
         presented in step with the programme; superimpose (e.g. "news
         flash, … time signal", Part 1 §3.1 "Superimpose") is not.
     (c) the first PES of the PID: with a PTS ("Synchronous PES: PES with
         PTS", B24 Part 3 §3.1) caption, without ("Asynchronous PES: PES
         without PTS") superimpose. (The stream_id values 0xBD / 0xBF of
         the two independent-PES forms are B24 Vol. 3, not local; only
         PTS presence is used.)
     (d) otherwise caption.
     In a recording used during development the caption stream (tag 0x30,
     info 0x3D = DMF '0011' Timing '01', stream_id 0xBD with PTS) and the
     superimpose stream (tag 0x38, info 0x3C = Timing '00', stream_id 0xBF
     without PTS) agree on all three signals; rule (a) decides.
  4. Rank within a role: component_tag ascending, untagged streams after
     the tagged ones, then PMT order. Audio rank 0 is the "main" audio
     (the operational main audio component_tag is 0x10, the lowest audio
     tag — from knowledge; with no tags, the first audio in the PMT).
- PID plan (`pids`, default `config::PidPlan::default`): video 0x0111,
  audio 0x0112 (then 0x0113, …), captions 0x0130, superimpose 0x0138, data
  (data PES, carousels, other) 0x0140. **Operational choice**: the PIDs
  terrestrial broadcasters use for these components (a live terrestrial
  capture uses
  0x0111/0x0112 for its full-seg video/audio; caption/superimpose/data
  values from knowledge of TR-B14 practice); no local spec assigns
  elementary PIDs (B10 Table 5-1 only fixes SI PIDs, "indirect
  designation by PAT/PMT" for the rest). The k-th stream of a role takes
  the role's k-th PID; beyond the list, the next PIDs after its last entry
  that are neither taken nor in the plan. `pid_map = "keep"` keeps the
  source PIDs; an explicit `pid_map` list renumbers the listed source PIDs
  (each must be a relayed ES of the program's PMT: otherwise the error
  names the PMT's streams, or why the stream is not relayed) and keeps
  the others.
- Report: every relayed source PID → role (with the deciding rule) →
  output PID, rate, carried / dropped descriptors; every source PID seen in
  the probe and not relayed with the reason (B10 Table 5-1 SI PIDs, PMTs,
  the program's PCR_PID, ECM = CA_PID of a CA descriptor, ES of other
  programs, unreferenced).
- No-TOML relay (`relay_template::RelayOptions`, `isdb … --relay`): the
  values of `examples/relay-ts.toml` with channel / remote key / area code
  / copy control / at_eof / eit_from_source / clock from options; the
  network, TS, service, provider and broadcaster names default to the
  source SDT service name of the selected program, else "ベンチ".
  [Operational.] `--auto-layout` (isdb-cli) takes the first
  `capacity::suggest` layout that changes only the relay's layer
  (partial reception and the layer set kept; the configured GI first).

## Media looping
- One loop period per service = the longest of its streams' durations;
  every pass offsets all timestamps by the period (continuous timestamps,
  continuous continuity counters, no discontinuity_indicator). A/V start
  aligned (audio starts at the first displayed picture, i.e. after the video
  reorder delay); the shorter stream has a gap at the seam (bench: video
  10.010 s vs audio 10.027 s → 17 ms video gap).

## Digital copy control descriptor (0xC1) and its user_defined bits
- Spec: B10 §6.2.23 Tables 6-38/6-39; Annex F (informative) Tables F-1–F-4.
- Text: "user_defined: This is a 4-bit field, and broadcasting service
  provider can define it originally." Annex F ("Example of service provider
  define bit"): `copy_control_type 2` then `APS_control_data 2` if
  copy_control_type == 01 else `reserved_future_use 2`; Table F-2: 00
  "Undefined", 01 "Output by encoding to serial interface", 10 "Undefined",
  11 "Output by not encoding to serial interface"; Table F-4 (APS) 00 "Can be
  copied without control condition", 01 "With pseudo-sync pulse", 10/11
  pseudo-sync + 2-/4-line reversed division burst.
- Interpretation: the Annex F layout is used as the typed form
  (`copy_control_type`, `aps_control_data`); the ARIB operational guidelines
  (TR-B14 terrestrial / TR-B15 BS, **not available locally**) are understood
  to make it normative. Default copy_control_type '01'
  ("serial-output-encoded") + APS '00' ("copy-free") = user_defined 0b0100,
  what live broadcasters send. With another copy_control_type the low 2 bits
  are reserved_future_use, sent '11' (B10 §3.1). `user_defined = <0..15>` is
  kept as a raw experiment override and conflicts with the typed fields.
- recording_control names (Table 6-39): '00' "copy-free" ("Copy can be made
  without control condition"), '01' "provider-defined" ("Defined by service
  provider"), '10' "copy-one-generation", '11' "copy-never" ("Copy is
  forbidden").
- maximum_bitrate: "rolling up in each 1/4Mbps" → `maximum_bitrate_mbps`
  coded ⌈rate × 4⌉, 0 < rate ≤ 63.75. component_control (per-component loop)
  is rejected with `in_pmt` ("When this descriptor is transmitted by PMT,
  component control flag should always be '0'").
- Live capture: PMT program loop and every EIT event
  `c1 02 a4 42` (copy-one-generation, user_defined 0b0100, maximum_bitrate
  66 = 16.5 Mbit/s); SDT `c1 01 84` (same, no bitrate); one-seg PMT
  `c1 02 a8 02` (user_defined 0b1000 = copy_control_type '10', 0.5 Mbit/s).
- **Receiver evidence — bisection on a Sony BDZ-series recorder** (conducted
  link, remote key 9; each line one change against a playing control that
  matched the live capture in all three values: 0xC8 = 0x47, EIT component
  0xB3, 0xC1 copy-one-generation + user_defined 0b0100 in PMT + SDT):

  | Variant | Result |
  |---|---|
  | 0xC8 derived (0x04) instead of 0x47 | plays (irrelevant) |
  | EIT component_type derived instead of 0xB3 | plays (irrelevant) |
  | recording_control '00' copy-free, user_defined 0b0100 | plays |
  | recording_control '10' copy-one-generation, user_defined 0b0000 | **refused**: "Unsupported signal: This program is not available" |
  | user_defined 0b0100, placement PMT + EIT instead of PMT + SDT | plays |

  Conclusion: the sole cause is the user_defined bits — copy_control_type
  '01' plays, '00' is refused. `isdb ts validate` warns (`si.copy_control`)
  on copy_control_type '00'.

## Remote control key (0xCD remote_control_key_id)
- Spec: B10 §6.2.42 Table 6-82; Part 1 Table 7-1.
- Text: "This 8-bit field indicates the recommended remote control key
  number to which the applicable TS shall be assigned"; Table 7-1 "Specified
  and operated by the company"; B21 §4.4.16/§5.2.17 "No stipulations are made
  for the shape or keys of the remote controller".
- Interpretation: B10 gives no range. The typed `RemoteControlKeyId` accepts
  1..=12, the one-touch keys of terrestrial receivers, and receivers number
  the TS's services as 3-digit channels key·10 + service index **[TR-B14
  practice, not available locally; from knowledge]**. Out-of-range values only
  via `[nit] byte_patches`. The examples and `--relay` use 9; choose a key
  that no station receivable at the receiver's location uses (a collision
  was observed on the bench, see "Receiver compatibility notes").

## Raw descriptors versus typed ones
- A raw descriptor (`descriptors`, `program_descriptors`, `sdt_descriptors`,
  event `descriptors`, `add_descriptors`) with the tag of a typed descriptor
  the same loop generates is rejected at load (`Config::validate`), naming the
  switch that turns the typed one off; component (0x50) / audio component
  (0xC4) descriptors collide only on the same component_tag. Descriptors that
  are always generated (0x40, 0x48, 0x4D, 0xFA, 0xCD, 0x41) can only be
  changed with byte_patches / replace_sections. byte_patches are not checked.

---

## Receiver compatibility notes

Findings on consumer receivers fed `isdb tx` output over a conducted link
(HackRF → coax → attenuators → receiver; Mode 3, GI 1/8, partial
reception, A 1 seg QPSK 2/3 I=2, B 12 seg 64QAM 3/4 I=2). They are
requirements of those receivers, not of the specifications, and are
encoded as defaults or validator warnings:

- **The Sony BDZ-series recorder requires digital_copy_control
  user_defined copy_control_type '01'** (B10 Annex F layout; e.g. 0b0100).
  With '00' it refuses playback ("Unsupported signal"). The recording
  mode, the placement (PMT + SDT or PMT + EIT), the 0xC8 value and the EIT
  component_type did not matter (bisection in "Digital copy control
  descriptor (0xC1)"). Defaults: copy_control_type '01' + APS '00';
  `isdb ts validate` warns `si.copy_control`.
- The same recorder registers services from the terrestrial NIT of the TS
  it receives, so a relayed recording needs this multiplexer's own NIT
  (`ts-program`), and `[network] channels` must equal the transmit
  channel (choice 20).
- remote_control_key_id must not collide with a local station's key, or
  the recorder's scan merges/ignores the service (choice 19).
- A consumer USB tuner (MyGica S270) decoded the TX with PER 0 and its
  capture validates with 0 errors; its `dvbv5-scan` section filter fails
  for driver reasons unrelated to the TS (SPECS.md §4).

## Operational choices (not dictated by the local specs — test on receivers)

**[live]** marks a value that follows live broadcaster practice (the live
capture); **[recorder]** one the bisection on the Sony BDZ-series recorder
showed to matter.

1. PCR packet positions: layer-local offset 0, 1, … per service, at
   `N/pcr_per_frame` spacing; dedicated PCR PIDs (bench 0x01FF, 0x01FE).
2. SDT, EIT, TOT/TDT, BIT on the PAT layer (layer A with partial reception).
   TR-B14 PID-per-layer EIT practice (0x0012 H-EIT, 0x0026/0x0027 M/L-EIT,
   B10 Table 5-1 note *2) is not applied by default; set `[eit] pid/layer`.
3. Sections start in fresh packets (no packing).
4. Section repetition at 0.9 × the B10 Table 5-2 criterion; first
   transmissions staggered by two layer slots.
5. additional_broadcasting_identification = 0x01 (system management descriptor).
6. transmission_type_info values of the TS information descriptor (bench:
   0x0F for layer B, 0xAF for layer A **[live]**); none by default.
7. NIT descriptor order (network loop: 0x40, 0xFE; TS loop: 0xFA, 0xCD, 0x41, 0xFB).
8. SDT running_status 0, EIT_user_defined_flags '111', free_CA_mode 0.
9. EIT p/f running_status 4/1; ISO_639 "jpn" in short event / component
   descriptors; content descriptor user nibbles 0xF.
10. Digital copy control / content availability descriptors: only when
    configured; placement flags (PMT program loop, EIT events, SDT; default
    PMT + EIT). user_defined default copy_control_type '01' + APS '00'
    (0b0100) **[live] [recorder: '00' refused]**. bench.toml: copy-one-generation
    **[live]** in PMT + SDT (the live capture also carries it in the EIT, with
    maximum_bitrate 16.5 Mbit/s in PMT and EIT; PMT + EIT also plays on the
    recorder). bench-minimal.toml: copy-free in PMT + EIT.
11. One-seg service in the example: service_type 0xC0, PMT PID 0x1FC8,
    component tags 0x81/0x83, declared without media; full-seg component
    tags 0x00 (video) / 0x10 (audio).
12. av_delay 700 ms (first DTS after the first packet), max T-STD delay
    950 ms effective; stream_id 0xE0 (video) / 0xC0 (audio) (the numbers
    are a choice; the ranges are B32 Part 3 §3.1 "Table: Stream id":
    '110xxxxx' audio incl. ISO/IEC 13818-7, '1110xxxx' video incl. H.262);
    original_or_copy / copyright = 0.
13. One ADTS frame per PES (B32 Part 2 §5.2.2 fixes one raw_data_block per
    ADTS frame but says nothing about ADTS frames per PES).
    `adts_compliance = "conform"` by default: header-conforming input
    passes unchanged, anything else is refused (see "AAC ADTS").
14. RAI on video PES that start with a sequence header; on every audio PES.
15. Component descriptor "16:9 without pan vectors" when derived; bench.toml
    sets 0xB3 `1080i-16:9-without-pan-vectors` explicitly **[live]**
    (irrelevant to the recorder). Audio quality_indicator derived from B32
    Part 2 Attachment Ch. 3 Table 1 with the conservative 256 kbit/s Mode 1
    threshold (a ≈ 189 kbit/s stereo file → '10' Mode 2 **[live]**: live
    EITs carry '10').
16. The 1/7 MHz frequency code derived from the UHF channel plan formula.
17. Null packets: PID 0x1FFF, payload 0xFF, CC 0.
18. Video decode control: derived by default; bench.toml sends the live byte
    0x47 explicitly **[live]** (irrelevant to the recorder).
19. remote_control_key_id 1..=12 only (TR-B14 practice); the examples use 9
    (choose a key no station in your area uses).
20. `[network] channels` must equal the transmit channel (receivers retune to
    the NIT frequency); the example keeps 27.
21. `ts-program` selection defaults (video, audio, PES private data;
    carousels off), carried ES descriptors (0x52, 0xC8, 0xFD; 0x09 with
    `ca = "keep"`, 0xC1 with `recording_control = "source"`) and dropped
    ones (everything else; program descriptors other than CA).
22. `ts-program` timing: first relayed packet 100 ms after the first slot;
    `base` rounded down to 300 ticks; `max_early_ms = 0` by default (no
    packet before its PCR-derived time; > 0 allows early sending when the
    stream's T-STD model accepts it).
23. `ts-program` cut-in at a sequence_header PES, leading B-picture PES of
    an open GOP dropped; unaligned audio PES not trimmed.
24. `ts-program` loop period (arrival span, PTS spans, drained buffers
    capped at a 0.5 s PTS gap) and the T-STD hold in the first 2 s after a
    seam (released 100 ms + 2 × the PES's remaining send time before
    decoding).
25. `ts-program` rate check: the whole-file need (every packet by its
    PES's decoding time + 2 ms, untimed packets within 1 s, FIFO at a
    constant rate) + PCR/PSI/SI estimate → error; layouts suggested: same mode, the most robust code rate per
    layout, a single 13-segment layer, GI 1/16 and 1/32.
26. `examples/relay-ts.toml`: program and PIDs from the recording
    (`pid_map = "auto"`: 0x0111/0x0112/0x0130/0x0138 for video / main
    audio / captions / superimpose), copy control from the source
    (`recording_control = "source"`, copy_control_type '01'), CA
    signalling kept (`ca = "keep"`), area_code 0x5A5 and network_id
    0x79EE as placeholders,
    one service on layer B 12 seg 64QAM 7/8 with partial reception (layer
    A carries PSI/SI only).
27. `ts-program` program selection when none is given (the only program,
    else the only one with ES packets, else an error listing them) and the
    service-name folding (full-width ASCII, case, white space).
28. `ts-program` caption / superimpose split by component_tag range
    (TR-B14, from knowledge), then data_component Timing, then PES PTS
    presence; main audio = lowest component_tag.
29. `ts-program` default PID plan: video 0x0111, audio 0x0112+, captions
    0x0130, superimpose 0x0138, data 0x0140 (overridable per role,
    `pids = { … }`).
30. No-TOML relay names: network / TS / service / provider / broadcaster
    = the source SDT service name (else "ベンチ"); `--auto-layout` picks
    the most robust modulation / code rate of the relay's layer that
    carries the whole-file need + PCR/PSI/SI (configured GI first, then 1/16,
    1/32), never switching to a single 13-segment layer by itself.
31. `ts-program` conditional access: `ca = "keep"` by default (owner
    decision): CA descriptors, ECM and CAT/EMM relayed with CA_PIDs
    following the PID mapping (`"auto"`: the source PID unless taken,
    then the next free one); `"drop"` relays none.
32. `ts-program` copy control: the source's value with the precedence PMT
    program loop > ES loops > SDT > EIT present > EIT following; any
    explicit value accepted as given and reported next to the source's.
33. `ts-program` re-timing hold: a due packet waits at most 2 ms
    (`relay::RETIME_HOLD`) while its T-STD buffer would overflow — above
    the ≈ 0.7 ms slot jitter measured on a relay of this crate's own
    output, small against the 0.1 s release margin of the seam hold.
