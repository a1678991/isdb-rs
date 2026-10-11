# Specifications used by the `isdb-rs` workspace

This file lists which specification documents the workspace depends on, where
each one comes from, which clauses are implemented and tested, and where the
code relies on interpretation or on knowledge from outside those documents.
The per-entry reasoning lives in the crate `SPEC-NOTES.md` files
([index](SPEC-NOTES.md)). The gr-isdbt differential results are in
[VERIFICATION.md](VERIFICATION.md).

The statuses below summarise evidence recorded elsewhere: the tests named in
each row, the crate `SPEC-NOTES.md` entries, [VERIFICATION.md](VERIFICATION.md)
(gr-isdbt oracle and real receivers) and the receiver compatibility notes.
Nothing here is new evidence.

**Obtaining the specifications.** The specification PDFs are copyrighted
and are not redistributed with this repository. The official download URLs
and the SHA-256 of the editions the code cites are in [§1.1](#11-documents-used),
followed by the `pdftotext` commands that produce the greppable text the
code comments and SPEC-NOTES quote (into the gitignored `specs/`). Real
captures are not distributed either; how to make and use your own is in
[docs/test-data.md](docs/test-data.md).

Naming: the crate notes and code say "B31 Part 2" for the B31 v2.2-E1
**Attachment** ("Operational Guidelines for Digital Terrestrial Television
Broadcasting", PDF printed pp. 89–138). This file uses the same name.

---

## 1. Inventory

### 1.1 Documents used

The PDFs are not distributed with this repository. Each URL was checked with
`curl -sI` on 2026-10-08. Every one returned HTTP 200 with
`Content-Type: application/pdf`, and the remote `Content-Length` equalled
the size of the file the hashes below were taken from.

| Spec | Edition used | Title | Official download | PDF file name | `specs/` text | Role here |
|---|---|---|---|---|---|---|
| ITU-T H.222.0 (05/2006) \| ISO/IEC 13818-1:2007 | 05/2006 | Information technology – Generic coding of moving pictures and associated audio information: Systems | https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-H.222.0-200605-S!!PDF-E&type=items (landing page https://www.itu.int/rec/T-REC-H.222.0-200605-S/en) | `T-REC-H.222.0-200605-S.pdf` | `ITU-T-H.222.0-2006.txt` | implemented (`isdb-ts`, `isdb-analyze`, `isdbt-mux`) |
| ARIB STD-B10 | 5.13-E1 (2019-12-05) | Service Information for Digital Broadcasting System | https://www.arib.or.jp/english/html/overview/doc/6-STD-B10v5_13-E1.pdf | `6-STD-B10v5_13-E1.pdf` | `ARIB-STD-B10-v5.13.txt` | implemented (`isdb-si`, `isdb-analyze`, `isdbt-mux`) |
| ARIB STD-B24 Fascicle 1 (Vol. 1) | 6.4-E1 (2017-07-27) | Data Coding and Transmission Specification for Digital Broadcasting | https://www.arib.or.jp/english/html/overview/doc/6-STD-B24v6_4-1p3-E1.pdf | `6-STD-B24v6_4-1p3-E1.pdf` | `ARIB-STD-B24-v6.4-vol1.txt` | Part 2 Ch. 7 and Part 3 Table 8-2 implemented (`isdb-text`) |
| ARIB STD-B31 | 2.2-E1 (2014-03-18) | Transmission System for Digital Terrestrial Television Broadcasting | https://www.arib.or.jp/english/html/overview/doc/6-STD-B31v2_2-E1.pdf | `6-STD-B31v2_2-E1.pdf` | `ARIB-STD-B31-v2.2.txt` | implemented (`isdbt-core`, `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy`, `isdbt-mux`) |
| ARIB STD-B32 Fascicle 1 (Part 1) | 3.11-E1 (2018-07-26) | Video Coding, Audio Coding, and Multiplexing Specifications for Digital Broadcasting — Part 1: Video Signal and Coding System | https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-1p3-E1.pdf | `6-STD-B32v3_11-1p3-E1.pdf` | `ARIB-STD-B32-v3.11-part1.txt` | §5.1.1 and Attachment 2 §5.1 used (`isdbt-mux` video) |
| ARIB STD-B32 Fascicle 2 (Part 2) | 3.11-E1 | … Part 2: Audio Signal and Coding System | https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-2p3-E1.pdf | `6-STD-B32v3_11-2p3-E1.pdf` | `ARIB-STD-B32-v3.11-part2.txt` | §5.2.1–5.2.3 header restrictions enforced by `isdbt_mux::adts::b32` and `isdb ts validate` (`aac.*`); channel mapping; Attachment Ch. 3 Table 1 (quality_indicator); Appendix 1 (CRC coverage), Appendix 2 (buffer sizes), Appendix 3 (adts_buffer_fullness meaning); tables checked against PDF pp. 60, 114, 125, 128; see §2.5.1 |
| ARIB STD-B32 Fascicle 3 (Part 3) | 3.11-E1 | … Part 3: Signal Multiplexing System | https://www.arib.or.jp/english/html/overview/doc/6-STD-B32v3_11-3p3-E1.pdf | `6-STD-B32v3_11-3p3-E1.pdf` | `ARIB-STD-B32-v3.11-part3.txt` | §3.1 stream_id and §3.10 (2) stream type identifier table cited in `isdbt-mux` SPEC-NOTES |
| ARIB STD-B21 | 5.12-E2 (2022-10-06) | Receiver for Digital Broadcasting (Desirable Specifications) | https://www.arib.or.jp/english/html/overview/doc/6-STD-B21v5_12-E2.pdf | `6-STD-B21v5_12-E2.pdf` | `ARIB-STD-B21-v5.12.txt` | referenced only (remote-control key, SDTT/CDT origin); no clause implemented |
| ARIB STD-B25 | 6.7-E1 (2019-01-21) | Conditional Access System Specifications for Digital Broadcasting | https://www.arib.or.jp/english/html/overview/doc/6-STD-B25v6_7-E1.pdf | `6-STD-B25v6_7-E1.pdf` | — | referenced only: descriptors 0xCB/0xCC are left `Unknown` (`isdb-si`) |
| ARIB STD-B19 | 1.0-E3 (1998-05-29) | Camera Positioning Information Conveyed by Ancillary Data Packets | https://www.arib.or.jp/english/html/overview/doc/6-STD-B19v1_0E3.pdf | `6-STD-B19v1_0E3.pdf` | — | **not used.** Studio ancillary-data standard (its scope is equipment per STD-B6 / BTA S-005B). The workspace never cites it ("B19" hits in the code are TMCC bit B19) |

SHA-256 of the PDF files used:

```
4a00a13540a20c372e6e1f98b3c65cb2f41f7ac1ac4fab2ac3cdaed5162651c2  T-REC-H.222.0-200605-S.pdf
715693648d0c9d60138b1ebc527cd0ceccf1ff25905480728eb6aac70686f47f  6-STD-B10v5_13-E1.pdf
586deb6816bf1b6312a8155dbf5eb4704938dcd2548c5e0f2eda4afc53a7e50d  6-STD-B19v1_0E3.pdf
98e99cf12bb74209c9b09a1ec0deb70a5f2a1bee8d9236842eb574545e3eb38d  6-STD-B21v5_12-E2.pdf
d3e947e05650beaefd1375d4adcf4168e82c3279d0232f1f679628fc8e58e233  6-STD-B24v6_4-1p3-E1.pdf
b3d9794f4f1859eefd52c72970678acd6c497cbda44a5cbe7cd8dd9a56ed0d7b  6-STD-B25v6_7-E1.pdf
757e156f2c0415378fe4bc8fe7206460d999a23fb50a9b0bb55f5c549cf836b3  6-STD-B31v2_2-E1.pdf
05f3b323f21accefbfce85fc78f7a4457775fce2ba98fd23847b003423348ddc  6-STD-B32v3_11-1p3-E1.pdf
0972d315aab3f1ec0d0036ffcc6d4ab4f6949c136b4177480fa3f0327c53b599  6-STD-B32v3_11-2p3-E1.pdf
c1fd9a88767beca0fa17d1ca15099c495132770f64a09278c5cf8053956f7742  6-STD-B32v3_11-3p3-E1.pdf
```

**Not committed.** The PDFs and their text extractions are copyrighted
(the ARIB translations carry "No part of this document may be reproduced …
without the prior written permission of ARIB"), so neither is in this
repository; `specs/` is gitignored. To get them, download the PDFs from the
URLs above, check them against the hashes, and extract the text:

```sh
PDFS=/path/to/spec-pdfs   # any directory outside the repository
mkdir -p "$PDFS" specs
# download the PDFs into "$PDFS" (URLs above), then compare
(cd "$PDFS" && sha256sum *.pdf)   # against the list above
pdftotext -layout "$PDFS"/T-REC-H.222.0-200605-S.pdf specs/ITU-T-H.222.0-2006.txt
pdftotext -layout "$PDFS"/6-STD-B10v5_13-E1.pdf     specs/ARIB-STD-B10-v5.13.txt
pdftotext -layout "$PDFS"/6-STD-B24v6_4-1p3-E1.pdf  specs/ARIB-STD-B24-v6.4-vol1.txt
pdftotext -layout "$PDFS"/6-STD-B31v2_2-E1.pdf      specs/ARIB-STD-B31-v2.2.txt
pdftotext -layout "$PDFS"/6-STD-B32v3_11-1p3-E1.pdf specs/ARIB-STD-B32-v3.11-part1.txt
pdftotext -layout "$PDFS"/6-STD-B32v3_11-2p3-E1.pdf specs/ARIB-STD-B32-v3.11-part2.txt
pdftotext -layout "$PDFS"/6-STD-B32v3_11-3p3-E1.pdf specs/ARIB-STD-B32-v3.11-part3.txt
pdftotext -layout "$PDFS"/6-STD-B21v5_12-E2.pdf     specs/ARIB-STD-B21-v5.12.txt
```

With poppler `pdftotext` 26.08.0, rerunning the B31 and H.222.0 commands gave
byte-identical output (checked 2026-10-08). Other poppler versions may
change the layout whitespace. Clause and table numbers do not depend on it.

Captures are not committed either: the committed test fixtures are
synthetic (built with this project's encoders), and the tests that read a
real capture take its path from `ISDB_RS_CAPTURE_TS` and skip without it
([docs/test-data.md](docs/test-data.md)).

Newer Japanese editions exist. Each ARIB English page
(`https://www.arib.or.jp/english/std_tr/broadcasting/std-<b10|b21|…>.html`,
fetched 2026-10-08) lists a later Japanese original than the English
translation used here: B10 5.14 (2025-03-25), B21 5.14 (2026-04-16), B24 6.5
(2022-10-06), B25 7.0 (2024-10-29), B31 2.3 (2025-10-30), B32 4.1
(2026-04-16). Each translation says "the original document shall prevail".
The code cites the English editions in the table above.

### 1.2 Referenced but not available locally

| Document | Status / where to get it | Crate decisions that depend on it |
|---|---|---|
| **ARIB TR-B14** v6.9 (2022-10-06), *地上デジタルテレビジョン放送運用規定* (Operational Guidelines for Digital Terrestrial Television Broadcasting), Japanese, 5 fascicles | Sold by ARIB. Only sample excerpts (cover, foreword, table of contents) are free: `https://www.arib.or.jp/image/kikaku/kikaku_sample/sample-tr-b14-{1..5}-6.9.pdf`. All 5 return 200; `-6-` and `-7-` return 404. The sample TOCs show where the relevant rules sit. Fascicle 4 = Part 4, PSI/SI operation: §6.3 running_status, §13 EIT transmission (H-/M-/L-EIT), §15 definition of on-air/off-air service, §21 digital copy control (§21.2 defaults), §22 PSI/SI layer in hierarchical transmission, §30.4 NIT (§30.4.3.3 partial reception, §30.4.3.4 TS information descriptor), §31.1 BIT, §31.2 SDT. Fascicle 5 = Part 7, transmission operation: §5.2.8 PCR operation, §9.2.1 network / TS / remote-control-key / service id assignments; Part 8, content protection: §5.3.2 output control by the digital copy control and content availability descriptors. Fascicle 1 = Part 2, receiver functional spec: §7.10 copy control | `isdb-si`: typed Annex F view of digital_copy_control user_defined ("understood to make this layout normative"). `isdbt-mux`: all 30 "Operational choices", incl. the `ts-program` caption / superimpose component_tag ranges 0x30–0x37 / 0x38–0x3F and the default relay PID plan 0x0111/0x0112/0x0130/0x0138/0x0140 (**from knowledge**), the remote_control_key_id range 1..=12 and the key·10 + index channel numbering (**[TR-B14 practice … from knowledge]**), SI_parameter_descriptor left raw, H-/M-/L-EIT PID-per-layer practice not applied by default, one-seg service_type 0xC0 (`examples/bench*.toml`). `isdb-analyze`: the operational split of EIT 0x58–0x5F is not used; the `si.copy_control` warning text. `isdb-text`: B10 Annex A defers "the details of the character set" to "the operational guidelines of service providers", so the SI initial state (G3 = Katakana) was fixed from live data instead |
| **ARIB TR-B15** v7.4 (BS / wide-band CS operational guidelines), Japanese | Sample excerpts `https://www.arib.or.jp/image/kikaku/kikaku_sample/sample-tr-b15-{1..4}-7.4.pdf` (200; `-5-` 404) | Named next to TR-B14 as the source that would make the Annex F layout normative (`isdb-si/src/values.rs`, `isdbt-mux/src/signalling.rs`, `isdb-analyze/src/validate.rs`) |
| **ITU-T H.262 \| ISO/IEC 13818-2** (code cites the 07/2000 edition) | ITU-T Recommendation, https://www.itu.int/rec/T-REC-H.262 (200, HTML landing page). No local copy | `isdbt-mux/src/video.rs`, marked **[H.262 from knowledge]**: start codes, sequence/picture headers and extensions (§6.2.x), display duration §6.3.10, Table 6-3 aspect_ratio_information (→ component_type column), Table 8-13 Rmax per level (T-STD), VUI default transfer_characteristics '00' (video_decode_control) |
| **ISO/IEC 13818-7** (AAC, ADTS) | ISO catalogue (paid). `https://www.iso.org/standard/43345.html` answered 403 to `curl` (bot filter), so this URL is not verified | `isdbt-mux/src/adts/`, marked **[from knowledge]**: adts_fixed/variable_header field widths (§6.2.1, Tables 6.4/6.5), ID '1', the bit-reservoir recurrence and /(32·NCC) scaling of adts_buffer_fullness; the CRC-16 itself is ISO/IEC 11172-3 §2.4.3.1 (also not local; its coverage is quoted in B32 Part 2 Appendix 1). Header values and the fullness recurrence were confirmed on the one-seg audio of a live capture. **Not implemented:** the raw_data_block syntax and the Annex A Huffman / scalefactor-band tables, so crc_check values are neither verified nor inserted. Implementing them needs 13818-7 obtained legally and the tables transcribed independently from the standard, not from another implementation ([`isdbt-mux` SPEC-NOTES "AAC ADTS: raw_data_block not parsed"](crates/isdbt-mux/SPEC-NOTES.md#aac-adts-raw_data_block-not-parsed)) |
| **JIS X 0208:1990, JIS X 0213:2004, JIS X 0221-1:2001, JIS X 0201-1997** | JIS standards (JSA / JISC). No local copy | `isdb-text`: 7 Kanji cells corrected away from the WHATWG/`encoding_rs` index (`tables::KANJI_OVERRIDES`: 1-29, 1-33, 1-34, 1-61, 1-81, 1-82, 2-44), "from general knowledge … (status: pending an authoritative JIS table)". JIS compatible Kanji Plane 1/2 beyond rows 1–84 is unsupported ("incomplete by design") |
| **ARIB STD-B24 Volumes 2–4** (Fascicles 2(1/2), 2(2/2), 3 of 6.4-E1: XML-based multimedia coding (BML), data transmission, application control) | Fascicle 3 at https://www.arib.or.jp/english/html/overview/doc/6-STD-B24v6_4-3p3-E1.pdf (200). The guessed Fascicle 2 name `…-2p3-E1.pdf` returned 404 (the real file name was not looked up) | None. Data broadcasting / DSM-CC carousels are not implemented. `isdb-analyze` only names DSM-CC table_ids and stream_types |
| **ARIB STD-B1** | Not local | Cited by B10 footnotes as the origin of SDTT (0xC3), CDT (0xC8) and the download content descriptor (0xC9). These stay `Unknown` in `isdb-si` |
| **ISO/IEC 10646:2014 Annex B; Unicode 5.2+ / 16.0 names** | Public | `isdb-text`: combining-character order (B24 Annex E.4 refers to ISO/IEC 10646 Annex B); additional-symbol PUA → Unicode ARIB symbols, "names checked with Python `unicodedata` 16.0" |
| **ISO/IEC 2022** | Public | `isdb-text`: unknown escape sequences are skipped by ISO/IEC 2022 shape |
| **Radio Act ordinance (UHF channel plan)** | Japanese law, not a broadcast spec | `isdbt-mux` NIT frequency code and `isdb-cli` `phyargs::channel_hz`: centre = 473 + 6·(ch − 13) + 1/7 MHz (**[not in the local specs]**) |

The SPEC-NOTES statements that depend on these documents. Search terms used:
"not available", "knowledge", "TR-B14", "pending".

| Statement | File |
|---|---|
| "not available locally" (ITU-T H.262, ISO/IEC 13818-7, ARIB TR-B14 / TR-B15) and every **[from knowledge]** tag | [crates/isdbt-mux/SPEC-NOTES.md](crates/isdbt-mux/SPEC-NOTES.md) (header, video, AAC, T-STD, PMT/EIT descriptor sections) |
| Operational choices 1–30 "not dictated by the local specs — test on receivers" | [crates/isdbt-mux/SPEC-NOTES.md](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers) |
| Annex F layout "understood" normative per TR-B14/TR-B15 | [crates/isdb-si/SPEC-NOTES.md](crates/isdb-si/SPEC-NOTES.md#isdb-si-digital_copy_control_descriptor-4-bit-field-after-the-flags) |
| SDTT/CDT/0xC9/0xCB/0xCC "To be typed from B21/B25 when those are admitted" | [crates/isdb-si/SPEC-NOTES.md](crates/isdb-si/SPEC-NOTES.md#isdb-si-descriptors-and-tables-defined-outside-b10) |
| "The operational guideline is not available here" (SI initial state) | [crates/isdb-text/SPEC-NOTES.md](crates/isdb-text/SPEC-NOTES.md#isdb-text-initial-state-for-si-strings) |
| JIS X 0221 / X 0213 "not available locally … from general knowledge" | [crates/isdb-text/SPEC-NOTES.md](crates/isdb-text/SPEC-NOTES.md#isdb-text-kanji-set-rows-184--jis-x-0208-unicode-mapping) |
| "The ARIB operational split of 0x58–0x5F … is not in B10 and is not used" | [crates/isdb-analyze/SPEC-NOTES.md](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-eit-within-8-days-table_ids) |

---

## 2. Coverage matrix

How each clause is confirmed. Each row carries the strongest level that
applies, and lower levels are listed when they are what actually covers the
clause.

| Code | Meaning |
|---|---|
| **S** | spec-vector or unit tests only (values printed in the spec asserted in `spec_*` / unit tests) |
| **P** | property tests (`prop_*`: lossless `encode(decode(x)) == x`, TX/RX inverse pairs) |
| **L** | decoded with zero errors and re-encoded byte-identically on a live terrestrial capture (120,091 packets, 10.777 s). This is a development result: the capture is not distributed, so the figures quoted with **L** in this file cannot be reproduced from the repository. The same opt-in tests run on your own capture through the `ISDB_RS_*` environment variables (`ISDB_RS_CAPTURE_TS`, …; [docs/test-data.md](docs/test-data.md)) and print the corresponding figures for it |
| **O** | gr-isdbt oracle, byte-exact in both directions ([VERIFICATION.md](VERIFICATION.md), 8 parameter sets / 9 runs) |
| **RX** | a real receiver accepted the Rust output over a conducted link (HackRF → coax → attenuators → receiver), configuration **C_rx**: `isdb tx` with Mode 3, GI 1/8, partial reception; layer A 1 seg QPSK 2/3 I=2; layer B 12 seg 64QAM 3/4 I=2; multiplex from `isdbt-mux`. MyGica S270 USB tuner: its 12.0 s capture validates with `isdb ts validate`: 0 errors, 5 warnings. A Sony BDZ-series recorder ("the recorder" below): registered the service in a channel scan and played it, with digital copy control copy_control_type '01' (§3.4). RX shows only that the whole chain was accepted in C_rx; it does not test each clause separately ([VERIFICATION.md](VERIFICATION.md#real-receivers)) |
| **RX(gr)** | a real receiver accepted *gr-isdbt* output that the Rust chain matches byte-exactly (S270, PER 0) |
| **—** | not implemented, or deliberately left raw |

### 2.1 ITU-T H.222.0 (05/2006)

| Clause / table | Implementation | Tests | Status |
|---|---|---|---|
| §2.4.3.2 Table 2-2 packet header; Table 2-3 PID ranges; Table 2-5 adaptation_field_control | `isdb_ts::packet` | `spec_table2_2_header_layout`, `spec_table2_5_reserved_afc_and_null_packet`, `view_matches_decode`, `prop_packet_roundtrip`, `prop_packet_bytes_lossless`, `capture_round_trips` (opt-in) | L, RX |
| §2.4.3.4–2.4.3.5 Table 2-6 adaptation field (incl. extension, stuffing) | `isdb_ts::packet` | `spec_table2_6_full_adaptation_field`, `spec_2_4_3_5_adaptation_field_length_rules` | L |
| §2.4.3.3 continuity_counter, duplicate packets; §2.4.3.5 discontinuity_indicator | `isdb_ts::cc`, `isdb_ts::demux` | `spec_2_4_3_3_counter_increment_rules`, `spec_2_4_3_3_duplicates_and_breaks`, `spec_2_4_3_3_duplicate_pcr_exception`, `spec_2_4_3_5_discontinuity_and_null_exempt`, `spec_2_4_3_3_cc_break_discards_partial`, `tei_discards_partial` | AF-only CC behaviour: L. Duplicates and discontinuity_indicator: S only (none in the capture; SPEC-NOTES "pending") |
| §2.4.2.2 eq. 2-1…2-3 PCR base/extension; PCR PIDs | `isdb_ts::pcr`, `isdb_ts::packet::check` | `spec_eq2_1_pcr_formula_and_layout`, `wrap_helpers` | L (232 PCRs) |
| §2.4.3.6–2.4.3.7 Tables 2-21…2-27 PES (PTS/DTS, optional fields, trick mode, extension) | `isdb_ts::pes` | `spec_table2_21_pts_dts_prefixes`, `spec_table2_21_optional_fields`, `spec_table2_21_pts_bit_layout`, `spec_2_4_3_7_length_zero_and_stream_id_classes`, `prop_pes_roundtrip`, `prop_pes_bytes_lossless`, `prop_pes_packetize_assemble`, `packetize_and_reassemble` | L (820 PES). PES_extension_flag_2 path: S only (pending) |
| §2.4.4, §2.4.4.1–2.4.4.2 sections, pointer_field, stuffing | `isdb_ts::demux`, `isdb_ts::packetize`, `isdb_ts::section` | `spec_2_4_4_section_spanning_three_packets`, `spec_2_4_4_two_sections_in_one_packet_then_stuffing`, `spec_2_4_4_2_pointer_field_completes_previous_section`, `spec_2_4_4_2_join_mid_stream_waits_for_pusi`, `section_header_split_across_packets`, `pointer_mismatch_discards`, `prop_section_packetize_assemble`, `long_roundtrip_preserves_reserved_bits` | L (825 sections, no assembler events), RX |
| Annex A CRC_32 | `isdb_ts::crc` | `check_value`, `residue_is_zero`, `table_matches_bitwise`, `crc_error_detected` | L (823 long sections; all 451 PMTs, despite the "Annex B" typo in §2.4.4.9) |
| §2.4.4.3–2.4.4.9 Tables 2-30/2-32/2-33 PAT, CAT, PMT | `isdb_ts::psi` | `spec_table2_30_pat_layout`, `spec_table2_32_cat_reserved18`, `spec_table2_33_pmt_layout`, `prop_pat_roundtrip`, `prop_cat_roundtrip`, `prop_pmt_roundtrip`, `prop_psi_body_bytes_lossless` | L, RX |
| §2.6 descriptor format, Table 2-45 tag ranges | `isdb_ts::descriptor` | `spec_2_6_descriptor_format_and_table2_45_ranges` | S |
| §2.4.3 Table 2-1 framing; 192/204-byte units (heuristic, not H.222.0) | `isdb_ts::reader` | `autodetects_each_format_with_garbage_prefix`, `resyncs_after_corruption`, `short_and_empty_inputs` | L |
| §2.1.64 reserved bits = '1' | `check` functions in `isdb_ts`, `isdb_si::conformance`, `isdb_analyze::validate` | `spec_conformance_reserved_bits`, `reserved_bits_are_warnings` | L (no violations in the live PSI) |
| §2.7.2 PCR interval ≤ 0.1 s | `isdb_analyze::validate`; generation in `isdbt_mux::mux` | `pcr_interval_over_100ms`; `pcr_and_timestamps` (mux) | Validator: L. It flags the live one-seg program, see §3.3. Mux: S, RX |
| §2.4.2.2 PCR tolerance ±500 ns; eq. 2-4 byte timing | `isdb_analyze::validate`, `isdb_analyze::timebase`; `isdbt_mux::pcrclock` (packet arrival times of a recorded TS: `isdb tx --mux-ts` pacing, `ts-program` relay re-timing) | `interpolates_between_pcrs_and_inverts`; `spec_eq_2_4_interpolation`, `wrap_is_continuous_and_anchor_maps_timestamps`, `discontinuity_continues_at_previous_rate`, `relays_a_foreign_program_as_a_terrestrial_service` | estimate (warning only); relay: S, plus the opt-in `relay_recording` test on a recording you supply (`ISDB_RS_RELAY_TS`) |
| §2.7.4 PTS spacing ≤ 0.7 s; §2.4.2.6 T-STD delay ≤ 1 s | `isdb_analyze::validate`; `isdbt_mux::tstd` | `pts_interval_over_700ms`; `tb_limits_bursts`, `underflow_detected`, `tables_conform_and_repeat` | S (mux), RX |
| §2.4.2.3–2.4.2.6 T-STD buffers (TBn/MBn/EBn/Bn) | `isdbt_mux::tstd` | as above; `media_ffprobe` (opt-in) | S, RX. TBsys/Bsys for PSI are **not modelled** |
| §2.1.1 access unit | `isdbt_mux::video` | `ibbp_timestamps` | S, RX |
| Table 2-34 stream_type | `isdb_analyze::names`, `isdbt_mux::config` | — | used as a lookup table |
| Program Stream (pack_header Table 2-39), TSDT, IPMP/metadata syntax | `pack_header()` kept raw only | — | — |

### 2.2 ARIB STD-B10 v5.13

| Clause / table | Implementation | Tests | Status |
|---|---|---|---|
| Part 2 §5.2.4–§5.2.15, Tables 5-3…5-17: NIT, BAT, SDT, EIT, TDT, TOT, RST, ST, PCAT, BIT, NBIT, LDT, stuffing | `isdb_si::tables`, `isdb_si::any` | `spec_nit`, `spec_bat`, `spec_sdt`, `spec_eit`, `spec_eit_undefined_times`, `spec_tdt`, `spec_tot`, `spec_rst`, `spec_st`, `spec_pcat`, `spec_bit`, `spec_nbit`, `spec_ldt`, `spec_dispatch_errors`, `prop_<table>_roundtrip` (12 tables), `prop_table_decode_is_lossless`, `synthetic_sections_crc_decode_reencode`, `capture_si_sections_crc_decode_reencode` (opt-in) | L for the tables present live: NIT actual 0x40, SDT actual 0x42, EIT p/f 0x4E (PIDs 0x0012 and 0x0027), EIT schedule 0x50/0x51/0x58/0x59, TOT 0x73, BIT 0xC4 (capture inventory; the synthetic fixture covers the same set). S+P only for BAT, TDT, RST, ST, PCAT (content_descriptor_length scope pending), NBIT, LDT. RX for the tables `isdbt-mux` sends (PAT/CAT/PMT/NIT/SDT/EIT/TOT/BIT) |
| Part 2 §6.2.1–§6.2.19, §6.2.25 (`descriptor::dvb`); §6.2.20–§6.2.45, §6.2.54 (`descriptor::arib`) | `isdb_si::descriptor` | `spec_descriptor_*` (language codes, digital_copy_control, ts_information, logo_transmission, event_group, component_group, ca, emergency, rejects_unaccounted_bytes), `prop_descriptor_roundtrip`, `prop_descriptor_decode_is_lossless`, `prop_strategies_cover_every_typed_descriptor` | L for the descriptors that occur in the capture: 0x09, 0x40, 0x41, 0x48, 0x4D, 0x4E, 0x50, 0x52, 0x54, 0xC1, 0xC4, 0xC7, 0xC8, 0xCD, 0xCE, 0xCF, 0xD6, 0xD7, 0xDE, 0xF6, 0xFA, 0xFB, 0xFD, 0xFE (`isdb ts inspect --decode` of the capture). S+P for the rest |
| Tables 6-5, 6-39, 6-43, 6-45, 6-60, 6-60-1; Annex F Tables F-1/F-2/F-4 (typed values) | `isdb_si::values`, `isdbt_mux::signalling` | `spec_table_6_5_component_type_mpeg2_video`, `spec_table_6_39_digital_recording_control`, `spec_table_6_43_audio_component_type`, `spec_table_6_45_sampling_rate`, `spec_table_6_60_video_encode_format`, `spec_table_6_60_1_transfer_characteristics`, `spec_annex_f_tables_f2_f4`, `spec_6_2_23_maximum_bitrate_unit`, `every_code_round_trips_and_names_are_unique`, `synthetic_pmt_descriptors_decode_to_named_variants`, `copy_control_user_defined` | L. **RX: Annex F copy_control_type is decisive on the recorder** (§3.4) |
| Annex C MJD/BCD, JST | `isdb_si::time`, `isdbt_mux::jst` | `spec_time_annex_c_example`, `spec_time_jst_example`, `spec_time_duration_example`, `spec_time_bcd`, `spec_time_annex_c_range_exhaustive`, `spec_annex_c_example`, `duration_example`, `mjd_time_rendered_jst` | S (exhaustive over 1900-03-01…2100-02-28), L |
| §5.1.1 section lengths; §5.1.3 Table 5-2 transmission level and repetition; §5.1.4 "4KB in 32 ms" | `isdb_si::conformance`, `isdb_analyze::validate`, `isdbt_mux::mux` | `psi_section_too_long`, `mandatory_sdt_missing`, `frequency_criterion_exceeded_is_warning`, `tables_conform_and_repeat`, `schedule_tdt_pcr_in_video_custom_intervals` | L (validator), S+RX (mux) |
| Part 1 / Part 2 Table 5-1 PID allocation | `isdb_analyze::validate`, `isdb_analyze::names` | `table_on_wrong_pid`, `es_on_allocated_si_pid` | L |
| §6.1 Table 6-1 mandatory descriptors | `isdb_si::conformance`, `isdb_analyze::validate` | `si_mandatory_descriptor` | L. The live capture fails it in 217 EIT events, see §3.3 |
| §5.2.4/§5.2.6/§5.2.7 cross-table identities; Appendix §1.4 EIT p/f and schedule guideline | `isdb_analyze::validate`; `isdbt_mux::si` | `sdt_tsid_and_onid_mismatch`, `pat_program_absent_from_sdt`, `eit_pf_two_events_in_present_section`, `eit_present_following` | L, S, RX |
| §6.2.21 system management, §6.2.31 Tables 6-62/6-63 terrestrial delivery (GI / mode codes, 1/7 MHz unit), §6.2.32 partial reception, §6.2.42 TS information / remote_control_key_id, Annex N network_id | `isdbt_mux::si`, `isdbt_mux::signalling` | `spec_b10_table_6_62_6_63`, `derived_nit_values`, `frequency_codes`, `remote_key_range`, `remote_key_range_is_validated` | S, RX |
| §6.2.30 video_decode_control, §6.2.3 component, §6.2.26 audio component (derived from media) | `isdbt_mux::signalling`, `isdbt_mux::validate` | `vdc_resolution`, `bench_toml_redecodes_to_live_values`, `bench_minimal_toml_redecodes_to_derived_values` | S, RX (the recorder plays with both the derived and the live values) |
| §6.2.22 hierarchical_transmission | `isdbt_mux::si` | `hierarchical_descriptor_derived` | S |
| SDTT 0xC3, CDT 0xC8, download content 0xC9, CA contract 0xCB, CA service 0xCC (syntax in B1/B21/B25) | kept as `AnyTable::Unknown` / `Descriptor::Unknown` (CRC checked) | — | — (deliberately deferred). The live CAT carries a 0xCC that decodes as `Unknown` |

### 2.3 ARIB STD-B24 v6.4 Vol. 1 (Part 2 Chapter 7; Part 3 Table 8-2)

| Clause / table | Implementation | Tests | Status |
|---|---|---|---|
| §7.1, Tables 7-1/7-2/7-3 invocation, designation, final bytes | `isdb_text::decode`, `isdb_text::sets` | `spec_table_7_1_invocations`, `spec_table_7_2_designations`, `spec_table_7_2_designations_decode`, `spec_table_7_3_final_bytes`, `prop_decode_arbitrary_bytes`, `prop_decode_structured_bytes` | L (every distinct NIT/SDT/EIT string of the development capture: zero issues, round-trip) |
| Part 3 Table 8-2 initial state; SI variant with G3 = Katakana | `isdb_text::sets::InitialState` | `spec_table_8_2_initial_state`, `prop_round_trip_si`, `prop_round_trip_caption_initial_state` | L (the SI variant is fixed by live data) |
| Table 7-4 Kanji (JIS X 0208 rows 1–84, Unicode mapping) | `isdb_text::tables` | `spec_table_7_4_kanji`, `prop_kana_kanji_compact`, `prop_repertoire_size` | L. The 7 override cells are pending a JIS table |
| Tables 7-5…7-9 alphanumeric, katakana, hiragana, mosaic, JIS X 0201 katakana | `isdb_text::sets`, `isdb_text::tables` | `spec_table_7_5_alphanumeric`, `spec_table_7_6_katakana`, `spec_table_7_7_hiragana`, `spec_table_7_8_mosaic_ignored`, `spec_table_7_9_jis_x0201_katakana` | L (alphanumeric, kana); S (mosaic is ignored by design, Annex E.4) |
| Tables 7-10/7-11/7-19 additional symbols | `isdb_text::addsym` | `spec_table_7_19_additional_symbols`, `spec_table_7_11_additional_kanji` | L ([字][デ][新][解][ほか]). 38 cells keep their PUA value |
| Tables 7-14…7-17 C0/C1/CSI control codes, character size (MSZ/NSZ width) | `isdb_text::control`, `isdb_text::decode` | `spec_table_7_15_c0_controls`, `spec_table_7_16_c1_controls`, `spec_table_7_16_rpc`, `spec_table_7_16_character_size_width`, `spec_table_7_17_csi`, `prop_round_trip_annex_e_width` | L (MSZ/NSZ, APR) |
| Table 7-18 default macros | `isdb_text::decode` | `spec_table_7_18_default_macros`, `spec_macro_expansion_bounded` | S |
| Annex E UCS mapping; Table E-1 non-spacing characters | `isdb_text::tables`, `isdb_text::encode` | `spec_table_e_1_non_spacing`, `prop_combining_needs_base`, `prop_unencodable_reported` | S |
| DRCS (§7.1.1.5, Annex E.5) | U+FFFD + `IssueKind::Drcs` | `spec_drcs_placeholder` | — (no mapping by design) |
| JIS compatible Kanji Plane 1 beyond rows 1–84, Plane 2 | `IssueKind::UnsupportedCode` | — | — (no JIS X 0213 table) |
| Robustness on truncated escape / control sequences | `isdb_text::decode` | `spec_truncated_sequences` | S |
| Part 1 §3.1 / Part 3 §3.1 definitions (superimpose; synchronous / asynchronous PES), Part 3 Ch. 4 (caption vs superimpose "not classified" in coding), §9.6 footnote 1 (data_component_id 0x0008), Tables 9-15/9-16 (additional_arib_caption_info Timing) | `isdbt_mux::relay::classify` (roles of relayed `ts-program` streams) | `classification_rules`, `roles_and_pid_plan_on_permuted_pids`, `relay_recording_without_toml` (opt-in, `ISDB_RS_RELAY_TS`) | S; during development also on a real recording (caption and superimpose PIDs classified as expected) |

### 2.4 ARIB STD-B31 v2.2

| Clause / table | Implementation | Tests | Status |
|---|---|---|---|
| Tables 3-1, 3-2, 3-3, 3-5, 3-6 (segment/carrier parameters, TI lengths, TSPs per frame, frame lengths) | `isdbt_core::params`, `isdbt_core::ofdm_params`, `isdbt_core::fec_params` | `table_3_2`, `table_3_3`, `table_3_5`, `frame_lengths`, `spec_table_3_1_counts`, `segment_ordering_rules`, `coded_bits_consistent_with_rate` | S, O, RX |
| §3.2 TS re-multiplexing, model receiver (Figs. 3-3/3-4/3-5) | `isdbt_fec::remux::MultiplexPattern`; pacing in `isdbt_mux::mux` | `spec_fig_3_3_and_3_5_example`, `spec_table_3_5_3_3_single_layer`, `spec_table_3_5_3_3_multi_layer`, `assemble_divide_roundtrip` | S. Fig. 3-3 is only "partially confirmed" (the figure is schematic). The real slot order is "pending real-world check". The oracle runs feed per-layer TS, so they do not exercise it. RX (C_rx pacing accepted) |
| §3.3 RS(204,188) shortened code (Fig. 3-6) | `isdbt_fec::rs` | `spec_3_3_primitive_element`, `spec_3_3_generator_roots`, `spec_3_3_shortening_equivalence`, `spec_3_3_correct_up_to_8`, `spec_3_3_failure_beyond_8`, `prop_rs_corrects_up_to_8` | O, RX |
| §3.4 layer division, one-byte frame shift (Fig. 3-7) | `isdbt_fec::layer` | `tx_slot_provenance`, `prop_layer_roundtrip_*` | O |
| §3.5 energy dispersal (Fig. 3-8 PRBS, OFDM-frame reset) | `isdbt_fec::energy` | `spec_fig_3_8_first_bits`, `spec_fig_3_8_period`, `spec_3_5_sync_skipped_register_advances` | O, RX |
| §3.6 delay adjustment, Table 3-7 | `isdbt_fec::delay`, `isdbt_core::fec_params` | `spec_table_3_7_delay_adjustment`, `delay_is_exact_slots`, `one_frame_latency_small_layer` | O |
| §3.7 byte interleaving (Fig. 3-9) | `isdbt_fec::byte_interleave` | `spec_fig_3_9_path_delays`, `spec_fig_3_9_memory_sizes`, `pair_latency`, `prop_byte_interleave_identity` | O, RX |
| §3.8 convolutional code (Fig. 3-10), puncturing (Table 3-8), Viterbi | `isdbt_fec::conv` | `spec_fig_3_10_impulse_response`, `spec_table_3_8_puncturing`, `spec_table_3_8_impulse_3_4`, `prop_viterbi_noiseless_all_rates`, `prop_viterbi_noiseless`, `viterbi_awgn_improves_ber` | O (all 5 rates), RX (2/3, 3/4) |
| §3.9 bit interleaving and mapping (Figs. 3-12…3-19, Tables 3-9/3-11) — QPSK, 16QAM, 64QAM | `isdbt_ofdm::mapping`, `isdbt_ofdm::delay` | `spec_fig_3_15_qpsk`, `spec_fig_3_17_16qam`, `spec_fig_3_19_64qam`, `spec_table_3_11_unit_power`, `spec_table_3_9_total_two_symbols`, `demap_signs_match_labels`, `prop_map_demap_hard`, `prop_bit_interleave_identity` | O (carrier-identical), RX (QPSK, 64QAM) |
| §3.9.3.1 π/4-shift DQPSK (Fig. 3-13, Table 3-10) | `isdbt_ofdm::mapping`; differential detection in `isdbt_ofdm::rx` | `spec_table_3_10_dqpsk`, `loopback_mode3_all_differential`, `loopback_mode1_three_layers_mid_frame_start` | **S + Rust loopback only.** gr-isdbt has no DQPSK. Pending an off-air capture |
| §3.10 layer combining (Figs. 3-20/3-21) | `isdbt_ofdm::combine` | `prop_noiseless_loopback` | O |
| §3.11.1 time interleaving (Fig. 3-23, Table 3-12) | `isdbt_ofdm::time_interleave` | `spec_table_3_12_frame_multiple`, `prop_time_interleave_identity` | O (I tested: M3 0/1/2/4, M2 0/2/4/8, M1 8/16; Mode 1 I = 0/4 untested). gr-isdbt deviates for I = 0 (D4) |
| §3.11.2 frequency interleaving (Figs. 3-26/3-27/3-28, Table 3-13) | `isdbt_ofdm::freq_interleave`, `isdbt_ofdm::tables` | `spec_table_3_13_permutations`, `spec_fig_3_26_3_27_frequency_interleave`, `spec_fig_3_28b_randomize_direction`, `prop_freq_interleave_identity` | O. Rotation numbering confirmed for coherent groups, RX(gr). The DQPSK group is not covered |
| §3.12 frame structure, SP/CP positions (Figs. 3-29/3-30, Tables 3-14/3-15) | `isdbt_ofdm::frame` | `spec_fig_3_29_3_30_segment_structure`, `spec_tables_3_14_3_15`, `spec_table_3_1_carrier_counts` | O (SP/CP/edge carriers identical) |
| §3.13 pilot PRBS and amplitudes (Fig. 3-31, Tables 3-16/3-17/3-19) | `isdbt_ofdm::frame` | `spec_table_3_16_prbs_initial_values` | O |
| §3.14 spectrum arrangement (Fig. 3-32), §3.14.1 Kc, §3.14.2 guard interval | `isdbt_ofdm::ofdm`, `isdbt_ofdm::frame` | `spec_fig_3_32_positions`, `spec_3_14_1_center_carrier`, `spec_frame_sample_counts` | O, RX |
| §3.15 TMCC (Tables 3-20…3-31) | `isdbt_ofdm::tmcc` | `spec_table_3_21_sync_alternation`, `spec_table_3_22_tmcc_fields`, `prop_tmcc_roundtrip` | O for B20–B66 and B68–B121. The differences in B17–B19 and B67 are gr-isdbt deviations (D1/D3). **RX: the S270 and the recorder accepted the conformant Rust TMCC in C_rx** (where gr-isdbt deviates, D1/D3) |
| §3.15.6.9 difference-set code (184,102) | `isdbt_ofdm::dsc` | `spec_3_15_6_9_generator_divides`, `spec_3_15_6_9_parity_vectors`, `difference_set_is_perfect` | Agrees with gr-isdbt in both directions. The bit order is ambiguous in the text, so it is **pending an off-air frame**. Whether consumer receivers check the parity is unknown |
| §3.13.4 AC carriers (DBPSK, stuffing 1); §3.16 seismic warning; Attachment §6.2.3.2/§6.3.4.2 serial AC mapping | `isdbt_ofdm::ac` | `spec_3_16_6_seismic_word`, `prop_seismic_roundtrip`, `prop_ac_stream_roundtrip`, `loopback_ac_seismic_and_emergency_flag` | **S + loopback only.** gr-isdbt leaves AC unmodulated (D2). AC parity uses the same bit-order convention as TMCC (pending) |
| §3.15.6.2/3.15.6.4 emergency flag, transmission-parameter switching countdown | `isdbt_ofdm::tmcc`, `isdbt_ofdm::rx` | `loopback_parameter_switch_reacquires`, `loopback_ac_seismic_and_emergency_flag` | loopback only (gr-isdbt has neither) |
| Receiver synchronisation and equalisation (§3.12.1–3.12.2, §3.13) | `isdbt_ofdm::rx::{acquire, chanest, mod}` | `loopback_*` (30 dB SNR, CFO 2.3 carriers, clock offset, long echo), `acquisition_helpers`, `ts_loopback_hierarchical_mode3`, `pipelined_receiver_matches_inline`, `loopback_passes_with_impairments` (CLI) | O (gr→Rust, noise-free). Impairments tested Rust→Rust only |
| Part 2 Figs. 3-8/3-9 two-symbol frame alignment | `isdbt_ofdm::latency`, `isdbt_ofdm::modulator` | `prop_bit_frame_alignment`, `modulator_alignment_option`, `papr_first_frames` | O, RX(gr), RX |
| Part 2 §3.2.1 Tables 3-1/3-3 PSI/SI layer placement; Table 3-2 robustness rank | `isdbt_mux::si` | `spec_b31_part2_table_3_2` | S, RX |
| Part 2 §3.2.4 Table 3-5 PCR packets per frame on the partial-reception layer | `isdbt_mux::mux` | `pcr_and_timestamps` | S, RX |
| Part 2 §3.2.2 duplicate packets for a PID carried on two layers | — (configuration rejected) | — | — (gap, `isdbt-mux` SPEC-NOTES "Layers of PSI/SI") |
| Part 2 Ch. 5 §5.5 broadcast-TS additional information (dummy-byte part, IIP ISDB-T_Information_Packet); Ch. 4 SFN synchronisation | — | — | — (no code cites or implements them) |

### 2.5 ARIB STD-B32 v3.11 Part 1

| Clause | Implementation | Tests | Status |
|---|---|---|---|
| §5.1.1 "vbv_delay shall always be set to 0xFFFF"; one frame per video PES; PTS in every PES | `isdbt_mux::video` (`force_vbv_delay_ffff`), `isdbt_mux::mux` | `vbv_rewrite`, `pcr_and_timestamps` ("PTS in every PES (B32 Part 1 §5.1.1)"), `media_ffprobe` (opt-in) | S, RX |
| Attachment 2 §5.1 field pairs per access unit; PTS_DTS_flags | `isdbt_mux::video` | `ibbp_timestamps` | S, RX |
| One-seg H.264 (stream_type 0x1B) and its PES packetisation | `StreamKind::Declared` (listed in the PMT, no packets) | — | — **not packetised**. `isdb ts validate` reports `pid.missing` on the one-seg ES PIDs (C_rx capture) |

### 2.5.1 ARIB STD-B32 v3.11 Part 2 (MPEG-2 AAC)

| Clause | Implementation | Tests | Status |
|---|---|---|---|
| §5.2.1 ADTS, LC profile, ≤ 5.1 channels; §5.2.2 (1) protection_absent '0', profile 1, sampling_frequency_index 0x3–0x8 (terrestrial TV); §5.2.2 (2) adts_buffer_fullness ≠ 0x7FF, one raw_data_block (header-level checks only; §5.2.2 (3) coupling channel, §5.2.3 (2) element order, §5.2.3 (3)(d) PCE fields and §5.2.3 (4)(a) SBR need a raw_data_block parser and are **not checked**) | `isdbt_mux::adts::b32` (`check_frame`, `conform`; `adts_compliance` policy), `isdb_analyze::aac` (`aac.*` findings, header rules parsed independently) | `each_rule_detected`, `conform_passes_conforming_input_unchanged`, `conform_refuses_unprotected_input`, `conform_refuses_protected_input_needing_a_header_rewrite`, `conform_refuses_unfixable_and_warn_passes`, `media_audio_gate_follows_header_report` (opt-in), `aac_adts_rules`, `media_aac_gate_matches_b32_part2_report` (opt-in) | S, L (the unscrambled one-seg audio of the live capture passes the header rules); **RX pending** for conforming audio (the recorder played a non-conforming test file: protection_absent '1', adts_buffer_fullness 0x7FF) |
| Appendix 1 ADTS CRC coverage (ISO/IEC 11172-3 CRC-16) | `isdbt_mux::audio::SilentFrame` only (layout known by construction); the crc_check of arbitrary input is neither verified nor rewritten (raw_data_block not parsed) | `crc16_known_answer`, `silent_frame_crc_matches_appendix_1` | S. No in-tree test checks the coverage reading on third-party data |
| Appendix 3 adts_buffer_fullness ("6,144 bits per channel"), Description 1 (3) fixed average rate | `isdbt_mux::adts::b32::cbr_fullness` | `fullness_model_recurrence`, `silent_frame_fullness_is_the_cbr_value` | L (the one-seg audio encoder of the live capture followed the model); R > mean for VBR input is an interpretation; computed values are reported, not inserted (a protected frame's header cannot be rewritten without recomputing crc_check, which needs a raw_data_block parser) |
| Appendix 2 AAC buffer sizes (Rxn/BSn = H.222.0 §2.4.2.3) | `isdbt_mux::audio::AdtsEs::{rx, bs}`, T-STD | `media_ffprobe` (opt-in), `pcr_and_timestamps` | S, RX |
| Attachment Ch. 3 Table 1 quality_indicator modes | `QualityMode::derive` (`audio_quality_indicator = "derived"` default), `QualityMode::contradiction` (`aac.quality_indicator`) | `quality_derivation_table_1`, `audio_quality_indicator_typed`, `aac_quality_indicator_consistency`, `capture_validate_findings` (opt-in) | S, L (the live quality_indicator '10' is consistent with Table 1); the Mode 1 bitrate range is ambiguous (SPEC-NOTES) |
| §5.2.3 (3)(a)–(c) PCE on audio-mode switches / every < 550 ms with downmix; §5.2.1 maximum bit rate "Compliant to ISO/IEC 13818-7"; §5.2.4 downmix | — | — | — (constant-mode files; 13818-7 limit not local) |

### 2.6 STD-B21, STD-B25, STD-B19

No clause is implemented. B21 §4.4.16/§5.2.17 ("No stipulations are made for
the shape or keys of the remote controller") is cited for the
remote_control_key_id range. B21/B25 are the deferred sources for SDTT, CDT and
the CA descriptors. B19 is unrelated (§1.1).

---

## 3. Interpretations and deviations index

Status words are those of the entry itself. *confirmed* means confirmed by a
second statement of the spec, by live data or by the oracle. *pending* means it
still needs a real-world check. *interpretation*, *convention* and *choice*
have no external confirmation, because none is possible from the local texts.

### 3.1 SPEC-NOTES entries

| Crate | Entry | Status |
|---|---|---|
| (workspace) | [OFDM frame alignment (B31 Part 2 Figs. 3-8/3-9)](SPEC-NOTES.md#cross-crate-decision-ofdm-frame-alignment-b31-part-2-figs-3-83-9) | confirmed via oracle + S270(gr); off-air desirable |
| isdb-ts | [Table numbering of the 2006 edition](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-table-numbering-of-the-2006-edition) | confirmed |
| isdb-ts | [PES_extension_field_length](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pes_extension_field_length-covers-the-stream_id_extension-byte) | **pending** (no PES_extension_flag_2 in capture) |
| isdb-ts | [adaptation_field_extension_length = 0](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-adaptation_field_extension_length--0) | confirmed (syntax) |
| isdb-ts | [Bytes not described by the packet syntax](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-bytes-not-described-by-the-packet-syntax) | confirmed (structural) |
| isdb-ts | [CC of adaptation-only packets](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-continuity_counter-of-adaptation-only-packets) | confirmed on live data |
| isdb-ts | [Duplicate packets](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-duplicate-packets) | **pending** |
| isdb-ts | [discontinuity_indicator in the reassemblers](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-discontinuity_indicator-in-the-reassemblers) | **pending** |
| isdb-ts | [pointer_field, partial sections, stuffing](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pointer_field-partial-sections-and-stuffing) | confirmed on live data |
| isdb-ts | [PMT CRC_32 "Annex B"](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pmt-crc_32-annex-b) | confirmed on live data (editorial error) |
| isdb-ts | [PES_packet_length 0](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pes_packet_length-0-vs-an-empty-bounded-pes-packet) | confirmed (structural) |
| isdb-ts | [PES header stuffing, trick mode raw](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pes-header-stuffing-and-trick-mode-byte-kept-raw) | confirmed on live data |
| isdb-ts | [Reserved bits](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-reserved-bits) | confirmed |
| isdb-ts | [PCR extension range, PCR PIDs](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-pcr-extension-range-and-allowed-pcr-pids) | confirmed |
| isdb-ts | [192/204-byte framings](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-192--and-204-byte-framings) | confirmed (format detection is a heuristic) |
| isdb-ts | [transport_error_indicator](crates/isdb-ts/SPEC-NOTES.md#isdb-ts-packets-with-transport_error_indicator) | confirmed on live data |
| isdb-text | [Initial state for SI strings](crates/isdb-text/SPEC-NOTES.md#isdb-text-initial-state-for-si-strings) | confirmed by live data (TR-B14 absent) |
| isdb-text | [Width of alphanumerics (MSZ/NSZ)](crates/isdb-text/SPEC-NOTES.md#isdb-text-width-of-alphanumeric-characters-and-sp-msz--nsz) | confirmed by live data |
| isdb-text | [05/12 and 07/14](crates/isdb-text/SPEC-NOTES.md#isdb-text-alphanumeric-set-0512-and-0714) | interpretation (spec text; not in capture) |
| isdb-text | [Kanji rows 1–84 = JIS X 0208](crates/isdb-text/SPEC-NOTES.md#isdb-text-kanji-set-rows-184--jis-x-0208-unicode-mapping) | **pending** an authoritative JIS table (7 override cells) |
| isdb-text | [Non-spacing characters](crates/isdb-text/SPEC-NOTES.md#isdb-text-non-spacing-characters) | interpretation (spec text only) |
| isdb-text | [Kanji rows 90–94 vs additional symbols](crates/isdb-text/SPEC-NOTES.md#isdb-text-kanji-set-rows-9094-vs-additional-symbols-set) | confirmed (Table 7-4 (8) + capture) |
| isdb-text | [Additional symbols → Unicode](crates/isdb-text/SPEC-NOTES.md#isdb-text-additional-symbols--unicode-table-7-19-pua-values) | confirmed for the symbols in the capture; 38 cells left PUA |
| isdb-text | [JIS Plane 1 / Plane 2](crates/isdb-text/SPEC-NOTES.md#isdb-text-jis-compatible-kanji-plane-1--plane-2) | incomplete by design |
| isdb-text | [DRCS, mosaic, macro](crates/isdb-text/SPEC-NOTES.md#isdb-text-drcs-mosaic-macro) | interpretation (none in capture) |
| isdb-text | [Control codes with text meaning](crates/isdb-text/SPEC-NOTES.md#isdb-text-control-codes-with-text-meaning) | confirmed (APR in capture) |
| isdb-text | [Designation forms not in Table 7-2](crates/isdb-text/SPEC-NOTES.md#isdb-text-designation-forms-not-in-table-7-2) | interpretation |
| isdb-text | [10/0 and 15/15](crates/isdb-text/SPEC-NOTES.md#isdb-text-100-and-1515) | interpretation |
| isdb-text | [Single shift + GR byte](crates/isdb-text/SPEC-NOTES.md#isdb-text-single-shift-followed-by-a-gr-byte) | interpretation |
| isdb-si | [Annex C exact integer arithmetic](crates/isdb-si/SPEC-NOTES.md#isdb-si-annex-c-formulas-evaluated-in-exact-integer-arithmetic) | confirmed |
| isdb-si | [Section length limits](crates/isdb-si/SPEC-NOTES.md#isdb-si-section-length-limits--511-vs-per-table-semantics) | interpretation |
| isdb-si | [PCAT content_descriptor_length](crates/isdb-si/SPEC-NOTES.md#isdb-si-pcat-content_descriptor_length-scope) | **pending** (no PCAT in capture) |
| isdb-si | [video_decode_control last 2 bits](crates/isdb-si/SPEC-NOTES.md#isdb-si-video_decode_control_descriptor-last-2-bits) | confirmed by the spec's own tables |
| isdb-si | [Semantics stating wrong widths](crates/isdb-si/SPEC-NOTES.md#isdb-si-semantics-stating-wrong-widths-syntax-table-followed) | confirmed by Part 1 figures |
| isdb-si | [digital_copy_control user_defined](crates/isdb-si/SPEC-NOTES.md#isdb-si-digital_copy_control_descriptor-4-bit-field-after-the-flags) | interpretation (Annex F typed view); receiver evidence (§3.4) |
| isdb-si | [stuffing_section](crates/isdb-si/SPEC-NOTES.md#isdb-si-stuffing_section-has-no-fixed-form) | interpretation |
| isdb-si | [conditional_access_descriptor source](crates/isdb-si/SPEC-NOTES.md#isdb-si-conditional_access_descriptor-syntax-source) | interpretation (H.222.0 Table 2-58) |
| isdb-si | [Descriptors/tables defined outside B10](crates/isdb-si/SPEC-NOTES.md#isdb-si-descriptors-and-tables-defined-outside-b10) | deferred (B1/B21/B25) |
| isdb-si | [Table 6-1 mandatory descriptors](crates/isdb-si/SPEC-NOTES.md#isdb-si-table-6-1-mandatory-descriptors-checked) | interpretation; live finding (217 events) |
| isdb-si | [ES_multi_lingual_flag "reserved"](crates/isdb-si/SPEC-NOTES.md#isdb-si-audio_component-es_multi_lingual_flag-reserved) | interpretation |
| isdb-si | [local_time_offset range](crates/isdb-si/SPEC-NOTES.md#isdb-si-local_time_offset-range) | interpretation |
| isdb-si | [Typed field values](crates/isdb-si/SPEC-NOTES.md#isdb-si-typed-field-values-values-module) | interpretation (Table 6-5 0xD2–0xD4 "4:3" read as typo) |
| isdb-analyze | [Severity classification](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-severity-classification) | interpretation |
| isdb-analyze | [Table 5-2 level vs frequency](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-b10-table-5-2-transmission-level-vs-frequency) | interpretation |
| isdb-analyze | ["EIT within 8 days" table_ids](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-eit-within-8-days-table_ids) | interpretation |
| isdb-analyze | [4 KB in 32 ms](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-4-kb-in-32-ms-per-pid) | interpretation (garbled text) |
| isdb-analyze | [PCR interval on PCR values](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-pcr-interval-measured-on-pcr-values) | confirmed on live data |
| isdb-analyze | [PCR accuracy](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-pcr-accuracy) | estimate |
| isdb-analyze | [Repetition time base](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-section-repetition-time-base) | estimate |
| isdb-analyze | [table_id per PID](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-table_id-per-pid) | interpretation |
| isdb-analyze | [MPEG-2 AAC ADTS on stream_type 0x0F](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-mpeg-2-aac-adts-b32-part-2-52-on-stream_type-0x0f) | confirmed on live data (no violation, '10' consistent) |
| isdb-analyze | [PID allocation](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-pid-allocation) | interpretation |
| isdb-analyze | [Version coherence](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-version-coherence-and-sub_table-completeness) | interpretation |
| isdb-analyze | [EIT rules](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-eit-rules) | interpretation |
| isdb-analyze | [Cross-table identities](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-cross-table-identities) | interpretation |
| isdb-analyze | [PES timing](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-pes-timing) | interpretation |
| isdb-analyze | [Section rewriting in `edit`](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-section-rewriting-in-edit-not-used-by-validation) | confirmed by tests |
| isdbt-mux | [Timing model](crates/isdbt-mux/SPEC-NOTES.md#timing-model-packet-arrival--multiplex-frame-slot) | confirmed (internal); C_rx output accepted (RX) |
| isdbt-mux | [PCR packets per frame](crates/isdbt-mux/SPEC-NOTES.md#pcr-packets-per-frame-at-fixed-positions) | confirmed for the C_rx layer pattern |
| isdbt-mux | [Layers of PSI/SI](crates/isdbt-mux/SPEC-NOTES.md#layers-of-psisi) | interpretation; duplicate-packet gap |
| isdbt-mux | [Repetition intervals](crates/isdbt-mux/SPEC-NOTES.md#repetition-intervals) | tested (`tables_conform_and_repeat`) |
| isdbt-mux | ["4KB in 32 ms"](crates/isdbt-mux/SPEC-NOTES.md#4kb-in-32-ms-per-pid) | conservative interpretation |
| isdbt-mux | [Section packing](crates/isdbt-mux/SPEC-NOTES.md#section-packing) | operational choice |
| isdbt-mux | [Version numbers](crates/isdbt-mux/SPEC-NOTES.md#version-numbers) | interpretation |
| isdbt-mux | [EIT present/following](crates/isdbt-mux/SPEC-NOTES.md#eit-presentfollowing) | tested (`eit_present_following`) |
| isdbt-mux | [EIT schedule](crates/isdbt-mux/SPEC-NOTES.md#eit-schedule) | interpretation (Appendix guideline) |
| isdbt-mux | [NIT](crates/isdbt-mux/SPEC-NOTES.md#nit) | derived values; channel plan **[not in local specs]** |
| isdbt-mux | [SDT, BIT, TOT, CAT](crates/isdbt-mux/SPEC-NOTES.md#sdt-bit-tot-cat) | interpretation; BIT first loop raw (TR-B14) |
| isdbt-mux | [Program selection, stream roles and the PID plan](crates/isdbt-mux/SPEC-NOTES.md#program-selection-stream-roles-and-the-pid-plan-relayclassify-pid_map--auto) | interpretation (B24 Part 3 Ch. 4 / §9.6 / Table 9-16); component_tag ranges **from knowledge** (TR-B14); PID plan operational |
| isdbt-mux | [PMT descriptors from media](crates/isdbt-mux/SPEC-NOTES.md#pmt-descriptors-derived-from-the-media) | **[H.262 from knowledge]**; stream_type cited from B32 Part 3 §3.10 (2) |
| isdbt-mux | [EIT component descriptors from media](crates/isdbt-mux/SPEC-NOTES.md#eit-component-descriptors-derived-from-the-media) | **[H.262 from knowledge]**; channel mapping and quality_indicator cited from B32 Part 2; quality_indicator `derived` from Table 1 (192 kbit/s stereo → Mode 2, as live broadcasters send) |
| isdbt-mux | [MPEG-2 video AUs and timestamps](crates/isdbt-mux/SPEC-NOTES.md#mpeg-2-video-access-units-and-timestamps-h262-from-knowledge) | **[H.262 from knowledge]**; RX (plays on the recorder) |
| isdbt-mux | [AAC ADTS: B32 Part 2 §5.2, CRC, adts_buffer_fullness](crates/isdbt-mux/SPEC-NOTES.md#aac-adts-b32-part-2-52-restrictions-crc-and-adts_buffer_fullness) | rules cited from B32 Part 2; header **[13818-7 from knowledge]**, confirmed on live data (header values, fullness recurrence); raw_data_block not parsed, so crc_check is neither verified nor inserted ([details](crates/isdbt-mux/SPEC-NOTES.md#aac-adts-raw_data_block-not-parsed)); VBR fullness rate choice is an interpretation; **RX pending** for conforming audio |
| isdbt-mux | [T-STD pacing](crates/isdbt-mux/SPEC-NOTES.md#t-std-pacing) | tested; Rmax **[H.262 Table 8-13 from knowledge]** |
| isdbt-mux | [Media looping](crates/isdbt-mux/SPEC-NOTES.md#media-looping) | implementation choice |
| isdbt-mux | [Digital copy control (0xC1)](crates/isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits) | **confirmed on a receiver** (bisection on the recorder) |
| isdbt-mux | [Remote control key (0xCD)](crates/isdbt-mux/SPEC-NOTES.md#remote-control-key-0xcd-remote_control_key_id) | **[TR-B14 practice, from knowledge]**; receiver evidence of collisions |
| isdbt-mux | [Raw vs typed descriptors](crates/isdbt-mux/SPEC-NOTES.md#raw-descriptors-versus-typed-ones) | implementation choice |
| isdbt-mux | [Operational choices 1–30](crates/isdbt-mux/SPEC-NOTES.md#operational-choices-not-dictated-by-the-local-specs--test-on-receivers) | **pending** TR-B14 / receiver tests (see §4) |
| isdbt-fec | [remux: model receiver timing](crates/isdbt-fec/SPEC-NOTES.md#remux-model-receiver-timing-multiplex-frame-pattern) | partially confirmed; **pending** live slot order |
| isdbt-fec | [remux: null TSP content](crates/isdbt-fec/SPEC-NOTES.md#remux-null-tsp-content-in-the-reproduced-ts) | convention |
| isdbt-fec | [layer: one-byte shift](crates/isdbt-fec/SPEC-NOTES.md#layer-one-byte-shift-of-the-frame-slot-structure) | confirmed |
| isdbt-fec | [energy: PRBS bit order](crates/isdbt-fec/SPEC-NOTES.md#energy-prbs-register-bit-order-and-output-tap) | confirmed |
| isdbt-fec | [energy: frame alignment](crates/isdbt-fec/SPEC-NOTES.md#energy-frame-alignment-and-position-in-the-chain) | confirmed (oracle, S270(gr)) |
| isdbt-fec | [delay: Table 3-7](crates/isdbt-fec/SPEC-NOTES.md#delay-table-3-7-adjustment) | confirmed |
| isdbt-fec | [byte_interleave: path-0 alignment](crates/isdbt-fec/SPEC-NOTES.md#byte_interleave-path-0-alignment) | confirmed |
| isdbt-fec | [conv: taps and encoder state](crates/isdbt-fec/SPEC-NOTES.md#conv-fig-3-10-taps-and-encoder-state) | confirmed (impulse response); start state unspecified |
| isdbt-fec | [conv: puncturing reset](crates/isdbt-fec/SPEC-NOTES.md#conv-puncturing-transmission-order-and-reset-point) | confirmed |
| isdbt-fec | [layer: latency contract](crates/isdbt-fec/SPEC-NOTES.md#layer-latency--alignment-contract) | confirmed |
| isdbt-fec | [layer: encoder start-up content](crates/isdbt-fec/SPEC-NOTES.md#layer-encoder-start-up-content-unspecified-implementation-choice) | implementation choice |
| isdbt-ofdm | [mapping: bit order](crates/isdbt-ofdm/SPEC-NOTES.md#mapping-bit-order-within-a-carrier-symbol) | confirmed (oracle) except DQPSK |
| isdbt-ofdm | [mapping: delayed branches](crates/isdbt-ofdm/SPEC-NOTES.md#mapping-which-branches-are-delayed-fig-3-18) | confirmed |
| isdbt-ofdm | [mapping: Table 3-9 delay adjustment](crates/isdbt-ofdm/SPEC-NOTES.md#mapping-delay-adjustment-table-3-9) | interpretation (bit-exact equivalent) |
| isdbt-ofdm | [mapping: constellations](crates/isdbt-ofdm/SPEC-NOTES.md#mapping-constellations-figs-3-15-3-17-3-19-and-table-3-11) | confirmed (every label asserted) |
| isdbt-ofdm | [mapping: π/4-DQPSK](crates/isdbt-ofdm/SPEC-NOTES.md#mapping-π4-shift-dqpsk-reference-and-recursion) | **pending** (loopback only) |
| isdbt-ofdm | [time_interleave: index of m_i](crates/isdbt-ofdm/SPEC-NOTES.md#combine--time_interleave-index-i-of-m_i-and-the-delay-adjustment) | confirmed |
| isdbt-ofdm | [freq_interleave: groups](crates/isdbt-ofdm/SPEC-NOTES.md#freq_interleave-groups) | interpretation |
| isdbt-ofdm | [freq_interleave: rotation numbering](crates/isdbt-ofdm/SPEC-NOTES.md#freq_interleave-carrier-rotation-direction-and-segment-number-ambiguous) | confirmed for coherent groups; DQPSK groups **pending** |
| isdbt-ofdm | [freq_interleave: Table 3-13 direction](crates/isdbt-ofdm/SPEC-NOTES.md#freq_interleave-table-3-13-direction) | confirmed |
| isdbt-ofdm | [frame: SP positions, data order](crates/isdbt-ofdm/SPEC-NOTES.md#frame-carrier-numbering-sp-positions-data-order) | interpretation (figure typos), asserted; oracle-identical |
| isdbt-ofdm | [frame: pilot PRBS](crates/isdbt-ofdm/SPEC-NOTES.md#frame-pilot-prbs-fig-3-31-tables-3-163-173-19) | confirmed |
| isdbt-ofdm | [frame: TMCC/AC DBPSK](crates/isdbt-ofdm/SPEC-NOTES.md#frame-tmcc--ac-dbpsk) | interpretation; B'0 identical to gr-isdbt |
| isdbt-ofdm | [spectrum, IFFT bin, scaling](crates/isdbt-ofdm/SPEC-NOTES.md#frame--ofdm-spectrum-arrangement-ifft-bin-scaling) | interpretation; scaling is a choice |
| isdbt-ofdm | [tmcc: bit order, sync, next info](crates/isdbt-ofdm/SPEC-NOTES.md#tmcc-bit-order-sync-alternation-next-information) | interpretation; oracle-equal fields, receivers accept (RX) |
| isdbt-ofdm | [dsc: difference-set bit order](crates/isdbt-ofdm/SPEC-NOTES.md#dsc-difference-set-code-bit-order-ambiguous) | agrees with gr-isdbt; **off-air pending** |
| isdbt-ofdm | [ac: content, seismic word](crates/isdbt-ofdm/SPEC-NOTES.md#ac-content-order-and-seismic-word) | **pending** (parity bit order) |
| isdbt-ofdm | [latency: two-symbol frame alignment](crates/isdbt-ofdm/SPEC-NOTES.md#latency-frame-alignment-of-the-bit-interleaving-delay-part-2-figs-3-83-9) | confirmed (oracle, S270(gr)) |

### 3.2 gr-isdbt deviations from B31 (oracle side; [VERIFICATION.md](VERIFICATION.md#discrepancies))

| # | gr-isdbt behaviour | B31 v2.2 | Effect on receivers |
|---|---|---|---|
| D1 | TMCC B17–B19 = 111 on every segment (`tmcc_encoder_impl.cc`) | §3.15.5: 000 coherent, 111 differential | The S270 decoded it anyway, so it ignores these bits |
| D2 | AC carriers carry W_i in every symbol (information 0) | §3.13.4: DBPSK, stuffing 1 | none observed |
| D3 | TMCC B67 = 0 while B27 = 1 | §3.15.6.4: 1 if there is no next information (or next = current) | Parity still checks both ways |
| D4 | Time-interleave delay adjustment of 204 symbols for I = 0 | Table 3-12: 0 for I = 0 | An I = 0 layer arrives one frame late against the other layers. Each layer still decodes |
| D5 | Layer C next-information I = 0 code written at B87 instead of B100 (typo) | Table 3-22 | Found by code inspection only; never exercised |
| D6 | Receiver start-up: first output packet unusable; one "TMCC NOT OK" on frame −1 of a Rust file (row 4) | — | gr acquisition artefact; not a disagreement |

### 3.3 Deviations observed in a live terrestrial broadcast capture

| Finding | Clause | Source |
|---|---|---|
| The one-seg program's PCR PID: 46 PCRs in 10.78 s, all 45 intervals 0.231 s (> 0.1 s). This is the only error-level finding on the capture. The interval equals one Mode 3 GI 1/8 frame (231.336 ms, `isdbt-core` `frame_lengths`), i.e. one PCR per multiplex frame. B31 Part 2 §3.2.4 Table 3-5 asks for four per frame in Mode 3 on the partial-reception layer (this comparison is an inference from the two numbers) | H.222.0 §2.7.2; B31 Part 2 §3.2.4 | [isdb-analyze SPEC-NOTES](crates/isdb-analyze/SPEC-NOTES.md#isdb-analyze-pcr-interval-measured-on-pcr-values) |
| 217 EIT schedule events without short_event_descriptor. 207 on two services (tables 0x50/0x51: 112 + 95) carry only an event_group_descriptor (group_type 0x1 "Event common", referencing a third service of the same broadcaster). 8 events in 0x58 carry only extended_event_descriptors. 2 events in 0x58 carry no descriptor. Reported as a finding, not suppressed | B10 Part 2 §6.1 Table 6-1 ("short_event_descriptor Mandatory in EIT"; footnote *2 substitution cannot be checked) | [isdb-si SPEC-NOTES](crates/isdb-si/SPEC-NOTES.md#isdb-si-table-6-1-mandatory-descriptors-checked) |
| SDT/EIT running_status 0 ("undefined") in every live station captured during development, where Table 5-6 defines 4 = running | B10 Table 5-6 | channel-scan captures of several stations (see §3.4) |
| video_decode_control transfer_characteristics '11' ("Undefined" in Table 6-60-1; the bits were reserved_future_use before v5) | B10 §6.2.30 | [isdb-si SPEC-NOTES](crates/isdb-si/SPEC-NOTES.md#isdb-si-typed-field-values-values-module) |

The recorder registered a relayed sample of this broadcast, so none of these
stops registration.

### 3.4 Receiver-imposed requirements beyond the specs (receiver compatibility notes)

| Requirement | Local spec position | Evidence | Status |
|---|---|---|---|
| **The recorder (a Sony BDZ-series Blu-ray recorder) requires digital_copy_control_descriptor (0xC1) user_defined copy_control_type '01'** (0b01xx, e.g. 0b0100 = '01' + APS '00'). With '00' it refuses playback: "Unsupported signal: This program is not available". The recording_control value (copy-free vs copy-one-generation), PMT+SDT vs PMT+EIT placement, the 0xC8 value and the EIT component_type do not matter | B10 §6.2.23: "user_defined … broadcasting service provider can define it originally"; Annex F (informative) Tables F-1/F-2/F-4 give the layout (F-2: '00' Undefined, '01' Output by encoding to serial interface). Expected to be normative in TR-B14 Part 4 §21 / Part 8 §5.3.2 (not available) | Bisection table in [crates/isdbt-mux/SPEC-NOTES.md](crates/isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits) | confirmed on a receiver. `isdbt-mux` defaults to '01' + '00'. `isdb ts validate` warns `si.copy_control` on '00' (`copy_control_type_00_warns`) |
| **remote_control_key_id must not collide with local stations.** If it does, the recorder's scan merges or ignores the service. The examples and `--relay` default to key 9 because the key originally tried was taken by a local station | B10 §6.2.42 gives no range; B21 §4.4.16/§5.2.17 makes no stipulation; assignments are in TR-B14 Part 7 §9.2.1 (not available) | [crates/isdbt-mux/SPEC-NOTES.md](crates/isdbt-mux/SPEC-NOTES.md#remote-control-key-0xcd-remote_control_key_id) and choice 19; `examples/bench.toml` | receiver evidence; range 1..=12 and channel numbering key·10 + index are **[from knowledge]** |
| `[network] channels` must equal the transmit channel, because "receivers retune to the NIT frequency" | B10 §6.2.31 defines the field, not the receiver behaviour | isdbt-mux choice 20 | stated in the notes; no isolated test recorded |
| running_status = 4 in the SDT of `isdbt-mux` output coincided with the recorder showing 休止中 (off air). The value live stations send, 0, was adopted. The recorder still showed 休止中 after this change, so the effect was **not isolated** | B10 Table 5-6; TR-B14 Part 4 §6.3 and §15 (not available) | receiver observation; isdbt-mux choice 8 (SDT 0) vs choice 9 (EIT p/f 4/1) | open |

---

## 4. Open verification items (prioritised)

1. **Off-air IQ capture of a real broadcaster** (HackRF RX). This is the only
   independent check left for:
   - the TMCC (184,102) and AC (187,105) difference-set parity bit order
     (`isdbt-ofdm` "dsc", "ac");
   - the π/4-DQPSK path and the rotation numbering inside a DQPSK group,
     if the captured broadcast uses DQPSK (gr-isdbt has no DQPSK);
   - AC content.

   Japanese broadcasts use partial reception, so a capture would also
   re-check the rotation numbering and the two-symbol frame alignment against
   a broadcast transmitter. No off-air ISDB-T IQ capture has been tested
   ([VERIFICATION.md](VERIFICATION.md#open-questions)). The live re-multiplexed slot order
   (`isdbt-fec` "remux", pending) needs a tuner that exposes the null slots or
   the IIP TSP_counter. An IQ capture would let it be reconstructed after RS
   decoding.
2. **More configurations on real receivers.** RX in this file covers only
   C_rx. The other oracle-matrix configurations, Mode 1/2 and three
   layers, have not been sent to the USB tuner or the recorder. The impairment
   behaviour (AWGN, CFO, sampling offset, multipath) was tested Rust→Rust
   only, not against the oracle.
3. **USB tuner `dvbv5-scan` section-filter failure (driver/libdvbv5 side).**
   With the MyGica S270, `dvb_read_sections: no data read on section
   filter` appears ~1.3 s after lock: libdvbv5 issues `DMX_SET_FILTER` and
   `DMX_STOP` and never reads the demux. The all-PID DVR path works, and the
   Rust TX output captured through it validates with 0 errors. Nothing
   points at the TS, but channel discovery on that tuner stays untested
   until this is solved.
4. **TR-B14 operational values** (isdbt-mux "Operational choices"; these need
   the purchased TR-B14 v6.9, sections from the sample TOCs in §1.2, or
   receiver bisection):
   - PCR positions and dedicated PCR PIDs (choice 1) — Part 7 §5.2.8;
   - PSI/SI layer placement and H-/M-/L-EIT PIDs 0x0012/0x0026/0x0027 per layer
     (choice 2) — Part 4 §22, §13.11–13.12;
   - section packing, repetition at 0.9 × Table 5-2 (choices 3–4);
   - additional_broadcasting_identification 0x01 and TS information
     transmission_type_info 0x0F/0xAF (choices 5–6) — Part 4 §30.4.3.4;
   - NIT descriptor order (choice 7) — Part 4 §30.4.2–30.4.3;
   - SDT running_status 0, EIT_user_defined_flags, free_CA_mode; EIT p/f
     running_status 4/1 (choices 8–9) — Part 4 §6.3, §15, §31.2;
   - copy control / content availability placement and defaults (choice 10)
     — Part 4 §21, Part 8 §5.3.2;
   - one-seg service_type 0xC0, PMT PID 0x1FC8, component tags (choice 11);
   - av_delay, T-STD margin, stream_ids (choice 12), one ADTS frame per PES
     (13), RAI placement (14), component/quality_indicator (15), null packet
     form (17), video decode control (18);
   - remote_control_key_id range and numbering (choice 19) — Part 7 §9.2.1;
   - BIT first loop / SI_parameter_descriptor content — Part 4 §31.1.
5. **Conforming AAC audio on a real receiver.** The B32 Part 2 §5.2
   header rules are enforced (`adts_compliance = "conform"` by default;
   `isdb ts validate` `aac.*`), but the receivers in C_rx were only shown a
   non-conforming test file (§2.5.1).
6. **JIS X 0208 / X 0213 / X 0221 tables** to settle the 7 `KANJI_OVERRIDES`
   cells and Plane 1/2 support (`isdb-text`).
7. **Captures with rare H.222.0/B10 features** to settle the pending
   `isdb-ts` items (duplicate packets, discontinuity_indicator,
   PES_extension_flag_2) and `isdb-si` PCAT content_descriptor_length.
8. **Gaps by design**, to implement when needed:
   - duplicate packets for a PID on two layers (B31 Part 2 §3.2.2);
   - one-seg H.264 packetisation (only `StreamKind::Declared`);
   - SDTT/CDT and the CA descriptors (B1/B21/B25);
   - broadcast-TS IIP / dummy-byte information (B31 Part 2 Ch. 5);
   - DRCS mapping;
   - T-STD TBsys/Bsys.
