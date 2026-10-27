//! TMCC information (BO.1408-1 §6.1, Figs. 7–10, Appendix 1 Table 3) and
//! the TMCC signal of one superframe (§6.2–6.4; MIC Ordinance No. 87 of
//! 2011, annexed table 50).
//!
//! # Information layout (384 bits, MSB first)
//!
//! | Bits | Field | Source |
//! |---|---|---|
//! | 5 | order of change | Fig. 7 (first field), App. 1 item 12 |
//! | 4 × (4 + 6) | combination word (Table 1) and its slot count | Fig. 8, CM = 4 |
//! | 48 × 3 | relative TS ID of slots 1..48 | Fig. 9, TM = 8 |
//! | 8 × 16 | TS_ID of relative TS 0..7 | Fig. 10 |
//! | 1 | emergency alert flag | App. 1 item 12 |
//! | 4 | uplink information | App. 1 item 12 |
//! | 62 | reserved | App. 1 item 12 |
//!
//! The field order and widths are BO.1408-1's; the slot-count and
//! relative-ID widths (6 and 3 bits) are the smallest that hold N = 48 and
//! TM = 8, and the bit order (MSB first, fields in Fig. 7 order, the "other
//! information" after the table) is an interpretation: the binding layout,
//! MIC Notification No. 304 of 2011 annexed table 2, is not available. It
//! is confirmed by the live signal (SPEC-NOTES "tmcc-layout").
//!
//! # Signal
//!
//! The 48 information bytes get 16 RS(64,48) parity bytes; the 64 bytes are
//! cut into 8 blocks of 8, one per frame, each between W1 and W2 (frame 1)
//! or W3 (frames 2–8). The X¹⁵+X¹⁴+1 PRBS starts at the first TMCC byte of
//! frame 1 and runs through the superframe's 96 header bytes, skipping
//! (but clocking over) the W words (§6.4, annexed table 50 別記 1: "先頭の
//! ２バイトを除き … Ｗ１、Ｗ２及びＷ３には加算は行わないが、擬似乱数符号系列の
//! 発生は継続する").

use crate::combination::Combination;
pub use crate::combination::CombinationWord;
use crate::frame::{FRAMES, SLOTS, TMCC_BYTES_PER_FRAME, W1, W2, W3};
use isdb_fec::{Prbs15, RS_64_48};

/// Total bits of the TMCC information (Table 3 item 13).
pub const INFO_BITS: usize = 384;
/// Bytes of the TMCC information.
pub const INFO_BYTES: usize = INFO_BITS / 8;
/// Combinations carried, CM (Table 3 item 8).
pub const CM: usize = 4;
/// TSs carried, TM (Table 3 item 9).
pub const TM: usize = 8;
/// Reserved bits at the end.
pub const RESERVED_BITS: usize = 62;
/// TS_ID of an unused entry of the table (observed: every unused entry is
/// FFFFh on air; BO.1408-1 does not say).
pub const UNUSED_TS_ID: u16 = 0xFFFF;

/// A combination entry of Fig. 8: the Table 1 word and its slot count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CombinationEntry {
    /// The 4-bit Table 1 word, every code kept.
    pub word: CombinationWord,
    /// Slots assigned (6 bits).
    pub slots: u8,
}

impl CombinationEntry {
    /// An entry not in use (§6.1.2: "the word 1111 shall be applied to the
    /// rest of the combinations and the number of slots assigned shall be
    /// set to zero").
    pub const DUMMY: Self = Self {
        word: CombinationWord::Dummy,
        slots: 0,
    };

    /// The combination, if the word is not dummy or reserved.
    pub const fn combination(&self) -> Option<Combination> {
        self.word.combination()
    }
}

/// The decoded TMCC information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccInfo {
    /// Order of change (5 bits).
    pub order_of_change: u8,
    /// The CM combination entries, most efficient first.
    pub combinations: [CombinationEntry; CM],
    /// Relative TS ID (0..8) of each of the 48 slots.
    pub relative_ts: [u8; SLOTS],
    /// TS_ID of relative TS 0..8.
    pub ts_ids: [u16; TM],
    /// Emergency alert flag.
    pub emergency: bool,
    /// Uplink information (4 bits).
    pub uplink: u8,
    /// The 62 reserved bits, MSB first in the low bits of a u64.
    pub reserved: u64,
}

/// Why TMCC information is inconsistent (it decodes, but cannot be used).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TmccError {
    /// The slot counts of the combinations do not sum to 48.
    #[error("TMCC: combination slot counts sum to {0}, not 48")]
    SlotSum(usize),
    /// A combination word is reserved (Table 1: 0000, 1000..1110).
    #[error("TMCC: reserved combination word {0:04b}")]
    ReservedWord(u8),
    /// A combination's slot count is not a whole number of its units
    /// (annexed table 48 別記 3).
    #[error("TMCC: {combination}: {slots} slots is not a multiple of the {unit}-slot unit")]
    Unit {
        /// The combination.
        combination: Combination,
        /// Its slot count.
        slots: usize,
        /// Its unit.
        unit: usize,
    },
    /// The combinations are not in descending efficiency (§3.2).
    #[error("TMCC: combinations not in descending order of efficiency")]
    Order,
}

/// Big-endian bit reader over a byte slice.
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let bit = (self.b[self.at / 8] >> (7 - self.at % 8)) & 1;
            v = (v << 1) | u64::from(bit);
            self.at += 1;
        }
        v
    }

    /// An `n`-bit field into a `u8`, `n <= 8` (asserted once here).
    fn take_u8(&mut self, n: usize) -> u8 {
        assert!(n <= 8, "take_u8: {n} bits");
        // n <= 8 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        let v = self.take(n) as u8;
        v
    }

    /// An `n`-bit field into a `u16`, `n <= 16` (asserted once here).
    fn take_u16(&mut self, n: usize) -> u16 {
        assert!(n <= 16, "take_u16: {n} bits");
        // n <= 16 is asserted above, so the value fits.
        #[allow(clippy::cast_possible_truncation)]
        let v = self.take(n) as u16;
        v
    }
}

/// Big-endian bit writer.
struct BitsMut {
    b: [u8; INFO_BYTES],
    at: usize,
}

impl BitsMut {
    fn put(&mut self, v: u64, n: usize) {
        for i in (0..n).rev() {
            if (v >> i) & 1 == 1 {
                self.b[self.at / 8] |= 1 << (7 - self.at % 8);
            }
            self.at += 1;
        }
    }
}

impl TmccInfo {
    /// Decode the 48 information bytes. Every field is kept as sent, so
    /// `encode(decode(x)) == x` for every input.
    pub fn decode(info: &[u8; INFO_BYTES]) -> Self {
        let mut r = Bits { b: info, at: 0 };
        let order_of_change = r.take_u8(5);
        let combinations = std::array::from_fn(|_| CombinationEntry {
            word: CombinationWord::from_word(r.take_u8(4)),
            slots: r.take_u8(6),
        });
        let relative_ts = std::array::from_fn(|_| r.take_u8(3));
        let ts_ids = std::array::from_fn(|_| r.take_u16(16));
        let emergency = r.take(1) == 1;
        let uplink = r.take_u8(4);
        let reserved = r.take(RESERVED_BITS);
        debug_assert_eq!(r.at, INFO_BITS);
        Self {
            order_of_change,
            combinations,
            relative_ts,
            ts_ids,
            emergency,
            uplink,
            reserved,
        }
    }

    /// Encode to the 48 information bytes.
    pub fn encode(&self) -> [u8; INFO_BYTES] {
        let mut w = BitsMut {
            b: [0; INFO_BYTES],
            at: 0,
        };
        w.put(u64::from(self.order_of_change & 0x1F), 5);
        for c in &self.combinations {
            w.put(u64::from(c.word.word()), 4);
            w.put(u64::from(c.slots & 0x3F), 6);
        }
        for &t in &self.relative_ts {
            w.put(u64::from(t & 7), 3);
        }
        for &t in &self.ts_ids {
            w.put(u64::from(t), 16);
        }
        w.put(u64::from(self.emergency), 1);
        w.put(u64::from(self.uplink & 0xF), 4);
        w.put(self.reserved & ((1 << RESERVED_BITS) - 1), RESERVED_BITS);
        debug_assert_eq!(w.at, INFO_BITS);
        w.b
    }

    /// Check the information against BO.1408-1 §3.2 and annexed table 48:
    /// counts sum to 48, no reserved word, whole slot units, descending
    /// efficiency. Dummy (1111) entries count their slots too (0 on air).
    pub fn validate(&self) -> Result<(), TmccError> {
        let mut sum = 0usize;
        let mut prev: Option<usize> = None;
        for c in &self.combinations {
            sum += usize::from(c.slots);
            let comb = match c.word {
                CombinationWord::Assigned(comb) => comb,
                CombinationWord::Dummy => continue,
                CombinationWord::Reserved(w) => return Err(TmccError::ReservedWord(w.get())),
            };
            let (unit, _) = comb.slot_unit();
            if usize::from(c.slots) % unit != 0 {
                return Err(TmccError::Unit {
                    combination: comb,
                    slots: c.slots.into(),
                    unit,
                });
            }
            let rank = Combination::ALL
                .iter()
                .position(|&x| x == comb)
                .expect("listed");
            if prev.is_some_and(|p| rank < p) {
                return Err(TmccError::Order);
            }
            prev = Some(rank);
        }
        if sum != SLOTS {
            return Err(TmccError::SlotSum(sum));
        }
        Ok(())
    }

    /// The combination of each of the 48 slots, in slot order (§3.2: the
    /// most efficient combination first); `None` for a dummy slot. Within a
    /// combination, the valid slots of each unit come first and its dummy
    /// slots last (BO.1408-1 Fig. 4: "one dummy slot is inserted for three
    /// active slots").
    pub fn slot_combinations(&self) -> [Option<Combination>; SLOTS] {
        let mut out = [None; SLOTS];
        let mut at = 0usize;
        for c in &self.combinations {
            let n = usize::from(c.slots);
            match c.word {
                CombinationWord::Assigned(comb) => {
                    let (unit, valid) = comb.slot_unit();
                    for k in 0..n {
                        if at + k < SLOTS && k % unit < valid {
                            out[at + k] = Some(comb);
                        }
                    }
                }
                // Dummy slots; a reserved word (refused by `validate`) is
                // treated the same so the slots it claims stay unassigned.
                CombinationWord::Dummy | CombinationWord::Reserved(_) => {}
            }
            at += n;
        }
        out
    }

    /// The TS_ID of each slot (`None` for dummy slots).
    pub fn slot_ts_ids(&self) -> [Option<u16>; SLOTS] {
        let comb = self.slot_combinations();
        std::array::from_fn(|i| comb[i].map(|_| self.ts_ids[usize::from(self.relative_ts[i] & 7)]))
    }
}

/// The 64 TMCC bytes of a superframe (information and RS parity), the PRBS
/// removed, from the 8 header blocks; returns the RS outcome. Blocks are
/// the 8 TMCC bytes between the W words of frames 1..8, as received.
pub fn derandomise(blocks: &[[u8; TMCC_BYTES_PER_FRAME]; FRAMES]) -> [u8; 64] {
    let mut prbs = Prbs15::new();
    let mut out = [0u8; 64];
    for (f, blk) in blocks.iter().enumerate() {
        if f > 0 {
            // this frame's W1: clocked, not applied
            prbs.next_byte();
            prbs.next_byte();
        }
        for (k, &b) in blk.iter().enumerate() {
            out[f * TMCC_BYTES_PER_FRAME + k] = b ^ prbs.next_byte();
        }
        // W2 / W3: clocked, not applied
        prbs.next_byte();
        prbs.next_byte();
    }
    out
}

/// The 8 header blocks (12 bytes each: W1, TMCC block, W2/W3) of a
/// superframe carrying `info`: RS(64,48) parity, energy dispersal, sync
/// words. The inverse of [`derandomise`] plus RS decoding.
pub fn headers(info: &TmccInfo) -> [[u8; 12]; FRAMES] {
    let mut word = [0u8; 64];
    word[..INFO_BYTES].copy_from_slice(&info.encode());
    RS_64_48.encode_in_place(&mut word);
    let mut blocks = [[0u8; TMCC_BYTES_PER_FRAME]; FRAMES];
    for (f, b) in blocks.iter_mut().enumerate() {
        b.copy_from_slice(&word[f * 8..f * 8 + 8]);
    }
    // XOR is self-inverse: derandomise applies the same PRBS.
    let scrambled = derandomise(&blocks);
    std::array::from_fn(|f| {
        let mut h = [0u8; 12];
        h[..2].copy_from_slice(&W1.to_be_bytes());
        h[2..10].copy_from_slice(&scrambled[f * 8..f * 8 + 8]);
        let end = if f == 0 { W2 } else { W3 };
        h[10..].copy_from_slice(&end.to_be_bytes());
        h
    })
}

/// Outcome of decoding one superframe's TMCC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccDecode {
    /// The information, when RS(64,48) succeeded.
    pub info: Option<TmccInfo>,
    /// Bytes corrected by RS (None when uncorrectable).
    pub corrected: Option<usize>,
}

/// Derandomise and RS-decode the 8 TMCC blocks of a superframe.
pub fn decode_blocks(blocks: &[[u8; TMCC_BYTES_PER_FRAME]; FRAMES]) -> TmccDecode {
    let mut word = derandomise(blocks);
    match RS_64_48.decode(&mut word) {
        Ok(n) => {
            let mut info = [0u8; INFO_BYTES];
            info.copy_from_slice(&word[..INFO_BYTES]);
            TmccDecode {
                info: Some(TmccInfo::decode(&info)),
                corrected: Some(n),
            }
        }
        Err(_) => TmccDecode {
            info: None,
            corrected: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> [u8; INFO_BYTES] {
        let v: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        v.try_into().unwrap()
    }

    /// TMCC information words received from the lab's satellite IF
    /// (2026-10-20 recordings), RS(64,48)-clean on every superframe; they
    /// decode to consistent assignments. TS_IDs are not station names.
    const BS_THREE_TS: &str = "5BE1E0781E0000000000000124924924924A4924924924920082008A0097FFFFFFFFFFFFFFFFFFF89FFFFFFFFFFFFFFF";
    const CS_ONE_TS: &str = "0261E0781E000000000000000000000000000000000000030107FFFFFFFFFFFFFFFFFFFFFFFFFFF91FFFFFFFFFFFFFFF";

    #[test]
    fn live_words_decode_consistently() {
        let bs = TmccInfo::decode(&hex(BS_THREE_TS));
        assert_eq!(bs.order_of_change, 11);
        assert_eq!(
            bs.combinations[0].combination(),
            Some(Combination::Tc8psk2_3)
        );
        assert_eq!(bs.combinations[0].slots, 48);
        assert!(
            bs.combinations[1..]
                .iter()
                .all(|&c| c == CombinationEntry::DUMMY)
        );
        assert_eq!(&bs.relative_ts[..16], &[0; 16]);
        assert_eq!(&bs.relative_ts[16..32], &[1; 16]);
        assert_eq!(&bs.relative_ts[32..], &[2; 16]);
        assert_eq!(&bs.ts_ids[..3], &[0x4010, 0x4011, 0x4012]);
        assert!(bs.ts_ids[3..].iter().all(|&t| t == UNUSED_TS_ID));
        assert!(!bs.emergency);
        assert_eq!(bs.uplink, 2);
        bs.validate().unwrap();
        let cs = TmccInfo::decode(&hex(CS_ONE_TS));
        assert_eq!(cs.combinations[0].combination(), Some(Combination::Qpsk3_4));
        assert_eq!(cs.combinations[0].slots, 48);
        assert_eq!(cs.ts_ids[0], 0x6020);
        assert_eq!(cs.uplink, 4);
        cs.validate().unwrap();
        // QPSK 3/4: units of 4 slots, the 4th a dummy.
        let sc = cs.slot_combinations();
        assert_eq!(sc.iter().filter(|c| c.is_some()).count(), 36);
        assert!(sc[3].is_none() && sc[0].is_some() && sc[47].is_none());
        for x in [BS_THREE_TS, CS_ONE_TS] {
            assert_eq!(TmccInfo::decode(&hex(x)).encode(), hex(x));
        }
    }

    /// A superframe of headers decodes back (sync words, PRBS, RS).
    #[test]
    fn headers_round_trip() {
        let info = TmccInfo::decode(&hex(BS_THREE_TS));
        let h = headers(&info);
        assert_eq!(&h[0][..2], &[0x1B, 0x95]);
        assert_eq!(&h[0][10..], &[0xA3, 0x40]);
        assert!(h[1..].iter().all(|b| b[10..] == [0x5C, 0xBF]));
        let blocks: [[u8; 8]; 8] = std::array::from_fn(|f| h[f][2..10].try_into().unwrap());
        let d = decode_blocks(&blocks);
        assert_eq!(d.corrected, Some(0));
        assert_eq!(d.info, Some(info));
    }

    /// The first TMCC byte of frame 1 meets PRBS byte 03h (the generator's
    /// first output byte, BO.1408-1 §6.4 "the first bit … to the MSB of the
    /// third byte of the first block").
    #[test]
    fn spec_tmcc_prbs_alignment() {
        let blocks = [[0u8; 8]; 8];
        let d = derandomise(&blocks);
        let mut p = Prbs15::new();
        let seq: Vec<u8> = (0..96).map(|_| p.next_byte()).collect();
        assert_eq!(d[0], 0x03);
        // frame f's block starts at header byte 12 f + 2, minus the 2 bytes
        // before the first TMCC byte.
        for f in 0..8 {
            assert_eq!(&d[f * 8..f * 8 + 8], &seq[12 * f..12 * f + 8]);
        }
    }

    /// Validation errors.
    #[test]
    fn validation() {
        let entry = |c, slots| CombinationEntry {
            word: CombinationWord::Assigned(c),
            slots,
        };
        let mut i = TmccInfo::decode(&hex(CS_ONE_TS));
        i.combinations[0].slots = 44;
        assert_eq!(i.validate(), Err(TmccError::SlotSum(44)));
        i.combinations[0].slots = 46;
        i.combinations[1] = entry(Combination::Tc8psk2_3, 2);
        assert_eq!(
            i.validate(),
            Err(TmccError::Unit {
                combination: Combination::Qpsk3_4,
                slots: 46,
                unit: 4
            })
        );
        i.combinations[0] = entry(Combination::Qpsk3_4, 44);
        i.combinations[1] = entry(Combination::Tc8psk2_3, 4);
        assert_eq!(i.validate(), Err(TmccError::Order));
        i.combinations[1] = CombinationEntry {
            word: CombinationWord::from_word(0b1000),
            slots: 4,
        };
        assert_eq!(i.validate(), Err(TmccError::ReservedWord(8)));
        // A reserved word claims its slots but assigns none of them.
        assert!(i.slot_combinations()[44..].iter().all(|c| c.is_none()));
    }
}
