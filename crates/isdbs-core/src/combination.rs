//! Modulation-code combinations (BO.1408-1 Table 1), their spectrum
//! efficiency, the slot units of MIC Ordinance No. 87 annexed table 48
//! 別記 3, and the punctured codes of Table 2 / annexed table 49 note 4.

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

/// The word of "dummy" / unused combination entries (Table 1: 1111).
pub const DUMMY_WORD: u8 = 0b1111;

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
