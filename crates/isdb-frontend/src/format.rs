//! The `Symbols` boundary, version 1: what leaves the front end (the FPGA
//! over DMA, or the CPU model) as a stream of 16-bit little-endian words.
//!
//! The stream is a sequence of blocks. A block is [`HEADER_WORDS`] header
//! words, the payload of L = 2^`block_log2` symbols and one trailer word:
//!
//! | Word | Content |
//! |---|---|
//! | 0, 1 | sync: `0x1ACF`, `0xFC1D` |
//! | 2 | bits 0–3: format version (1); 4–7: bits per component − 1; 8–11: `block_log2`; 12–15: flags (0) |
//! | 3, 4 | block sequence number, 32 bits, low word first (wraps) |
//! | 5 | AGC gain used for the block's first symbol (Q6.10) |
//! | 6, 7 | period register at the block's first symbol, Q8.24 samples, low word first |
//! | 8 | check: words 2 to 8 sum to `0xFFFF` (mod 2¹⁶) |
//!
//! Payload: each symbol is `I | Q << bits` (two's-complement fields of
//! `bits` bits), appended to a bit stream least significant bit first and
//! cut into 16-bit words, also least significant bit first. 2·bits·L is a
//! multiple of 16, so every block is a whole number of words.
//!
//! Trailer: [`crc16`] of the payload words with the low 16 bits of the
//! block's sequence number as initial value.
//!
//! A reader knows the block length from the header and accepts a block
//! when the trailer at its end matches the CRC of its payload under the
//! header's sequence number: a loss inside the block moves other data into
//! the payload and the trailer position, and a loss of whole blocks
//! splices a later block's payload and trailer onto the header (another
//! sequence number), so any loss is detected whatever the payload holds
//! (a false match needs a 16-bit coincidence). Sequence numbers
//! count the missing blocks; a damaged or missing block is dropped and
//! counted, and the blocks around it stay in order with the gap reported.

use crate::params::{FORMAT_VERSION, Params};

/// Header words per block.
pub const HEADER_WORDS: usize = 9;
/// Trailer words per block.
pub const TRAILER_WORDS: usize = 1;
/// CRC-16 with the CCITT polynomial x¹⁶ + x¹² + x⁵ + 1 (0x1021), no
/// final inversion: each word is shifted in most significant bit first,
/// starting from `init`.
pub fn crc16(init: u16, words: &[u16]) -> u16 {
    words.iter().fold(init, |mut crc, &w| {
        for bit in (0..16).rev() {
            let fb = ((crc >> 15) ^ (w >> bit)) & 1;
            crc = (crc << 1) ^ if fb != 0 { 0x1021 } else { 0 };
        }
        crc
    })
}

/// The two sync words.
pub const SYNC: [u16; 2] = [0x1ACF, 0xFC1D];

/// A block header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Bits per component.
    pub bits: u8,
    /// log2 of the symbols per block.
    pub block_log2: u8,
    /// Flags (0 in version 1).
    pub flags: u8,
    /// Block sequence number.
    pub seq: u32,
    /// AGC gain at the block's first symbol (Q6.10).
    pub gain: u16,
    /// Period at the block's first symbol (Q8.24 input samples).
    pub period: u32,
}

impl Header {
    /// The header words.
    pub fn words(&self) -> [u16; HEADER_WORDS] {
        let mut w = [
            SYNC[0],
            SYNC[1],
            FORMAT_VERSION
                | u16::from(self.bits - 1) << 4
                | u16::from(self.block_log2) << 8
                | u16::from(self.flags) << 12,
            self.seq as u16,
            (self.seq >> 16) as u16,
            self.gain,
            self.period as u16,
            (self.period >> 16) as u16,
            0,
        ];
        let sum = w[2..8].iter().fold(0u16, |a, &b| a.wrapping_add(b));
        w[8] = 0xFFFF_u16.wrapping_sub(sum);
        w
    }

    /// Parse header words; `None` unless the sync and version match and
    /// the fields are in range.
    pub fn parse(w: &[u16]) -> Option<Self> {
        if w.len() < HEADER_WORDS || w[0] != SYNC[0] || w[1] != SYNC[1] {
            return None;
        }
        if w[2] & 0xF != FORMAT_VERSION
            || w[2..HEADER_WORDS]
                .iter()
                .fold(0u16, |a, &b| a.wrapping_add(b))
                != 0xFFFF
        {
            return None;
        }
        let bits = ((w[2] >> 4) & 0xF) as u8 + 1;
        let block_log2 = ((w[2] >> 8) & 0xF) as u8;
        if !(2..=8).contains(&bits) || !(8..=15).contains(&block_log2) {
            return None;
        }
        Some(Self {
            bits,
            block_log2,
            flags: (w[2] >> 12) as u8,
            seq: u32::from(w[3]) | u32::from(w[4]) << 16,
            gain: w[5],
            period: u32::from(w[6]) | u32::from(w[7]) << 16,
        })
    }

    /// Symbols per block.
    pub fn symbols(&self) -> usize {
        1 << self.block_log2
    }

    /// Payload words per block.
    pub fn payload_words(&self) -> usize {
        self.symbols() * 2 * usize::from(self.bits) / 16
    }

    /// Words of the whole block (header, payload, trailer).
    pub fn block_words(&self) -> usize {
        HEADER_WORDS + self.payload_words() + TRAILER_WORDS
    }
}

/// Packs symbols into blocks of words, as the FPGA does.
#[derive(Debug, Clone)]
pub struct Packer {
    bits: u8,
    block_log2: u8,
    /// Sequence number of the block being filled.
    seq: u32,
    crc: u16,
    in_block: usize,
    acc: u64,
    nacc: u32,
}

impl Packer {
    /// A packer for `p`'s output width and block length, sequence 0.
    pub fn new(p: &Params) -> Self {
        Self {
            bits: p.bits,
            block_log2: p.block_log2,
            seq: 0,
            crc: 0,
            in_block: 0,
            acc: 0,
            nacc: 0,
        }
    }

    /// Append the words for one symbol (`q` = output components, `gain` and
    /// `period` as used for it) to `out`.
    pub fn push(&mut self, q: [i8; 2], gain: u16, period: u32, out: &mut Vec<u16>) {
        if self.in_block == 0 {
            out.extend_from_slice(
                &Header {
                    bits: self.bits,
                    block_log2: self.block_log2,
                    flags: 0,
                    seq: self.seq,
                    gain,
                    period,
                }
                .words(),
            );
            self.crc = self.seq as u16;
        }
        let b = u32::from(self.bits);
        let mask = (1u64 << b) - 1;
        let sym = (q[0] as u8 as u64 & mask) | ((q[1] as u8 as u64 & mask) << b);
        self.acc |= sym << self.nacc;
        self.nacc += 2 * b;
        while self.nacc >= 16 {
            self.crc = crc16(self.crc, &[self.acc as u16]);
            out.push(self.acc as u16);
            self.acc >>= 16;
            self.nacc -= 16;
        }
        self.in_block += 1;
        if self.in_block == 1 << self.block_log2 {
            debug_assert_eq!(self.nacc, 0);
            out.push(self.crc);
            self.seq = self.seq.wrapping_add(1);
            self.in_block = 0;
        }
    }
}

/// What a [`Reader`] found in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A whole block.
    Block {
        /// Its header.
        header: Header,
        /// Its symbols (sign-extended components).
        symbols: Vec<[i8; 2]>,
    },
    /// Blocks lost before the next good block: `blocks` from the sequence
    /// numbers (0 when the stream was resynchronised without one), and
    /// the words skipped to find the next header.
    Loss {
        /// Missing blocks (sequence numbers skipped).
        blocks: u64,
        /// Words discarded while resynchronising.
        words: u64,
    },
    /// The stream ended inside a block: its words are dropped.
    Truncated {
        /// Words after the last whole block.
        words: u64,
    },
}

/// Parses the word stream back into blocks, reporting losses.
#[derive(Debug, Clone, Default)]
pub struct Reader {
    buf: Vec<u16>,
    pos: usize,
    expect_seq: Option<u32>,
    skipped: u64,
    odd_byte: Option<u8>,
}

impl Reader {
    /// A reader at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed little-endian bytes (any split).
    pub fn feed_bytes(&mut self, bytes: &[u8], out: &mut Vec<Event>) {
        let mut b = bytes;
        let mut words = Vec::with_capacity(b.len() / 2 + 1);
        if let Some(lo) = self.odd_byte.take() {
            if let Some((&hi, rest)) = b.split_first() {
                words.push(u16::from_le_bytes([lo, hi]));
                b = rest;
            } else {
                self.odd_byte = Some(lo);
            }
        }
        let (pairs, rest) = b.as_chunks::<2>();
        words.extend(pairs.iter().map(|&p| u16::from_le_bytes(p)));
        if let [last] = rest {
            self.odd_byte = Some(*last);
        }
        self.feed(&words, out);
    }

    /// Feed words.
    pub fn feed(&mut self, words: &[u16], out: &mut Vec<Event>) {
        self.buf.extend_from_slice(words);
        loop {
            let avail = &self.buf[self.pos..];
            if avail.len() < HEADER_WORDS {
                break;
            }
            let block = Header::parse(avail).map(|h| (h, h.block_words()));
            match block {
                Some((_, len)) if avail.len() < len => break,
                Some((h, len))
                    if avail[len - 1]
                        == crc16(h.seq as u16, &avail[HEADER_WORDS..len - TRAILER_WORDS]) =>
                {
                    self.deliver(h, len, out);
                    continue;
                }
                _ => {}
            }
            // No intact block here: resynchronise on the next sync word.
            let next = avail[1..]
                .iter()
                .position(|&w| w == SYNC[0])
                .map_or(avail.len() - (HEADER_WORDS - 1), |i| i + 1);
            self.skipped += next as u64;
            self.pos += next;
        }
        if self.pos > 1 << 16 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn deliver(&mut self, h: Header, len: usize, out: &mut Vec<Event>) {
        let missing = self
            .expect_seq
            .map_or(0, |s| u64::from(h.seq.wrapping_sub(s)));
        if missing > 0 || self.skipped > 0 {
            out.push(Event::Loss {
                blocks: missing,
                words: self.skipped,
            });
            self.skipped = 0;
        }
        let avail = &self.buf[self.pos..];
        out.push(Event::Block {
            header: h,
            symbols: unpack(&avail[HEADER_WORDS..len - TRAILER_WORDS], h.bits),
        });
        self.expect_seq = Some(h.seq.wrapping_add(1));
        self.pos += len;
    }

    /// End of the stream: what is left is not a whole block.
    pub fn finish(&mut self, out: &mut Vec<Event>) {
        let left = (self.buf.len() - self.pos) as u64 + self.skipped;
        if left > 0 {
            out.push(Event::Truncated { words: left });
        }
        self.buf.clear();
        self.pos = 0;
        self.skipped = 0;
    }
}

/// Unpack `bits`-bit symbol pairs from payload words.
pub fn unpack(words: &[u16], bits: u8) -> Vec<[i8; 2]> {
    let b = u32::from(bits);
    let n = words.len() * 16 / (2 * b as usize);
    let mut out = Vec::with_capacity(n);
    let (mut acc, mut nacc) = (0u64, 0u32);
    let mut it = words.iter();
    let sign = |v: u64| -> i8 {
        let v = v as i64;
        (if v & (1 << (b - 1)) != 0 {
            v - (1 << b)
        } else {
            v
        }) as i8
    };
    for _ in 0..n {
        while nacc < 2 * b {
            acc |= u64::from(*it.next().expect("enough words")) << nacc;
            nacc += 16;
        }
        let mask = (1u64 << b) - 1;
        out.push([sign(acc & mask), sign((acc >> b) & mask)]);
        acc >>= 2 * b;
        nacc -= 2 * b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Design;
    use proptest::prelude::*;

    fn params(bits: u8, block_log2: u8) -> Params {
        let mut d = Design::new(40e6, 28.86e6, 0.35);
        d.bits = bits;
        d.block_log2 = block_log2;
        Params::design(&d).unwrap()
    }

    #[test]
    fn spec_crc16_xmodem_check_value() {
        // CRC-16/XMODEM (poly 0x1021, init 0, MSB first, no final XOR):
        // check value of ASCII "123456789".
        assert_eq!(crc16_bytes(b"123456789"), 0x31C3);
    }

    #[test]
    fn crc16_of_words_is_the_crc_of_their_big_endian_bytes() {
        let w = [0x3132, 0x3334, 0x3536, 0x3738, 0xFC1D];
        let bytes: Vec<u8> = w.iter().flat_map(|v: &u16| v.to_be_bytes()).collect();
        assert_eq!(crc16(0, &w), crc16_bytes(&bytes));
    }

    fn crc16_bytes(b: &[u8]) -> u16 {
        b.iter().fold(0u16, |mut crc, &x| {
            crc ^= u16::from(x) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
            crc
        })
    }

    #[test]
    fn spec_header_layout() {
        let h = Header {
            bits: 5,
            block_log2: 12,
            flags: 0,
            seq: 0x1234_5678,
            gain: 0x0400,
            period: 0x0162_3456,
        };
        assert_eq!(
            h.words(),
            [
                0x1ACF, 0xFC1D, 0x0C41, 0x5678, 0x1234, 0x0400, 0x3456, 0x0162, 0x515A
            ]
        );
        assert_eq!(Header::parse(&h.words()), Some(h));
        assert_eq!(h.payload_words(), 4096 * 10 / 16);
    }

    fn pack(bits: u8, block_log2: u8, syms: &[[i8; 2]]) -> Vec<u16> {
        let mut pk = Packer::new(&params(bits, block_log2));
        let mut w = Vec::new();
        for &s in syms {
            pk.push(s, 1024, 1 << 24, &mut w);
        }
        w
    }

    proptest! {
        #[test]
        fn prop_pack_unpack_round_trip(bits in 2u8..=8, blocks in 1usize..4, seed in any::<u64>()) {
            let l = 256usize;
            let lim = 1i64 << (bits - 1);
            let mut s = seed;
            let syms: Vec<[i8; 2]> = (0..l * blocks).map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let a = ((s >> 33) as i64).rem_euclid(2 * lim) - lim;
                let b = ((s >> 13) as i64).rem_euclid(2 * lim) - lim;
                [a as i8, b as i8]
            }).collect();
            let words = pack(bits, 8, &syms);
            let mut r = Reader::new();
            let mut ev = Vec::new();
            r.feed(&words, &mut ev);
            r.finish(&mut ev);
            let losses = ev.iter().filter(|e| matches!(e, Event::Loss { .. })).count();
            prop_assert_eq!(losses, 0);
            let got: Vec<[i8; 2]> = ev.iter().flat_map(|e| match e {
                Event::Block { symbols, .. } => symbols.clone(),
                Event::Loss { .. } | Event::Truncated { .. } => Vec::new(),
            }).collect();
            prop_assert_eq!(got, syms);
        }

        #[test]
        fn prop_any_loss_is_reported_and_the_rest_kept(bits in 2u8..=8, cut_at in 0usize..2000, cut_len in 1usize..700) {
            let l = 256usize;
            let syms: Vec<[i8; 2]> = (0..l * 8).map(|i| [(i % 3) as i8 - 1, (i % 2) as i8]).collect();
            let words = pack(bits, 8, &syms);
            let block_words = HEADER_WORDS + l * 2 * usize::from(bits) / 16 + TRAILER_WORDS;
            let cut_at = cut_at.min(words.len() - 1);
            let cut_end = (cut_at + cut_len).min(words.len());
            let mut damaged: Vec<u16> = words[..cut_at].to_vec();
            damaged.extend_from_slice(&words[cut_end..]);
            let mut r = Reader::new();
            let mut ev = Vec::new();
            for chunk in damaged.chunks(97) {
                r.feed(chunk, &mut ev);
            }
            r.finish(&mut ev);
            let mut seqs = Vec::new();
            let mut lost = 0u64;
            for e in &ev {
                match e {
                    Event::Block { header, symbols } => {
                        let s = header.seq as usize;
                        prop_assert_eq!(&symbols[..], &syms[s * l..(s + 1) * l]);
                        seqs.push(s);
                    }
                    Event::Loss { blocks, .. } => lost += blocks,
                    Event::Truncated { .. } => {}
                }
            }
            // Every block the cut did not touch is delivered.
            for b in 0..8usize {
                let (start, end) = (b * block_words, (b + 1) * block_words);
                if cut_end <= start || cut_at >= end {
                    prop_assert!(seqs.contains(&b), "block {} missing: {:?}", b, seqs);
                }
            }
            prop_assert!(seqs.windows(2).all(|w| w[1] > w[0]));
            // Every block between the first and last delivered ones is
            // either delivered or counted as lost.
            if let (Some(&a), Some(&b)) = (seqs.first(), seqs.last()) {
                prop_assert_eq!(lost as usize + seqs.len(), b - a + 1);
            }
        }
    }
}
