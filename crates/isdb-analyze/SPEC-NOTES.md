# isdb-analyze spec interpretation notes

How the validator turns spec text into checks and severities, and the
interpretations it adopts.  Texts: `specs/ITU-T-H.222.0-2006.txt`,
`specs/ARIB-STD-B10-v5.13.txt`, `specs/ARIB-STD-B32-v3.11-part2.txt`
(AAC).  Format as in the workspace `SPEC-NOTES.md`.

## isdb-analyze: severity classification
- Spec: H.222.0 §2.1.64; B10 Part 2 §3.1, §5.1.3, §6.1 Table 6-1 *2; B10 Appendix preface.
- Text: H.222.0 §2.1.64 / B10 §3.1: "Unless otherwise specified within the
  present document all "reserved_future_use" bits shall be set to "1"."
  Table 5-2: "The value specified as sending frequency in table 5-2 is only a
  criterion of operation and is not the standard value."  Table 6-1 *2: "Can
  be substituted with the descriptor defined by service provider, if it has
  at least the same function."  Appendix preface: "This appendix is
  established as a guideline … and is not a part of the standard."
- Interpretation: **error** = a "shall" of H.222.0 or B10 Part 1/2 violated;
  **warning** = reserved bits not '1' (decoders ignore them), Table 6-1
  mandatory descriptors marked *2 (substitutable), Table 5-2 frequencies,
  Appendix guidelines, and timing estimates; **info** = observations.  No
  finding is suppressed because live broadcasts show it; live data decides
  only the classification text in the report.
- Confirmation: a live terrestrial capture recorded during development produces no error from
  these classes except the PCR interval below.  Status: interpretation.

## isdb-analyze: B10 Table 5-2 transmission level vs frequency
- Spec: B10 Part 2 §5.1.3 Table 5-2.
- Text: "Table 5-2 lists the values, which shall be used for table_id and
  transmission level for the SI"; "0x01 CAT*1 Mandatory Once or more/1
  sec."; "*2: Transmitting either TDT or TOT is mandatory."
- Interpretation: missing Mandatory tables (PAT, CAT, PMT per PAT program,
  NIT actual 0x40, SDT actual 0x42, EIT p/f actual 0x4E, TDT-or-TOT) are
  **errors** (transmission level is in the "shall be used" sentence); the
  frequency column is a **warning**.  CAT is mandatory per B10 even though
  H.222.0 §2.4.4 requires it only "whenever one or more elementary streams
  … are scrambled".
- Measurement: per (PID, table_id, table_id_extension, section_number) the
  intervals between successive section starts, plus the gaps from capture
  start to the first and from the last occurrence to capture end.  The edge
  gaps are a lower bound of the true maximum interval, so a gap above the
  criterion in a capture longer than the criterion is reported as
  "exceeded (edge gap)"; captures shorter than the criterion report
  "insufficient capture".  Status: interpretation.

## isdb-analyze: "EIT within 8 days" table_ids
- Spec: B10 Part 2 Table 5-2; Appendix §1.4.2.1 b), c).
- Text: Table 5-2: "EIT (Program within 8 days of the actual stream) …
  Once or more/10 sec.", "EIT (Program after 8 days …) Once or more/30
  sec." for 0x50–0x5F; Appendix: "the 256 sections under each sub-table are
  divided into 32 segments of 8 sections each", "each segment contains
  information about events that start anywhere within a three-hour period".
- Interpretation: one table_id spans 32 × 3 h = 4 days, so 0x50/0x51
  (0x60/0x61) carry days 0–7 → 10 s criterion; 0x52–0x5F (0x62–0x6F) → 30 s.
  The ARIB operational split of 0x58–0x5F for extended event information
  is not in B10 and is not used.  Status: interpretation.

## isdb-analyze: 4 KB in 32 ms per PID
- Spec: B10 Part 2 §5.1.4.
- Text: "Moreover, TS packets of the same PID is transmitted within the
  range of 4KB±100% in 32msec each. The rule of "4KB in 32msec" is a
  detailed specification of 1Mbit per 1 sec. for every PID."
- Interpretation: checked on SI PIDs 0x0010–0x002F as at most 8 KiB
  (4 KB + 100 %) of TS packets (188 bytes each) in any 32 ms window of the
  PCR time base; **warning** (the wording is garbled and the window is an
  estimate).  Status: interpretation.

## isdb-analyze: PCR interval measured on PCR values
- Spec: H.222.0 §2.7.2.
- Text: "the time interval between the bytes containing the last bit of
  program_clock_reference_base fields in successive occurrences of the PCRs
  in Transport Stream packets of the PCR_PID for each program shall be less
  than or equal to 0.1 s."
- Interpretation: the interval is the difference of successive PCR values
  (27 MHz, wrap-aware) on each program's PCR_PID — the PCRs define the
  stream's time base (eq. 2-4), and packet positions are not time in a
  capture with null packets stripped.  Jumps > 5 s or backwards are counted
  as discontinuities, not intervals.  **error**.
- Live finding (a terrestrial capture recorded during development): the
  partial-reception (one-seg) program: 46 PCRs in 10.78 s, every interval 0.231
  s → reported as error (x45). Status: confirmed on live data; kept as error
  (the text is a "shall").

## isdb-analyze: PCR accuracy
- Spec: H.222.0 §2.4.2.2.
- Text: "The PCR tolerance is ± 500 ns."
- Interpretation: only estimable when packet positions are constant-rate
  time; the deviation of PCRs from a least-squares constant-rate fit is
  reported only when null packets are present (CBR); > 500 ns is a
  **warning** (the mux could be VBR).  Status: estimate.

## isdb-analyze: section repetition time base
- Spec: H.222.0 §2.4.2.2 eq. 2-4 (byte arrival time interpolated between PCRs).
- Interpretation: section start packets are timed by linear interpolation
  between the PCRs of the first program's PCR_PID (else the PID with most
  PCRs, else `--bitrate`).  In captures with null packets stripped the
  interpolation error is up to roughly one PCR interval's share of packet
  bunching (≈ ±10 ms observed on a live capture: PAT 0.088–0.112 s around
  0.100 s), so marginal frequency warnings on such captures are measurement
  noise. Status: estimate, documented in reports.

## isdb-analyze: table_id per PID
- Spec: H.222.0 §2.4.4; B10 Part 1 §5.1 Table 5-1, Part 2 §5.1.3 Table 5-1.
- Text: H.222.0: PAT "Only sections with this value of table_id are
  permitted within Transport Stream packets with PID value of 0x0000" (same
  for CAT/0x0001).  B10 Part 1: "PID values of the Transport Stream packets
  carrying tables … shall be as shown in table 5-1"; ST "Exclude 0x0000,
  0x0001, 0x0014".
- Interpretation: Table 5-1 maps table → PID.  A table on a PID other than
  its allocated one is an **error**; other PIDs carrying sections of a
  table_id with indirect PID designation (PMT, ECM, EMM, DSM-CC, …) are not
  errors.  A table_id without fixed PID found on an allocated SI PID
  (0x0010–0x002F, outside the company range 0x90–0xBF) is a **warning**
  (Table 5-1 is not stated to be exclusive).  PIDs 0x0000/0x0001 are
  exclusive (**error**).  `isdb-si` Placement deviations are dropped in
  favour of this check (same rule, wider table set incl. SDTT/CDT/DIT/SIT/AMT).
  Status: interpretation.

## isdb-analyze: PID allocation
- Spec: H.222.0 Table 2-3; B10 Part 1 Table 5-1.
- Interpretation: packets on 0x0004–0x000F (H.222.0 reserved) → **error**;
  a PMT/elementary PID equal to a PID B10 Table 5-1 allocates to a table →
  **error**; other PIDs in 0x0010–0x002F used as PMT/ES → **info** (B10 does
  not reserve the unlisted values).  PIDs with packets but no PSI/SI
  designation → **info** ("Other private data may exist in the Transport
  Stream without being listed", H.222.0 §2.4.4).  PIDs whose only packets
  have transport_error_indicator set are skipped (the PID itself may be a
  bit error).  Status: interpretation.

## isdb-analyze: version coherence and sub_table completeness
- Spec: H.222.0 §2.4.4.5/§2.4.4.9 version_number; B10 Part 2 §5.1.1 d).
- Text: H.222.0: "The version number shall be incremented by 1 modulo 32
  whenever the definition of the Program Association Table changes."  B10:
  "A new version of the SI data is signaled by sending a sub_table with the
  same identifiers as the previous sub_table containing the relevant data,
  but with the next value of version_number."
- Interpretation: more than one distinct body under one (PID, table_id,
  table_id_extension, version, section_number) → **error**; differing
  last_section_number within a sub_table version → **error**; sections of
  a non-EIT-schedule sub_table not seen in the capture → **warning** (a short
  capture may miss them).  Short sections (TDT/TOT/RST/ST) have no version.
  Status: interpretation.

## isdb-analyze: EIT rules
- Spec: B10 Part 2 §5.2.7; Appendix §1.4.1, §1.4.2.1.
- Text: §5.2.7 "The present/following table shall contain only information
  pertaining to the present event and the chronologically following event
  … except in the case of a Near Video On Demand (NVOD) reference service";
  "The event information shall be chronologically ordered."  Appendix:
  "The SI bit stream shall have two sections per service for an EIT
  Present/Following with the section_number 0x00 reserved for the
  description of the present event and section_number 0x01 for the
  following event … The event after the following event can be implied
  optionally, using the section_number 0x02 and after."
- Interpretation: more than one event in p/f section 0 or 1 → **error**
  (NVOD services are not distinguished); events not chronological within a
  section → **error**; missing p/f section 0 or 1 → **warning** (guideline);
  sections ≥ 2 accepted.  Schedule: segment_last_section_number outside
  [section_number, s0+7] → **warning** (Appendix §1.4.2.1 e–g, guideline);
  segment_last_section_number > last_section_number and last_table_id below
  table_id or outside the same EIT kind → **error** (§5.2.7 semantics).
  Status: interpretation.

## isdb-analyze: cross-table identities
- Spec: B10 Part 2 §5.2.4, §5.2.6, §5.2.7, §6.1 Table 6-1; Appendix §1.1 c), §1.3.
- Text: §5.2.4: "In the event that the NIT is transmitted on the network
  on which the TS was originated, the network_id and the
  original_network_id shall take the same value."  Appendix §1.3: "the SI
  bit stream shall list in the SDT of a particular TS at least all the
  services of that TS."  Table 6-1: terrestrial_delivery_system_descriptor
  "Mandatory for digital terrestrial broadcasting"; system_control
  (system_management) descriptor "Mandatory either in PMT or NIT".
- Interpretation: SDT/EIT actual transport_stream_id ≠ PAT
  transport_stream_id → **error**; original_network_id disagreeing across
  SDT, EIT actual, NIT TS loop and BIT → **error**; network_id ≠ own TS
  original_network_id → **warning** (conditional on origination); PAT
  programs absent from SDT actual / NIT service_list → **warning**; missing
  terrestrial delivery / system management descriptor → **warning** (the
  analysed TS may not be terrestrial).  SDT EIT flags vs EIT presence:
  §5.2.6 "should not be present" → **warning**; schedule flag 1 without
  schedule in a short capture → **info**.  Status: interpretation.

## isdb-analyze: `si.copy_control` (receiver evidence, not a B10 rule)
- Spec: B10 Part 2 §6.2.23 (digital_copy_control_descriptor,
  `user_defined`) and Annex F (Informative) Tables F-1/F-2.
- Text: §6.2.23 "user_defined: This is a 4-bit field, and broadcasting
  service provider can define it originally."  Annex F is headed "Annex F
  (Informative)" / "Example of service provider define bit of digital copy
  control descriptor"; Table F-2 lists copy_control_type 00 as "Undefined".
- Interpretation: B10 gives no normative rule on these bits, so the check
  is **not** derived from a "shall".  It encodes receiver evidence: a Sony
  BDZ-series recorder refuses ("Unsupported signal") a service whose 0xC1
  user_defined carries copy_control_type '00' in the Annex F layout and
  plays it with '01' (user_defined 0b0100, what live broadcasters send);
  see the bisection table in
  [isdbt-mux SPEC-NOTES](../isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits).
  The ARIB
  operational guidelines that may make the layout normative (TR-B14/TR-B15)
  are not available locally.  Severity: **warning** (receiver
  compatibility, not conformance), one finding per table location, for
  the program-level and per-component user_defined fields.
- Confirmation: one receiver model, one bisection.  Status: bench
  evidence.

## isdb-analyze: PES timing
- Spec: H.222.0 §2.7.4, §2.4.2.6, §2.4.3.7, Table 2-22.
- Text: §2.7.4 "the maximum difference between coded presentation
  timestamps referring to each elementary video or audio stream is 0.7 s";
  §2.4.2.6 "The delay of any data through the System Target Decoder buffers
  shall be less than or equal to one second except for still picture video
  data and ISO/IEC 14496 streams … For ISO/IEC 14496 streams, the delay is
  constrained by tdn(j) – t(i) ≤ 10 seconds".
- Interpretation: §2.7.4 is checked on the sorted (presentation-order)
  PTS of audio/video stream types → **error**; no PTS at all → **error**.
  The T-STD delay is estimated as (DTS, else PTS) − PCR interpolated at the
  packet carrying the PES header, limit 1 s (10 s for stream_types
  0x10–0x13/0x1B) → **warning** (estimate; first byte of the PES only).
  Decode time not increasing in stream order → **warning**; PTS < DTS →
  **error**; stream_id class inconsistent with stream_type → **warning**.
  Optional PES headers that fit in the first packet but do not decode
  (PES_header_data_length shorter than the flagged fields) → **error**.
  Status: interpretation.

## isdb-analyze: section rewriting in `edit` (not used by validation)
- Spec: H.222.0 §2.4.4, §2.4.4.2, §2.4.3.3.
- Text: "packet stuffing bytes of value 0xFF may be found in the payload …
  only after the last byte of a section. In this case all bytes until the
  end of the Transport Stream packet shall also be stuffing bytes … the
  payload of the next Transport Stream packet with the same PID value shall
  begin with a pointer_field of value 0x00"; B10 Part 2 §5.1.2 "one section
  is finished before the next one is allowed to be started".
- Interpretation: the packer starts a new section in a packet only if the
  section's original start position has been reached and at least one byte
  fits after the pointer_field; otherwise the packet is 0xFF-stuffed and the
  next packet starts with pointer_field 0.  Continuity counters of the
  rewritten PID are renumbered from its first original counter.  Identity
  rewrites reproduce the PSI/SI section sequences of a live capture exactly
  (packets are byte-identical except where the live mux packed differently).
  Status: confirmed by tests.

## isdb-analyze: MPEG-2 AAC ADTS (B32 Part 2 §5.2) on stream_type 0x0F
- Spec: B32 Part 2 §5.2.1–5.2.2; Attachment Ch. 3 Table 1; B10 Table
  6-44. The rules, quotes and their 13818-7 dependencies are listed once,
  in `crates/isdbt-mux/SPEC-NOTES.md` "AAC ADTS" (the rule set
  `isdbt_mux::adts::b32::Rule` is shared, with clause and quoted text).
  Header-level only: the raw_data_block is not parsed, so the crc_check
  value (Appendix 1), coupling channel, element order, PCE, SBR fill and
  LC tool rules are not checked (`isdbt-mux` SPEC-NOTES "AAC ADTS:
  raw_data_block not parsed"); `aac.summary` counts the frames that
  carry a crc_check.
- Reassembly: PES payload bytes of the PID are concatenated from the first
  PES start; a frame is accepted only when the next syncword follows it
  (false syncs and frames cut by packet loss are skipped and counted as
  `aac.sync` resynchronisations; the last frame of a capture is not
  checked). The syncword search requires layer '00', so a layer ≠ '00'
  stream yields `aac.frames` (no frame) rather than `aac.layer`. Packets
  with transport_error_indicator are not fed.
- Severity: every §5.2.1 / §5.2.2 "shall" / "not permitted" / fixed value
  is an **error** (`aac.<rule>` with frame count, first frame, quoted
  text); `aac.id` cites ISO/IEC 13818-7 from knowledge.
- Independence: the header fields are parsed in `aac::header`, separately
  from `isdbt_mux::adts::AdtsHeader`, so a symmetric header encode/parse
  bug in `isdbt-mux` (e.g. in `SilentFrame`) is checked by other code.
- quality_indicator: for each audio component descriptor (EIT events and
  PMT), component_tag → PID through the PMT stream_identifier_descriptor;
  rate = Σ frame bytes × 8 × fs / samples; rate per stereo pair = rate × 2
  / coded channels from channel_configuration (1–5 → 1–5, 6 → 5: LFE
  excluded; 0 and 7 undetermined, counted as 2). **Warning** only for
  clear contradictions of Table 1: Mode 1 below the 192 kbit/s/stereo
  reference range, Mode 2 below 144 kbit/s/stereo (Note 3), Mode 1 or 2
  below 32 kHz ("ex. Sampling frequency below 32 kHz" for Mode 3);
  otherwise **info** with the mode Table 1 derives. The reference
  bitrates are "for reference purposes" in the text, hence warning, not
  error. Implicit SBR (output rate 2 × fs) cannot be seen in the header:
  at sampling_frequency_index 0x6–0x8, where §5.2.3 (4)(a) permits it, a
  contradiction that disappears at 2 × fs is reported as **info** naming
  the ambiguity.
- Live findings (a live terrestrial capture recorded during development):
  the full-seg audio PID, 504 frames LC 48 kHz stereo 191.7 kbit/s
  (192 kbit/s CBR), crc_check in every frame, fullness 37..=128; the
  one-seg audio PID, 249 frames LC 24 kHz (with SBR), 48.1 kbit/s,
  crc_check, fullness 85..=160 — no violation (crc_check presence counted;
  values not verified, see above); EIT quality_indicator '10'
  (Mode 2) for the full-seg services, consistent; the one-seg EIT has no
  audio component descriptor. Status: confirmed on live data.
