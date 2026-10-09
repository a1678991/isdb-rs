//! Spec vectors: ARIB STD-B24 v6.4 Vol.1 Part 2 §7.1 tables, asserted
//! value by value.

use isdb_text::addsym::{self, Mapping};
use isdb_text::tables;
use isdb_text::{
    AribString, DecodeOptions, GraphicSet as G, InitialState, IssueKind, Shift, WidthMode,
};

fn dec(bytes: &[u8]) -> isdb_text::Decoded {
    AribString(bytes.to_vec()).decode()
}

fn text(bytes: &[u8]) -> String {
    let d = dec(bytes);
    assert!(d.issues.is_empty(), "{bytes:02x?}: {:?}", d.issues);
    d.text
}

const ESC: u8 = 0x1B;

// ---- Table 7-3: final bytes -------------------------------------------

#[test]
fn spec_table_7_3_final_bytes() {
    let g_sets = [
        (G::Kanji, 0x42, 2),
        (G::Alphanumeric, 0x4A, 1),
        (G::Hiragana, 0x30, 1),
        (G::Katakana, 0x31, 1),
        (G::MosaicA, 0x32, 1),
        (G::MosaicB, 0x33, 1),
        (G::MosaicC, 0x34, 1),
        (G::MosaicD, 0x35, 1),
        (G::ProportionalAlphanumeric, 0x36, 1),
        (G::ProportionalHiragana, 0x37, 1),
        (G::ProportionalKatakana, 0x38, 1),
        (G::JisX0201Katakana, 0x49, 1),
        (G::JisKanjiPlane1, 0x39, 2),
        (G::JisKanjiPlane2, 0x3A, 2),
        (G::AdditionalSymbols, 0x3B, 2),
    ];
    for (set, f, n) in g_sets {
        assert_eq!(set.final_byte(), f, "{set:?}");
        assert_eq!(set.bytes_per_char(), n, "{set:?}");
        assert_eq!(G::from_designation(n == 2, false, f), Some(set));
    }
    // DRCS-0 = 04/0 (2-byte), DRCS-1..15 = 04/1..04/15, Macro = 07/0.
    assert_eq!(G::Drcs(0).final_byte(), 0x40);
    assert_eq!(G::Drcs(0).bytes_per_char(), 2);
    assert_eq!(G::from_designation(true, true, 0x40), Some(G::Drcs(0)));
    for n in 1..=15u8 {
        assert_eq!(G::Drcs(n).final_byte(), 0x40 + n);
        assert_eq!(G::from_designation(false, true, 0x40 + n), Some(G::Drcs(n)));
    }
    assert_eq!(G::Macro.final_byte(), 0x70);
    assert_eq!(G::from_designation(false, true, 0x70), Some(G::Macro));
    // 04/2 and 04/9 mean DRCS-2 / DRCS-9 in the DRCS form only.
    assert_eq!(G::from_designation(false, true, 0x42), Some(G::Drcs(2)));
    assert_eq!(G::from_designation(false, false, 0x42), None);
    assert_eq!(G::from_designation(false, true, 0x49), Some(G::Drcs(9)));
}

// ---- Table 7-2: designation sequences ----------------------------------

#[test]
fn spec_table_7_2_designations() {
    assert_eq!(G::Alphanumeric.designation(0), [ESC, 0x28, 0x4A]);
    assert_eq!(G::Alphanumeric.designation(1), [ESC, 0x29, 0x4A]);
    assert_eq!(G::Hiragana.designation(2), [ESC, 0x2A, 0x30]);
    assert_eq!(G::Katakana.designation(3), [ESC, 0x2B, 0x31]);
    // 2-byte G set: ESC 02/4 F for G0, ESC 02/4 02/9..02/11 F for G1..G3.
    assert_eq!(G::Kanji.designation(0), [ESC, 0x24, 0x42]);
    assert_eq!(G::Kanji.designation(1), [ESC, 0x24, 0x29, 0x42]);
    assert_eq!(G::AdditionalSymbols.designation(3), [ESC, 0x24, 0x2B, 0x3B]);
    // 1-byte DRCS: ESC 02/8+n 02/0 F;  2-byte DRCS: ESC 02/4 02/8+n 02/0 F.
    assert_eq!(G::Drcs(1).designation(0), [ESC, 0x28, 0x20, 0x41]);
    assert_eq!(G::Drcs(15).designation(3), [ESC, 0x2B, 0x20, 0x4F]);
    assert_eq!(G::Drcs(0).designation(0), [ESC, 0x24, 0x28, 0x20, 0x40]);
    assert_eq!(G::Drcs(0).designation(2), [ESC, 0x24, 0x2A, 0x20, 0x40]);
    assert_eq!(G::Macro.designation(3), [ESC, 0x2B, 0x20, 0x70]);
}

#[test]
fn spec_table_7_2_designations_decode() {
    // Katakana to G0 (GL): 03/1 = ア.
    assert_eq!(text(&[ESC, 0x28, 0x31, 0x22]), "ア");
    // Hiragana to G1, LS1: あ.
    assert_eq!(text(&[ESC, 0x29, 0x30, 0x0E, 0x22]), "あ");
    // Kanji to G2 (GR): 16-1 亜 as A0|… in GR.
    assert_eq!(text(&[ESC, 0x24, 0x2A, 0x42, 0xB0, 0xA1]), "亜");
    // Additional symbols to G0 (as broadcast: ESC 02/4 03/11 LS0 7A 56 = [字]).
    assert_eq!(text(&[ESC, 0x24, 0x3B, 0x0F, 0x7A, 0x56]), "\u{1F211}");
    // JIS X 0201 katakana to G3, LS3R: 03/1 = ｱ.
    assert_eq!(text(&[ESC, 0x2B, 0x49, ESC, 0x7C, 0xB1]), "ｱ");
    // Proportional sets decode as their monospaced sets (Annex E.4).
    assert_eq!(text(&[ESC, 0x28, 0x37, 0x22]), "あ");
    assert_eq!(text(&[ESC, 0x28, 0x38, 0x22]), "ア");
    // An undefined final byte is reported and the designation not applied.
    let d = dec(&[ESC, 0x28, 0x7A, 0x30, 0x21]);
    assert_eq!(d.text, "亜");
    assert_eq!(
        d.issues[0].kind,
        IssueKind::UnknownEscape(vec![ESC, 0x28, 0x7A])
    );
}

// ---- Table 7-1: invocations ---------------------------------------------

#[test]
fn spec_table_7_1_invocations() {
    assert_eq!(Shift::Ls0.bytes(), [0x0F]);
    assert_eq!(Shift::Ls1.bytes(), [0x0E]);
    assert_eq!(Shift::Ls2.bytes(), [ESC, 0x6E]);
    assert_eq!(Shift::Ls3.bytes(), [ESC, 0x6F]);
    assert_eq!(Shift::Ls1r.bytes(), [ESC, 0x7E]);
    assert_eq!(Shift::Ls2r.bytes(), [ESC, 0x7D]);
    assert_eq!(Shift::Ls3r.bytes(), [ESC, 0x7C]);
    assert_eq!(Shift::Ss2.bytes(), [0x19]);
    assert_eq!(Shift::Ss3.bytes(), [0x1D]);
    // MSZ so that the alphanumeric set is half-width.
    let msz = 0x89;
    assert_eq!(text(&[msz, 0x0E, 0x41, 0x0F, 0x30, 0x21]), "A亜"); // LS1, LS0
    assert_eq!(text(&[ESC, 0x6E, 0x22]), "あ"); // LS2
    assert_eq!(text(&[ESC, 0x6F, 0x22]), "ア"); // LS3
    assert_eq!(text(&[msz, ESC, 0x7E, 0xC1]), "A"); // LS1R
    assert_eq!(text(&[ESC, 0x7C, 0xA2, ESC, 0x7D, 0xA2]), "アあ"); // LS3R, LS2R
    // Single shifts affect one character only.
    assert_eq!(text(&[0x1D, 0x22, 0x30, 0x21]), "ア亜"); // SS3
    assert_eq!(text(&[ESC, 0x28, 0x31, 0x19, 0x22, 0x22]), "あア"); // SS2
}

#[test]
fn spec_table_8_2_initial_state() {
    let si = DecodeOptions::default();
    assert_eq!(si.initial, InitialState::Si);
    assert_eq!(
        InitialState::Si.designations(),
        [G::Kanji, G::Alphanumeric, G::Hiragana, G::Katakana]
    );
    assert_eq!(
        InitialState::Caption.designations(),
        [G::Kanji, G::Alphanumeric, G::Hiragana, G::Macro]
    );
    assert_eq!(InitialState::Si.invocations(), (0, 2));
    // GL = G0 (Kanji), GR = G2 (Hiragana), G1 = Alphanumeric, size normal.
    assert_eq!(text(&[0x30, 0x21, 0xA2, 0x0E, 0x41]), "亜あＡ");
    // SI: G3 is katakana (off-air strings send LS3R + katakana undesignated).
    assert_eq!(text(&[ESC, 0x7C, 0xB9, 0xC6, 0xEC, 0xAA]), "ステレオ");
    // Caption: G3 is the macro set; 03/9 has no default macro.
    let cap = DecodeOptions {
        initial: InitialState::Caption,
        ..Default::default()
    };
    let d = AribString(vec![ESC, 0x7C, 0xB9]).decode_with(&cap);
    assert_eq!(d.text, "");
    assert_eq!(d.issues[0].kind, IssueKind::UndefinedMacro(0x39));
}

// ---- Tables 7-5 … 7-9: 1-byte sets --------------------------------------

#[test]
fn spec_table_7_5_alphanumeric() {
    assert_eq!(tables::alphanumeric(0x21), Some('!'));
    assert_eq!(tables::alphanumeric(0x30), Some('0'));
    assert_eq!(tables::alphanumeric(0x41), Some('A'));
    assert_eq!(tables::alphanumeric(0x5C), Some('\u{A5}')); // 05/12 YEN SIGN
    assert_eq!(tables::alphanumeric(0x60), Some('`'));
    assert_eq!(tables::alphanumeric(0x7C), Some('|'));
    assert_eq!(tables::alphanumeric(0x7E), Some('~')); // overline → TILDE, Annex E.4
    assert_eq!(tables::alphanumeric(0x7F), None);
    assert_eq!(tables::ALPHANUMERIC.chars().count(), 94);
}

#[test]
fn spec_table_7_6_katakana() {
    let k = tables::katakana;
    assert_eq!(k(0x21), Some('ァ'));
    assert_eq!(k(0x22), Some('ア'));
    assert_eq!(k(0x2F), Some('ク'));
    assert_eq!(k(0x30), Some('グ'));
    assert_eq!(k(0x39), Some('ス'));
    assert_eq!(k(0x46), Some('テ'));
    assert_eq!(k(0x6C), Some('レ'));
    assert_eq!(k(0x73), Some('ン'));
    assert_eq!(k(0x74), Some('ヴ'));
    assert_eq!(k(0x75), Some('ヵ'));
    assert_eq!(k(0x76), Some('ヶ'));
    assert_eq!(k(0x77), Some('ヽ'));
    assert_eq!(k(0x78), Some('ヾ'));
    assert_eq!(k(0x79), Some('ー'));
    assert_eq!(k(0x7A), Some('。'));
    assert_eq!(k(0x7B), Some('「'));
    assert_eq!(k(0x7C), Some('」'));
    assert_eq!(k(0x7D), Some('、'));
    assert_eq!(k(0x7E), Some('・'));
    assert_eq!(tables::KATAKANA.chars().count(), 94);
}

#[test]
fn spec_table_7_7_hiragana() {
    let h = tables::hiragana;
    assert_eq!(h(0x21), Some('ぁ'));
    assert_eq!(h(0x22), Some('あ'));
    assert_eq!(h(0x40), Some('だ'));
    assert_eq!(h(0x6E), Some('ゎ'));
    assert_eq!(h(0x73), Some('ん'));
    assert_eq!(h(0x74), None); // 07/4..07/6 blank
    assert_eq!(h(0x75), None);
    assert_eq!(h(0x76), None);
    assert_eq!(h(0x77), Some('ゝ'));
    assert_eq!(h(0x78), Some('ゞ'));
    assert_eq!(h(0x79), Some('ー'));
    assert_eq!(h(0x7E), Some('・'));
    assert_eq!(tables::HIRAGANA.chars().count(), 94);
    // Blank position decodes to U+FFFD with an issue.
    let d = dec(&[0xF4]);
    assert_eq!(d.text, "\u{FFFD}");
    assert_eq!(
        d.issues[0].kind,
        IssueKind::UndefinedCode {
            set: G::Hiragana,
            code: 0x74
        }
    );
}

#[test]
fn spec_table_7_9_jis_x0201_katakana() {
    let j = tables::jis_x0201_katakana;
    assert_eq!(j(0x21), Some('｡'));
    assert_eq!(j(0x26), Some('ｦ'));
    assert_eq!(j(0x30), Some('ｰ'));
    assert_eq!(j(0x31), Some('ｱ'));
    assert_eq!(j(0x5D), Some('ﾝ'));
    assert_eq!(j(0x5E), Some('ﾞ'));
    assert_eq!(j(0x5F), Some('ﾟ'));
    assert_eq!(j(0x60), None); // columns 6-7 "Undefined"
    assert_eq!(tables::JIS_X0201_KATAKANA.chars().count(), 63);
}

#[test]
fn spec_table_7_8_mosaic_ignored() {
    // Annex E.4: "Any character in the mosaic set is ignored."
    let d = dec(&[ESC, 0x28, 0x32, 0x21, 0x0E, 0x41]);
    assert_eq!(d.text, "Ａ");
    assert_eq!(
        d.issues[0].kind,
        IssueKind::Mosaic {
            set: G::MosaicA,
            code: 0x21
        }
    );
}

// ---- Table 7-4: Kanji set ------------------------------------------------

#[test]
fn spec_table_7_4_kanji() {
    assert_eq!(tables::kanji(1, 1), Some('\u{3000}'));
    assert_eq!(tables::kanji(3, 33), Some('Ａ'));
    assert_eq!(tables::kanji(4, 2), Some('あ'));
    assert_eq!(tables::kanji(16, 1), Some('亜'));
    assert_eq!(tables::kanji(84, 6), Some('熙'));
    assert_eq!(tables::kanji(13, 1), None); // rows 9-15 empty in Table 7-4
    assert_eq!(tables::kanji(89, 1), None); // rows 85-89 empty in Table 7-4
    // Annex E.1 (JIS X 0221 / JIS X 0213:2004 mapping), not the Windows one.
    assert_eq!(tables::kanji(1, 33), Some('\u{301C}')); // WAVE DASH
    assert_eq!(tables::kanji(1, 61), Some('\u{2212}')); // MINUS SIGN
    // Rows 90-94 are the additional symbols, except 90-45..63 and 90-66..84.
    assert_eq!(tables::kanji(90, 1), Some('\u{26CC}'));
    assert_eq!(tables::kanji(90, 45), None);
    assert_eq!(tables::kanji(90, 54), None);
    assert_eq!(tables::kanji(90, 64), Some('\u{2B1B}'));
    assert_eq!(tables::kanji(90, 65), Some('\u{2B24}'));
    assert_eq!(tables::kanji(90, 84), None);
    assert_eq!(tables::kanji(94, 93), Some('\u{325B}'));
}

#[test]
fn spec_table_e_1_non_spacing() {
    // §7.1.1.4: row 1 cells 13-18 and row 2 cell 94 are non-spacing; they
    // precede the character they combine with. Table E-1 gives the UCS
    // combining characters, which follow their base.
    assert_eq!(tables::kanji(1, 13), Some('\u{0301}'));
    assert_eq!(tables::kanji(1, 18), Some('\u{0332}'));
    assert_eq!(tables::kanji(2, 94), Some('\u{20DD}'));
    assert_eq!(text(&[0x21, 0x2D, 0x23, 0x41]), "Ａ\u{0301}");
    assert_eq!(text(&[0x21, 0x2D, 0x21, 0x2F, 0xA2]), "あ\u{0301}\u{0308}");
    let d = dec(&[0x22, 0x7E]);
    assert_eq!(d.text, "\u{20DD}");
    assert_eq!(d.issues[0].kind, IssueKind::DanglingNonSpacing);
}

// ---- Tables 7-10, 7-11, 7-19: additional symbols ------------------------

#[test]
fn spec_table_7_19_additional_symbols() {
    let a = |r, c| addsym::additional_symbol(r, c).map(|s| (s.table_7_19, s.unicode, s.mapping));
    assert_eq!(a(90, 1), Some((0x26CC, '\u{26CC}', Mapping::Table719)));
    assert_eq!(a(90, 7), None); // blank in Tables 7-10 and 7-19
    assert_eq!(a(90, 45), Some((0x2491, '⒑', Mapping::Table719)));
    assert_eq!(a(90, 83), Some((0x3299, '㊙', Mapping::Table719)));
    assert_eq!(a(91, 13), Some((0x0FD6, '\u{0FD6}', Mapping::Table719)));
    assert_eq!(a(92, 91), Some((0x213B, '℻', Mapping::Table719)));
    assert_eq!(a(93, 48), Some((0x00BD, '½', Mapping::Table719)));
    assert_eq!(a(93, 87), None);
    assert_eq!(a(94, 1), Some((0x2160, 'Ⅰ', Mapping::Table719)));
    assert_eq!(a(94, 93), Some((0x325B, '㉛', Mapping::Table719)));
    assert_eq!(a(94, 94), None);
    assert_eq!(a(85, 1), Some((0x3402, '\u{3402}', Mapping::Table719)));
    assert_eq!(a(86, 43), Some((0x9EB5, '麵', Mapping::Table719)));
    assert_eq!(a(86, 44), None);
    assert_eq!(a(87, 1), None);
    // PUA in Table 7-19, Unicode 5.2 ARIB symbols used instead.
    assert_eq!(a(90, 48), Some((0xE0F8, '\u{1F14A}', Mapping::Unicode52))); // [HV]
    assert_eq!(a(90, 54), Some((0xE0FE, '\u{1F211}', Mapping::Unicode52))); // [字]
    assert_eq!(a(90, 56), Some((0xE180, '\u{1F213}', Mapping::Unicode52))); // [デ]
    assert_eq!(a(90, 84), Some((0xE19C, '\u{1F200}', Mapping::Unicode52))); // [ほか]
    assert_eq!(a(94, 33), Some((0xE383, '\u{1F110}', Mapping::Unicode52))); // (A)
    assert_eq!(a(94, 58), Some((0xE39C, '\u{1F129}', Mapping::Unicode52))); // (Z)
    // Duplicates listed under Table 7-19.
    assert_eq!(a(91, 8).map(|x| x.1), tables::kanji(2, 9)); // 〒
    assert_eq!(a(93, 30).map(|x| x.1), a(90, 58).map(|x| x.1));
    assert_eq!(a(93, 91).map(|x| x.1), a(91, 43).map(|x| x.1)); // ☎
    assert_eq!(a(92, 7).map(|x| x.1), tables::kanji(39, 15)); // 年
    // No Unicode equivalent: PUA kept.
    assert_eq!(a(92, 26), Some((0xE290, '\u{E290}', Mapping::PrivateUse)));
    assert_eq!(a(92, 86), Some((0xE3A7, '\u{E3A7}', Mapping::PrivateUse)));
    let pua = addsym::all()
        .filter(|s| s.mapping == Mapping::PrivateUse)
        .count();
    assert_eq!(pua, 38);
    // Every remaining PUA value is in the BMP PUA, every replaced one is not.
    for s in addsym::all() {
        let in_pua = (0xE000..=0xF8FF).contains(&(s.unicode as u32));
        assert_eq!(in_pua, s.mapping == Mapping::PrivateUse, "{s:?}");
    }
}

#[test]
fn spec_table_7_11_additional_kanji() {
    // Graphic symbol numbers 1..94 = row 85, 95..137 = row 86.
    let u = |r, c| addsym::additional_symbol(r, c).map(|s| s.unicode as u32);
    assert_eq!(u(85, 2), Some(0x20158)); // #2 ISO/IEC 10646: U+20158
    assert_eq!(u(85, 15), Some(0x20BB7)); // #15
    assert_eq!(u(85, 53), Some(0x233CC)); // #53
    assert_eq!(u(85, 54), Some(0x233FE)); // #54
    assert_eq!(u(85, 55), Some(0x235C4)); // #55
    assert_eq!(u(85, 68), Some(0x242EE)); // #68
    assert_eq!(u(85, 43), Some(0x66D9)); // #43*
    assert_eq!(u(86, 1), Some(0x9FC5)); // #95
    assert_eq!(u(86, 40), Some(0x9BD6)); // #134*
    // Table 7-12: 85-52 is the same character as JIS 1-22-91.
    assert_eq!(
        addsym::additional_symbol(85, 52).map(|s| s.unicode),
        tables::kanji(22, 91)
    );
}

// ---- Tables 7-14 … 7-17: control codes -----------------------------------

#[test]
fn spec_table_7_15_c0_controls() {
    // NUL BEL APB APF APD APU CS CAN RS US carry no parameters.
    for c in [0x00, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x18, 0x1E, 0x1F] {
        assert_eq!(text(&[c, 0x30, 0x21]), "亜", "{c:02x}");
    }
    assert_eq!(text(&[0x16, 0x41, 0x30, 0x21]), "亜"); // PAPF P1
    assert_eq!(text(&[0x1C, 0x41, 0x42, 0x30, 0x21]), "亜"); // APS P1 P2
    assert_eq!(text(&[0x30, 0x21, 0x0D, 0x30, 0x22]), "亜\n唖"); // APR → line break
    // Blanks of the C0 area are undefined (Table 7-14 Note 2).
    let d = dec(&[0x01, 0x30, 0x21]);
    assert_eq!(d.text, "亜");
    assert_eq!(d.issues[0].kind, IssueKind::UndefinedControl(0x01));
}

#[test]
fn spec_table_7_16_c1_controls() {
    let k = [0x30, 0x21]; // 亜
    let cases: &[&[u8]] = &[
        &[0x80],             // BKF
        &[0x87],             // WHF
        &[0x88],             // SSZ
        &[0x8A],             // NSZ
        &[0x8B, 0x41],       // SZX P1
        &[0x90, 0x48],       // COL P1
        &[0x90, 0x20, 0x4F], // COL 02/0 P2 (palette)
        &[0x91, 0x40],       // FLC P1
        &[0x92, 0x40],       // CDC P1
        &[0x92, 0x20, 0x41], // CDC 02/0 P2
        &[0x93, 0x41],       // POL P1
        &[0x94, 0x44],       // WMM P1
        &[0x95, 0x4F],       // MACRO 04/15
        &[0x97, 0x41],       // HLC P1
        &[0x99],             // SPL
        &[0x9A],             // STL
        &[0x9D, 0x20, 0x45], // TIME 02/0 P2
        &[0x9D, 0x28, 0x41], // TIME 02/8 P2
        // TIME 02/9 P1… I1 P2… I2 P3… I3 P4… I F (12:34:56.789, F = 04/0)
        &[
            0x9D, 0x29, 0x31, 0x32, 0x3B, 0x33, 0x34, 0x3B, 0x35, 0x36, 0x3B, 0x37, 0x38, 0x39,
            0x20, 0x40,
        ],
    ];
    for &c in cases {
        let mut v = c.to_vec();
        v.extend(k);
        assert_eq!(text(&v), "亜", "{c:02x?}");
    }
    // Blanks of the C1 area are undefined.
    let d = dec(&[0x8C, 0x30, 0x21]);
    assert_eq!(d.issues[0].kind, IssueKind::UndefinedControl(0x8C));
}

#[test]
fn spec_table_7_16_rpc() {
    assert_eq!(text(&[0x98, 0x43, 0xA2, 0xA2]), "ああああ");
    let d = dec(&[0x98, 0x40, 0xA2]);
    assert_eq!(d.text, "あ");
    assert_eq!(d.issues[0].kind, IssueKind::RepeatToEndOfLine);
}

#[test]
fn spec_table_7_16_character_size_width() {
    // Normal size (initial, NSZ): alphanumeric and SP fill a full block.
    // Middle (MSZ) and small (SSZ) size: half width.
    let (msz, nsz, ssz) = (0x89, 0x8A, 0x88);
    assert_eq!(
        text(&[0x0E, 0x41, 0x20, msz, 0x41, 0x20, nsz, 0x41]),
        "Ａ\u{3000}A Ａ"
    );
    assert_eq!(text(&[ssz, 0x0E, 0x7E, 0x5C]), "~\u{A5}");
    assert_eq!(text(&[0x0E, 0x7E, 0x5C]), "～￥");
    assert_eq!(text(&[0x8B, 0x60, 0x0E, 0x41]), "A"); // SZX tiny
    assert_eq!(text(&[0x8B, 0x45, 0x0E, 0x41]), "Ａ"); // SZX double height and width
    // Annex E.4 mode ignores the size.
    let annex = DecodeOptions {
        width: WidthMode::AnnexE,
        ..Default::default()
    };
    assert_eq!(
        AribString(vec![0x0E, 0x41, 0x20]).decode_with(&annex).text,
        "A "
    );
}

#[test]
fn spec_table_7_17_csi() {
    let cases: &[&[u8]] = &[
        &[0x9B, 0x37, 0x20, 0x53],                               // SWF 7
        &[0x9B, 0x38, 0x3B, 0x31, 0x3B, 0x32, 0x30, 0x20, 0x53], // SWF P1 I1 P2 I2 P3 I4 F
        &[0x9B, 0x31, 0x32, 0x3B, 0x33, 0x34, 0x20, 0x61],       // ACPS
        &[0x9B, 0x5B],                                           // PLD
        &[0x9B, 0x5C],                                           // PLU
        &[0x9B, 0x30, 0x20, 0x6F],                               // SCS
    ];
    for &c in cases {
        let mut v = c.to_vec();
        v.extend([0x30, 0x21]);
        assert_eq!(text(&v), "亜", "{c:02x?}");
    }
}

#[test]
fn spec_table_7_18_default_macros() {
    let cap = DecodeOptions {
        initial: InitialState::Caption,
        ..Default::default()
    };
    let d = |b: &[u8]| AribString(b.to_vec()).decode_with(&cap);
    // G0 := Alphanumeric; SS3 06/0 (default macro: G0 Kanji, G1 Alnum,
    // G2 Hiragana, G3 Macro, LS0, LS2R) restores Kanji in GL.
    let r = d(&[ESC, 0x28, 0x4A, 0x41, 0x1D, 0x60, 0x30, 0x21]);
    assert!(r.issues.is_empty(), "{:?}", r.issues);
    assert_eq!(r.text, "Ａ亜");
    // 06/14: G0 Katakana, G1 Hiragana, G2 Alphanumeric; GR = G2.
    let r = d(&[0x1D, 0x6E, 0x22, 0xC1]);
    assert!(r.issues.is_empty(), "{:?}", r.issues);
    assert_eq!(r.text, "アＡ");
    // MACRO 04/0 MC body MACRO 04/15 defines; invoking executes the body.
    let r = d(&[
        0x95, 0x40, 0x21, 0x30, 0x21, 0xA2, 0x95, 0x4F, 0x1D, 0x21, 0x1D, 0x21,
    ]);
    assert!(r.issues.is_empty(), "{:?}", r.issues);
    assert_eq!(r.text, "亜あ亜あ");
}

// ---- robustness ----------------------------------------------------------

#[test]
fn spec_drcs_placeholder() {
    // DRCS-1 to G1 (1-byte DRCS form), LS1, 02/1.
    let d = dec(&[ESC, 0x29, 0x20, 0x41, 0x0E, 0x21]);
    assert_eq!(d.text, "\u{FFFD}");
    assert_eq!(d.issues[0].kind, IssueKind::Drcs { set: 1, code: 0x21 });
    assert_eq!(d.issues[0].offset, 5);
    // DRCS-0 to G0 (2-byte).
    let d = dec(&[ESC, 0x24, 0x28, 0x20, 0x40, 0x21, 0x22]);
    assert_eq!(
        d.issues[0].kind,
        IssueKind::Drcs {
            set: 0,
            code: 0x2122
        }
    );
}

#[test]
fn spec_truncated_sequences() {
    for b in [
        &[0x30][..],
        &[ESC],
        &[ESC, 0x24],
        &[0x90],
        &[0x90, 0x20],
        &[0x9B, 0x30],
        &[0x19],
    ] {
        let d = dec(b);
        assert!(
            d.issues.iter().any(|i| i.kind == IssueKind::Truncated),
            "{b:02x?}: {:?}",
            d.issues
        );
    }
}

#[test]
fn spec_macro_expansion_bounded() {
    // A macro whose body invokes another macro 60 times, nested: decoding
    // must stay bounded and report the limit.
    let cap = DecodeOptions {
        initial: InitialState::Caption,
        ..Default::default()
    };
    let mut b = Vec::new();
    for (mc, inner) in [
        (0x21u8, None),
        (0x22, Some(0x21u8)),
        (0x23, Some(0x22)),
        (0x24, Some(0x23)),
    ] {
        b.extend([0x95, 0x40, mc]);
        match inner {
            None => b.extend([0x30, 0x21]),
            Some(i) => (0..60).for_each(|_| b.extend([0x1D, i])),
        }
        b.extend([0x95, 0x4F]);
    }
    b.extend([0x1D, 0x24]);
    let d = AribString(b).decode_with(&cap);
    assert!(d.issues.iter().any(|i| i.kind == IssueKind::MacroLimit));
    assert!(d.text.chars().count() < 100_000);
}
