//! Property tests: encode → decode is the identity on the encodable
//! repertoire, and decoding arbitrary bytes is total.

use isdb_text::{
    AribString, DecodeOptions, EncodeError, EncodeOptions, InitialState, WidthMode, addsym,
    encode_with, tables,
};
use proptest::prelude::*;

/// Every character the decoder can produce without an issue, for `width`:
/// all cells of the Kanji, additional symbols, alphanumeric (both widths
/// in the size-aware mode), hiragana, katakana and JIS X 0201 katakana
/// sets, SP in both widths, and APR. Combining characters are generated
/// separately.
fn repertoire(width: WidthMode) -> Vec<char> {
    let mut v: Vec<char> = Vec::new();
    for t in [
        tables::ALPHANUMERIC,
        tables::HIRAGANA,
        tables::KATAKANA,
        tables::JIS_X0201_KATAKANA,
    ] {
        v.extend(t.chars().filter(|&c| c != '\0'));
    }
    if width == WidthMode::CharacterSize {
        v.extend(tables::ALPHANUMERIC.chars().map(tables::fullwidth));
    }
    for row in 1..=94 {
        for cell in 1..=94 {
            v.extend(tables::kanji(row, cell).filter(|&c| !tables::is_non_spacing(c)));
        }
    }
    v.extend(addsym::all().map(|s| s.unicode));
    v.extend([' ', '\u{3000}', '\n']);
    v.sort_unstable();
    v.dedup();
    v
}

const MARKS: [char; 7] = [
    '\u{0301}', '\u{0300}', '\u{0308}', '\u{0302}', '\u{0305}', '\u{0332}', '\u{20DD}',
];

fn text_strategy(width: WidthMode) -> impl Strategy<Value = String> {
    let rep = repertoire(width);
    let n = rep.len();
    prop::collection::vec((0..n, prop::option::weighted(0.05, 0..MARKS.len())), 0..60).prop_map(
        move |items| {
            let mut s = String::new();
            for (i, mark) in items {
                let c = rep[i];
                s.push(c);
                if let (Some(m), false) = (mark, c == '\n') {
                    s.push(MARKS[m]);
                }
            }
            s
        },
    )
}

/// Hiragana, katakana, JIS level-1 Kanji (rows 16-47) and 、。ー.
fn japanese() -> impl Strategy<Value = char> {
    let mut v: Vec<char> = "ぁあいうえおかがきくけこさしすせそたちつてとなにぬねのはばぱまみむめもやゆよらりるれろわをん\
                            アイウエオカガキクケコサシスセソタチツテトナニヌネノハバパマミムメモヤユヨラリルレロワヲンヴ、。ー"
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    v.extend((16..=47).flat_map(|r| (1..=94).filter_map(move |c| tables::kanji(r, c))));
    prop::sample::select(v)
}

fn round_trip(s: &str, initial: InitialState, width: WidthMode) -> Result<(), TestCaseError> {
    let e = encode_with(s, &EncodeOptions { initial, width })
        .map_err(|e| TestCaseError::fail(format!("{e}")))?;
    let d = e.decode_with(&DecodeOptions { initial, width });
    prop_assert!(d.issues.is_empty(), "{:02x?}: {:?}", e.0, d.issues);
    prop_assert_eq!(d.text, s, "bytes {:02x?}", e.0);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn prop_round_trip_si(s in text_strategy(WidthMode::CharacterSize)) {
        round_trip(&s, InitialState::Si, WidthMode::CharacterSize)?;
    }

    #[test]
    fn prop_round_trip_caption_initial_state(s in text_strategy(WidthMode::CharacterSize)) {
        round_trip(&s, InitialState::Caption, WidthMode::CharacterSize)?;
    }

    #[test]
    fn prop_round_trip_annex_e_width(s in text_strategy(WidthMode::AnnexE)) {
        round_trip(&s, InitialState::Si, WidthMode::AnnexE)?;
    }

    /// Plain Japanese text (kana, JIS level-1 Kanji, punctuation): the cheap
    /// sets must be used, so the output never exceeds 2 bytes per character
    /// plus a few set switches.
    #[test]
    fn prop_kana_kanji_compact(s in prop::collection::vec(japanese(), 0..40)) {
        let s: String = s.into_iter().collect();
        let e = isdb_text::encode(&s).map_err(|e| TestCaseError::fail(format!("{e}")))?;
        prop_assert_eq!(e.decode().text, s.clone());
        prop_assert!(e.0.len() <= 2 * s.chars().count() + 2 * 8, "{} bytes for {:?}", e.0.len(), s);
    }

    /// Decoding is total, and any issue-free decoding re-encodes to bytes
    /// that decode to the same text.
    #[test]
    fn prop_decode_arbitrary_bytes(b in prop::collection::vec(any::<u8>(), 0..80)) {
        let d = AribString(b).decode();
        if d.issues.is_empty() {
            round_trip(&d.text, InitialState::Si, WidthMode::CharacterSize)?;
        }
    }

    /// Like `prop_decode_arbitrary_bytes`, biased toward well-formed
    /// input (graphic bytes, shifts, size controls, designations) so that
    /// the re-encode branch is exercised.
    #[test]
    fn prop_decode_structured_bytes(parts in prop::collection::vec(prop_oneof![
        8 => (0x21u8..0x7F).prop_map(|b| vec![b]),
        8 => (0xA1u8..0xFF).prop_map(|b| vec![b]),
        1 => prop::sample::select(vec![0x0E, 0x0F, 0x20, 0x0D, 0x89, 0x8A, 0x88]).prop_map(|b| vec![b]),
        1 => prop::sample::select(vec![
            vec![0x1B, 0x7C], vec![0x1B, 0x7D], vec![0x1B, 0x7E], vec![0x1B, 0x6E], vec![0x1B, 0x6F],
            vec![0x1B, 0x28, 0x4A], vec![0x1B, 0x29, 0x31], vec![0x1B, 0x2A, 0x49], vec![0x1B, 0x2B, 0x30],
            vec![0x1B, 0x24, 0x3B], vec![0x1B, 0x24, 0x29, 0x42], vec![0x1B, 0x24, 0x2B, 0x3B],
            vec![0x19], vec![0x1D],
        ]),
    ], 0..40)) {
        let b: Vec<u8> = parts.concat();
        let d = AribString(b).decode();
        if d.issues.is_empty() {
            round_trip(&d.text, InitialState::Si, WidthMode::CharacterSize)?;
        }
    }

    /// Characters outside the repertoire are rejected with their position.
    #[test]
    fn prop_unencodable_reported(prefix in "[a-z]{0,5}") {
        let s = format!("{prefix}\u{1F600}");
        prop_assert_eq!(
            isdb_text::encode(&s),
            Err(EncodeError::Unencodable { index: prefix.len(), ch: '\u{1F600}' })
        );
    }
}

#[test]
fn prop_combining_needs_base() {
    assert_eq!(
        isdb_text::encode("\u{0301}a"),
        Err(EncodeError::NoBase {
            index: 0,
            ch: '\u{0301}'
        })
    );
    assert_eq!(
        isdb_text::encode("a\n\u{0301}"),
        Err(EncodeError::NoBase {
            index: 2,
            ch: '\u{0301}'
        })
    );
}

#[test]
fn prop_repertoire_size() {
    // Sanity: 6879 JIS X 0208 characters (minus the 7 non-spacing cells),
    // the additional symbols and the 1-byte sets.
    let n = repertoire(WidthMode::CharacterSize).len();
    assert!(n > 7000, "{n}");
}
