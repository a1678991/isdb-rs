//! Additional symbols set (F = 03/11): rows 85–86 (additional Kanji,
//! Table 7-11) and rows 90–94 (additional symbols, Table 7-10).
//!
//! The UCS values are transcribed from ARIB STD-B24 v6.4 Vol.1 Part 2
//! §7.2.1.1.1, Table 7-19 "Code Values for Added Symbols Set" (printed pages
//! 101–104). Table 7-19 is restricted to the BMP and gives Private Use Area
//! values for symbols that had no BMP code point. Where the glyph identity
//! is unambiguous this module replaces those PUA values (see [`Mapping`]):
//!
//! * rows 85–86: by the supplementary-plane code point printed in
//!   Table 7-11 for the same graphic symbol number;
//! * duplicates listed under Table 7-19 ("90-58 and 93-30", "91-8 and 2-9",
//!   "91-43 and 93-91", "92-07..10 and 39-15 / 23-78 / 38-92 / 17-63"):
//!   by the code point of the character they duplicate;
//! * otherwise by the code point Unicode 5.2+ assigned to that ARIB symbol
//!   (Enclosed Alphanumeric / Ideographic Supplement), checked against the
//!   glyph and description in Table 7-10 (printed pages 53–70).
//!
//! The remaining PUA values (70 %-size Kanji prefixes 92-26..31, the
//! parenthesised instrument abbreviations 92-56..85 and the disc symbols
//! 92-86/87) have no Unicode equivalent and are kept as Table 7-19 gives
//! them ([`Mapping::PrivateUse`]).

/// How [`AdditionalSymbol::unicode`] was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapping {
    /// Table 7-19 value used as is (a non-PUA code point).
    Table719,
    /// Table 7-19 gives a PUA value; Table 7-11 gives the
    /// supplementary-plane code point used here.
    Table711,
    /// Table 7-19 gives a PUA value and lists the cell as a duplicate of
    /// another character; that character's code point is used.
    Duplicate,
    /// Table 7-19 gives a PUA value; the Unicode 5.2+ code point encoded
    /// for this ARIB symbol is used.
    Unicode52,
    /// Table 7-19 gives a PUA value and no Unicode character exists; the
    /// PUA value is kept.
    PrivateUse,
}

/// One cell of the additional symbols set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdditionalSymbol {
    /// Row (85, 86, 90..=94).
    pub row: u8,
    /// Cell (1..=94).
    pub cell: u8,
    /// UCS value printed in Table 7-19.
    pub table_7_19: u32,
    /// Unicode character produced by the decoder.
    pub unicode: char,
    /// Provenance of `unicode`.
    pub mapping: Mapping,
}

/// Table 7-19, rows 85, 86, 90, 91, 92, 93, 94; 94 cells each, 0 = blank.
const T719: [(u8, [u32; 94]); 7] = [
    (
        85,
        [
            0x3402, 0xE081, 0x4EFD, 0x4EFF, 0x4F9A, 0x4FC9, 0x509C, 0x511E, 0x51BC, 0x351F, 0x5307,
            0x5361, // 1-12
            0x536C, 0x8A79, 0xE084, 0x544D, 0x5496, 0x549C, 0x54A9, 0x550E, 0x554A, 0x5672, 0x56E4,
            0x5733, // 13-24
            0x5734, 0xFA10, 0x5880, 0x59E4, 0x5A23, 0x5A55, 0x5BEC, 0xFA11, 0x37E2, 0x5EAC, 0x5F34,
            0x5F45, // 25-36
            0x5FB7, 0x6017, 0xFA6B, 0x6130, 0x6624, 0x66C8, 0x66D9, 0x66FA, 0x66FB, 0x6852, 0x9FC4,
            0x6911, // 37-48
            0x693B, 0x6A45, 0x6A91, 0x6ADB, 0xE08A, 0xE08B, 0xE08C, 0x6BF1, 0x6CE0, 0x6D2E, 0xFA45,
            0x6DBF, // 49-60
            0x6DCA, 0x6DF8, 0xFA46, 0x6F5E, 0x6FF9, 0x7064, 0xFA6C, 0xE08E, 0x7147, 0x71C1, 0x7200,
            0x739F, // 61-72
            0x73A8, 0x73C9, 0x73D6, 0x741B, 0x7421, 0xFA4A, 0x7426, 0x742A, 0x742C, 0x7439, 0x744B,
            0x3EDA, // 73-84
            0x7575, 0x7581, 0x7772, 0x4093, 0x78C8, 0x78E0, 0x7947, 0x79AE, 0x9FC6,
            0x4103, // 85-94
        ],
    ),
    (
        86,
        [
            0x9FC5, 0x79DA, 0x7A1E, 0x7B7F, 0x7C31, 0x4264, 0x7D8B, 0x7FA1, 0x8118, 0x813A, 0xFA6D,
            0x82AE, // 1-12
            0x845B, 0x84DC, 0x84EC, 0x8559, 0x85CE, 0x8755, 0x87EC, 0x880B, 0x88F5, 0x89D2, 0x8AF6,
            0x8DCE, // 13-24
            0x8FBB, 0x8FF6, 0x90DD, 0x9127, 0x912D, 0x91B2, 0x9233, 0x9288, 0x9321, 0x9348, 0x9592,
            0x96DE, // 25-36
            0x9903, 0x9940, 0x9AD9, 0x9BD6, 0x9DD7, 0x9EB4, 0x9EB5, 0, 0, 0, 0, 0, // 37-48
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 49-60
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 61-72
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 73-84
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 85-94
        ],
    ),
    (
        90,
        [
            0x26CC, 0x26CD, 0x2757, 0x26CF, 0x26D0, 0x26D1, 0, 0x26D2, 0x26D5, 0x26D3, 0x26D4,
            0, // 1-12
            0, 0, 0, 0xE0D8, 0xE0D9, 0, 0, 0x26D6, 0x26D7, 0x26D8, 0x26D9, 0x26DA, // 13-24
            0x26DB, 0x26DC, 0x26DD, 0x26DE, 0x26DF, 0x26E0, 0x26E1, 0x2B55, 0x3248, 0x3249, 0x324A,
            0x324B, // 25-36
            0x324C, 0x324D, 0x324E, 0x324F, 0, 0, 0, 0, 0x2491, 0x2492, 0x2493,
            0xE0F8, // 37-48
            0xE0F9, 0xE0FA, 0xE0FB, 0xE0FC, 0xE0FD, 0xE0FE, 0xE0FF, 0xE180, 0xE181, 0xE182, 0xE183,
            0xE184, // 49-60
            0xE185, 0xE186, 0xE187, 0x2B1B, 0x2B24, 0xE18A, 0xE18B, 0xE18C, 0xE18D, 0xE18E, 0x26BF,
            0xE190, // 61-72
            0xE191, 0xE192, 0xE193, 0xE194, 0xE195, 0xE196, 0xE197, 0xE198, 0xE199, 0xE19A, 0x3299,
            0xE19C, // 73-84
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 85-94
        ],
    ),
    (
        91,
        [
            0x26E3, 0x2B56, 0x2B57, 0x2B58, 0x2B59, 0x2613, 0x328B, 0xE1AE, 0x26E8, 0x3246, 0x3245,
            0x26E9, // 1-12
            0x0FD6, 0x26EA, 0x26EB, 0x26EC, 0x2668, 0x26ED, 0x26EE, 0x26EF, 0x2693, 0x2708, 0x26F0,
            0x26F1, // 13-24
            0x26F2, 0x26F3, 0x26F4, 0x26F5, 0xE1C3, 0x24B9, 0x24C8, 0x26F6, 0xE1C7, 0xE1C8, 0xE1C9,
            0xE1CA, // 25-36
            0xE1CB, 0x26F7, 0x26F8, 0x26F9, 0x26FA, 0xE1D0, 0x260E, 0x26FB, 0x26FC, 0x26FD, 0x26FE,
            0xE1D6, // 37-48
            0x26FF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 49-60
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 61-72
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 73-84
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 85-94
        ],
    ),
    (
        92,
        [
            0x27A1, 0x2B05, 0x2B06, 0x2B07, 0x2B2F, 0x2B2E, 0xE28B, 0xE28C, 0xE28D, 0xE28E, 0x33A1,
            0x33A5, // 1-12
            0x339D, 0x33A0, 0x33A4, 0xE28F, 0x2488, 0x2489, 0x248A, 0x248B, 0x248C, 0x248D, 0x248E,
            0x248F, // 13-24
            0x2490, 0xE290, 0xE291, 0xE292, 0xE293, 0xE294, 0xE295, 0xE296, 0xE297, 0xE298, 0xE299,
            0xE29A, // 25-36
            0xE29B, 0xE29C, 0xE29D, 0xE29E, 0xE29F, 0x3233, 0x3236, 0x3232, 0x3231, 0x3239, 0x3244,
            0x25B6, // 37-48
            0x25C0, 0x3016, 0x3017, 0x27D0, 0x00B2, 0x00B3, 0xE2A4, 0xE2A5, 0xE2A6, 0xE2A7, 0xE2A8,
            0xE2A9, // 49-60
            0xE2AA, 0xE2AB, 0xE2AC, 0xE2AD, 0xE2AE, 0xE2AF, 0xE2B0, 0xE2B1, 0xE2B2, 0xE2B3, 0xE2B4,
            0xE2B5, // 61-72
            0xE2B6, 0xE2B7, 0xE2B8, 0xE2B9, 0xE2BA, 0xE2BB, 0xE2BC, 0xE2BD, 0xE2BE, 0xE2BF, 0xE2C0,
            0xE2C1, // 73-84
            0xE2C2, 0xE3A7, 0xE3A8, 0x3247, 0xE2C4, 0xE2C5, 0x213B, 0, 0, 0, // 85-94
        ],
    ),
    (
        93,
        [
            0x322A, 0x322B, 0x322C, 0x322D, 0x322E, 0x322F, 0x3230, 0x3237, 0x337E, 0x337D, 0x337C,
            0x337B, // 1-12
            0x2116, 0x2121, 0x3036, 0x26BE, 0xE2CD, 0xE2CE, 0xE2CF, 0xE2D0, 0xE2D1, 0xE2D2, 0xE2D3,
            0xE2D4, // 13-24
            0xE2D5, 0xE2D6, 0xE2D7, 0xE2D8, 0xE2D9, 0xE2DA, 0xE2DB, 0xE2DC, 0xE2DD, 0xE2DE, 0xE2DF,
            0xE2E0, // 25-36
            0xE2E1, 0xE2E2, 0x2113, 0x338F, 0x3390, 0x33CA, 0x339E, 0x33A2, 0x3371, 0, 0,
            0x00BD, // 37-48
            0x2189, 0x2153, 0x2154, 0x00BC, 0x00BE, 0x2155, 0x2156, 0x2157, 0x2158, 0x2159, 0x215A,
            0x2150, // 49-60
            0x215B, 0x2151, 0x2152, 0x2600, 0x2601, 0x2602, 0x26C4, 0x2616, 0x2617, 0x26C9, 0x26CA,
            0x2666, // 61-72
            0x2665, 0x2663, 0x2660, 0x26CB, 0x2A00, 0x203C, 0x2049, 0x26C5, 0x2614, 0x26C6, 0x2603,
            0x26C7, // 73-84
            0x26A1, 0x26C8, 0, 0x269E, 0x269F, 0x266C, 0xE2FB, 0, 0, 0, // 85-94
        ],
    ),
    (
        94,
        [
            0x2160, 0x2161, 0x2162, 0x2163, 0x2164, 0x2165, 0x2166, 0x2167, 0x2168, 0x2169, 0x216A,
            0x216B, // 1-12
            0x2470, 0x2471, 0x2472, 0x2473, 0x2474, 0x2475, 0x2476, 0x2477, 0x2478, 0x2479, 0x247A,
            0x247B, // 13-24
            0x247C, 0x247D, 0x247E, 0x247F, 0x3251, 0x3252, 0x3253, 0x3254, 0xE383, 0xE384, 0xE385,
            0xE386, // 25-36
            0xE387, 0xE388, 0xE389, 0xE38A, 0xE38B, 0xE38C, 0xE38D, 0xE38E, 0xE38F, 0xE390, 0xE391,
            0xE392, // 37-48
            0xE393, 0xE394, 0xE395, 0xE396, 0xE397, 0xE398, 0xE399, 0xE39A, 0xE39B, 0xE39C, 0x3255,
            0x3256, // 49-60
            0x3257, 0x3258, 0x3259, 0x325A, 0x2460, 0x2461, 0x2462, 0x2463, 0x2464, 0x2465, 0x2466,
            0x2467, // 61-72
            0x2468, 0x2469, 0x246A, 0x246B, 0x246C, 0x246D, 0x246E, 0x246F, 0x2776, 0x2777, 0x2778,
            0x2779, // 73-84
            0x277A, 0x277B, 0x277C, 0x277D, 0x277E, 0x277F, 0x24EB, 0x24EC, 0x325B,
            0, // 85-94
        ],
    ),
];

/// Replacements for Table 7-19 PUA values: (row, first cell, last cell,
/// first code point, provenance, Table 7-10/7-11 glyph). Code points
/// increase by one per cell across a range.
const REPLACEMENTS: &[(u8, u8, u8, u32, Mapping, &str)] = &[
    // Table 7-11 graphic symbol numbers 2, 15, 53, 54, 55, 68 (ISO/IEC 10646 values).
    (85, 2, 2, 0x20158, Mapping::Table711, "#2"),
    (85, 15, 15, 0x20BB7, Mapping::Table711, "#15"),
    (85, 53, 53, 0x233CC, Mapping::Table711, "#53"),
    (85, 54, 54, 0x233FE, Mapping::Table711, "#54"),
    (85, 55, 55, 0x235C4, Mapping::Table711, "#55"),
    (85, 68, 68, 0x242EE, Mapping::Table711, "#68"),
    // Row 90 (Table 7-10, printed pp.53-56).
    (
        90,
        16,
        16,
        0x1F17F,
        Mapping::Unicode52,
        "parking space (P on black square)",
    ),
    (
        90,
        17,
        17,
        0x1F18A,
        Mapping::Unicode52,
        "parking space closed (crossed P)",
    ),
    (90, 48, 48, 0x1F14A, Mapping::Unicode52, "HDTV [HV]"),
    (90, 49, 49, 0x1F14C, Mapping::Unicode52, "SDTV [SD]"),
    (90, 50, 50, 0x1F13F, Mapping::Unicode52, "progressive [P]"),
    (90, 51, 51, 0x1F146, Mapping::Unicode52, "wide-format [W]"),
    (90, 52, 52, 0x1F14B, Mapping::Unicode52, "multi-view [MV]"),
    (90, 53, 55, 0x1F210, Mapping::Unicode52, "[手][字][双]"),
    (90, 56, 56, 0x1F213, Mapping::Unicode52, "[デ]"),
    (90, 57, 57, 0x1F142, Mapping::Unicode52, "stereo [S]"),
    (90, 58, 60, 0x1F214, Mapping::Unicode52, "[二][多][解]"),
    (90, 61, 61, 0x1F14D, Mapping::Unicode52, "surround [SS]"),
    (90, 62, 62, 0x1F131, Mapping::Unicode52, "B-mode stereo [B]"),
    (90, 63, 63, 0x1F13D, Mapping::Unicode52, "news [N]"),
    (
        90,
        66,
        70,
        0x1F217,
        Mapping::Unicode52,
        "[天][交][映][無][料]",
    ),
    (
        90,
        72,
        81,
        0x1F21C,
        Mapping::Unicode52,
        "[前][後][再][新][初][終][生][販][声][吹]",
    ),
    (
        90,
        82,
        82,
        0x1F14E,
        Mapping::Unicode52,
        "pay-per-view [PPV]",
    ),
    (90, 84, 84, 0x1F200, Mapping::Unicode52, "and others [ほか]"),
    // Row 91 (Table 7-10, printed pp.56-58).
    (
        91,
        8,
        8,
        0x3012,
        Mapping::Duplicate,
        "post office 〒 = Kanji 2-9",
    ),
    (
        91,
        29,
        29,
        0x1F157,
        Mapping::Unicode52,
        "hotel (H on black circle)",
    ),
    (
        91,
        33,
        33,
        0x1F15F,
        Mapping::Unicode52,
        "parking space (P on black circle)",
    ),
    (
        91,
        34,
        34,
        0x1F18B,
        Mapping::Unicode52,
        "interchange [IC] negative",
    ),
    (
        91,
        35,
        35,
        0x1F18D,
        Mapping::Unicode52,
        "service area [SA] negative",
    ),
    (
        91,
        36,
        36,
        0x1F18C,
        Mapping::Unicode52,
        "parking area [PA] negative",
    ),
    (
        91,
        37,
        37,
        0x1F179,
        Mapping::Unicode52,
        "junction [J] negative",
    ),
    (
        91,
        42,
        42,
        0x1F17B,
        Mapping::Unicode52,
        "leisure center [L] negative",
    ),
    (
        91,
        48,
        48,
        0x1F17C,
        Mapping::Unicode52,
        "museum [M] negative",
    ),
    // Row 92 (Table 7-10, printed pp.58-62).
    (92, 7, 7, 0x5E74, Mapping::Duplicate, "年 = Kanji 39-15"),
    (92, 8, 8, 0x6708, Mapping::Duplicate, "月 = Kanji 23-78"),
    (92, 9, 9, 0x65E5, Mapping::Duplicate, "日 = Kanji 38-92"),
    (92, 10, 10, 0x5186, Mapping::Duplicate, "円 = Kanji 17-63"),
    (92, 16, 16, 0x1F100, Mapping::Unicode52, "0."),
    (92, 32, 41, 0x1F101, Mapping::Unicode52, "0, 1, … 9,"),
    (92, 55, 55, 0x1F12D, Mapping::Unicode52, "circled CD"),
    (92, 89, 89, 0x1F190, Mapping::Unicode52, "disc jockey DJ"),
    (92, 90, 90, 0x1F226, Mapping::Unicode52, "performed by [演]"),
    // Row 93 (Table 7-10, printed pp.62-66).
    (
        93,
        17,
        25,
        0x1F240,
        Mapping::Unicode52,
        "〔本〕〔三〕〔二〕〔安〕〔点〕〔打〕〔盗〕〔勝〕〔敗〕",
    ),
    (93, 26, 26, 0x1F12A, Mapping::Unicode52, "〔S〕"),
    (93, 27, 29, 0x1F227, Mapping::Unicode52, "[投][捕][一]"),
    (
        93,
        30,
        30,
        0x1F214,
        Mapping::Duplicate,
        "[二] = additional symbol 90-58",
    ),
    (
        93,
        31,
        38,
        0x1F22A,
        Mapping::Unicode52,
        "[三][遊][左][中][右][指][走][打]",
    ),
    (
        93,
        91,
        91,
        0x260E,
        Mapping::Duplicate,
        "☎ = additional symbol 91-43",
    ),
    // Row 94 (Table 7-10, printed pp.68-69).
    (94, 33, 58, 0x1F110, Mapping::Unicode52, "(A) … (Z)"),
];

fn is_pua(u: u32) -> bool {
    (0xE000..=0xF8FF).contains(&u)
}

/// Look up an additional-symbols cell.
///
/// ARIB STD-B24 Vol.1 Part 2 Tables 7-10, 7-11 and 7-19. `None` for cells
/// that are blank in Table 7-19 and for rows other than 85, 86, 90–94.
pub fn additional_symbol(row: u8, cell: u8) -> Option<AdditionalSymbol> {
    if !(1..=94).contains(&cell) {
        return None;
    }
    let (_, cells) = T719.iter().find(|(r, _)| *r == row)?;
    let table_7_19 = cells[cell as usize - 1];
    if table_7_19 == 0 {
        return None;
    }
    let (unicode, mapping) = REPLACEMENTS
        .iter()
        .find(|&&(r, a, b, ..)| r == row && (a..=b).contains(&cell))
        .map(|&(_, a, _, first, m, _)| (first + (cell - a) as u32, m))
        .unwrap_or(if is_pua(table_7_19) {
            (table_7_19, Mapping::PrivateUse)
        } else {
            (table_7_19, Mapping::Table719)
        });
    Some(AdditionalSymbol {
        row,
        cell,
        table_7_19,
        unicode: char::from_u32(unicode)?,
        mapping,
    })
}

/// All defined cells of the additional symbols set, in row/cell order.
pub fn all() -> impl Iterator<Item = AdditionalSymbol> {
    T719.iter()
        .flat_map(|&(row, _)| (1..=94).filter_map(move |cell| additional_symbol(row, cell)))
}
