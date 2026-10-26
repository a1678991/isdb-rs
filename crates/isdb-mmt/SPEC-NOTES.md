# isdb-mmt — spec interpretation notes

Specs: ARIB STD-B60 v1.14-E1 (MMT-SI §7, TLV-SI §5, MPU/MFU §6, video
and audio §8, captions §9, NTP §3). Not available: ISO/IEC 23008-1
(MMT), ARIB STD-B61 (scrambling), ARIB STD-B62 (operational, incl. text),
ITU-T H.265, ISO/IEC 14496-3 (SPECS.md §1.2). "Lab recordings": TLV
streams of BS7, BS11, BS17 decoded by `isdb rx --system isdbs3`, 0.55 s
offline (2026-10-25) and 40 s live through the FPGA front end
(2026-10-26).

## descriptor-length: width of `descriptor_length`
- Spec: §7.4.3: "The bit length of the descriptor length field is varied
  with the descriptor." Table 4-10 marks 0000h–3FFFh "8-bit length",
  4000h–6FFFh "16-bit", 7000h–7FFFh "32-bit".
- Interpretation: the width each descriptor's own structure gives (read
  from every structure in §7.4.3, §10.3.3–§10.3.5, §11.2.2): 16 bits for
  the dependency descriptor (0002h, although Table 4-10 puts it in the
  8-bit range) and for F000h–FFFFh (MH-linkage, MH-short and extended
  event, event message, stuffing, broadcast id, network identification);
  8 bits otherwise; Table 4-10's widths for the reserved ranges.
- Confirmation: every descriptor loop of the MPTs, MH-EITs, MH-SDTs and
  MH-BITs of the lab recordings splits exactly to its loop length.
  Status: confirmed (live data) for the descriptors seen.

## mmt-si-text: character coding
- Spec: §7.1: "The coding letter group for letters which are used in the
  control information shall be UCS (ISO/IEC 10646:2012), and the coding
  scheme for letters shall be UTF-8 without BOM", details in STD-B62.
- Confirmation: MH-EIT event names and MH-SDT service names decode as
  valid UTF-8 Japanese text on the lab recordings (ARIB 8-unit decoding
  gives nonsense). Status: confirmed (live data).

## tlv-si-text: text in TLV-NIT
- Spec: §5 does not state the coding of TLV-SI text (the network name
  descriptor's `char`); §7.1's UTF-8 rule is written for MMT-SI.
- Interpretation: UTF-8, as MMT-SI.
- Confirmation: the TLV-NIT network name of the live recordings is valid
  UTF-8 text. Status: confirmed (live data).

## scrambled-mpu: what is in the clear in a scrambled MPU payload
- Spec: §6.4: the MMTP multi-type header extension 0001h carries
  "information related to scramble specified in ARIB STD-B61"; B61 (not
  local) defines what is encrypted.
- Observation: over 3 852 scrambled MPU payloads of three TLV streams
  (`examples/scrambleprobe`), the 8-byte payload header is always
  consistent (payload_length = payload − 2, fragment_type 2, timed 1,
  MPU sequence numbers continuous), while the MFU header after it is not
  (the offset of whole MFUs, 0 by definition, never reads 0).
- Interpretation: the payload header (payload_length to
  MPU_sequence_number) is in the clear and everything after it is
  scrambled. Scrambled payloads are counted from the header and never
  parsed further or delivered; an aggregated scrambled payload (whose MFU
  lengths are scrambled) counts as one MFU. Status: observation.

## ntp: NTP packets among the MMTP flows
- Spec: §3.2: NTP in UDP to port 123, IPv6 destination ending in :101,
  IP header not compressed.
- Observation: one uncompressed IPv6 packet per TLV stream per frame is
  NTP; parsed as MMTP it would read as packet id 0 (the PA message's id).
- Interpretation: UDP destination port 123 is NTP and is never parsed as
  MMTP; the transmit timestamp (last 8 bytes of Table 3-1) is kept.
  Status: confirmed (live data).

## mpu-header-codes: fragment type and fragmentation indicator tables
- Spec: Table 6-1 (MMTP payload structure) gives `fragment_type` (4
  bits) and `fragmentation_indicator` (2 bits); their codes are in Table
  6-2 (0 MPU metadata, 1 movie fragment metadata, 2 MFU, 3–15 "provided
  by the Ministry or private standardization organization"; note 1: 0
  and 1 "are provided in MMT Standard, but are not used in this
  standard") and Table 6-3 (00 "one or more than one data in perfect
  form", 01 "header part of divided data", 10 "a part of divided data
  which is neither header part nor last part", 11 "last part of divided
  data").
- Interpretation: `mpu::FragmentType` is a lossless coded enum naming 0–2
  (0 and 1 are assigned codes a receiver may meet, even though §6.2 says
  the metadata "shall not be transmitted") with `Reserved(code)` for
  3–15. `mpu::FragmentationIndicator` names all four codes after the
  table's words — `Complete`, `Head`, `Middle`, `Last` — and needs no
  reserved variant. The reassembler's state machine (`Reassembler::push`)
  matches on these variants; a scrambled payload counts as one MFU when
  its indicator `completes()` (`Complete` or `Last`).
- Confirmation: `spec_b60_table_6_1_fragment_types`,
  `spec_b60_fragmentation_indicators` and
  `prop_payload_header_codes_round_trip` (every code parses and
  re-encodes to itself); `prop_divided_mfus_reassemble` unchanged in
  meaning. Status: confirmed (spec text).

## mfu-fragment-counter: reassembling divided MFUs
- Spec: Table 6-1 / Table 6-3: fragmentation indicator 01 / 10 / 11;
  fragment_counter "the number of fragmented data which is after the
  part where this MMTP Payload stores … When the number of fragmentation
  is more than 255, this indicates the remainder with which the number
  of fragmented data divided by 255".
- Interpretation: a divided MFU is the payloads from 01 to 11 of one
  packet id with consecutive MMTP packet sequence numbers; the fragment
  counter is not checked (a 4K NAL unit takes more than 255 payloads),
  the packet sequence numbers detect a loss.
- Confirmation: the property test and the lab recordings (0 MFUs dropped
  without a packet loss). Status: confirmed for unscrambled data in
  tests; on air all media were scrambled.

## hevc-annex-b: HEVC elementary stream (from knowledge)
- Spec: §8.1.1: each NAL unit is an MFU preceded by its size as a 32-bit
  unsigned integer; VPS, SPS, PPS are MFUs of their own (asset type
  hev1).
- From knowledge (H.265 not local): an Annex B byte stream puts each NAL
  unit after a start code `00 00 00 01`; the NAL unit type is bits 1–6
  of its first byte (VPS 32, SPS 33, PPS 34, AUD 35, SEI 39/40, IRAP
  slices 16–21). Status: from knowledge; not exercised on air (all video
  scrambled).

## loas: MPEG-4 audio elementary stream (from knowledge)
- Spec: §8.2.1: each MFU is an AudioMuxElement() "that synchronization
  byte and length information are removed from AudioSyncStream()"; the
  receiver adds them back.
- From knowledge (ISO/IEC 14496-3 not local): AudioSyncStream() is an
  11-bit syncword 0x2B7 and a 13-bit audioMuxLengthBytes before each
  AudioMuxElement (LOAS). Status: from knowledge; not exercised on air
  (all audio scrambled).

## message-fragment-counter: divided control messages
- Spec: Table 6-1, payload type 02h: fragmentation_indicator and
  fragment_counter, the latter "the number of fragmented data which is
  after the part where this MMTP Payload stores".
- Observation: on the lab recordings the fragment_counter of control
  messages reads 0 on every fragment, including a 01 (first) fragment
  followed by 10 and 11 fragments (MH-EIT sections on packet 8000h,
  `examples/mmtpdump` from `isdb-tlv`).
- Interpretation: a divided message is the payloads from 01 to 11 of one
  packet id with consecutive MMTP packet sequence numbers; the counter is
  not used. Requiring it dropped 141 MH-EIT fragments of a 40 s stream;
  without it every section reassembles CRC-clean (MH-EIT events of BS7
  service 0097h: 175 → 263). Status: observation.

## async-mpu-numbering: MPU sequence numbers of non-timed assets
- Spec: §10.2.1: an application file is an asynchronous MPU; its
  sequence number "shall be unique at least in the asset". Timed media
  (§6.2) number their MPUs one after another.
- Observation: application assets (aapp) of the lab recordings jump
  between nearly every MPU (e.g. 184 jumps in 185 MPUs).
- Interpretation: `isdb tlv validate` reports MPU sequence jumps as errors
  for timed assets only (hev1, hvc1, mp4a, stpp, asgd). Status:
  confirmed (spec text and live data).
