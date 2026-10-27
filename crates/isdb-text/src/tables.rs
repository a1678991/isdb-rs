//! Code tables of the graphic sets, mapped to Unicode.
//!
//! ARIB STD-B24 v6.4 Vol.1 Part 2 §7.1.1.3 (Tables 7-4 … 7-12), §7.2.1.1.1
//! (Table 7-19, UCS code values of the additional symbols) and Annex E
//! (conversion from the 8-bit code to UCS).
//!
//! The 1-byte sets are transcribed position by position from the code
//! table figures (printed pages 47–51). The Kanji set rows 1–84 are JIS X 0208
//! (Table 7-4 (1)–(8) show exactly the JIS X 0208:1990 repertoire there,
//! rows 9–15 and 85–89 empty); the row/cell → Unicode mapping of those rows
//! comes from the `encoding_rs` EUC-JP decoder with the corrections listed
//! in [`KANJI_OVERRIDES`].

use std::collections::HashMap;
use std::sync::OnceLock;

/// Alphanumeric set, 02/1 … 07/14, as transcribed from Table 7-5.
///
/// 05/12 is the YEN SIGN (Table 7-5 glyph; JIS X 0201 Roman, §7.2.1.1.1
/// "For code values of the alphanumerical set, refer to JIS X 0201"),
/// and 07/14 is the JIS X 0201 OVERLINE, which Annex E.4 converts to
/// TILDE U+007E ("OVER LINE (0x7E) defined in JIS X 0201 is converted to
/// TILDE (0x007E)").
pub const ALPHANUMERIC: &str = "!\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\u{A5}]^_`abcdefghijklmnopqrstuvwxyz{|}~";

/// Hiragana set, 02/1 … 07/14, transcribed from Table 7-7. `\0` marks the
/// three empty positions 07/4 … 07/6.
pub const HIRAGANA: &str = concat!(
    "ぁあぃいぅうぇえぉおかがきぎく",   // 02/1 - 02/15
    "ぐけげこごさざしじすずせぜそぞた", // 03/0 - 03/15
    "だちぢっつづてでとどなにぬねのは", // 04/0 - 04/15
    "ばぱひびぴふぶぷへべぺほぼぽまみ", // 05/0 - 05/15
    "むめもゃやゅゆょよらりるれろゎわ", // 06/0 - 06/15
    "ゐゑをん\0\0\0ゝゞー。「」、・",   // 07/0 - 07/14
);

/// Katakana set, 02/1 … 07/14, transcribed from Table 7-6.
pub const KATAKANA: &str = concat!(
    "ァアィイゥウェエォオカガキギク",   // 02/1 - 02/15
    "グケゲコゴサザシジスズセゼソゾタ", // 03/0 - 03/15
    "ダチヂッツヅテデトドナニヌネノハ", // 04/0 - 04/15
    "バパヒビピフブプヘベペホボポマミ", // 05/0 - 05/15
    "ムメモャヤュユョヨラリルレロヮワ", // 06/0 - 06/15
    "ヰヱヲンヴヵヶヽヾー。「」、・",   // 07/0 - 07/14
);

/// JIS X 0201 katakana set, 02/1 … 05/15, transcribed from Table 7-9
/// (columns 6 and 7 are "Undefined"). Mapped to the halfwidth forms
/// U+FF61 … U+FF9F, the JIS X 0221 mapping of JIS X 0201 katakana
/// (Annex E.1).
pub const JIS_X0201_KATAKANA: &str = concat!(
    "｡｢｣､･ｦｧｨｩｪｫｬｭｮｯ",  // 02/1 - 02/15
    "ｰｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿ", // 03/0 - 03/15
    "ﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏ", // 04/0 - 04/15
    "ﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝﾞﾟ",   // 05/0 - 05/15
);

/// Characters of a 1-byte table string, indexed by `code - 0x21`.
fn table_chars(table: &'static str) -> &'static [char] {
    static CACHE: OnceLock<[Vec<char>; 4]> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        [ALPHANUMERIC, HIRAGANA, KATAKANA, JIS_X0201_KATAKANA].map(|t| t.chars().collect())
    });
    let i = match table {
        t if t == ALPHANUMERIC => 0,
        t if t == HIRAGANA => 1,
        t if t == KATAKANA => 2,
        _ => 3,
    };
    &all[i]
}

/// Look up a 1-byte table string at code `c` (02/1 … 07/14).
fn one_byte(table: &'static str, c: u8) -> Option<char> {
    let i = (c as usize).checked_sub(0x21)?;
    table_chars(table).get(i).copied().filter(|&ch| ch != '\0')
}

/// Alphanumeric set character at GL code `c` (0x21..=0x7E), as the
/// half-width (JIS X 0201) code point. Table 7-5, Annex E.4.
pub fn alphanumeric(c: u8) -> Option<char> {
    one_byte(ALPHANUMERIC, c)
}

/// Hiragana set character at GL code `c`. Table 7-7.
pub fn hiragana(c: u8) -> Option<char> {
    one_byte(HIRAGANA, c)
}

/// Katakana set character at GL code `c`. Table 7-6.
pub fn katakana(c: u8) -> Option<char> {
    one_byte(KATAKANA, c)
}

/// JIS X 0201 katakana set character at GL code `c`. Table 7-9.
pub fn jis_x0201_katakana(c: u8) -> Option<char> {
    one_byte(JIS_X0201_KATAKANA, c)
}

/// Full-width form of a half-width alphanumeric character, used when the
/// alphanumeric set is presented in a normal-size (full-width) character
/// field: U+0021..U+007E → U+FF01..U+FF5E, YEN SIGN → FULLWIDTH YEN SIGN.
pub fn fullwidth(c: char) -> char {
    match c {
        '!'..='~' => char::from_u32(c as u32 - 0x21 + 0xFF01).unwrap_or(c),
        '\u{A5}' => '\u{FFE5}',
        _ => c,
    }
}

/// Non-spacing characters of the Kanji set and their UCS mapping.
///
/// ARIB STD-B24 Vol.1 Part 2 §7.1.1.4 (row 1 cells 13–18, row 2 cell 94
/// are non-spacing) and Annex E.4, Table E-1 (UCS combining characters).
pub const NON_SPACING: [(u8, u8, char); 7] = [
    (1, 13, '\u{0301}'), // ACUTE ACCENT → COMBINING ACUTE ACCENT
    (1, 14, '\u{0300}'), // GRAVE ACCENT → COMBINING GRAVE ACCENT
    (1, 15, '\u{0308}'), // DIAERESIS → COMBINING DIAERESIS
    (1, 16, '\u{0302}'), // CIRCUMFLEX ACCENT → COMBINING CIRCUMFLEX ACCENT
    (1, 17, '\u{0305}'), // OVERLINE → COMBINING OVERLINE
    (1, 18, '\u{0332}'), // LOW LINE → COMBINING LOW LINE
    (2, 94, '\u{20DD}'), // LARGE CIRCLE → COMBINING ENCLOSING CIRCLE
];

/// Whether `c` is one of the combining characters of Table E-1.
pub fn is_non_spacing(c: char) -> bool {
    NON_SPACING.iter().any(|&(_, _, u)| u == c)
}

/// Corrections to the `encoding_rs` (WHATWG) JIS X 0208 mapping.
///
/// Annex E.1: "Mapping a character code in the tables defined in JIS X0201,
/// JIS X0208 … onto a corresponding character code in UCS complies with
/// Appendix 2, JIS X0221-1:2001. When a difference is found between
/// Appendix 2, JIS X0221-1:2001 and JIS X0213:2004, JIS X0213:2004 should
/// be used." The WHATWG index follows the Windows code page for these seven
/// cells; JIS X 0221 / JIS X 0213:2004 map them as below (1-29 per
/// JIS X 0213:2004, which differs from JIS X 0221's U+2015).
pub const KANJI_OVERRIDES: [(u8, u8, char); 7] = [
    (1, 29, '\u{2014}'), // EM DASH          (WHATWG: U+2015)
    (1, 33, '\u{301C}'), // WAVE DASH        (WHATWG: U+FF5E)
    (1, 34, '\u{2016}'), // DOUBLE VERTICAL LINE (WHATWG: U+2225)
    (1, 61, '\u{2212}'), // MINUS SIGN       (WHATWG: U+FF0D)
    (1, 81, '\u{00A2}'), // CENT SIGN        (WHATWG: U+FFE0)
    (1, 82, '\u{00A3}'), // POUND SIGN       (WHATWG: U+FFE1)
    (2, 44, '\u{00AC}'), // NOT SIGN         (WHATWG: U+FFE2)
];

fn jis_x0208_rows(row: u8) -> bool {
    (1..=8).contains(&row) || (16..=84).contains(&row)
}

/// JIS X 0208 (rows 1–8, 16–84) via encoding_rs, with [`KANJI_OVERRIDES`]
/// applied. Index `(row-1)*94 + (cell-1)`, `'\0'` = undefined.
fn jis_x0208_table() -> &'static [char] {
    static T: OnceLock<Vec<char>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec!['\0'; 94 * 94];
        for row in 1..=94u8 {
            if !jis_x0208_rows(row) {
                continue;
            }
            for cell in 1..=94u8 {
                let euc = [row + 0xA0, cell + 0xA0];
                let (s, _, had_errors) = encoding_rs::EUC_JP.decode(&euc);
                let mut it = s.chars();
                if let (false, Some(c), None) = (had_errors, it.next(), it.next()) {
                    t[(row as usize - 1) * 94 + cell as usize - 1] = c;
                }
            }
        }
        for &(row, cell, c) in &KANJI_OVERRIDES {
            t[(row as usize - 1) * 94 + cell as usize - 1] = c;
        }
        t
    })
}

/// JIS X 0208 row/cell → Unicode (rows 1–8, 16–84), Annex E.1 mapping
/// (spacing forms; the Kanji set's non-spacing cells are handled by
/// [`kanji`]).
pub fn jis_x0208(row: u8, cell: u8) -> Option<char> {
    if !(1..=94).contains(&row) || !(1..=94).contains(&cell) {
        return None;
    }
    let c = jis_x0208_table()[(row as usize - 1) * 94 + cell as usize - 1];
    (c != '\0').then_some(c)
}

/// Kanji set (Table 7-4) row/cell → Unicode.
///
/// Rows 1–84: JIS X 0208 with the non-spacing cells of §7.1.1.4 mapped to
/// combining characters per Table E-1. Rows 90–94: the additional symbols
/// (Table 7-10 "contains the same characters as those in the table 7-4
/// except the range from Row 90, Cell 45 to Cell 63, and the range from
/// Row 90, Cell 66 to Cell 84", which exist only in the additional symbols
/// set; §7.2.1.1.1 maps Kanji rows 90–94 through Table 7-19). Rows 9–15
/// and 85–89 are empty in Table 7-4.
pub fn kanji(row: u8, cell: u8) -> Option<char> {
    if let Some(&(_, _, c)) = NON_SPACING.iter().find(|&&(r, k, _)| r == row && k == cell) {
        return Some(c);
    }
    match row {
        90 if (45..=63).contains(&cell) || (66..=84).contains(&cell) => None,
        90..=94 => crate::addsym::additional_symbol(row, cell).map(|s| s.unicode),
        _ => jis_x0208(row, cell),
    }
}

/// Additional symbols set (F = 03/11) row/cell → Unicode: rows 85–86
/// (Table 7-11 additional Kanji) and 90–94 (Table 7-10 additional symbols).
pub fn additional_symbols(row: u8, cell: u8) -> Option<char> {
    crate::addsym::additional_symbol(row, cell).map(|s| s.unicode)
}

/// Reverse map for the encoder: Unicode → (row, cell) of the Kanji set.
/// Where several cells decode to the same character the first in row/cell
/// order wins (all of them decode identically).
pub(crate) fn kanji_reverse() -> &'static HashMap<char, (u8, u8)> {
    static M: OnceLock<HashMap<char, (u8, u8)>> = OnceLock::new();
    M.get_or_init(|| reverse(kanji))
}

/// Reverse map for the encoder: Unicode → (row, cell) of the additional
/// symbols set.
pub(crate) fn additional_reverse() -> &'static HashMap<char, (u8, u8)> {
    static M: OnceLock<HashMap<char, (u8, u8)>> = OnceLock::new();
    M.get_or_init(|| reverse(additional_symbols))
}

fn reverse(f: fn(u8, u8) -> Option<char>) -> HashMap<char, (u8, u8)> {
    let mut m = HashMap::new();
    for row in 1..=94u8 {
        for cell in 1..=94u8 {
            if let Some(c) = f(row, cell) {
                m.entry(c).or_insert((row, cell));
            }
        }
    }
    m
}

/// Reverse lookup in a 1-byte table: Unicode → GL code (0x21..=0x7E).
pub(crate) fn one_byte_reverse(table: &'static str, c: char) -> Option<u8> {
    if c == '\0' {
        return None;
    }
    table_chars(table)
        .iter()
        .zip(0x21u8..=0x7E)
        .find(|&(&t, _)| t == c)
        .map(|(_, code)| code)
}
