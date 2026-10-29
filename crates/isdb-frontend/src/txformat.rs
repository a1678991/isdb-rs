//! The TX boundary, format version 1: one 4-bit code per ISDB-S symbol,
//! eight codes per DAC DMA sample. What a host sends the FPGA transmit
//! block (`SymbolsTx` in `fpga/`) instead of IQ samples, and what
//! [`crate::shape::fixed::Interpolator`], the block's bit-exact model,
//! takes.
//!
//! # Codes
//!
//! | Code | Symbol |
//! |---|---|
//! | 0 | the zero symbol (0 + 0j: silence) |
//! | 1 to 7 | reserved; the block treats them as the zero symbol |
//! | 8 + k, k = 0..7 | the point e^{jkπ/4} |
//!
//! k is the angle index of MIC Ordinance No. 87 of 2011, annexed table 46
//! (BO.1408-1 Fig. 15): every ISDB-S symbol is one of its eight points
//! (header and burst BPSK at 0° / 180°, BPSK, QPSK at ±45° / ±135° and
//! TC8PSK of the main signal), so the code stream is lossless.
//! [`LabelCodes`] maps the symbol sources to codes; [`LABEL_ANGLE`] is
//! table 46's label → k map. Zero padding, which libiio and the DMA
//! deliver when the host has nothing, is the zero symbol: silence.
//!
//! # DMA words
//!
//! The DAC DMA sample is two little-endian `i16` words, `voltage0` (I)
//! then `voltage1` (Q), both channels enabled. One DMA sample carries
//! eight codes, the first in time in the lowest nibble:
//!
//! ```text
//! I word (voltage0)            Q word (voltage1)
//! 15..12 11..8  7..4  3..0     15..12 11..8  7..4  3..0
//! code 3 code 2 code 1 code 0  code 7 code 6 code 5 code 4
//! ```
//!
//! As one `u32` (the vectors' `words`, [`word`]): `Q << 16 | I`, code j in
//! bits 4j + 3..4j. At 28.86 Mbaud that is 28.86 / 8 × 4 B = 14.43 MB/s
//! ([`link_rate`]), against a measured host → board ceiling of 64 MB/s
//! (VERIFICATION.md, "ISDB-S transmitter in the FPGA: board checks"). There
//! is no header, valid bit or sequence number: the DMA is lossless and in
//! order, and its only failure, an underrun, inserts zero symbols that the
//! block counts (crate README, "The TX boundary").
//!
//! No broadcast specification defines this format; it is this project's
//! own, like [`crate::format`].

use num_complex::Complex64;

/// Version of the TX boundary and of the interpolator's arithmetic
/// ([`crate::shape::fixed`]); the FPGA block reports it in its ID word
/// ([`crate::control::tx::TxIdentity`]). Any change to either increments
/// it.
pub const FORMAT_VERSION: u16 = 1;

/// Bits per code.
pub const CODE_BITS: u32 = 4;

/// Codes per DMA sample (two 16-bit channel words).
pub const CODES_PER_WORD: usize = 8;

/// The zero symbol's code.
pub const ZERO: u8 = 0;

/// The code of the point e^{j0}; point k is `POINT + k`.
pub const POINT: u8 = 8;

/// The angle index k (point e^{jkπ/4}) of each annexed table 46 label
/// [C2 C1 C0]: 000 → 0, 001 → 1, 011 → 2, 010 → 3, 100 → 4, 101 → 5,
/// 111 → 6, 110 → 7 (MIC Ordinance No. 87 of 2011, annexed table 46).
pub const LABEL_ANGLE: [u8; 8] = [0, 1, 3, 2, 4, 5, 7, 6];

/// The code of the point e^{jkπ/4} (k modulo 8).
pub const fn point_code(k: u8) -> u8 {
    POINT | (k & 7)
}

/// The angle index k of a code's point, `None` for the zero symbol and
/// the reserved codes 1 to 7. Only the code's low four bits count.
pub const fn angle(code: u8) -> Option<u8> {
    if code & POINT != 0 {
        Some(code & 7)
    } else {
        None
    }
}

/// The symbol a code stands for: e^{jkπ/4} as
/// `Complex64::from_polar(1, k·π/4)` (the expression of
/// `isdbs_phy::demod::psk8_point`), or 0 for the zero symbol and the
/// reserved codes.
pub fn point(code: u8) -> Complex64 {
    match angle(code) {
        Some(k) => Complex64::from_polar(1.0, f64::from(k) * std::f64::consts::FRAC_PI_4),
        None => Complex64::new(0.0, 0.0),
    }
}

/// The code of the conjugate symbol: point k → point −k (mod 8); the zero
/// symbol and the reserved codes are their own conjugates.
pub const fn conjugate(code: u8) -> u8 {
    match angle(code) {
        Some(k) => point_code(8 - k),
        None => code & 0xF,
    }
}

/// The codes of the ISDB-S symbol sources (MIC Ordinance No. 87 of 2011,
/// annexed table 46): an 8-entry table from the label [C2 C1 C0] to the
/// code, so a relabelling (a conjugated constellation,
/// [`LabelCodes::conjugated`]) is a different table and needs nothing in
/// the block.
///
/// ```
/// use isdb_frontend::txformat::LabelCodes;
///
/// let c = LabelCodes::TABLE_46;
/// assert_eq!(c.label(0b011), 8 + 2); // 90°
/// assert_eq!(c.bpsk(1), 8 + 4); // 180°
/// assert_eq!(c.qpsk(1, 0), 8 + 3); // (P1, P0) = 10: +135°
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LabelCodes {
    /// The code of each label 0..8.
    pub codes: [u8; 8],
}

impl LabelCodes {
    /// Annexed table 46 as printed: label l → point [`LABEL_ANGLE`]`[l]`.
    pub const TABLE_46: Self = {
        let mut codes = [0u8; 8];
        let mut l = 0;
        while l < 8 {
            codes[l] = point_code(LABEL_ANGLE[l]);
            l += 1;
        }
        Self { codes }
    };

    /// The code of TC8PSK label [C2 C1 C0] = `label` (its low three bits).
    pub const fn label(&self, label: u8) -> u8 {
        self.codes[(label & 7) as usize]
    }

    /// The code of a BPSK bit (header, bursts, BPSK main signal): 0 at 0°
    /// (label 000), 1 at 180° (label 100); annexed table 46.
    pub const fn bpsk(&self, bit: u8) -> u8 {
        self.label((bit & 1) << 2)
    }

    /// The code of the QPSK pair (P1, P0) of annexed table 46: 00 +45°
    /// (label 001), 10 +135° (010), 11 −135° (101), 01 −45° (110), i.e.
    /// C2 = P0, C1 = P1 ⊕ P0, C0 = ¬(P1 ⊕ P0); P1 is the sign of I and P0
    /// that of Q (`isdbs_phy::demod::qpsk_point`).
    pub const fn qpsk(&self, p1: u8, p0: u8) -> u8 {
        let (p1, p0) = (p1 & 1, p0 & 1);
        let x = p1 ^ p0;
        self.label(p0 << 2 | x << 1 | (x ^ 1))
    }

    /// The same table for the conjugated constellation (z → z̄: label
    /// 001 ↔ 110, 011 ↔ 111, 010 ↔ 101; 000 and 100 fixed).
    #[must_use]
    pub const fn conjugated(&self) -> Self {
        let mut codes = self.codes;
        let mut l = 0;
        while l < 8 {
            codes[l] = conjugate(codes[l]);
            l += 1;
        }
        Self { codes }
    }
}

impl Default for LabelCodes {
    fn default() -> Self {
        Self::TABLE_46
    }
}

/// The DMA sample of eight codes, as `Q << 16 | I` (module
/// documentation): code j in bits 4j + 3..4j; only each code's low four
/// bits are used.
pub fn word(codes: [u8; CODES_PER_WORD]) -> u32 {
    codes
        .iter()
        .zip(0u32..)
        .fold(0, |w, (&c, j)| w | u32::from(c & 0xF) << (CODE_BITS * j))
}

/// The eight codes of a DMA sample, first in time first.
pub fn codes(word: u32) -> [u8; CODES_PER_WORD] {
    let b = word.to_le_bytes();
    std::array::from_fn(|j| (b[j / 2] >> (4 * (j % 2))) & 0xF)
}

/// The DMA sample's two channel words `[I, Q]` (`voltage0`, `voltage1`) as
/// the `i16` values a TX buffer carries.
pub fn components(word: u32) -> [i16; 2] {
    let b = word.to_le_bytes();
    [
        i16::from_le_bytes([b[0], b[1]]),
        i16::from_le_bytes([b[2], b[3]]),
    ]
}

/// The DMA sample of two channel words `[I, Q]`: the inverse of
/// [`components`].
pub fn from_components(c: [i16; 2]) -> u32 {
    let (i, q) = (c[0].to_le_bytes(), c[1].to_le_bytes());
    u32::from_le_bytes([i[0], i[1], q[0], q[1]])
}

/// `words` as the interleaved `i16` stream of a two-channel TX buffer
/// (I, Q, I, Q, …), appended to `out`.
pub fn to_dma(words: &[u32], out: &mut Vec<i16>) {
    out.reserve(2 * words.len());
    for &w in words {
        out.extend(components(w));
    }
}

/// The bytes per second of the code stream at `symbol_rate` baud: four
/// bytes per eight symbols (14.43 MB/s at 28.86 Mbaud).
pub fn link_rate(symbol_rate: f64) -> f64 {
    symbol_rate / CODES_PER_WORD as f64 * 4.0
}

/// Packs codes into DMA samples, carrying a partial sample across calls
/// (an ISDB-S frame, 39 936 symbols, is a whole number of samples, but a
/// caller's cut need not be).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodePacker {
    pending: u32,
    /// Codes in `pending`, 0..8.
    n: u32,
}

impl CodePacker {
    /// An empty packer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Codes held for the next sample (0..8).
    pub fn pending(&self) -> usize {
        self.n as usize
    }

    /// Append the samples `codes` completes to `out`; the rest is held.
    pub fn push(&mut self, codes: &[u8], out: &mut Vec<u32>) {
        out.reserve((self.pending() + codes.len()) / CODES_PER_WORD);
        for &c in codes {
            // n < 8, so the shift is below 32.
            self.pending |= u32::from(c & 0xF) << (CODE_BITS * self.n);
            self.n += 1;
            if self.n == 8 {
                out.push(self.pending);
                self.pending = 0;
                self.n = 0;
            }
        }
    }

    /// Append the held codes as a last sample, padded with zero symbols
    /// (nothing when none are held).
    pub fn finish(&mut self, out: &mut Vec<u32>) {
        if self.n > 0 {
            out.push(self.pending);
            self.pending = 0;
            self.n = 0;
        }
    }
}

/// `codes` as DMA samples, the last padded with zero symbols.
pub fn pack(codes: &[u8]) -> Vec<u32> {
    let mut p = CodePacker::new();
    let mut out = Vec::new();
    p.push(codes, &mut out);
    p.finish(&mut out);
    out
}

/// The codes of `words` in time order, eight per word (zero padding
/// included): the block's unpacker.
pub fn unpack(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|&w| codes(w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Annexed table 46: label → angle, and the QPSK and BPSK sources at
    /// their printed angles.
    #[test]
    fn spec_table_46_codes() {
        let deg = |c: u8| f64::from(angle(c).unwrap()) * 45.0;
        let want = [
            (0b000, 0.0),
            (0b001, 45.0),
            (0b011, 90.0),
            (0b010, 135.0),
            (0b100, 180.0),
            (0b101, 225.0),
            (0b111, 270.0),
            (0b110, 315.0),
        ];
        let t = LabelCodes::TABLE_46;
        for (l, a) in want {
            assert_eq!(deg(t.label(l)), a, "{l:03b}");
        }
        assert_eq!(deg(t.bpsk(0)), 0.0);
        assert_eq!(deg(t.bpsk(1)), 180.0);
        // (P1, P0): 00 +45°, 10 +135°, 11 −135°, 01 −45°
        assert_eq!(deg(t.qpsk(0, 0)), 45.0);
        assert_eq!(deg(t.qpsk(1, 0)), 135.0);
        assert_eq!(deg(t.qpsk(1, 1)), 225.0);
        assert_eq!(deg(t.qpsk(0, 1)), 315.0);
        // the QPSK labels are the four with C1 ≠ C0
        for (p1, p0) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let l = t.codes.iter().position(|&c| c == t.qpsk(p1, p0)).unwrap();
            assert_ne!(l >> 1 & 1, l & 1, "({p1}, {p0}): label {l:03b}");
        }
        assert_eq!(LabelCodes::default(), t);
    }

    /// The zero and reserved codes are no point; conjugation negates the
    /// angle, twice is the identity, and the conjugated table is table 46
    /// read for z̄ (boundary-host.md §1.3).
    #[test]
    fn zero_reserved_and_conjugate() {
        for c in 0..8 {
            assert_eq!(angle(c), None);
            assert_eq!(point(c), Complex64::new(0.0, 0.0));
            assert_eq!(conjugate(c), c);
        }
        for k in 0..8u8 {
            let c = point_code(k);
            assert_eq!(angle(c), Some(k));
            assert!((point(conjugate(c)) - point(c).conj()).norm() < 1e-15);
            assert_eq!(conjugate(conjugate(c)), c);
        }
        let conj = LabelCodes::TABLE_46.conjugated();
        let want = [
            (0b000, 0b000),
            (0b001, 0b110),
            (0b011, 0b111),
            (0b010, 0b101),
            (0b100, 0b100),
        ];
        for (l, m) in want {
            assert_eq!(conj.label(l), LabelCodes::TABLE_46.label(m), "{l:03b}");
            assert_eq!(conj.label(m), LabelCodes::TABLE_46.label(l), "{m:03b}");
        }
    }

    /// The word layout of the module documentation: code 0 in I bits 3..0,
    /// code 4 in Q bits 3..0, code 7 in Q bits 15..12.
    #[test]
    fn spec_word_layout() {
        let w = word([1, 2, 3, 4, 5, 6, 7, 15]);
        assert_eq!(w, 0xF765_4321);
        assert_eq!(components(w), [0x4321, 0xF765_u16.cast_signed()]);
        assert_eq!(from_components(components(w)), w);
        assert_eq!(codes(w), [1, 2, 3, 4, 5, 6, 7, 15]);
        let mut dma = Vec::new();
        to_dma(&[w, 0x0000_0008], &mut dma);
        assert_eq!(dma, [0x4321, 0xF765_u16.cast_signed(), 8, 0]);
        assert!((link_rate(28.86e6) - 14.43e6).abs() < 1.0);
    }

    /// A partial word is held across calls and padded with zero symbols
    /// by `finish`.
    #[test]
    fn packer_carries_and_pads() {
        let mut p = CodePacker::new();
        let mut out = Vec::new();
        p.push(&[8, 9, 10], &mut out);
        assert!(out.is_empty());
        assert_eq!(p.pending(), 3);
        p.push(&[11, 12, 13, 14, 15, 9], &mut out);
        assert_eq!(out, [word([8, 9, 10, 11, 12, 13, 14, 15])]);
        p.finish(&mut out);
        assert_eq!(out[1], word([9, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(p.pending(), 0);
        p.finish(&mut out);
        assert_eq!(out.len(), 2);
    }
}
