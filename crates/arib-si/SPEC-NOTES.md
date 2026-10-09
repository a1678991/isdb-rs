# arib-si spec interpretation notes

Ambiguous, garbled or inconsistent places in ARIB STD-B10 v5.13 (English,
`specs/ARIB-STD-B10-v5.13.txt`, extracted from the ARIB PDF
`6-STD-B10v5_13-E1.pdf`, not redistributed; PDF page = printed page + 14) and the interpretation `arib-si` adopts.
Format as in the workspace `SPEC-NOTES.md`.

## arib-si: Annex C formulas evaluated in exact integer arithmetic
- Spec: B10 Part 2 Annex C (informative), (a)–(e).
- Text: "Y' = int[(MJD – 15078.2) / 365.25] … M' = int{[MJD – 14956.1 – int (Y' × 365.25)] / 30.6001} …";
  "[Note]: These formulas are effective from March 1, 1900 to February 28, 2100."
- Interpretation: the decimal constants are exact decimals; every `int[…]`
  is evaluated as an exact rational floor (e.g. `(100·MJD − 1507820) div 36525`),
  never with binary floating point.  Outside MJD 15079 (1900-03-01) … 88127
  (2100-02-28) conversion returns `None`; the 16-bit field covers MJD
  0…65535, so MJD < 15079 is representable but not convertible.
- Confirmation: Annex C worked example (MJD 45218 = 1982-09-06, WD 1,
  W 4315, WY 82, WN 36), §5.2.7 Example 1 (0xC079124500 = 93/10/13
  12:45:00), §5.2.8 Note (16-bit MJD ends 2038-04-22 = 0xFFFF), and an
  exhaustive day-by-day calendar continuity test over the whole range
  (`spec_time_annex_c_range_exhaustive`).  Status: confirmed.

## arib-si: section length limits — §5.1.1 vs per-table semantics
- Spec: B10 Part 2 §5.1.1 vs §5.2.11–§5.2.15 semantics.
- Text: §5.1.1 "The sections within each table are limited to 1024 bytes in
  length, except for sections within the EIT, which are limited to 4096
  bytes."  But PCAT/BIT/NBIT/LDT/ST semantics each say "The section_length
  shall not exceed 4093 so that the entire section has a maximum length of
  4096 bytes."
- Interpretation: the per-table clause is more specific and later; the
  conformance checker uses 1021 for NIT/BAT/SDT/RST (whose semantics say
  1021) and 4093 for EIT/PCAT/BIT/NBIT/LDT/ST.  TOT: only "first two bits …
  '00'" (≤ 1023) is stated.  Codecs accept up to 4093 for every table.
- Confirmation: none needed for decoding; the sections of a live terrestrial
  capture (2026-10-07) are all within both readings except EIT (3139 bytes, allowed by both).  Status: interpretation.

## arib-si: PCAT content_descriptor_length scope
- Spec: B10 Part 2 §5.2.12, Table 5-12.
- Text: "content_descriptor_length: This 12-bit field gives the total length
  in bytes of the following schedule loop and descriptor loop."  The field
  is followed by `reserved_future_use(4) schedule_description_length(12)`,
  then the schedule loop, then the descriptors.
- Interpretation: taken literally — schedule loop bytes + descriptor loop
  bytes, *excluding* the 2-byte reserved_future_use/schedule_description_length
  word.  Descriptor loop length = content_descriptor_length −
  schedule_description_length.
- Confirmation: pending — no PCAT in the live terrestrial capture (PID 0x0022 absent).
  If a real PCAT disagrees, the decoder fails loudly (lengths would not add
  up to the section), it does not mis-decode silently.

## arib-si: video_decode_control_descriptor last 2 bits
- Spec: B10 Part 2 §6.2.30, Table 6-59 (PDF p.190); Part 1 Fig. 6.2-32.
- Text: Table 6-59 lists `transfer_characteristics 2 bslbf`; the semantics
  say "transfer_characteristics (Transfer characteristics): This is an 8-bit
  field"; Part 1 Fig. 6.2-32 still shows "Reserved future use 2".
- Interpretation: 2 bits (the syntax table, checked on the PDF page; the
  descriptor is 1 byte and Table 6-60-1 has 2-bit codes).  Not checked as
  reserved_future_use.
- Confirmation: syntax table + Table 6-60-1 code width.  Status: confirmed by
  the spec's own tables.

## arib-si: semantics stating wrong widths (syntax table followed)
- Spec: B10 Part 2 §6.2.22 Table 6-37; §6.2.40 Table 6-77; §6.2.6 Table 6-8.
- Text: "reference_PID: This 3-bit information indicates PID" (table: 13
  bits); "description_type: This 8-bit field" (table: 4 bits, Part 1 Fig.
  6.2-47: 4); Table 6-8 names the 28-bit field "system_rate", semantics
  "symbol_rate".
- Interpretation: syntax-table widths (they sum to whole bytes and agree with
  the Part 1 figures); field named `symbol_rate`.  Status: confirmed by Part 1
  figures.

## arib-si: digital_copy_control_descriptor 4-bit field after the flags
- Spec: B10 Part 2 §6.2.23 Table 6-38 vs Part 1 Fig. 6.2-25.
- Text: Table 6-38 `user_defined 4`; Part 1 figure labels it "reserved".
- Interpretation: `user_defined` (Part 2 is the definition); not checked for
  '1's.  The inner loop's 1-bit field *is* reserved_future_use (both sources)
  and is checked.
- Typed view (`values::UserDefinedAnnexF`, `DigitalCopyControl::user_defined_annex_f`):
  the example layout of Annex F (informative) Table F-1 — copy_control_type (2,
  Table F-2) then APS_control_data (2, Table F-4) when copy_control_type = '01',
  else reserved_future_use (2).  An exact bijection on 0..=15, so the raw field
  stays authoritative.  The ARIB operational guidelines (TR-B14/TR-B15, not
  available locally) are understood to make this layout normative; the Sony
  BDZ-series recorder refuses copy_control_type '00' and accepts '01' (bench
  2026-10-08, see `crates/arib-mux/SPEC-NOTES.md`).  Live terrestrial capture:
  0b0100 (HD services), 0b1000 (one-seg).

## arib-si: stuffing_section has no fixed form
- Spec: B10 Part 2 §5.2.11, Table 5-11.
- Text: "section_syntax_indicator: This 1-bit field may take either the value
  '1' or '0'." No CRC_32 in the syntax.
- Interpretation: `StuffingSection` codes the whole section (header + data
  bytes), never via the long/short envelope, and never checks a CRC — even
  when section_syntax_indicator = 1 the bytes after section_length are all
  `data_byte`s per the table.

## arib-si: conditional_access_descriptor syntax source
- Spec: B10 Part 1 Table 5-3 ("Conditional access descriptor*1", *1 "In
  accordance with the Notification"), Part 1 Fig. 6.2-1; Part 2 has no table.
- Interpretation: syntax from H.222.0 §2.6.16 Table 2-58 (identical to Part 1
  Fig. 6.2-1: 16 / '111' 3 / 13 / private).  The 3 reserved bits are checked
  as '111' (H.222.0 Table 2-58 "reserved", B10 Fig. 6.2-1 shows '111').

## arib-si: descriptors and tables defined outside B10
- Spec: B10 Part 1 Table 5-3 footnotes *3/*5, Table 5-2 footnotes *1/*2.
- Text: "0xC9 Download content descriptor*3", "0xCB CA contract information
  descriptor*5", "0xCC CA service descriptor*5" (*3 "specified in ARIB STD-B1
  and B21", *5 "specified in ARIB STD-B25"); "0xC3 SDTT*1", "0xC8 CDT*2"
  (*2 "Specified in ARIB STD-B1, B21").
- Interpretation: B10 gives no syntax; under the B10-only ground-truth rule
  they stay `Descriptor::Unknown` / `AnyTable::Unknown` (CRC still verified
  for long sections).  To be typed from B21/B25 when those are admitted.

## arib-si: Table 6-1 "mandatory" descriptors checked
- Spec: B10 Part 2 §6.1 Table 6-1.
- Text: "network_name_descriptor Mandatory"; "service_list_descriptor
  Mandatory in NIT/TLV-NIT (actual network) … Mandatory in BAT";
  "bouquet_name_descriptor Mandatory in BAT"; "service_descriptor Mandatory
  in SDT (actual stream)"; "short_event_descriptor Mandatory in EIT".
  Footnote *2: "Can be substituted with the descriptor defined by service
  provider, if it has at least the same function."
- Interpretation: only these unconditional rules are checked; conditional
  ones ("for digital terrestrial broadcasting", "for hierarchical
  transmission", "either in PMT or NIT") need context the section does not
  carry and are not checked.  Table 6-1 does not say which NIT loop, so
  presence anywhere in the NIT satisfies the check.  The *2 substitution
  cannot be verified from B10, so absence is reported (Kind
  `MandatoryDescriptor`) and the user decides.
- Finding (live terrestrial capture, 2026-10-07): 217 EIT schedule events
  lack a short_event_descriptor: 207 on the two sub-channel services (tables
  0x50/0x51) carry only an event_group_descriptor with group_type 0x1 "Event
  common" referencing the main HD service; 8 events in table 0x58 carry only
  extended_event_descriptors; 2 events in 0x58 carry no descriptor at all.
  B10 text gives no exemption; reported, not suppressed.  The synthetic
  multiplex of `tests/synthetic` reproduces both shapes, and
  `tests/synthetic_si.rs` asserts they are the only deviations reported.

## arib-si: audio_component ES_multi_lingual_flag "reserved"
- Spec: B10 Part 2 §6.2.26.
- Text: "ES_multi_lingual_flag: … set to '1' when 2-language multilingual …
  is made in ES at 1/0 + 1/0 mode. In case of other mode, this bit is
  reserved."
- Interpretation: not checked — "reserved" bits are '1' (§3.1), but '1' would
  make ISO_639_language_code_2 present, so the literal reading is
  self-contradictory.  The flag is coded as presence of `language_2`.

## arib-si: local_time_offset range
- Spec: B10 Part 2 §6.2.25.
- Text: "the current offset time from JST (UTC+9 hours) in the range between
  -12 hours and +12 hours".
- Interpretation: conformance flags BCD offsets whose hour exceeds 12 or that
  are not BCD; polarity carries the sign.

## arib-si: typed field values (`values` module)
- Spec: B10 Part 2 Tables 6-5, 6-39, 6-43, 6-45, 6-60, 6-60-1; Annex F
  Tables F-2/F-4.
- Interpretation: each enum maps a variant to exactly the printed code;
  codes a table reserves decode to a `Reserved`/`Extension` variant carrying
  the value, so the typed view never fails and never loses bits.  Short
  names are derived from the table text (e.g. Table 6-60 "1080/I" →
  `"1080i"`, "2160/60/P" → `"2160p60"`; Table 6-43 modes keep the spec
  notation, `"3/2+LFE"`).
- Table 6-60-1 '11' is "Undefined"; it is also the value of the 2 bits when
  they were reserved_future_use (pre-v5 editions; Part 1 Fig. 6.2-32), which
  is why the 0x47 that live HD services send carries '11'.
- Table 6-5 stream_content 0x1 rows 0xD2–0xD4 read "Video 240p, 4:3 aspect
  ratio, with pan vectors / without pan vector / > 16:9" in the text and on
  PDF p.142 (printed p.128).  Every other row of the 0x1 and 0x5 blocks has
  16:9 in the x2/x3 positions (0x5/0xD2: "Video 240p, 16:9 aspect ratio with
  pan vectors"), so the variants are named `240p-16:9-with-pan-vectors`,
  `240p-16:9-without-pan-vectors`, `240p-over-16:9`; the descriptions keep
  the printed text with "[sic]".  Status: interpretation (typo).
- Table 6-45 '000' "Reserved for future use" and '100' "Reserved" both map
  to `SamplingRate::Reserved`.
