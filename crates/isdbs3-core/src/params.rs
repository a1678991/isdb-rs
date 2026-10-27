//! Frame geometry, modulations and inner code rates of ISDB-S3 (ARIB
//! STD-B44 v2.1 §3.2–§3.4, §3.8).

/// LDPC code length (B44 §3.4 (2)).
pub const N_LDPC: usize = 44_880;
/// Slot header bits (Table 3.2-1).
pub const HEADER_BITS: usize = 176;
/// BCH parity bits (Table 3.2-1).
pub const BCH_PARITY_BITS: usize = 192;
/// Stuffing bits `111111` (Table 3.2-1).
pub const STUFF_BITS: usize = 6;
/// Slots per frame (§3.2 (1)).
pub const SLOTS: usize = 120;
/// Slots per allocation unit (Table 3.2-2).
pub const SLOT_UNIT: usize = 5;
/// Synchronisation symbols at the head of a modulation slot (Fig. 3.3-1).
pub const SYNC_SYMBOLS: usize = 24;
/// Pilot symbols per modulation slot (Fig. 3.3-1: 32 symbols).
pub const PILOT_SYMBOLS: usize = 32;
/// Main-signal symbols between two TMCC groups (Fig. 3.3-1).
pub const MAIN_RUN: usize = 136;
/// TMCC symbols per group (Fig. 3.3-1).
pub const TMCC_RUN: usize = 4;
/// Main/TMCC groups per modulation slot (Fig. 3.3-1: parts 1…66).
pub const RUNS: usize = 66;
/// Symbols per modulation slot: 24 + 32 + 66 · (136 + 4) = 9 296.
pub const MOD_SLOT: usize = SYNC_SYMBOLS + PILOT_SYMBOLS + RUNS * (MAIN_RUN + TMCC_RUN);
/// Main-signal symbols per modulation slot: 66 · 136 = 8 976.
pub const MAIN_PER_MOD_SLOT: usize = RUNS * MAIN_RUN;
/// TMCC symbols per modulation slot: 66 · 4 = 264.
pub const TMCC_PER_MOD_SLOT: usize = RUNS * TMCC_RUN;
/// Symbols per frame: 120 · 9 296 = 1 115 520.
pub const FRAME_SYMBOLS: usize = SLOTS * MOD_SLOT;
/// TMCC signal bits per frame (Fig. 3.2-5): 9 422 + 192 + 22 066.
pub const TMCC_SIGNAL_BITS: usize = 31_680;
/// TMCC information bits per frame (§3.11).
pub const TMCC_INFO_BITS: usize = 9_422;
/// Nominal symbol rate, baud (B44 Annex B.3).
pub const SYMBOL_RATE: f64 = 33.7561e6;

/// Frame synchronisation word (Fig. 3.3-1 note).
pub const FSYNC: u32 = 0x52_F866;
/// Its inverse.
pub const NFSYNC: u32 = 0xAD_0799;
/// Slot synchronisation word.
pub const SSYNC: u32 = 0x36_715A;

/// Main-signal modulation (§3.8, TMCC Table 3.11-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modulation {
    /// π/2-shift BPSK, 1 bit/symbol.
    Bpsk,
    /// QPSK, 2 bits/symbol.
    Qpsk,
    /// 8PSK, 3 bits/symbol.
    Psk8,
    /// 16APSK (4 + 12), 4 bits/symbol.
    Apsk16,
    /// 32APSK (4 + 12 + 16), 5 bits/symbol.
    Apsk32,
}

impl Modulation {
    /// Bits per symbol.
    pub const fn bits(self) -> usize {
        match self {
            Self::Bpsk => 1,
            Self::Qpsk => 2,
            Self::Psk8 => 3,
            Self::Apsk16 => 4,
            Self::Apsk32 => 5,
        }
    }
    /// Valid (non-dummy) slots per unit of five (Tables 3.2-2, 3.2-3): the
    /// number of LDPC codewords that fill the five modulation slots.
    pub const fn valid_per_unit(self) -> usize {
        match self {
            Self::Bpsk => 1,
            Self::Qpsk => 2,
            Self::Psk8 => 3,
            Self::Apsk16 => 4,
            Self::Apsk32 => 5,
        }
    }
    /// TMCC code (Table 3.11-1).
    pub const fn code(self) -> u8 {
        match self {
            Self::Bpsk => 0b0001,
            Self::Qpsk => 0b0010,
            Self::Psk8 => 0b0011,
            Self::Apsk16 => 0b0100,
            Self::Apsk32 => 0b0101,
        }
    }
    /// From a TMCC code; `None` for reserved or "no scheme" (1111).
    pub const fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0b0001 => Self::Bpsk,
            0b0010 => Self::Qpsk,
            0b0011 => Self::Psk8,
            0b0100 => Self::Apsk16,
            0b0101 => Self::Apsk32,
            _ => return None,
        })
    }
    /// Name as the standard writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bpsk => "π/2-BPSK",
            Self::Qpsk => "QPSK",
            Self::Psk8 => "8PSK",
            Self::Apsk16 => "16APSK",
            Self::Apsk32 => "32APSK",
        }
    }
}

/// Inner code rate r/120 (Table 3.4-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodeRate(u8);

/// The eleven rates, numerators of r/120.
pub const RATES: [u8; 11] = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109];

impl CodeRate {
    /// Rate r/120 if r is one of [`RATES`].
    pub fn new(r: u8) -> Option<Self> {
        RATES.contains(&r).then_some(Self(r))
    }
    /// The numerator r.
    pub const fn numerator(self) -> u8 {
        self.0
    }
    /// LDPC information bits: 374 · r.
    pub const fn k_ldpc(self) -> usize {
        374 * self.0 as usize
    }
    /// Main data bits per slot D (Table 3.2-1): k − 176 − 192 − 6.
    pub const fn data_bits(self) -> usize {
        self.k_ldpc() - HEADER_BITS - BCH_PARITY_BITS - STUFF_BITS
    }
    /// q of the parity addressing: (n − k) / 374 (§3.4 (2) note 3 table).
    pub const fn q(self) -> usize {
        (N_LDPC - self.k_ldpc()) / 374
    }
    /// TMCC code per B44 v2.1 Table 3.11-2: 0001 = 41/120 … 1011 =
    /// 109/120 in table order. (The table prints 101/120 twice, at 1001
    /// and 1010; 1010 is read as 105/120, the only rate missing — SPEC-NOTES
    /// "TMCC code-rate table".)
    pub fn code(self) -> u8 {
        (1u8..)
            .zip(RATES)
            .find_map(|(code, r)| (r == self.0).then_some(code))
            .expect("rate")
    }
    /// From a TMCC code (B44 v2.1 Table 3.11-2 reading above).
    pub fn from_code(c: u8) -> Option<Self> {
        (1..=11).contains(&c).then(|| Self(RATES[c as usize - 1]))
    }
    /// The approximate value printed next to it (Table 3.4-2).
    pub const fn approx(self) -> &'static str {
        match self.0 {
            41 => "1/3",
            49 => "2/5",
            61 => "1/2",
            73 => "3/5",
            81 => "2/3",
            89 => "3/4",
            93 => "7/9",
            97 => "4/5",
            101 => "5/6",
            105 => "7/8",
            _ => "9/10",
        }
    }
}

impl std::fmt::Display for CodeRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/120 ({})", self.0, self.approx())
    }
}

/// 16APSK radius ratio γ = R2/R1 (Table 3.8-2).
pub fn gamma16(r: CodeRate) -> f64 {
    match r.0 {
        41 => 3.09,
        49 => 2.97,
        61 => 3.93,
        73 => 2.87,
        81 => 2.92,
        89 => 2.97,
        93 => 2.87,
        97 => 2.73,
        101 => 2.67,
        105 => 2.76,
        _ => 2.69,
    }
}

/// 32APSK radius ratios (γ1 = R2/R1, γ2 = R3/R1) (Table 3.8-3).
pub fn gamma32(r: CodeRate) -> (f64, f64) {
    let g2 = match r.0 {
        41 => 6.53,
        49 => 7.17,
        61 => 8.03,
        73 => 5.61,
        81 => 5.68,
        89 => 5.57,
        93 => 5.33,
        97 => 5.05,
        101 => 4.80,
        105 => 4.82,
        _ => 4.66,
    };
    (gamma16(r), g2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_fig_3_3_1_geometry() {
        assert_eq!(MOD_SLOT, 9_296);
        assert_eq!(MAIN_PER_MOD_SLOT, 8_976);
        assert_eq!(FRAME_SYMBOLS, 1_115_520);
        // Fig. 3.2-4: 1 600 control bits per modulation slot for 24 slots:
        // 120 sync + 160 pilot + 1 320 TMCC; TMCC = 31 680 over 120 slots
        assert_eq!(TMCC_PER_MOD_SLOT * SLOTS, TMCC_SIGNAL_BITS);
        assert_eq!(SLOTS * SYNC_SYMBOLS, 2_880);
    }

    #[test]
    fn spec_table_3_2_1_bit_distribution() {
        // (r, D, Pldpc) from Table 3.2-1
        let t = [
            (41, 14_960, 29_546),
            (49, 17_952, 26_554),
            (61, 22_440, 22_066),
            (73, 26_928, 17_578),
            (81, 29_920, 14_586),
            (89, 32_912, 11_594),
            (93, 34_408, 10_098),
            (97, 35_904, 8_602),
            (101, 37_400, 7_106),
            (105, 38_896, 5_610),
            (109, 40_392, 4_114),
        ];
        for (r, d, p) in t {
            let c = CodeRate::new(r).unwrap();
            assert_eq!(c.data_bits(), d, "{r}");
            assert_eq!(N_LDPC - c.k_ldpc(), p, "{r}");
        }
    }

    #[test]
    fn spec_q_table() {
        let q = [79, 71, 59, 47, 39, 31, 27, 23, 19, 15, 11];
        for (r, q) in RATES.iter().zip(q) {
            assert_eq!(CodeRate::new(*r).unwrap().q(), q);
        }
    }

    #[test]
    fn spec_modulation_main_symbols_fill_whole_codewords() {
        // A unit of five modulation slots carries the valid slots' LDPC
        // codewords: 5 · 8 976 symbols · bits = valid · 44 880.
        for m in [
            Modulation::Bpsk,
            Modulation::Qpsk,
            Modulation::Psk8,
            Modulation::Apsk16,
            Modulation::Apsk32,
        ] {
            assert_eq!(
                5 * MAIN_PER_MOD_SLOT * m.bits(),
                m.valid_per_unit() * N_LDPC,
                "{m:?}"
            );
        }
    }

    #[test]
    fn code_tables_round_trip() {
        for r in RATES {
            let c = CodeRate::new(r).unwrap();
            assert_eq!(CodeRate::from_code(c.code()), Some(c));
        }
        assert_eq!(CodeRate::from_code(0b1010).unwrap().numerator(), 105);
        assert_eq!(Modulation::from_code(0b0100), Some(Modulation::Apsk16));
        assert_eq!(Modulation::from_code(0b1111), None);
    }
}
