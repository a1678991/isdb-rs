//! Modulation-code combinations (BO.1408-1 Table 1), their spectrum
//! efficiency, the slot units of MIC Ordinance No. 87 annexed table 48
//! 別記 3, and the punctured codes of Table 2 / annexed table 49 note 4.

use std::fmt;

/// A modulation-code combination of the main signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Combination {
    /// BPSK, r = 1/2 (word 0001).
    Bpsk1_2,
    /// QPSK, r = 1/2 (0010).
    Qpsk1_2,
    /// QPSK, r = 2/3 (0011).
    Qpsk2_3,
    /// QPSK, r = 3/4 (0100).
    Qpsk3_4,
    /// QPSK, r = 5/6 (0101).
    Qpsk5_6,
    /// QPSK, r = 7/8 (0110).
    Qpsk7_8,
    /// Trellis-coded 8PSK, r = 2/3 (0111).
    Tc8psk2_3,
}

/// The modulation of a combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modulation {
    /// BPSK.
    Bpsk,
    /// QPSK.
    Qpsk,
    /// Trellis-coded 8PSK (B1 uncoded, B0 into the K = 7 code).
    Tc8psk,
}

/// The word of "dummy" / unused combination entries (Table 1: 1111), the
/// code of [`CombinationWord::Dummy`].
pub const DUMMY_WORD: u8 = 0b1111;

/// A word Table 1 does not assign (0000 or 1000–1110), as decoded.
///
/// Only [`CombinationWord::from_word`] builds one, after it has excluded
/// the seven assigned words and the dummy word: the field is private, so a
/// [`Reserved`](CombinationWord::Reserved) value is canonical by
/// construction (one value per wire code, never the spelling of `Dummy`)
/// and `==` on the enum is wire equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReservedCode(u8);

impl ReservedCode {
    /// `w` must be a reserved word of Table 1, already masked to 4 bits;
    /// only `from_word` upholds this.
    pub(crate) const fn new_unchecked(w: u8) -> Self {
        Self(w)
    }

    /// The 4-bit word.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Binary for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for ReservedCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

/// A reserved word cannot be spelled as a [`CombinationWord`] outside this
/// crate; the only way in is [`CombinationWord::from_word`], so a decoded
/// value is canonical:
///
/// ```compile_fail,E0624
/// use isdbs_core::combination::{CombinationWord, ReservedCode};
/// let _ = CombinationWord::Reserved(ReservedCode::new_unchecked(0b1000));
/// ```
///
/// ```
/// use isdbs_core::CombinationWord;
/// assert_eq!(CombinationWord::from_word(0b1000), CombinationWord::from_word(0xF8));
/// assert_eq!(CombinationWord::from_word(0b1000).word(), 0b1000);
/// ```
#[doc(hidden)]
pub const _COMBINATION_WORD_CANONICAL: () = ();

/// The 4-bit word of a Fig. 8 combination entry: the whole of Table 1,
/// kept lossless. The seven assigned words 0001–0111 are a
/// [`Combination`]; 1111 is "Dummy" (§6.1.2: the entries not in use, with
/// zero slots); 0000 and 1000–1110 are "Reserved" and keep their value, so
/// `from_word(w).word() == w` for every 4-bit `w` and the TMCC codec stays
/// bit-exact. The `Reserved` payload is a [`ReservedCode`] only
/// `from_word` can build, so every value is canonical and `==` is wire
/// equality. [`crate::tmcc::TmccInfo::validate`] refuses `Reserved`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CombinationWord {
    /// 0001–0111: a modulation-code combination.
    Assigned(Combination),
    /// 1111: "Dummy".
    Dummy,
    /// 0000 or 1000–1110: "Reserved", the 4-bit word as received.
    Reserved(ReservedCode),
}

impl CombinationWord {
    /// The word of a 4-bit Table 1 code (higher bits of `w` are ignored).
    /// Total: every code has a variant.
    pub const fn from_word(w: u8) -> Self {
        let w = w & 0xF;
        match Combination::from_word(w) {
            Some(c) => Self::Assigned(c),
            None if w == DUMMY_WORD => Self::Dummy,
            None => Self::Reserved(ReservedCode::new_unchecked(w)),
        }
    }

    /// The 4-bit Table 1 code: the inverse of [`from_word`](Self::from_word).
    pub const fn word(self) -> u8 {
        match self {
            Self::Assigned(c) => c.word(),
            Self::Dummy => DUMMY_WORD,
            Self::Reserved(w) => w.get(),
        }
    }

    /// The combination of an assigned word; `None` for dummy and reserved.
    pub const fn combination(self) -> Option<Combination> {
        match self {
            Self::Assigned(c) => Some(c),
            Self::Dummy | Self::Reserved(_) => None,
        }
    }
}

impl Combination {
    /// Every combination, from the most to the least efficient (the slot
    /// order of BO.1408-1 §3.2).
    pub const ALL: [Self; 7] = [
        Self::Tc8psk2_3,
        Self::Qpsk7_8,
        Self::Qpsk5_6,
        Self::Qpsk3_4,
        Self::Qpsk2_3,
        Self::Qpsk1_2,
        Self::Bpsk1_2,
    ];

    /// The 4-bit word of Table 1.
    pub const fn word(self) -> u8 {
        match self {
            Self::Bpsk1_2 => 0b0001,
            Self::Qpsk1_2 => 0b0010,
            Self::Qpsk2_3 => 0b0011,
            Self::Qpsk3_4 => 0b0100,
            Self::Qpsk5_6 => 0b0101,
            Self::Qpsk7_8 => 0b0110,
            Self::Tc8psk2_3 => 0b0111,
        }
    }

    /// The combination of a Table 1 word (`None` for reserved words and
    /// for 1111, the dummy).
    pub const fn from_word(w: u8) -> Option<Self> {
        Some(match w {
            0b0001 => Self::Bpsk1_2,
            0b0010 => Self::Qpsk1_2,
            0b0011 => Self::Qpsk2_3,
            0b0100 => Self::Qpsk3_4,
            0b0101 => Self::Qpsk5_6,
            0b0110 => Self::Qpsk7_8,
            0b0111 => Self::Tc8psk2_3,
            _ => return None,
        })
    }

    /// The modulation.
    pub const fn modulation(self) -> Modulation {
        match self {
            Self::Bpsk1_2 => Modulation::Bpsk,
            Self::Tc8psk2_3 => Modulation::Tc8psk,
            _ => Modulation::Qpsk,
        }
    }

    /// Code rate as (numerator, denominator).
    pub const fn rate(self) -> (usize, usize) {
        match self {
            Self::Bpsk1_2 | Self::Qpsk1_2 => (1, 2),
            Self::Qpsk2_3 | Self::Tc8psk2_3 => (2, 3),
            Self::Qpsk3_4 => (3, 4),
            Self::Qpsk5_6 => (5, 6),
            Self::Qpsk7_8 => (7, 8),
        }
    }

    /// Coded bits per symbol (1, 2 or 3).
    pub const fn bits_per_symbol(self) -> usize {
        match self.modulation() {
            Modulation::Bpsk => 1,
            Modulation::Qpsk => 2,
            Modulation::Tc8psk => 3,
        }
    }

    /// Spectrum efficiency E_k as (numerator, denominator) information bits
    /// per symbol (equation (1) of BO.1408-1 §3.2).
    pub const fn efficiency(self) -> (usize, usize) {
        match self {
            // TC8PSK: B1 uncoded plus B0 at rate 1/2 = 2 bits per symbol.
            Self::Tc8psk2_3 => (2, 1),
            _ => {
                let (n, d) = self.rate();
                (n * self.bits_per_symbol(), d)
            }
        }
    }

    /// Slot unit of annexed table 48 別記 3: (slots per unit, valid slots
    /// per unit). The other slots of a unit are dummy slots.
    pub const fn slot_unit(self) -> (usize, usize) {
        match self {
            Self::Bpsk1_2 => (4, 1),
            Self::Qpsk1_2 => (2, 1),
            Self::Qpsk2_3 => (3, 2),
            Self::Qpsk3_4 => (4, 3),
            Self::Qpsk5_6 => (6, 5),
            Self::Qpsk7_8 => (8, 7),
            Self::Tc8psk2_3 => (1, 1),
        }
    }

    /// Symbols one slot unit occupies on air: `slots per unit` TC8PSK slot
    /// durations of 8·203 / E_max = 812 symbols (the valid slots coded at
    /// this combination fill exactly that time, BO.1408-1 equations (1)–(4)).
    pub const fn symbols_per_unit(self) -> usize {
        self.slot_unit().0 * 812
    }

    /// The puncturing of Table 2 / annexed table 49 note 4: the X and Y
    /// rows (one character per input bit; `1` = sent) and the order in
    /// which the kept bits form the (P1, P0) pairs. `None` for the codes
    /// without puncturing (rate 1/2, and TC8PSK's B0).
    pub const fn puncturing(self) -> Option<Puncture> {
        match self {
            Self::Qpsk2_3 => Some(Puncture::R2_3),
            Self::Qpsk3_4 => Some(Puncture::R3_4),
            Self::Qpsk5_6 => Some(Puncture::R5_6),
            Self::Qpsk7_8 => Some(Puncture::R7_8),
            _ => None,
        }
    }
}

impl std::fmt::Display for Combination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (n, d) = self.rate();
        let m = match self.modulation() {
            Modulation::Bpsk => "BPSK",
            Modulation::Qpsk => "QPSK",
            Modulation::Tc8psk => "TC8PSK",
        };
        write!(f, "{m} {n}/{d}")
    }
}

/// A mother-code bit of a puncturing period: `('X' | 'Y', index from 1)`.
pub type MotherBit = (char, usize);

/// The punctured codes of the QPSK combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Puncture {
    /// r = 2/3.
    R2_3,
    /// r = 3/4.
    R3_4,
    /// r = 5/6.
    R5_6,
    /// r = 7/8.
    R7_8,
}

impl Puncture {
    /// The transmitted sequence over one period as (P1, P0) pairs of mother
    /// code bits, written ('X' | 'Y', index from 1). Annexed table 49 note 4
    /// lists the sequence ("Ｐｎ: Ｘ１ Ｙ１ Ｙ２ Ｘ３ …" for 3/4, the order of
    /// ARIB STD-B31 Table 3-8); BO.1408-1 Table 2 lists the same bits as P1
    /// and P0 rows ("P1 = X1 Y2, P0 = Y1 X3"). Consecutive pairs of the
    /// sequence are the (P1, P0) of one QPSK symbol, which satisfies both.
    pub const fn pairs(self) -> &'static [(MotherBit, MotherBit)] {
        match self {
            Self::R2_3 => &[
                (('X', 1), ('Y', 1)),
                (('Y', 2), ('X', 3)),
                (('Y', 3), ('Y', 4)),
            ],
            Self::R3_4 => &[(('X', 1), ('Y', 1)), (('Y', 2), ('X', 3))],
            Self::R5_6 => &[
                (('X', 1), ('Y', 1)),
                (('Y', 2), ('X', 3)),
                (('Y', 4), ('X', 5)),
            ],
            Self::R7_8 => &[
                (('X', 1), ('Y', 1)),
                (('Y', 2), ('Y', 3)),
                (('Y', 4), ('X', 5)),
                (('Y', 6), ('X', 7)),
            ],
        }
    }

    /// Input bits per period.
    pub const fn period(self) -> usize {
        match self {
            Self::R2_3 => 4,
            Self::R3_4 => 3,
            Self::R5_6 => 5,
            Self::R7_8 => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BO.1408-1 Table 1 words, round trip; 1111 and reserved words map to
    /// no combination.
    #[test]
    fn spec_table_1_words() {
        let words: Vec<u8> = Combination::ALL.iter().map(|c| c.word()).collect();
        assert_eq!(words, [7, 6, 5, 4, 3, 2, 1]);
        for c in Combination::ALL {
            assert_eq!(Combination::from_word(c.word()), Some(c));
        }
        for w in [0b0000, 0b1000, 0b1110, DUMMY_WORD] {
            assert_eq!(Combination::from_word(w), None);
        }
    }

    /// BO.1408-1 Fig. 8 / Table 1 as a whole: all 16 words round-trip
    /// through `CombinationWord`; 0001–0111 are the seven combinations (in
    /// `Combination::ALL`), 1111 is the dummy, 0000 and 1000–1110 are
    /// reserved and keep their value.
    #[test]
    fn spec_fig_8_combination_words() {
        let assigned: Vec<u8> = Combination::ALL.iter().map(|c| c.word()).collect();
        for w in 0..16u8 {
            let cw = CombinationWord::from_word(w);
            assert_eq!(cw.word(), w, "{w:04b}");
            assert_eq!(CombinationWord::from_word(w | 0xF0), cw, "{w:04b}: masked");
            match cw {
                CombinationWord::Assigned(c) => {
                    assert!(assigned.contains(&w), "{w:04b}");
                    assert_eq!(c.word(), w);
                    assert_eq!(cw.combination(), Some(c));
                }
                CombinationWord::Dummy => {
                    assert_eq!(w, DUMMY_WORD);
                    assert_eq!(cw.combination(), None);
                }
                CombinationWord::Reserved(r) => {
                    assert_eq!(r.get(), w);
                    assert!(w == 0b0000 || (0b1000..=0b1110).contains(&w), "{w:04b}");
                    assert_eq!(cw.combination(), None);
                }
            }
        }
        let n = (0..16u8)
            .filter(|&w| matches!(CombinationWord::from_word(w), CombinationWord::Assigned(_)))
            .count();
        assert_eq!(n, Combination::ALL.len());
    }

    /// `from_word` is total and masks to the 4-bit field: every `u8`
    /// decodes to the canonical value of its low bits (the `Reserved`
    /// payload can only come from `from_word`, so `==` is wire equality).
    #[test]
    fn prop_combination_word_masks_to_width() {
        for w in 0..=255u8 {
            let cw = CombinationWord::from_word(w);
            assert_eq!(cw, CombinationWord::from_word(w & 0xF), "{w:#04x}");
            assert_eq!(cw.word(), w & 0xF, "{w:#04x}");
        }
    }

    /// Annexed table 48 別記 3: the slot units follow from the spectrum
    /// efficiency against E_max = 2 (valid / unit = E_k / E_max), and every
    /// unit lasts a whole number of 812-symbol TC8PSK slots.
    #[test]
    fn spec_table_48_slot_units() {
        let units: Vec<(usize, usize)> = Combination::ALL.iter().map(|c| c.slot_unit()).collect();
        assert_eq!(
            units,
            [(1, 1), (8, 7), (6, 5), (4, 3), (3, 2), (2, 1), (4, 1)]
        );
        for c in Combination::ALL {
            let (n, d) = c.efficiency();
            let (unit, valid) = c.slot_unit();
            assert_eq!(valid * 2 * d, unit * n, "{c}");
            // symbols of a unit = valid slots · 8·203 bits / E_k
            assert_eq!(valid * 8 * 203 * d % n, 0);
            assert_eq!(valid * 8 * 203 * d / n, c.symbols_per_unit(), "{c}");
        }
    }

    /// Table 2 / annexed table 49 note 4: each period sends the bits the X
    /// and Y rows keep (X 10 / Y 11 for 2/3 over two periods, X 101 / Y 110
    /// for 3/4, X 10101 / Y 11010 for 5/6, X 1000101 / Y 1111010 for 7/8),
    /// each exactly once.
    #[test]
    fn spec_table_2_puncturing() {
        let rows = [
            (Puncture::R2_3, "1010", "1111"),
            (Puncture::R3_4, "101", "110"),
            (Puncture::R5_6, "10101", "11010"),
            (Puncture::R7_8, "1000101", "1111010"),
        ];
        for (p, x, y) in rows {
            assert_eq!(x.len(), p.period());
            let mut kept: Vec<(char, usize)> = Vec::new();
            for (i, (cx, cy)) in x.chars().zip(y.chars()).enumerate() {
                if cx == '1' {
                    kept.push(('X', i + 1));
                }
                if cy == '1' {
                    kept.push(('Y', i + 1));
                }
            }
            let mut sent: Vec<(char, usize)> =
                p.pairs().iter().flat_map(|&(a, b)| [a, b]).collect();
            sent.sort_by_key(|&(c, i)| (i, c));
            kept.sort_by_key(|&(c, i)| (i, c));
            assert_eq!(sent, kept, "{p:?}");
        }
    }
}
