# isdb-text

ARIB 8-unit character coding, the text encoding of every string field in
ARIB SI (service and network names, event titles and descriptions, component
text). The crate decodes the raw bytes to Unicode, reporting every code that
cannot be represented exactly, and encodes Unicode text back to a conformant
byte sequence that decodes to the same text.

## Spec scope

ARIB STD-B24 v6.4 Vol.1 Part 2 Chapter 7 (`specs/ARIB-STD-B24-v6.4-vol1.txt`;
the chapter still carries "Version 6.2-E1" page headers), as required by
ARIB STD-B10 Annex A ("Characters and control codes used in SI are in
accordance with the 8-unit character code specified in sub-clause 7.1").

| Module | Clauses / tables |
|---|---|
| `sets` | §7.1.1.1 Table 7-1 (invocation: LS0–LS3R, SS2/SS3), Table 7-2 (designation escapes), Table 7-3 (final bytes); `InitialState` (SI, or Part 3 Table 8-2 captions) |
| `tables` | Tables 7-4 (Kanji rows 1–84 and 90–94), 7-5 (alphanumeric), 7-6 (katakana), 7-7 (hiragana), 7-9 (JIS X 0201 katakana), Annex E (UCS conversion, Table E-1 non-spacing) |
| `addsym` | additional symbols set: Table 7-10/7-19 (rows 90–94), Table 7-11 (additional Kanji rows 85–86) |
| `control` | §7.1.2 Table 7-14 code table, Table 7-15 (C0), Table 7-16 (C1: size controls, RPC, MACRO, …), Table 7-17 (CSI) parameter lengths |
| `decode` | §7.1 decoder, Table 7-18 default macros, Annex E.4 |
| `encode` | Unicode → 8-unit code using the sets above plus MSZ/NSZ, SP and APR |

## Position in the stack

- Depends on: no workspace crate (`encoding_rs` for the JIS X 0208 row/cell
  table, `thiserror`).
- Used by: `isdb-si` (every string field is an `AribString`),
  `isdb-analyze` (decoded strings in reports, typed `--set` mutations),
  `isdbt-mux` (encodes configured names and event text); `isdb-cli` reaches
  it only through those.

## Key types and entry points

- `AribString(Vec<u8>)` — the raw bytes as carried in SI (lossless);
  `decode()` / `decode_with(&DecodeOptions)` → `Decoded { text, issues }`;
  `to_string_lossy()`.
- `Issue { offset, kind: IssueKind }` — `UnknownEscape`, `UndefinedControl`,
  `MalformedControl`, `Truncated`, `UndefinedCode`, `UnsupportedCode`,
  `Drcs`, `Mosaic`, `UndefinedMacro`, `MacroLimit`, `LoneSingleShift`,
  `DanglingNonSpacing`, `RepeatToEndOfLine`.
- `encode(&str)` / `encode_with(&str, &EncodeOptions)` →
  `Result<AribString, EncodeError>` (`Unencodable`, `NoBase`).
- `DecodeOptions` / `EncodeOptions`: `initial: InitialState` (`Si` default,
  `Caption`) and `width: WidthMode` (`CharacterSize` default, `AnnexE`).

```rust
use isdb_text::{AribString, encode};

// Audio component text as broadcasters send it: ESC 07/12 (LS3R, G3 =
// katakana in the SI initial state), then 39 46 6C 2A of Table 7-6.
let s = AribString(vec![0x1b, 0x7c, 0xb9, 0xc6, 0xec, 0xaa]);
let d = s.decode();
assert_eq!(d.text, "ステレオ");
assert!(d.issues.is_empty());

// LS1 + "TEST!" at normal size: alphanumerics fill a full display block.
assert_eq!(AribString(vec![0x0e, 0x54, 0x45, 0x53, 0x54, 0x21]).to_string_lossy(), "ＴＥＳＴ！");

// Encoding round-trips through the decoder.
let bytes = encode("ニュース TEST！ 7:00").unwrap();
assert_eq!(bytes.decode().text, "ニュース TEST！ 7:00");
```

## Design notes

- **Lossless storage, total decoder.** `AribString` keeps the bytes; the
  decoder never panics and never fails. Codes it cannot represent exactly
  produce U+FFFD (undefined / unsupported / DRCS) or nothing (mosaic,
  undefined macro) plus an `Issue` with the byte offset, so analysers can
  report them instead of silently losing them.
- **SI initial state.** No available spec states it; `InitialState::Si`
  is captions' Table 8-2 with G3 = Katakana instead of Macro, which the real
  capture requires (it invokes G3 with LS3R and sends katakana).
- **Width follows the character size.** By default alphanumerics and SP map
  to ASCII / U+0020 in MSZ/SSZ (and SZX tiny) and to full-width forms /
  U+3000 otherwise; `WidthMode::AnnexE` applies Annex E.4 literally. The
  encoder sends ASCII in MSZ and returns to NSZ before full-width text.
- **Encoder = shortest path.** The choice of set, code element and locking
  vs single shift is a shortest-path search over the code-extension state;
  the guarantee is `decode(encode(s)) == s` for the same initial state and
  width mode. Combining characters (Table E-1) are moved in front of their
  base, as the 8-unit code sends non-spacing characters first.
- **Bounded macros.** Macro expansion is limited (`MACRO_WORK_LIMIT`,
  nesting) and reported as `MacroLimit`.

## Verification

| Test | What it proves | Data |
|---|---|---|
| `tests/spec.rs` | spec vectors, one test per table: Table 7-3 final bytes, 7-2 designations (encode and decode), 7-1 invocations, 8-2 initial state, 7-5/7-6/7-7/7-9 code points, 7-8 mosaic ignored, 7-4 Kanji incl. the JIS-vs-Windows overrides, E-1 non-spacing, 7-19 additional symbols, 7-11 additional Kanji, 7-15/7-16/7-17 controls, RPC, size-dependent width, 7-18 default macros, DRCS placeholder, truncated sequences, bounded macro expansion | synthetic |
| `tests/prop.rs` | encode → decode is the identity on the whole encodable repertoire (SI and caption initial states, both width modes); kana/Kanji text encodes compactly; decoding arbitrary or structured byte soup is total; unencodable characters and combining characters without base are reported | synthetic (proptest; regressions in `prop.proptest-regressions`) |
| `tests/synthetic.rs` | `si_style_byte_sequences`: hand-built SI-style byte sequences (LS3R/LS1R/LS2R into GR, LS2/LS3 into GL, SS2/SS3, G0–G3 re-designated to the additional symbols set and back, additional Kanji row 85, JIS X 0201 katakana, SSZ/MSZ/NSZ, APR) decode to the expected fictional text with zero issues and round-trip; `encoder_strings`: fictional SI texts (names, event text with line breaks, items, symbols) encode, decode with zero issues and back to the same text, and the output uses LS0/LS1/LS3R, MSZ/NSZ, APR and an additional-symbols designation; `capture_si_strings` (opt-in): extracts the NIT/SDT/EIT strings of a real capture (own CRC and section parser, no `isdb-ts`) and requires at least one string, zero issues and round trip for all | synthetic; `capture_si_strings` reads the file named by `ISDB_RS_CAPTURE_TS` and prints `SKIP` on stderr (test passes) when the variable is unset or the file is missing |

Print the decoded strings for a human check:
`cargo test -p isdb-text --test synthetic -- --nocapture`. Run
`capture_si_strings` on your own capture with
`ISDB_RS_CAPTURE_TS=/path/to/capture.ts cargo test -p isdb-text --test synthetic capture`
([docs/test-data.md](../../docs/test-data.md)); during development, on a
live terrestrial capture, it found 100 distinct SI strings, all decoded
with zero issues and round-tripped.

## Known limitations

- DRCS characters decode to U+FFFD with `IssueKind::Drcs` (Annex E.5 gives
  no allocation rule); mosaic sets produce no text.
- JIS compatible Kanji Plane 1 decodes only rows 1–84 (as Table 7-4);
  the rest of Plane 1 and all of Plane 2 are `UnsupportedCode` (no JIS X
  0213 table available). The encoder never uses these sets, nor DRCS,
  mosaic or macro sets.
- The seven JIS-vs-Windows Kanji overrides (`tables::KANJI_OVERRIDES`) come
  from general knowledge of JIS X 0221 / X 0213, not an authoritative local
  table (SPEC-NOTES: pending).
- 38 additional-symbol cells have no Unicode equivalent and keep their
  Table 7-19 PUA value (`addsym::Mapping::PrivateUse`).
- Control codes are interpreted only for text: APR → `'\n'`, SP, size
  controls, RPC, MACRO; position, colour and other attribute controls are
  parsed and skipped. RPC 04/0 (repeat to end of line) outputs the
  character once (`RepeatToEndOfLine`).
- Designation forms not in Table 7-2 (e.g. `ESC 02/4 02/8 F`) are reported
  as `UnknownEscape` and not applied.

## See also

- [SPEC-NOTES.md](SPEC-NOTES.md) — interpretations with quoted spec text and
  confirmation status.
- Workspace [README](../../README.md) — ground truth, verification levels,
  conventions.
