//! Graphic sets, their final bytes, and the designation / invocation escape
//! sequences of the 8-unit code.
//!
//! ARIB STD-B24 v6.4 Vol.1 Part 2 §7.1.1.1, Table 7-1 (invocation),
//! Table 7-2 (designation), Table 7-3 (classification and final bytes).

/// A graphic set that can be designated to G0..G3.
///
/// ARIB STD-B24 Vol.1 Part 2 §7.1.1.2, Table 7-3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GraphicSet {
    /// Kanji set, Table 7-4 (2-byte, F = 04/2).
    Kanji,
    /// Alphanumeric set, Table 7-5 (1-byte, F = 04/10).
    Alphanumeric,
    /// Hiragana set, Table 7-7 (1-byte, F = 03/0).
    Hiragana,
    /// Katakana set, Table 7-6 (1-byte, F = 03/1).
    Katakana,
    /// Mosaic set A, Table 7-8 (1) (1-byte, F = 03/2).
    MosaicA,
    /// Mosaic set B, Table 7-8 (2) (1-byte, F = 03/3).
    MosaicB,
    /// Mosaic set C (non-spacing), Table 7-8 (3) (1-byte, F = 03/4).
    MosaicC,
    /// Mosaic set D (non-spacing), Table 7-8 (4) (1-byte, F = 03/5).
    MosaicD,
    /// Proportional alphanumeric set, Table 7-5 (1-byte, F = 03/6).
    ProportionalAlphanumeric,
    /// Proportional hiragana set, Table 7-7 (1-byte, F = 03/7).
    ProportionalHiragana,
    /// Proportional katakana set, Table 7-6 (1-byte, F = 03/8).
    ProportionalKatakana,
    /// JIS X 0201 katakana set, Table 7-9 (1-byte, F = 04/9).
    JisX0201Katakana,
    /// JIS compatible Kanji Plane 1, §7.1.1.3 (2-byte, F = 03/9).
    JisKanjiPlane1,
    /// JIS compatible Kanji Plane 2, §7.1.1.3 (2-byte, F = 03/10).
    JisKanjiPlane2,
    /// Additional symbols set, Tables 7-10 / 7-11 (2-byte, F = 03/11).
    AdditionalSymbols,
    /// DRCS-0 (2-byte) or DRCS-1..DRCS-15 (1-byte), §7.1.1.5, Table 7-3.
    Drcs(u8),
    /// Macro set, §7.1.1.6, Table 7-3 (1-byte, F = 07/0, designated with
    /// the DRCS form of Table 7-2).
    Macro,
}

impl GraphicSet {
    /// Bytes per character: 2 for the 2-byte sets of Table 7-3, else 1.
    ///
    /// ARIB STD-B24 Vol.1 Part 2 Table 7-3 "Remarks" column.
    pub fn bytes_per_char(self) -> usize {
        match self {
            GraphicSet::Kanji
            | GraphicSet::JisKanjiPlane1
            | GraphicSet::JisKanjiPlane2
            | GraphicSet::AdditionalSymbols
            | GraphicSet::Drcs(0) => 2,
            _ => 1,
        }
    }

    /// True for the sets designated with the DRCS form of Table 7-2
    /// (intermediate byte 02/0 before F): DRCS-0..15 and the macro set.
    pub fn is_drcs_form(self) -> bool {
        matches!(self, GraphicSet::Drcs(_) | GraphicSet::Macro)
    }

    /// Final byte F of the designation sequence.
    ///
    /// ARIB STD-B24 Vol.1 Part 2 Table 7-3.
    pub fn final_byte(self) -> u8 {
        match self {
            GraphicSet::Kanji => 0x42,
            GraphicSet::Alphanumeric => 0x4A,
            GraphicSet::Hiragana => 0x30,
            GraphicSet::Katakana => 0x31,
            GraphicSet::MosaicA => 0x32,
            GraphicSet::MosaicB => 0x33,
            GraphicSet::MosaicC => 0x34,
            GraphicSet::MosaicD => 0x35,
            GraphicSet::ProportionalAlphanumeric => 0x36,
            GraphicSet::ProportionalHiragana => 0x37,
            GraphicSet::ProportionalKatakana => 0x38,
            GraphicSet::JisX0201Katakana => 0x49,
            GraphicSet::JisKanjiPlane1 => 0x39,
            GraphicSet::JisKanjiPlane2 => 0x3A,
            GraphicSet::AdditionalSymbols => 0x3B,
            GraphicSet::Drcs(n) => 0x40 + (n & 0x0F),
            GraphicSet::Macro => 0x70,
        }
    }

    /// Set selected by a designation, given whether the sequence was a
    /// 2-byte one (`ESC 02/4 …`) and whether it used the DRCS form
    /// (`… 02/0 F`). `None` if Table 7-3 has no such set.
    ///
    /// ARIB STD-B24 Vol.1 Part 2 Table 7-2 and Table 7-3.
    pub fn from_designation(two_byte: bool, drcs: bool, f: u8) -> Option<GraphicSet> {
        match (two_byte, drcs) {
            (false, false) => Some(match f {
                0x4A => GraphicSet::Alphanumeric,
                0x30 => GraphicSet::Hiragana,
                0x31 => GraphicSet::Katakana,
                0x32 => GraphicSet::MosaicA,
                0x33 => GraphicSet::MosaicB,
                0x34 => GraphicSet::MosaicC,
                0x35 => GraphicSet::MosaicD,
                0x36 => GraphicSet::ProportionalAlphanumeric,
                0x37 => GraphicSet::ProportionalHiragana,
                0x38 => GraphicSet::ProportionalKatakana,
                0x49 => GraphicSet::JisX0201Katakana,
                _ => return None,
            }),
            (true, false) => Some(match f {
                0x42 => GraphicSet::Kanji,
                0x39 => GraphicSet::JisKanjiPlane1,
                0x3A => GraphicSet::JisKanjiPlane2,
                0x3B => GraphicSet::AdditionalSymbols,
                _ => return None,
            }),
            (false, true) => match f {
                0x41..=0x4F => Some(GraphicSet::Drcs(f - 0x40)),
                0x70 => Some(GraphicSet::Macro),
                _ => None,
            },
            (true, true) => (f == 0x40).then_some(GraphicSet::Drcs(0)),
        }
    }

    /// The escape sequence (starting with ESC) designating `self` to G`n`
    /// (`n` in 0..=3).
    ///
    /// ARIB STD-B24 Vol.1 Part 2 Table 7-2. For a 2-byte set to G0 the
    /// table gives `ESC 02/4 F` (no 02/8); for every other combination the
    /// intermediate is 02/8 + n.
    pub fn designation(self, n: u8) -> Vec<u8> {
        let n = n & 3;
        let mut v = vec![ESC];
        if self.bytes_per_char() == 2 {
            v.push(0x24);
            if n != 0 || self.is_drcs_form() {
                v.push(0x28 + n);
            }
        } else {
            v.push(0x28 + n);
        }
        if self.is_drcs_form() {
            v.push(0x20);
        }
        v.push(self.final_byte());
        v
    }
}

/// ESC, 01/11. ARIB STD-B24 Vol.1 Part 2 Table 7-1 note (1), Table 7-14.
pub const ESC: u8 = 0x1B;

/// Locking / single shifts. ARIB STD-B24 Vol.1 Part 2 Table 7-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shift {
    /// LS0 (00/15): G0 → GL, locking.
    Ls0,
    /// LS1 (00/14): G1 → GL, locking.
    Ls1,
    /// LS2 (ESC 06/14): G2 → GL, locking.
    Ls2,
    /// LS3 (ESC 06/15): G3 → GL, locking.
    Ls3,
    /// LS1R (ESC 07/14): G1 → GR, locking.
    Ls1r,
    /// LS2R (ESC 07/13): G2 → GR, locking.
    Ls2r,
    /// LS3R (ESC 07/12): G3 → GR, locking.
    Ls3r,
    /// SS2 (01/9): G2 → GL for one character.
    Ss2,
    /// SS3 (01/13): G3 → GL for one character.
    Ss3,
}

impl Shift {
    /// Coded representation. ARIB STD-B24 Vol.1 Part 2 Table 7-1.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Shift::Ls0 => &[0x0F],
            Shift::Ls1 => &[0x0E],
            Shift::Ls2 => &[ESC, 0x6E],
            Shift::Ls3 => &[ESC, 0x6F],
            Shift::Ls1r => &[ESC, 0x7E],
            Shift::Ls2r => &[ESC, 0x7D],
            Shift::Ls3r => &[ESC, 0x7C],
            Shift::Ss2 => &[0x19],
            Shift::Ss3 => &[0x1D],
        }
    }

    /// The code element (0..=3 for G0..G3) this shift invokes.
    pub fn element(self) -> u8 {
        match self {
            Shift::Ls0 => 0,
            Shift::Ls1 | Shift::Ls1r => 1,
            Shift::Ls2 | Shift::Ls2r | Shift::Ss2 => 2,
            Shift::Ls3 | Shift::Ls3r | Shift::Ss3 => 3,
        }
    }
}

/// Character size selected by SSZ / MSZ / NSZ / SZX.
///
/// ARIB STD-B24 Vol.1 Part 2 Table 7-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharSize {
    /// SSZ (08/8): small size.
    Small,
    /// MSZ (08/9): middle size (half width).
    Middle,
    /// NSZ (08/10): normal size.
    Normal,
    /// SZX (08/11) with its parameter P1 (06/0 tiny, 04/1 double height,
    /// 04/4 double width, 04/5 double height and width, 06/11 special 1,
    /// 06/4 special 2).
    Szx(u8),
}

impl CharSize {
    /// Whether the character field is narrower than a normal-size field,
    /// i.e. whether an alphanumeric character is presented half-width.
    /// SSZ (1/2 × 1/2), MSZ (1/2 × 1) and SZX tiny are narrow; NSZ and the
    /// other SZX sizes are not. (Sizes per Part 3 Table 4-1:
    /// "standard, 1 x 2, 2 x 1, 2 x 2, 1/2 x 1, and 1/2 x 1/2".)
    pub fn is_narrow(self) -> bool {
        matches!(
            self,
            CharSize::Small | CharSize::Middle | CharSize::Szx(0x60)
        )
    }
}

/// Initial designation/invocation/size state before the first byte of a
/// string.
///
/// The caption state is ARIB STD-B24 Vol.1 Part 3 Chapter 8, Table 8-2
/// ("Invocation and designation of code", character coding). SI follows
/// the same table except G3: SI strings observed off-air invoke G3 with
/// LS3R and send katakana without designating anything, so G3 must start
/// as the katakana set (see SPEC-NOTES.md, "initial state for SI").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InitialState {
    /// SI / 8-unit code strings: G0 Kanji, G1 Alphanumeric, G2 Hiragana,
    /// G3 Katakana; GL = G0, GR = G2; normal size.
    #[default]
    Si,
    /// Caption text (Part 3 Table 8-2): G0 Kanji, G1 Alphanumeric,
    /// G2 Hiragana, G3 Macro; GL = G0 (LS0), GR = G2 (LS2R); size 1 × 1.
    Caption,
}

impl InitialState {
    /// G0..G3 designations.
    pub fn designations(self) -> [GraphicSet; 4] {
        let g3 = match self {
            InitialState::Si => GraphicSet::Katakana,
            InitialState::Caption => GraphicSet::Macro,
        };
        [
            GraphicSet::Kanji,
            GraphicSet::Alphanumeric,
            GraphicSet::Hiragana,
            g3,
        ]
    }

    /// (GL, GR) code elements: G0 in GL, G2 in GR (Table 8-2).
    pub fn invocations(self) -> (u8, u8) {
        (0, 2)
    }
}
