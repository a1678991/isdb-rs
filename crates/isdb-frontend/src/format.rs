//! The `Symbols` boundary: what leaves the front end (the FPGA over DMA,
//! or the CPU model) as a stream of 16-bit little-endian words. Versions
//! 1 to 4 share the header and payload layout; version 2 changed the
//! arithmetic (see [`crate::params::FORMAT_VERSION`]), version 3 the
//! trailer (a CRC-32C instead of a CRC-16) and version 4 the loop again
//! (same words as version 3). A [`Reader`] accepts all four; a [`Packer`]
//! writes version 4.
//!
//! The stream is a sequence of blocks. A block is [`HEADER_WORDS`] header
//! words, the payload of L = 2^`block_log2` symbols and the trailer
//! ([`trailer_words`]: one word in versions 1 and 2, two from version 3):
//!
//! | Word | Content |
//! |---|---|
//! | 0, 1 | sync: `0x1ACF`, `0xFC1D` |
//! | 2 | bits 0–3: format version (1 to 4); 4–7: bits per component − 1; 8–11: `block_log2`; 12–15: flags (0) |
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
//! **Trailer, versions 3 and 4**: [`crc32c`] of the block's sequence number and
//! payload, as two words, low word first: the standard CRC-32C
//! (CRC-32/ISCSI: polynomial 0x1EDC6F41, reflected in and out, initial
//! value and final XOR 0xFFFFFFFF; check value of ASCII "123456789"
//! 0xE3069283) of the byte string *sequence number as 4 bytes little
//! endian, then the payload words as 2 bytes each, little endian* — the
//! payload's own byte order on the wire, so any CRC-32C library checks a
//! block. Why CRC-32C rather than the Ethernet CRC-32 (0x04C11DB7): at
//! these lengths (49 152 payload bits for 6 bits × 4 096 symbols, up to
//! 524 288 for 8 bits × 2¹⁵) the Ethernet polynomial guarantees only
//! Hamming distance 3 beyond 91 607 bits, CRC-32C distance 4 up to about
//! 2³¹ bits (Castagnoli, Bräuer and Herrmann, IEEE Trans. Commun. 1993;
//! Koopman, DSN 2002, "32-bit cyclic redundancy codes for Internet
//! applications" — cited, not re-derived here). Why seeded with the whole
//! sequence number: a splice of a later block's payload and trailer onto
//! a header then fails the CRC for any of 2³² sequence differences, not
//! 2¹⁶. The FPGA updates the CRC one payload word per clock (a parallel
//! XOR network derived from the polynomial).
//!
//! **Trailer, versions 1 and 2**: [`crc16`] of the payload words with the
//! low 16 bits of the block's sequence number as initial value.
//!
//! A reader knows the block length from the header and accepts a block
//! when the trailer at its end matches the CRC of its payload under the
//! header's sequence number: a loss inside the block moves other data into
//! the payload and the trailer position, and a loss of whole blocks
//! splices a later block's payload and trailer onto the header (another
//! sequence number), so any loss is detected whatever the payload holds,
//! up to a coincidence of the CRC's width. Sequence numbers count the
//! missing blocks; a damaged or missing block is dropped and counted, and
//! the blocks around it stay in order with the gap reported.
//!
//! **Versions 1 and 2: framing check.** A 16-bit CRC lets a splice through
//! on a 2⁻¹⁶ coincidence: a loss can glue a block's intact header to later
//! payload words and make an arbitrary payload word land in the trailer
//! position (seen with a property-test seed, `examples/formatcut.rs`).
//! For these versions the reader therefore also requires the block to be
//! *framed*: the next block's header (sync and header check) must start
//! at the word right after the trailer, or the stream must end exactly
//! there ([`Reader::finish`]). A spliced block ends at an arbitrary point,
//! so nothing frames it. The cost is one block: the intact block just
//! before a loss is not framed either, so it is dropped and counted as
//! lost with the damaged one (never delivered wrong), and delivery lags
//! the input by at most one block.
//!
//! **Version 3: the CRC-32C alone.** A version-3 block is accepted on its
//! CRC, without the framing check: no block is dropped besides the
//! damaged ones and nothing is held back. A false accept needs a 2⁻³²
//! coincidence at a position where a valid header was found. After a
//! loss the reader tries at most a few header positions (sync word,
//! header sum and version must all match first), so even at one loss per
//! second — far more than the loss-free link rates measured on the
//! PlutoSky (0 losses in 5 min, VERIFICATION.md) — a damaged block would
//! be delivered about once in 2³² s ≈ 136 years.

use crate::params::{FORMAT_VERSION, MIN_READ_VERSION, Params};

/// Header words per block.
pub const HEADER_WORDS: usize = 9;
/// Trailer words per block of the version a [`Packer`] writes
/// ([`FORMAT_VERSION`]); [`trailer_words`] gives any version's.
pub const TRAILER_WORDS: usize = 2;

/// Trailer words of a block of format `version`: 1 (CRC-16) for versions
/// 1 and 2, 2 (CRC-32C) from version 3.
pub fn trailer_words(version: u16) -> usize {
    if version >= 3 { 2 } else { 1 }
}

/// CRC-32C (CRC-32/ISCSI) lookup tables for slicing by 8: `T[0]` is the
/// byte-at-a-time table, `T[k][b]` the CRC of byte `b` followed by `k`
/// zero bytes.
const CRC32C_TABLES: [[u32; 256]; 8] = {
    let mut t = [[0u32; 256]; 8];
    let mut b = 0;
    while b < 256 {
        let mut c = b as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0x82F6_3B78
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][b] = c;
        b += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut b = 0;
        while b < 256 {
            let prev = t[k - 1][b];
            t[k][b] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            b += 1;
        }
        k += 1;
    }
    t
};

/// Update a reflected CRC-32C register (no initial value or final XOR
/// applied) with `bytes`, eight at a time.
fn crc32c_update(mut crc: u32, bytes: &[u8]) -> u32 {
    let t = &CRC32C_TABLES;
    let (chunks, rest) = bytes.as_chunks::<8>();
    for c in chunks {
        let lo = crc ^ u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        crc = t[7][(lo & 0xFF) as usize]
            ^ t[6][(lo >> 8 & 0xFF) as usize]
            ^ t[5][(lo >> 16 & 0xFF) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][c[4] as usize]
            ^ t[2][c[5] as usize]
            ^ t[1][c[6] as usize]
            ^ t[0][c[7] as usize];
    }
    for &b in rest {
        crc = (crc >> 8) ^ t[0][((crc ^ u32::from(b)) & 0xFF) as usize];
    }
    crc
}

/// The CRC-32C (CRC-32/ISCSI) of `bytes`: the standard function, check
/// value `crc32c_bytes(b"123456789") == 0xE3069283`.
pub fn crc32c_bytes(bytes: &[u8]) -> u32 {
    !crc32c_update(!0, bytes)
}

/// The version-3 trailer value: CRC-32C of `seq` (4 bytes, little
/// endian) followed by `payload` (2 bytes per word, little endian).
pub fn crc32c(seq: u32, payload: &[u16]) -> u32 {
    let mut crc = crc32c_update(!0, &seq.to_le_bytes());
    // words to bytes in batches, so the slicing loop sees long runs
    let mut buf = [0u8; 512];
    for chunk in payload.chunks(256) {
        for (d, w) in buf.as_chunks_mut::<2>().0.iter_mut().zip(chunk) {
            *d = w.to_le_bytes();
        }
        crc = crc32c_update(crc, &buf[..2 * chunk.len()]);
    }
    !crc
}

/// Whether `block` (exactly one block of `h`'s length, header included)
/// carries the right trailer for `h`'s version.
fn trailer_ok(h: &Header, block: &[u16]) -> bool {
    let len = block.len();
    let tw = trailer_words(h.version);
    let payload = &block[HEADER_WORDS..len - tw];
    if tw == 2 {
        let c = crc32c(h.seq, payload);
        block[len - 2] == c as u16 && block[len - 1] == (c >> 16) as u16
    } else {
        block[len - 1] == crc16(h.seq as u16, payload)
    }
}
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
    /// Format version of the block (written: [`FORMAT_VERSION`]).
    pub version: u16,
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
            (self.version & 0xF)
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
        let version = w[2] & 0xF;
        if !(MIN_READ_VERSION..=FORMAT_VERSION).contains(&version)
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
            version,
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
        HEADER_WORDS + self.payload_words() + trailer_words(self.version)
    }
}

/// Packs symbols into blocks of words, as the FPGA does.
#[derive(Debug, Clone)]
pub struct Packer {
    version: u16,
    bits: u8,
    block_log2: u8,
    /// Sequence number of the block being filled.
    seq: u32,
    /// The block's payload words so far (for its trailer).
    payload: Vec<u16>,
    in_block: usize,
    acc: u64,
    nacc: u32,
}

impl Packer {
    /// A packer for `p`'s output width and block length, sequence 0,
    /// writing format [`FORMAT_VERSION`].
    pub fn new(p: &Params) -> Self {
        Self::with_version(p, FORMAT_VERSION)
    }

    /// A packer writing blocks of format `version` (1..=[`FORMAT_VERSION`];
    /// older versions differ only in the trailer here: for tests and for
    /// writing streams an older reader takes).
    pub fn with_version(p: &Params, version: u16) -> Self {
        assert!(
            (MIN_READ_VERSION..=FORMAT_VERSION).contains(&version),
            "format version {version}"
        );
        Self {
            version,
            bits: p.bits,
            block_log2: p.block_log2,
            seq: 0,
            payload: Vec::new(),
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
                    version: self.version,
                    bits: self.bits,
                    block_log2: self.block_log2,
                    flags: 0,
                    seq: self.seq,
                    gain,
                    period,
                }
                .words(),
            );
            self.payload.clear();
        }
        let b = u32::from(self.bits);
        let mask = (1u64 << b) - 1;
        let sym = (q[0] as u8 as u64 & mask) | ((q[1] as u8 as u64 & mask) << b);
        self.acc |= sym << self.nacc;
        self.nacc += 2 * b;
        while self.nacc >= 16 {
            self.payload.push(self.acc as u16);
            out.push(self.acc as u16);
            self.acc >>= 16;
            self.nacc -= 16;
        }
        self.in_block += 1;
        if self.in_block == 1 << self.block_log2 {
            debug_assert_eq!(self.nacc, 0);
            if trailer_words(self.version) == 2 {
                let c = crc32c(self.seq, &self.payload);
                out.extend_from_slice(&[c as u16, (c >> 16) as u16]);
            } else {
                out.push(crc16(self.seq as u16, &self.payload));
            }
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
    /// Fault injection for the tests: accept any trailer.
    #[cfg(test)]
    skip_crc: bool,
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
                Some((h, len)) if self.trailer_ok(&h, &avail[..len]) => {
                    if trailer_words(h.version) == 2 {
                        // version 3: the CRC-32C alone (module doc)
                        self.deliver(h, len, out);
                        continue;
                    }
                    // versions 1, 2: framed, the next header starts right
                    // after the trailer
                    let next = &avail[len..];
                    if next.len() < HEADER_WORDS {
                        // not decidable yet (or the stream ends here: `finish`)
                        break;
                    }
                    if Header::parse(next).is_some() {
                        self.deliver(h, len, out);
                        continue;
                    }
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

    fn trailer_ok(&self, h: &Header, block: &[u16]) -> bool {
        #[cfg(test)]
        if self.skip_crc {
            return true;
        }
        trailer_ok(h, block)
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
            symbols: unpack(&avail[HEADER_WORDS..len - trailer_words(h.version)], h.bits),
        });
        self.expect_seq = Some(h.seq.wrapping_add(1));
        self.pos += len;
    }

    /// End of the stream: a checked block that ends exactly at the end is
    /// framed by it and delivered; what is left otherwise is not a whole,
    /// framed block.
    pub fn finish(&mut self, out: &mut Vec<Event>) {
        let avail = &self.buf[self.pos..];
        if let Some(h) = Header::parse(avail) {
            let len = h.block_words();
            if avail.len() == len && self.trailer_ok(&h, avail) {
                self.deliver(h, len, out);
            }
        }
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
        // A version-1 header (still read) …
        let h = Header {
            version: 1,
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
        // … and the same block in version 2: only the version nibble and
        // the check word change.
        let h2 = Header { version: 2, ..h };
        assert_eq!(
            h2.words(),
            [
                0x1ACF, 0xFC1D, 0x0C42, 0x5678, 0x1234, 0x0400, 0x3456, 0x0162, 0x5159
            ]
        );
        assert_eq!(Header::parse(&h2.words()), Some(h2));
        // … and in version 3 (same header; the block is one word longer).
        let h3 = Header { version: 3, ..h };
        assert_eq!(
            h3.words(),
            [
                0x1ACF, 0xFC1D, 0x0C43, 0x5678, 0x1234, 0x0400, 0x3456, 0x0162, 0x5158
            ]
        );
        assert_eq!(Header::parse(&h3.words()), Some(h3));
        assert_eq!(h2.block_words(), HEADER_WORDS + 2560 + 1);
        assert_eq!(h3.block_words(), HEADER_WORDS + 2560 + 2);
        // … and in version 4 (version 3's words; the loop changed).
        let h4 = Header { version: 4, ..h };
        assert_eq!(
            h4.words(),
            [
                0x1ACF, 0xFC1D, 0x0C44, 0x5678, 0x1234, 0x0400, 0x3456, 0x0162, 0x5157
            ]
        );
        assert_eq!(Header::parse(&h4.words()), Some(h4));
        assert_eq!(h4.block_words(), HEADER_WORDS + 2560 + 2);
        // Versions outside 1..=4 are not headers.
        for v in [0, 5, 15] {
            let mut w = Header { version: v, ..h }.words();
            w[8] = 0;
            let sum = w[2..8].iter().fold(0u16, |a, &b| a.wrapping_add(b));
            w[8] = 0xFFFF_u16.wrapping_sub(sum);
            assert_eq!(Header::parse(&w), None, "version {v}");
        }
    }

    fn pack_v(version: u16, bits: u8, block_log2: u8, syms: &[[i8; 2]]) -> Vec<u16> {
        let mut pk = Packer::with_version(&params(bits, block_log2), version);
        let mut w = Vec::new();
        for &s in syms {
            pk.push(s, 1024, 1 << 24, &mut w);
        }
        w
    }

    #[test]
    fn spec_crc32c_iscsi_check_value() {
        // CRC-32/ISCSI (CRC-32C): poly 0x1EDC6F41 reflected, init and
        // final XOR 0xFFFFFFFF; check value of ASCII "123456789".
        assert_eq!(crc32c_bytes(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c_bytes(b""), 0);
        // the slicing-by-8 path against a bit-at-a-time definition, on
        // every length 0..64 and both alignments
        let data: Vec<u8> = (0..80u32).map(|i| (i * 37 + 11) as u8).collect();
        for a in 0..2 {
            for n in 0..64 {
                let b = &data[a..a + n];
                assert_eq!(crc32c_bytes(b), crc32c_bitwise(b), "len {n} at {a}");
            }
        }
    }

    fn crc32c_bitwise(b: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &x in b {
            crc ^= u32::from(x);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0x82F6_3B78
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn spec_v3_trailer_is_the_crc32c_of_the_le_byte_string() {
        let payload = [0x3231u16, 0x3433, 0x3635, 0x3837, 0x0039];
        let seq = 0x0403_0201u32;
        let mut bytes = seq.to_le_bytes().to_vec();
        bytes.extend(payload.iter().flat_map(|w| w.to_le_bytes()));
        assert_eq!(crc32c(seq, &payload), crc32c_bytes(&bytes));
        // seq 0 and "123456789" as payload bytes: 4 zero bytes in front
        let w: Vec<u16> = b"12345678"
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| u16::from_le_bytes(p))
            .collect();
        let mut b = vec![0u8; 4];
        b.extend_from_slice(b"12345678");
        assert_eq!(crc32c(0, &w), crc32c_bytes(&b));
        // a long payload crosses the 256-word batches
        let long: Vec<u16> = (0..3072u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 7) as u16)
            .collect();
        let mut lb = 7u32.to_le_bytes().to_vec();
        lb.extend(long.iter().flat_map(|w| w.to_le_bytes()));
        assert_eq!(crc32c(7, &long), crc32c_bitwise(&lb));
    }

    #[test]
    fn every_version_round_trips_and_mixes_in_one_stream() {
        let syms: Vec<[i8; 2]> = (0..1024)
            .map(|i| [(i % 7) as i8 - 3, (i % 5) as i8 - 2])
            .collect();
        let mut words = Vec::new();
        for v in MIN_READ_VERSION..=FORMAT_VERSION {
            let w = pack_v(v, 4, 8, &syms);
            let h = Header::parse(&w).unwrap();
            assert_eq!(h.version, v);
            assert_eq!(w.len(), 4 * h.block_words());
            words.extend(w);
        }
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&words, &mut ev);
        r.finish(&mut ev);
        let blocks: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Block { header, symbols } => Some((header.version, symbols.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(blocks.len(), 4 * usize::from(FORMAT_VERSION));
        for (i, (v, s)) in blocks.iter().enumerate() {
            assert_eq!(*v as usize, 1 + i / 4);
            assert_eq!(&s[..], &syms[(i % 4) * 256..(i % 4 + 1) * 256]);
        }
        // sequence numbers restart per packer: counted as a loss of
        // 2³² − 4 blocks at each version change, so drop them from the
        // check; nothing was skipped
        assert!(
            ev.iter()
                .all(|e| !matches!(e, Event::Loss { words, .. } if *words > 0))
        );
    }

    /// Cut `cut_len` words at `cut_at` out of 8 blocks of format `version`
    /// and check the reader: every delivered block is right, untouched
    /// blocks are delivered (versions 1 and 2: except the framing cost,
    /// module doc), order kept and every gap counted. `skip_crc` makes
    /// the reader accept any trailer (fault injection).
    fn cut_check(
        version: u16,
        bits: u8,
        cut_at: usize,
        cut_len: usize,
        skip_crc: bool,
    ) -> Result<(), String> {
        let l = 256usize;
        let syms: Vec<[i8; 2]> = (0..l * 8)
            .map(|i| [(i % 3) as i8 - 1, (i % 2) as i8])
            .collect();
        let words = pack_v(version, bits, 8, &syms);
        let block_words = HEADER_WORDS + l * 2 * usize::from(bits) / 16 + trailer_words(version);
        let cut_at = cut_at.min(words.len() - 1);
        let cut_end = (cut_at + cut_len).min(words.len());
        let mut damaged: Vec<u16> = words[..cut_at].to_vec();
        damaged.extend_from_slice(&words[cut_end..]);
        let mut r = Reader::new();
        r.skip_crc = skip_crc;
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
                    if s >= 8 || symbols[..] != syms[s * l..(s + 1) * l] {
                        return Err(format!("block {s} delivered wrong"));
                    }
                    seqs.push(s);
                }
                Event::Loss { blocks, .. } => lost += blocks,
                Event::Truncated { .. } => {}
            }
        }
        // Every block the cut did not touch is delivered; for versions 1
        // and 2 except the one that ends right before the first damaged
        // block (the cut also removes or moves the header that frames it,
        // so it is counted as lost, never delivered wrong).
        let first_damaged = cut_at / block_words;
        for b in 0..8usize {
            let (start, end) = (b * block_words, (b + 1) * block_words);
            let framing_cost = trailer_words(version) == 1
                && b + 1 == first_damaged
                && cut_end > first_damaged * block_words;
            if (cut_end <= start || cut_at >= end) && !framing_cost && !seqs.contains(&b) {
                return Err(format!("block {b} missing: {seqs:?}"));
            }
        }
        if !seqs.windows(2).all(|w| w[1] > w[0]) {
            return Err(format!("out of order: {seqs:?}"));
        }
        // Every block between the first and last delivered ones is either
        // delivered or counted as lost.
        if let (Some(&a), Some(&b)) = (seqs.first(), seqs.last())
            && lost as usize + seqs.len() != b - a + 1
        {
            return Err(format!(
                "{lost} lost + {} delivered != {}",
                seqs.len(),
                b - a + 1
            ));
        }
        Ok(())
    }

    /// The splice found by the property test (seed `bits = 5, cut_at =
    /// 208, cut_len = 282`): before the framing check, a version-2 reader
    /// delivered block 1's intact header, 29 of its payload words and
    /// words from inside block 3 as block 1, a block-3 payload word in the
    /// trailer position equalling the CRC-16 of that spliced payload.
    #[test]
    fn spliced_block_with_a_matching_crc_is_not_delivered() {
        let l = 256usize;
        let syms: Vec<[i8; 2]> = (0..l * 8)
            .map(|i| [(i % 3) as i8 - 1, (i % 2) as i8])
            .collect();
        let words = pack_v(2, 5, 8, &syms);
        let mut damaged = words[..208].to_vec();
        damaged.extend_from_slice(&words[490..]);
        // the splice really does pass the header check and the CRC-16
        let h = Header::parse(&damaged[170..]).expect("block 1's header is intact");
        let len = h.block_words();
        let spliced = &damaged[170..170 + len];
        assert_eq!(
            spliced[len - 1],
            crc16(h.seq as u16, &spliced[HEADER_WORDS..len - 1])
        );
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&damaged, &mut ev);
        r.finish(&mut ev);
        let mut seqs = Vec::new();
        for e in &ev {
            if let Event::Block { header, symbols } = e {
                let s = header.seq as usize;
                assert_eq!(
                    &symbols[..],
                    &syms[s * l..(s + 1) * l],
                    "block {s} delivered wrong"
                );
                seqs.push(s);
            }
        }
        assert_eq!(seqs, [0, 3, 4, 5, 6, 7]);
        assert!(ev.contains(&Event::Loss {
            blocks: 2,
            words: 58
        }));
    }

    /// The two cuts the version-2 property test found (the splice, and the
    /// framing cost of a one-word cut), replayed on both formats: version
    /// 2 drops the intact block before the loss, version 3 keeps it.
    #[test]
    fn found_cuts_on_both_versions() {
        for (bits, at, len) in [(5u8, 208usize, 282usize), (7, 702, 1)] {
            cut_check(2, bits, at, len, false).unwrap();
            cut_check(3, bits, at, len, false).unwrap();
        }
        // version 3: the splice cut costs only the two damaged blocks
        let syms: Vec<[i8; 2]> = (0..256 * 8)
            .map(|i| [(i % 3) as i8 - 1, (i % 2) as i8])
            .collect();
        let words = pack_v(3, 7, 8, &syms);
        let mut damaged = words[..702].to_vec();
        damaged.extend_from_slice(&words[703..]);
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&damaged, &mut ev);
        r.finish(&mut ev);
        let seqs: Vec<u32> = ev
            .iter()
            .filter_map(|e| match e {
                Event::Block { header, .. } => Some(header.seq),
                _ => None,
            })
            .collect();
        // 7-bit blocks are 9 + 224 + 2 = 235 words: word 702 is inside
        // block 2, so only block 2 is lost
        assert_eq!(seqs, [0, 1, 3, 4, 5, 6, 7]);
    }

    /// Fault injection: with the trailer check skipped, the version-3 loss
    /// check must fail (so the property test below can fail).
    #[test]
    fn loss_check_fails_when_the_reader_skips_the_crc() {
        // Cuts inside a payload are what only the CRC catches (a cut that
        // takes a header is caught by the header check anyway): on a grid
        // of such cuts, the check must fail for most with the CRC skipped
        // and for none with it.
        let (mut tried, mut caught) = (0, 0);
        for bits in [3u8, 5, 6, 8] {
            for at in (40..1800).step_by(137) {
                for len in [1usize, 2, 5, 33] {
                    tried += 1;
                    cut_check(3, bits, at, len, false).unwrap();
                    if cut_check(3, bits, at, len, true).is_err() {
                        caught += 1;
                    }
                }
            }
        }
        assert!(caught * 2 > tried, "{caught} of {tried} cuts caught");
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
            for v in [2, FORMAT_VERSION] {
                let words = pack_v(v, bits, 8, &syms);
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
                prop_assert_eq!(got, syms.clone());
            }
        }

        /// Versions 1 and 2 (CRC-16 and the framing check). The name is
        /// the original test's, so its recorded seeds still replay here.
        #[test]
        fn prop_any_loss_is_reported_and_the_rest_kept(bits in 2u8..=8, cut_at in 0usize..2000, cut_len in 1usize..700) {
            if let Err(e) = cut_check(2, bits, cut_at, cut_len, false) {
                prop_assert!(false, "{}", e);
            }
        }

        /// Version 3 (CRC-32C alone): every untouched block is delivered,
        /// including the one right before a loss.
        #[test]
        fn prop_v3_any_loss_is_reported_and_every_untouched_block_kept(bits in 2u8..=8, cut_at in 0usize..2000, cut_len in 1usize..700) {
            if let Err(e) = cut_check(3, bits, cut_at, cut_len, false) {
                prop_assert!(false, "{}", e);
            }
        }
    }
}
