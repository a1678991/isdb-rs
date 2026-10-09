# arib-text: spec interpretation notes

Spec: ARIB STD-B24 v6.4 Vol.1 (ARIB PDF `6-STD-B24v6_4-1p3-E1.pdf`, not
redistributed; text in `specs/ARIB-STD-B24-v6.4-vol1.txt`). Part 2 Chapter 7 still carries
"Version 6.2-E1" page headers; clause/table numbers are as printed. PDF page
numbers below are 1-based indices into the PDF file, not the printed numbers.

Format per entry: spec + clause, quoted text, interpretation, confirmation.

## arib-text: initial state for SI strings
- Spec: B24 Vol.1 Part 3 Chapter 8, Table 8-2 "Initial status" (captions);
  B10 Annex A (SI).
- Text (Table 8-2): "Designation G0 Kanji system set / G1 Alphanumeric set /
  G2 Hiragana set / G3 Macro code set; Invocation GL LS0 (G0) / GR LS2R (G2)";
  "Character size 1 x 1 (standard)".
  B10 Annex A: "Characters and control codes used in SI are in accordance
  with the 8-unit character code specified in sub-clause 7.1 … However, the
  details of the character set shall be specified in the operational
  guidelines of service providers." (The operational guideline is not
  available here.)
- Interpretation: no clause of the available specs states the SI initial
  state. `InitialState::Si` (default) = Table 8-2 except G3 = Katakana;
  `InitialState::Caption` = Table 8-2 exactly.
- Confirmation: confirmed by real data. The SI strings of a live
  terrestrial capture recorded during development use G0 Kanji in GL
  (`40 24` …), hiragana in GR (`b7 e7` …) and alphanumerics after
  bare LS1 without designating anything, and invoke G3 with LS3R and send
  katakana without designating it: audio component text `1b 7c b9 c6 ec aa`
  = ESC 07/12, then 03/9 03/6 06/12 02/10 of Table 7-6 = "ステレオ". With
  G3 = Macro those codes would be macro codes without a default statement
  (Table 7-18 Note 2), i.e. empty text. All distinct SI strings of that
  capture (NIT/SDT/EIT) decode with zero issues under this
  state; `tests/synthetic.rs` `capture_si_strings` repeats the check on any
  capture named by `ISDB_RS_CAPTURE_TS`.

## arib-text: width of alphanumeric characters and SP (MSZ / NSZ)
- Spec: Table 7-16 (SSZ "small size", MSZ "middle size", NSZ "normal size",
  SZX); Part 3 Table 4-1 ("standard, 1 x 2, 2 x 1, 2 x 2, 1/2 x 1, and
  1/2 x 1/2"); §7.1.2.3 ("SP (space) makes the entire specified current
  character field in background colour"); Annex E.4.
- Text (Annex E.4): "To convert 8bit-code to UCS, OVER LINE (0x7E) defined in
  JIS X 0201 is converted to TILDE (0x007E)." Annex E does not mention the
  character size.
- Interpretation: a character occupies a character field whose size the C1
  size controls set; at normal size an alphanumeric character fills a full
  display block, at middle/small size half of it. Unicode distinguishes
  exactly this with full-width vs half-width forms, so by default
  (`WidthMode::CharacterSize`) the alphanumeric set maps to U+0021…U+007E
  (05/12 → U+00A5, 07/14 → U+007E) when the size is SSZ, MSZ or SZX 06/0
  (tiny), and to their full-width forms U+FF01…U+FF5E / U+FFE5 otherwise;
  SP likewise maps to U+0020 or U+3000. Kana and Kanji sets are not
  affected (Unicode has no half-width hiragana; the half-width katakana
  belong to the JIS X 0201 katakana set, which always maps to U+FF61…).
  `WidthMode::AnnexE` applies Annex E.4 literally (always half-width).
- Confirmation: real data uses the size controls with exactly this meaning:
  in the live capture an EIT title sends a Latin word as LS1R MSZ <letters>
  NSZ <full-width "!"> (rendered half-width letters then "！"), the event
  text sends the same word after a bare LS1 at normal size (rendered
  full-width), and the service guide name is sent with the alphanumeric set
  and SP at normal size (full-width letters and U+3000). The encoder therefore sends ASCII in MSZ and switches back to
  NSZ before full-width characters.

## arib-text: alphanumeric set 05/12 and 07/14
- Spec: Table 7-5 (glyphs ¥ at 05/12, overline at 07/14); §7.2.1.1.1 "For
  code values of the alphanumerical set, refer to JIS X 0201-1997"; Annex E.1
  (JIS X 0221 mapping), E.4 (overline → TILDE).
- Interpretation: 05/12 → U+00A5 YEN SIGN, 07/14 → U+007E TILDE. REVERSE
  SOLIDUS U+005C is therefore not in the repertoire (only full-width ＼,
  Kanji 1-32, is).
- Confirmation: spec text; no occurrence in the capture.

## arib-text: Kanji set rows 1–84 = JIS X 0208, Unicode mapping
- Spec: §7.1.1.3, Table 7-4 (1)–(8) (PDF pp.57–64); Annex E.1.
- Text (E.1): "Mapping a character code in the tables defined in JIS X0201,
  JIS X0208, JIS X0212, and JIS X0213:2004 onto a corresponding character code
  in UCS complies with Appendix 2, JIS X0221-1:2001. When a difference is
  found between Appendix 2, JIS X0221-1:2001 and JIS X0213:2004, JIS
  X0213:2004 should be used."
- Interpretation: Table 7-4 shows the JIS X 0208:1990 repertoire in rows
  1–84 (rows 9–15 and 85–89 empty). The row/cell → Unicode mapping is taken
  from `encoding_rs` EUC-JP restricted to rows 1–8 and 16–84 (encoding_rs
  also maps NEC row 13 and IBM rows 89–92, which are *not* in Table 7-4 and
  are excluded). The WHATWG index follows Windows-31J for seven cells; they
  are corrected to the JIS X 0221/X 0213 values (`tables::KANJI_OVERRIDES`):
  1-29 U+2014, 1-33 U+301C, 1-34 U+2016, 1-61 U+2212, 1-81 U+00A2,
  1-82 U+00A3, 2-44 U+00AC. These values are the well-known JIS-vs-Windows
  differences; JIS X 0221 / X 0213 themselves are not available locally, so
  this list is from general knowledge of those standards
  (status: pending an authoritative JIS table).
- Confirmation: per-row cell counts from encoding_rs (94, 53, 62, 83, 86,
  48, 66, 32, …, 51 in row 47, 6 in row 84) equal the JIS X 0208:1990
  repertoire; a title in the live capture uses 1-33 (decoded U+301C).

## arib-text: non-spacing characters
- Spec: §7.1.1.4; Annex E.4, Table E-1.
- Text: "Non-spacing character shall be row 1 cell 13 to 18 in Table 7-4 (1)
  … and row 2 cell 94 … Non-spacing character … is displayed by cumulating
  character, mosaic or space, etc. specified by the successive code."
  Table E-1 maps them to U+0301, U+0300, U+0308, U+0302, U+0305, U+0332,
  U+20DD; "Any resulting UCS code should be handled as specified in
  ISO/IEC 10646:2014 Annex B … List of combining characters."
- Interpretation: in the 8-unit code the non-spacing character *precedes*
  its base; in UCS a combining character *follows* it. The decoder holds
  non-spacing characters until the next spacing character or SP and emits
  them after it (`IssueKind::DanglingNonSpacing` if none follows before APR
  or the end). The encoder moves combining characters in front of their base
  and rejects one with no base (`EncodeError::NoBase`). Consequently Kanji
  2-94 is never the spacing ◯ U+25EF.
- Confirmation: spec text only (no non-spacing characters in the capture).

## arib-text: Kanji set rows 90–94 vs additional symbols set
- Spec: Table 7-10 trailer (PDF p.89); §7.2.1.1.1; Table 7-4 (8) (PDF p.64).
- Text: "The table 7-10 contains the same characters as those in the table
  7-4 except the range from Row 90, Cell 45 to Cell 63, and the range from
  Row 90, Cell 66 to Cell 84." §7.2.1.1.1: "To reference any character
  belonging to Rows 90 to 94 in the Kanji set, the corresponding character
  defined in the additional symbols set should be used."
- Interpretation: Kanji set rows 90–94 map through Table 7-19 like the
  additional symbols set, except 90-45…63 and 90-66…84, which are undefined
  in the Kanji set. Rows 85–86 exist only in the additional symbols set.
- Confirmation: Table 7-4 (8) row 90 shows only cells 64–65 (■ ●) in the
  48–94 range. The capture designates the additional symbols set
  (`ESC 02/4 03/11`, to G0..G3) before 90-54 [字], 90-56 [デ], etc.

## arib-text: additional symbols → Unicode (Table 7-19 PUA values)
- Spec: §7.2.1.1.1 Table 7-19 (PDF pp.119–122), Table 7-10 glyphs
  (PDF pp.71–88), Table 7-11 (UCS values of the additional Kanji).
- Text: Table 7-19 gives BMP values, PUA (U+E0xx–E3xx) where no BMP
  character existed. Duplicates are listed: "90-58(U+E182) and
  93-30(U+E2DA)", "91-8(U+E1AE) and 2-9(U+3012)", "91-43(U+260E) and
  93-91(U+E2FB)", "92-07(U+E28B) and 39-15(U+5E74) Year", … "92-10 … 17-63
  Money".
- Interpretation (`addsym::Mapping`): non-PUA values used as printed; PUA
  values replaced by (a) Table 7-11's supplementary-plane value for rows
  85–86 (#2 U+20158, #15 U+20BB7, #53 U+233CC, #54 U+233FE, #55 U+235C4,
  #68 U+242EE), (b) the duplicated character's code point for the listed
  duplicates, (c) the Unicode 5.2+ ARIB symbol whose name matches the
  Table 7-10 glyph/description (U+1F100–1F10A digit+full stop/comma,
  U+1F110–1F129 parenthesised A–Z, U+1F12A 〔S〕, U+1F12D circled CD,
  U+1F131/13D/13F/142/146 squared B N P S W, U+1F14A–1F14E HV MV SD SS
  PPV, U+1F157/15F negative circled H/P, U+1F179/17B/17C/17F negative
  squared J L M P, U+1F18A crossed P, U+1F18B–1F18D IC PA SA, U+1F190 DJ,
  U+1F200 ほか, U+1F210–1F231 squared ideographs/デ, U+1F240–1F248
  tortoise-shell ideographs; names checked with Python `unicodedata` 16.0).
- Not mapped (38 cells, PUA kept as Table 7-19 gives them,
  `Mapping::PrivateUse`): 92-26…31 (70 %-size 氏 副 元 故 前 新), 92-56…85
  (parenthesised instrument abbreviations (vn) … (per), some spanning two
  cells), 92-86/87 (disc record ®, compact disc ©: Table 7-20 moved them off
  U+00AE/U+00A9 to PUA; Unicode has CIRCLED ITALIC LATIN CAPITAL LETTER R/C
  U+1F12C/1F12B, but the Table 7-10 glyphs are upright, so the identity is
  not considered clear).
- Confirmation: [字] [デ] [新] [解] [ほか] in the live capture's EIT titles
  decode to 🈑 🈓 🈟 🈖 🈀.

## arib-text: JIS compatible Kanji Plane 1 / Plane 2
- Spec: §7.1.1.3: "The JIS compatible Kanji Plane 1 set is identical with …
  JIS X213: 2004 [plane 1] … When the Kanji Set for Information interchange,
  Plane 1 is not used, the range of Row 1 to Row 84 in Table 7-4 is imported
  to the JIS compatible Kanji Plane 1."
- Interpretation: no JIS X 0213 table is available, so Plane 1 decodes rows
  1–84 like Table 7-4 and reports everything else (and all of Plane 2) as
  `IssueKind::UnsupportedCode` with U+FFFD. The encoder never uses them.
- Confirmation: not used in the capture. (status: incomplete by design)

## arib-text: DRCS, mosaic, macro
- Spec: §7.1.1.5, Annex E.4 ("Any character in the mosaic set is ignored"),
  Annex E.5 ("Any character in DRCS is mapped into the Private Use Area …
  The area available to DRCS starts with Row EC, Cell 00."), §7.1.1.6,
  Table 7-16 MACRO, Table 7-18.
- Interpretation: E.5 gives no allocation rule (and 15×94 + 8836 codes do not
  fit from U+EC00), so DRCS characters decode to U+FFFD with
  `IssueKind::Drcs { set, code }`. Mosaic characters produce no output and
  `IssueKind::Mosaic`. Macro codes run the MACRO-defined body if any, else
  the default macro statement of Table 7-18 (06/0…06/15), else nothing
  (`IssueKind::UndefinedMacro`). The macro set is designated with the 1-byte
  DRCS form (`ESC 02/11 02/0 07/0`, as in Table 7-18).
- Confirmation: spec text; none in the capture.

## arib-text: control codes with text meaning
- Spec: Table 7-15.
- Text: "APR Active position return: Active position down is made, moving
  to the first position of the same line."
- Interpretation: APR → `'\n'` (and the encoder sends `'\n'` as APR); APD
  (down without return) and the other position/attribute controls produce
  no text. RPC repeats the next spacing character P1 & 0x3F times; RPC 04/0
  (to end of line) outputs it once with `IssueKind::RepeatToEndOfLine`.
- Confirmation: extended-event-like line breaks in the capture's short
  event text are sent as 00/13 and decode to line breaks.

## arib-text: designation forms not in Table 7-2
- Spec: Table 7-2.
- Interpretation: only the listed forms are accepted. In particular
  `ESC 02/4 02/8 F` (ISO/IEC 2022's alternative 2-byte G0 form) is not in
  Table 7-2 and is reported as `UnknownEscape`; the designation is not
  applied. Unknown escapes are skipped by ISO/IEC 2022 shape (ESC,
  intermediates 02/x, one final byte).
- Confirmation: none in the capture.

## arib-text: 10/0 and 15/15
- Spec: Figure 7-1 note ("Geometric coding shall add … *2 (10/0) and *4
  (15/15) to GR area"), Table 7-14.
- Interpretation: in character coding they are not graphic positions of any
  94-set; reported as `UndefinedControl` and skipped. DEL (07/15) produces
  no text.

## arib-text: single shift followed by a GR byte
- Spec: Table 7-1 note (3): "Single shift means to invoke one code following
  to it in the GL or GR area temporary."
- Interpretation: the character after SS2/SS3 is taken from G2/G3 whether
  its byte is in GL or GR. The encoder always sends the GL form.
