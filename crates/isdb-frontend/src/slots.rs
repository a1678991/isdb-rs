//! The `Slots` boundary, format version 2: what leaves the radio when the
//! whole ISDB-S3 receive chain runs in its FPGA (docs/plans/isdbs3-fpga-rx.md
//! "Boundary"; the chain's model is `isdbs3_phy::chain`). A stream of
//! 16-bit little-endian words in blocks with the `Symbols` framing
//! ([`crate::format`]): the same sync words, a 9-word header with a
//! sequence number and a header check, and the same CRC-32C trailer
//! seeded with the sequence number ([`crate::format::crc32c`]).
//!
//! | Word | Content |
//! |---|---|
//! | 0, 1 | sync: `0x1ACF`, `0xFC1D` |
//! | 2 | bits 0–3: `0xF` (the `Slots` marker); 4–7: block type ([`BlockType`]); 8–11: `Slots` format version ([`SLOTS_VERSION`] = 2 written; [`SLOTS_VERSION_MIN`] = 1 and 2 read); 12–15: flags (0) |
//! | 3, 4 | block sequence number, 32 bits, low word first (wraps; one counter for all block types) |
//! | 5 | payload words P |
//! | 6, 7 | frame counter, 32 bits, low word first |
//! | 8 | check: words 2 to 8 sum to `0xFFFF` (mod 2¹⁶) |
//!
//! then P payload words and the 2 trailer words (CRC-32C of the sequence
//! number as 4 bytes little endian and the payload words as 2 bytes each,
//! little endian, low word first).
//!
//! **Why `0xF` in the version nibble**: a `Symbols` reader
//! ([`crate::format::Reader`]) accepts versions 1 to 4 there and refuses
//! the rest, so a `Slots` stream given to it is rejected header by
//! header (no block delivered, every word counted as skipped), and a
//! `Slots` reader refuses `Symbols` blocks likewise; the two cannot be
//! confused, and the radio can send either on the same DMA.
//!
//! Bit fields in a payload are packed as `Symbols` packs its symbols:
//! bit i of the field is bit (i mod 16) of word ⌊i / 16⌋, least
//! significant bit first, padded with zeros to a whole word. Payloads by
//! block type:
//!
//! - **slot** (type 1, once per slot of a decoded frame, in slot order,
//!   dummies included):
//!   - w0: slot index (0–119) | mode (0–7) << 8 | dummy << 11 | converged
//!     << 12 | BCH ok << 13;
//!   - w1: modulation code (Table 3.11-1) | rate code (Table 3.11-2) << 4
//!     | LDPC iterations (saturated to 255) << 8;
//!   - w2: BCH bits corrected (`0xFFFF`: uncorrectable or not
//!     converged);
//!   - then the slot's 176 header bits and the data bits D of its rate
//!     (Table 3.2-1), after dispersal and BCH, in transmission order, as a
//!     bit field (absent for a dummy slot). A slot whose LDPC or BCH
//!     failed carries its bits as decoded; the host treats it as lost.
//! - **tmcc** (type 2, once per frame, after its slots): w0: BCH bits
//!   corrected (`0xFFFF`: failed); w1: LDPC iterations (`0xFFFF`: not
//!   converged); w2, w3: the frame counter of the first frame its layout
//!   applies to (frame + layout lag); then the 9 422 TMCC information bits
//!   as decoded (§3.11) as a bit field.
//! - **status** (type 3, once per frame, after its tmcc): the fields of
//!   [`Status`] in the order of its definition, each as the words given
//!   there (signed values two's complement, 32-bit values low word
//!   first).
//! - **sums** (type 4, version 2; at most once per frame, right after its
//!   status block: for each frame whose training sums the equaliser
//!   latched at its end, i.e. each decoded frame, a frame with a layout;
//!   [`SumsBlock`]): the equaliser's normal-equation sums of the frame's
//!   6 720 training symbols, what the chain's registers `SUMS_FRAME`,
//!   `SUMS_COUNT`, `SUMS_R` and `SUMS_P` hold
//!   ([`crate::control::chain`]), 465 payload words (476 with the header
//!   and trailer):
//!
//!   | Payload word | Content |
//!   |---|---|
//!   | 0, 1 | the frame summed (`SUMS_FRAME`), 32 bits, low word first: the header's frame counter |
//!   | 2 | training symbols summed (`SUMS_COUNT`, at most 6 720) |
//!   | 3 + 6i … 8 + 6i | sum i < 77: re bits 0–15, 16–31, 32–47, then im bits 0–15, 16–31, 32–47: each a 48-bit two's-complement integer, low word first |
//!
//!   Sums 0–65 are R's upper triangle `R[r][c] = Σ conj(x_r) · x_c`, r ≤
//!   c, row by row (`r = 0: c = 0 … 10`, then `r = 1: c = 1 … 10`, …):
//!   the order of the registers `SUMS_R + 4i`; sums 66–76 are `p[r] =
//!   Σ conj(x_r) · d`, r = 0 … 10, the order of `SUMS_P + 4r`
//!   ([`crate::control::chain::all_sum_registers`]).
//!
//! **Versions.** Version 2 is version 1 and the sums block: the slot,
//! tmcc and status blocks are word for word the same, only header word
//! 2's version nibble reads 2 (every block of a stream carries its
//! writer's version). The [`Reader`] reads both. A version-1 reader (the
//! hosts of builds 1–9) refuses every header of a version-2 stream (its
//! version nibble), so it delivers nothing rather than a stream it cannot
//! fully read; the host also reads the chain's version at open
//! (`VERSION`, and the ID word's bits 11–8) and refuses an image whose
//! format it does not read. A block whose type this reader does not know
//! for its header's version (types 0 and 5–15, or type 4 in a version-1
//! header), with a valid header check and CRC, is reported as
//! [`Event::Unknown`] and skipped: it is an intact block of a writer this
//! reader does not fully understand, not damage, so it neither counts as
//! a loss (which would close the host's TLV assemblers for nothing) nor
//! disappears unnoticed.

use crate::format::{HEADER_WORDS, SYNC, crc32c};
use tracing::{debug, warn};

/// The `Slots` format version this module writes (and the newest it
/// reads).
pub const SLOTS_VERSION: u16 = 2;
/// The oldest `Slots` format version this module reads.
pub const SLOTS_VERSION_MIN: u16 = 1;
/// The first version with the sums block ([`BlockType::Sums`]).
pub const SUMS_VERSION: u16 = 2;
/// The version nibble of a `Slots` header.
pub const SLOTS_MARKER: u16 = 0xF;
/// Trailer words (CRC-32C).
pub const TRAILER_WORDS: usize = 2;
/// Payload words of a block at most (the header's word 5).
pub const MAX_PAYLOAD: usize = 4096;

/// Whether this module reads `Slots` format `version`.
pub fn readable(version: u16) -> bool {
    (SLOTS_VERSION_MIN..=SLOTS_VERSION).contains(&version)
}

/// A block's type (header word 2, bits 4–7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// One slot.
    Slot,
    /// One frame's TMCC.
    Tmcc,
    /// One frame's status.
    Status,
    /// One frame's training sums (version 2).
    Sums,
    /// A type this module does not know (its code: 0 or 5–15).
    Unknown(u8),
}

impl BlockType {
    /// The code in header word 2.
    pub fn code(self) -> u16 {
        match self {
            Self::Slot => 1,
            Self::Tmcc => 2,
            Self::Status => 3,
            Self::Sums => 4,
            Self::Unknown(c) => u16::from(c & 0xF),
        }
    }

    fn from_code(c: u16) -> Self {
        match c & 0xF {
            1 => Self::Slot,
            2 => Self::Tmcc,
            3 => Self::Status,
            4 => Self::Sums,
            x => Self::Unknown(u8::try_from(x).expect("4 bits")),
        }
    }

    /// Whether `Slots` format `version` defines this type.
    pub fn in_version(self, version: u16) -> bool {
        match self {
            Self::Slot | Self::Tmcc | Self::Status => true,
            Self::Sums => version >= SUMS_VERSION,
            Self::Unknown(_) => false,
        }
    }
}

/// A `Slots` block header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Block type.
    pub kind: BlockType,
    /// `Slots` format version (header word 2, bits 8–11).
    pub version: u16,
    /// Sequence number.
    pub seq: u32,
    /// Payload words.
    pub payload: u16,
    /// Frame counter.
    pub frame: u32,
}

impl Header {
    /// The header words.
    pub fn words(&self) -> [u16; HEADER_WORDS] {
        let mut w = [
            SYNC[0],
            SYNC[1],
            SLOTS_MARKER | self.kind.code() << 4 | (self.version & 0xF) << 8,
            lo(self.seq),
            hi(self.seq),
            self.payload,
            lo(self.frame),
            hi(self.frame),
            0,
        ];
        let sum = w[2..8].iter().fold(0u16, |a, &b| a.wrapping_add(b));
        w[8] = 0xFFFF_u16.wrapping_sub(sum);
        w
    }

    /// Parse header words: `None` unless the sync, the marker, a version
    /// this module reads ([`readable`]), zero flags, a payload of at most
    /// [`MAX_PAYLOAD`] words and the check all hold. Any type code is
    /// taken ([`BlockType::Unknown`] for one this module does not know).
    pub fn parse(w: &[u16]) -> Option<Self> {
        if w.len() < HEADER_WORDS || w[0] != SYNC[0] || w[1] != SYNC[1] {
            return None;
        }
        let version = (w[2] >> 8) & 0xF;
        if w[2] & 0xF != SLOTS_MARKER
            || !readable(version)
            || w[2] >> 12 != 0
            || usize::from(w[5]) > MAX_PAYLOAD
            || w[2..HEADER_WORDS]
                .iter()
                .fold(0u16, |a, &b| a.wrapping_add(b))
                != 0xFFFF
        {
            return None;
        }
        Some(Self {
            kind: BlockType::from_code((w[2] >> 4) & 0xF),
            version,
            seq: u32::from(w[3]) | u32::from(w[4]) << 16,
            payload: w[5],
            frame: u32::from(w[6]) | u32::from(w[7]) << 16,
        })
    }

    /// Words of the whole block.
    pub fn block_words(&self) -> usize {
        HEADER_WORDS + usize::from(self.payload) + TRAILER_WORDS
    }
}

fn lo(x: u32) -> u16 {
    // the low half of a 32-bit value: truncation is the value
    #[allow(clippy::cast_possible_truncation)]
    let v = x as u16;
    v
}

fn hi(x: u32) -> u16 {
    lo(x >> 16)
}

/// Pack bits (0/1, LSB first) into words, zero-padded.
pub fn pack_bits(bits: &[u8], out: &mut Vec<u16>) {
    for c in bits.chunks(16) {
        out.push(
            c.iter()
                .enumerate()
                .fold(0u16, |a, (i, &b)| a | (u16::from(b & 1) << i)),
        );
    }
}

/// The first `n` bits of `words` (LSB first).
pub fn unpack_bits(words: &[u16], n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| u8::from(words.get(i / 16).is_some_and(|w| (w >> (i % 16)) & 1 == 1)))
        .collect()
}

/// Bytes (MSB first, as `isdbs3_phy` holds a slot's data) as bits.
pub fn bytes_to_bits(bytes: &[u8], n: usize) -> Vec<u8> {
    (0..n).map(|i| (bytes[i / 8] >> (7 - i % 8)) & 1).collect()
}

/// Bits back to bytes, MSB first (the last byte zero-padded).
pub fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|c| {
            c.iter()
                .enumerate()
                .fold(0u8, |a, (i, &b)| a | ((b & 1) << (7 - i)))
        })
        .collect()
}

/// One slot block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotBlock {
    /// Frame counter.
    pub frame: u32,
    /// Slot index 0 … 119.
    pub slot: u8,
    /// Transmission mode 0 … 7.
    pub mode: u8,
    /// A dummy slot (no bits).
    pub dummy: bool,
    /// LDPC converged.
    pub converged: bool,
    /// BCH bits corrected (`None`: uncorrectable or not converged).
    pub bch: Option<u16>,
    /// Modulation code (Table 3.11-1).
    pub modulation: u8,
    /// Rate code (Table 3.11-2).
    pub rate: u8,
    /// LDPC iterations (saturated to 255).
    pub iterations: u8,
    /// The header and data bits (0/1), transmission order.
    pub bits: Vec<u8>,
}

/// One tmcc block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmccBlock {
    /// Frame counter of the frame it was decoded from.
    pub frame: u32,
    /// BCH bits corrected (`None`: failed).
    pub bch: Option<u16>,
    /// LDPC iterations (`None`: not converged).
    pub iterations: Option<u16>,
    /// The first frame its layout applies to.
    pub applies_from: u32,
    /// The 9 422 information bits.
    pub bits: Vec<u8>,
}

/// Information bits of a TMCC block.
pub const TMCC_BITS: usize = 9_422;

/// Complex sums in a sums block: R's 66 upper-triangle entries and p's
/// 11 ([`crate::control::chain::R_ENTRIES`], [`crate::control::chain::TAPS`]).
pub const SUMS: usize = crate::control::chain::R_ENTRIES + crate::control::chain::TAPS;
/// Payload words of a sums block: the frame (2), the count (1) and 6 per
/// complex sum.
pub const SUMS_PAYLOAD: usize = 3 + 6 * SUMS;

/// One sums block (version 2): a frame's training sums as the chain
/// latched them at its end (module documentation, "sums"; the registers
/// of [`crate::control::chain`] hold the same values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumsBlock {
    /// Frame counter (header): the frame whose status block it follows.
    pub frame: u32,
    /// The frame summed (payload words 0, 1; `SUMS_FRAME`): the same as
    /// `frame` from the chain, kept as sent.
    pub sums_frame: u32,
    /// Training symbols summed (`SUMS_COUNT`).
    pub count: u16,
    /// `R[r][c]`, r ≤ c, row by row (66 complex `[re, im]`, 48-bit two's
    /// complement: values outside ±2⁴⁷ are written truncated to 48 bits).
    pub r: Vec<[i64; 2]>,
    /// `p[r]` (11 complex, as `r`).
    pub p: Vec<[i64; 2]>,
}

impl SumsBlock {
    /// The sums as the host's slow loop takes them (the registers' view,
    /// [`crate::control::chain::sums_from_words`]).
    pub fn to_training(&self) -> crate::control::chain::TrainingSums {
        crate::control::chain::TrainingSums {
            frame: self.sums_frame,
            count: u32::from(self.count),
            r: self.r.clone(),
            p: self.p.clone(),
        }
    }

    /// The block of `t` (its frame in the header too); `None` when `t`
    /// does not have 66 + 11 sums or its count does not fit 16 bits.
    pub fn from_training(t: &crate::control::chain::TrainingSums) -> Option<Self> {
        (t.r.len() == crate::control::chain::R_ENTRIES && t.p.len() == crate::control::chain::TAPS)
            .then_some(())?;
        Some(Self {
            frame: t.frame,
            sums_frame: t.frame,
            count: u16::try_from(t.count).ok()?,
            r: t.r.clone(),
            p: t.p.clone(),
        })
    }
}

/// The three 16-bit words of a 48-bit two's-complement value, low first.
fn w48(out: &mut Vec<u16>, v: i64) {
    for k in 0..3 {
        // 16-bit pieces of the low 48 bits: truncation is the value
        #[allow(clippy::cast_possible_truncation)]
        out.push((v >> (16 * k)) as u16);
    }
}

/// [`w48`] inverted: the value sign-extended from bit 47.
fn r48(w: &[u16]) -> i64 {
    let u = u64::from(w[0]) | u64::from(w[1]) << 16 | u64::from(w[2]) << 32;
    (u << 16).cast_signed() >> 16
}

/// One status block: the chain's state at a frame's end. Each field's
/// words are given (s: two's complement).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// Frame counter (header).
    pub frame: u32,
    /// Sync: the last period's peak (2 words), its position (1), the sum
    /// of the period's accumulator (3 words, 48 bits), flags (1: bit 0
    /// locked, bit 1 SSync parity), the locked offset (1), the stray and
    /// missed frame markers (2 + 2).
    pub peak: u32,
    /// See `peak`.
    pub peak_pos: u16,
    /// See `peak`.
    pub sum: u64,
    /// See `peak`.
    pub locked: bool,
    /// See `peak`.
    pub ssync_parity: u8,
    /// See `peak`.
    pub offset: u16,
    /// See `peak`.
    pub stray_markers: u32,
    /// See `peak`.
    pub missed_markers: u32,
    /// The carrier register, 2⁻³² turn per symbol (2 words, s).
    pub carrier: i32,
    /// The input AGC gain, Q6.10 (1).
    pub agc: u16,
    /// The equaliser taps in use, Q2.14 (22 words, s: re, im per tap).
    pub taps: [[i16; 2]; 11],
    /// The frame the taps were solved from (2; `0xFFFF_FFFF` none).
    pub taps_from: Option<u32>,
    /// The frame of the training sums latched (2; `0xFFFF_FFFF` none).
    pub sums_frame: Option<u32>,
    /// n0 in force, Q.20 (2) and its LLR scale (2).
    pub n0: u32,
    /// See `n0`.
    pub llr_scale: u32,
    /// The frame the constellation and n0 were learned on (2;
    /// `0xFFFF_FFFF` none).
    pub learned_from: Option<u32>,
    /// The learned constellation in force for each modulation (π/2-BPSK,
    /// QPSK, 8PSK, 16APSK, 32APSK): a count word (0: nominal, else the
    /// 2^bits points) then the points' re, im (Q.10, s).
    pub constellation: [Vec<[i16; 2]>; 5],
    /// Counters (2 words each, the iteration sum 4): frames,
    /// undecoded frames, TMCC failures, slots, LDPC failures, BCH
    /// failures, iterations.
    pub frames: u32,
    /// See `frames`.
    pub frames_undecoded: u32,
    /// See `frames`.
    pub tmcc_failed: u32,
    /// See `frames`.
    pub slots: u32,
    /// See `frames`.
    pub ldpc_failed: u32,
    /// See `frames`.
    pub bch_failed: u32,
    /// See `frames`.
    pub iterations: u64,
}

const NONE32: u32 = u32::MAX;

fn w32(out: &mut Vec<u16>, v: u32) {
    out.push(lo(v));
    out.push(hi(v));
}

impl Status {
    fn payload(&self) -> Vec<u16> {
        let mut o = Vec::new();
        w32(&mut o, self.peak);
        o.push(self.peak_pos);
        for k in 0..3 {
            // 16-bit pieces of the 48-bit sum: truncation is the value
            #[allow(clippy::cast_possible_truncation)]
            o.push((self.sum >> (16 * k)) as u16);
        }
        o.push(u16::from(self.locked) | u16::from(self.ssync_parity & 1) << 1);
        o.push(self.offset);
        w32(&mut o, self.stray_markers);
        w32(&mut o, self.missed_markers);
        w32(&mut o, self.carrier.cast_unsigned());
        o.push(self.agc);
        for t in &self.taps {
            o.push(t[0].cast_unsigned());
            o.push(t[1].cast_unsigned());
        }
        w32(&mut o, self.taps_from.unwrap_or(NONE32));
        w32(&mut o, self.sums_frame.unwrap_or(NONE32));
        w32(&mut o, self.n0);
        w32(&mut o, self.llr_scale);
        w32(&mut o, self.learned_from.unwrap_or(NONE32));
        for c in &self.constellation {
            o.push(u16::try_from(c.len()).expect("at most 32 points"));
            for p in c {
                o.push(p[0].cast_unsigned());
                o.push(p[1].cast_unsigned());
            }
        }
        for v in [
            self.frames,
            self.frames_undecoded,
            self.tmcc_failed,
            self.slots,
            self.ldpc_failed,
            self.bch_failed,
        ] {
            w32(&mut o, v);
        }
        for k in 0..4 {
            // 16-bit pieces of the 64-bit sum: truncation is the value
            #[allow(clippy::cast_possible_truncation)]
            o.push((self.iterations >> (16 * k)) as u16);
        }
        o
    }

    fn parse(frame: u32, p: &[u16]) -> Option<Self> {
        let mut i = 0usize;
        let mut take = |n: usize| -> Option<&[u16]> {
            let s = p.get(i..i + n)?;
            i += n;
            Some(s)
        };
        let r32 = |s: &[u16]| u32::from(s[0]) | u32::from(s[1]) << 16;
        let opt = |v: u32| (v != NONE32).then_some(v);
        let peak = r32(take(2)?);
        let peak_pos = take(1)?[0];
        let s = take(3)?;
        let sum = u64::from(s[0]) | u64::from(s[1]) << 16 | u64::from(s[2]) << 32;
        let fl = take(1)?[0];
        let offset = take(1)?[0];
        let stray_markers = r32(take(2)?);
        let missed_markers = r32(take(2)?);
        let carrier = r32(take(2)?).cast_signed();
        let agc = take(1)?[0];
        let tw = take(22)?;
        let taps = std::array::from_fn(|k| [tw[2 * k].cast_signed(), tw[2 * k + 1].cast_signed()]);
        let taps_from = opt(r32(take(2)?));
        let sums_frame = opt(r32(take(2)?));
        let n0 = r32(take(2)?);
        let llr_scale = r32(take(2)?);
        let learned_from = opt(r32(take(2)?));
        let mut constellation: [Vec<[i16; 2]>; 5] = Default::default();
        for c in &mut constellation {
            let n = usize::from(take(1)?[0]);
            if n > 32 {
                return None;
            }
            let w = take(2 * n)?;
            *c = (0..n)
                .map(|k| [w[2 * k].cast_signed(), w[2 * k + 1].cast_signed()])
                .collect();
        }
        let mut c = [0u32; 6];
        for v in &mut c {
            *v = r32(take(2)?);
        }
        let it = take(4)?;
        let iterations = (0..4).fold(0u64, |a, k| a | u64::from(it[k]) << (16 * k));
        if i != p.len() {
            return None;
        }
        Some(Self {
            frame,
            peak,
            peak_pos,
            sum,
            locked: fl & 1 == 1,
            ssync_parity: u8::from(fl & 2 != 0),
            offset,
            stray_markers,
            missed_markers,
            carrier,
            agc,
            taps,
            taps_from,
            sums_frame,
            n0,
            llr_scale,
            learned_from,
            constellation,
            frames: c[0],
            frames_undecoded: c[1],
            tmcc_failed: c[2],
            slots: c[3],
            ldpc_failed: c[4],
            bch_failed: c[5],
            iterations,
        })
    }
}

/// A block's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A slot.
    Slot(SlotBlock),
    /// A TMCC.
    Tmcc(TmccBlock),
    /// A status.
    Status(Box<Status>),
    /// A frame's training sums (version 2).
    Sums(Box<SumsBlock>),
}

impl Block {
    /// The frame counter.
    pub fn frame(&self) -> u32 {
        match self {
            Self::Slot(s) => s.frame,
            Self::Tmcc(t) => t.frame,
            Self::Status(s) => s.frame,
            Self::Sums(s) => s.frame,
        }
    }

    /// Its type.
    pub fn kind(&self) -> BlockType {
        match self {
            Self::Slot(_) => BlockType::Slot,
            Self::Tmcc(_) => BlockType::Tmcc,
            Self::Status(_) => BlockType::Status,
            Self::Sums(_) => BlockType::Sums,
        }
    }

    fn payload(&self) -> Vec<u16> {
        let mut o = Vec::new();
        match self {
            Self::Slot(s) => {
                o.push(
                    u16::from(s.slot)
                        | u16::from(s.mode & 7) << 8
                        | u16::from(s.dummy) << 11
                        | u16::from(s.converged) << 12
                        | u16::from(s.bch.is_some()) << 13,
                );
                o.push(
                    u16::from(s.modulation & 0xF)
                        | u16::from(s.rate & 0xF) << 4
                        | u16::from(s.iterations) << 8,
                );
                o.push(s.bch.unwrap_or(0xFFFF));
                pack_bits(&s.bits, &mut o);
            }
            Self::Tmcc(t) => {
                o.push(t.bch.unwrap_or(0xFFFF));
                o.push(t.iterations.unwrap_or(0xFFFF));
                w32(&mut o, t.applies_from);
                pack_bits(&t.bits, &mut o);
            }
            Self::Status(s) => o = s.payload(),
            Self::Sums(s) => {
                w32(&mut o, s.sums_frame);
                o.push(s.count);
                for v in s.r.iter().chain(&s.p) {
                    w48(&mut o, v[0]);
                    w48(&mut o, v[1]);
                }
            }
        }
        o
    }

    /// The block's payload parsed; `n_bits` gives a slot's bit count from
    /// its rate code (header + data bits; the slot payload's padding
    /// leaves it otherwise undecidable).
    fn parse(h: &Header, p: &[u16], n_bits: &dyn Fn(u8) -> Option<usize>) -> Option<Self> {
        Some(match h.kind {
            BlockType::Slot => {
                if p.len() < 3 {
                    return None;
                }
                let dummy = p[0] >> 11 & 1 == 1;
                let rate = u8::try_from(p[1] >> 4 & 0xF).ok()?;
                let n = if dummy { 0 } else { n_bits(rate)? };
                if p.len() != 3 + n.div_ceil(16) {
                    return None;
                }
                let bch_ok = p[0] >> 13 & 1 == 1;
                Self::Slot(SlotBlock {
                    frame: h.frame,
                    slot: u8::try_from(p[0] & 0xFF).ok()?,
                    mode: u8::try_from(p[0] >> 8 & 7).ok()?,
                    dummy,
                    converged: p[0] >> 12 & 1 == 1,
                    bch: (bch_ok && p[2] != 0xFFFF).then_some(p[2]),
                    modulation: u8::try_from(p[1] & 0xF).ok()?,
                    rate,
                    iterations: u8::try_from(p[1] >> 8).ok()?,
                    bits: unpack_bits(&p[3..], n),
                })
            }
            BlockType::Tmcc => {
                if p.len() != 4 + TMCC_BITS.div_ceil(16) {
                    return None;
                }
                Self::Tmcc(TmccBlock {
                    frame: h.frame,
                    bch: (p[0] != 0xFFFF).then_some(p[0]),
                    iterations: (p[1] != 0xFFFF).then_some(p[1]),
                    applies_from: u32::from(p[2]) | u32::from(p[3]) << 16,
                    bits: unpack_bits(&p[4..], TMCC_BITS),
                })
            }
            BlockType::Status => Self::Status(Box::new(Status::parse(h.frame, p)?)),
            BlockType::Sums => {
                if p.len() != SUMS_PAYLOAD {
                    return None;
                }
                let v: Vec<[i64; 2]> = p[3..]
                    .chunks(6)
                    .map(|c| [r48(&c[..3]), r48(&c[3..])])
                    .collect();
                let nr = crate::control::chain::R_ENTRIES;
                Self::Sums(Box::new(SumsBlock {
                    frame: h.frame,
                    sums_frame: u32::from(p[0]) | u32::from(p[1]) << 16,
                    count: p[2],
                    r: v[..nr].to_vec(),
                    p: v[nr..].to_vec(),
                }))
            }
            BlockType::Unknown(_) => return None,
        })
    }
}

/// The header and data bits of a slot of rate code `c` (Table 3.11-2:
/// 0001 = 41/120 … 1011 = 109/120): 374·r − 192 − 6 (Table 3.2-1).
pub fn slot_bits(c: u8) -> Option<usize> {
    const RATES: [usize; 11] = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109];
    let r = RATES.get(usize::from(c).checked_sub(1)?)?;
    Some(374 * r - 192 - 6)
}

/// Writes blocks as words, one sequence number per block.
#[derive(Debug, Clone)]
pub struct Writer {
    seq: u32,
    version: u16,
}

impl Default for Writer {
    /// [`Writer::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// A writer of format [`SLOTS_VERSION`] at sequence 0.
    pub fn new() -> Self {
        Self {
            seq: 0,
            version: SLOTS_VERSION,
        }
    }

    /// A writer of format `version` at sequence 0 (version 1: what the
    /// images of builds 1–9 send, for tests of a host that reads both).
    ///
    /// # Panics
    ///
    /// Unless [`readable`]`(version)`.
    pub fn with_version(version: u16) -> Self {
        assert!(readable(version), "Slots format {version}");
        Self { seq: 0, version }
    }

    /// The format version it writes.
    pub fn version(&self) -> u16 {
        self.version
    }

    /// The next block's sequence number.
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Append `b`'s words to `out`.
    ///
    /// # Panics
    ///
    /// If the payload is longer than [`MAX_PAYLOAD`] words (a slot's
    /// bits are at most 40 392 + 176: 2 536 words), or the writer's
    /// version has no such block (a sums block in version 1).
    pub fn write(&mut self, b: &Block, out: &mut Vec<u16>) {
        let p = b.payload();
        assert!(p.len() <= MAX_PAYLOAD, "payload of {} words", p.len());
        assert!(
            b.kind().in_version(self.version),
            "a {:?} block in Slots format {}",
            b.kind(),
            self.version
        );
        let h = Header {
            kind: b.kind(),
            version: self.version,
            seq: self.seq,
            payload: u16::try_from(p.len()).expect("≤ 4 096"),
            frame: b.frame(),
        };
        out.extend_from_slice(&h.words());
        out.extend_from_slice(&p);
        let c = crc32c(self.seq, &p);
        out.push(lo(c));
        out.push(hi(c));
        self.seq = self.seq.wrapping_add(1);
    }
}

/// What a [`Reader`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A block, with its sequence number.
    Block {
        /// Its sequence number.
        seq: u32,
        /// Its content.
        block: Block,
    },
    /// Blocks lost before the next block (from the sequence numbers; 0
    /// when the stream was resynchronised without a gap), and the words
    /// skipped.
    Loss {
        /// Missing blocks.
        blocks: u64,
        /// Words discarded.
        words: u64,
    },
    /// The stream ended inside a block.
    Truncated {
        /// Words after the last whole block.
        words: u64,
    },
    /// An intact block (header check and CRC) of a type this reader does
    /// not know for its header's version (module documentation,
    /// "Versions"): skipped, its sequence number taken (not a loss).
    Unknown {
        /// Its sequence number.
        seq: u32,
        /// Its type code (header word 2, bits 4–7).
        kind: u16,
        /// Its header's version.
        version: u16,
        /// Its frame counter.
        frame: u32,
        /// Its payload words.
        payload: u16,
    },
}

/// Parses a `Slots` word stream back into blocks, reporting losses by
/// sequence gaps (as [`crate::format::Reader`]: a block is accepted on
/// its CRC-32C; a damaged one is skipped to the next sync word).
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

    /// Feed bytes (16-bit little-endian words, as a radio's DMA or a words
    /// file holds them); an odd byte at the end waits for the next call.
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
            if let Some(h) = Header::parse(avail) {
                let len = h.block_words();
                if avail.len() < len {
                    break;
                }
                let p = &avail[HEADER_WORDS..len - TRAILER_WORDS];
                let c = crc32c(h.seq, p);
                if avail[len - 2] == lo(c) && avail[len - 1] == hi(c) {
                    if !h.kind.in_version(h.version) {
                        let e = Event::Unknown {
                            seq: h.seq,
                            kind: h.kind.code(),
                            version: h.version,
                            frame: h.frame,
                            payload: h.payload,
                        };
                        debug!(
                            seq = h.seq,
                            kind = h.kind.code(),
                            version = h.version,
                            "slots block {} of type {} unknown in format {}: skipped",
                            h.seq,
                            h.kind.code(),
                            h.version
                        );
                        self.deliver(h, e, len, out);
                        continue;
                    }
                    if let Some(block) = Block::parse(&h, p, &slot_bits) {
                        self.deliver(h, Event::Block { seq: h.seq, block }, len, out);
                        continue;
                    }
                }
            }
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

    /// `e` (a block, or an unknown one) for the block of header `h` and
    /// `len` words, after the loss its sequence number shows.
    fn deliver(&mut self, h: Header, e: Event, len: usize, out: &mut Vec<Event>) {
        let missing = self
            .expect_seq
            .map_or(0, |s| u64::from(h.seq.wrapping_sub(s)));
        if missing > 0 || self.skipped > 0 {
            if self.expect_seq.is_some() {
                warn!(
                    blocks = missing,
                    words = self.skipped,
                    seq = h.seq,
                    "slots stream loss before block {}: {missing} block(s) missing, {} words skipped",
                    h.seq,
                    self.skipped
                );
            } else {
                debug!(
                    words = self.skipped,
                    seq = h.seq,
                    "slots stream joined mid-block"
                );
            }
            out.push(Event::Loss {
                blocks: missing,
                words: self.skipped,
            });
            self.skipped = 0;
        }
        out.push(e);
        self.expect_seq = Some(h.seq.wrapping_add(1));
        self.pos += len;
    }

    /// End of the stream: what is left is not a whole block.
    pub fn finish(&mut self, out: &mut Vec<Event>) {
        // an odd byte left over counts as a word's worth of tail
        let left =
            (self.buf.len() - self.pos) as u64 + self.skipped + u64::from(self.odd_byte.is_some());
        self.odd_byte = None;
        if left > 0 {
            out.push(Event::Truncated { words: left });
        }
        self.buf.clear();
        self.pos = 0;
        self.skipped = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn slot(frame: u32, slot: u8, rate: u8, seed: u64) -> Block {
        let n = slot_bits(rate).unwrap();
        let mut s = seed;
        let bits = (0..n)
            .map(|_| {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                u8::from(s >> 63 == 1)
            })
            .collect();
        Block::Slot(SlotBlock {
            frame,
            slot,
            mode: 0,
            dummy: false,
            converged: true,
            bch: Some(0),
            modulation: 4,
            rate,
            iterations: 2,
            bits,
        })
    }

    fn status() -> Block {
        let mut s = Status {
            frame: 9,
            peak: 123_456,
            sum: 0x1234_5678_9ABC,
            locked: true,
            ssync_parity: 1,
            carrier: -97_000,
            taps_from: Some(5),
            n0: 35_000,
            llr_scale: 122_000,
            iterations: 1 << 40,
            ..Status::default()
        };
        s.taps[5] = [16_384, -3];
        s.constellation[3] = (0..16).map(|k| [k * 10, -k]).collect();
        Block::Status(Box::new(s))
    }

    #[test]
    fn spec_header_layout() {
        let h = Header {
            kind: BlockType::Slot,
            version: 1,
            seq: 0x1234_5678,
            payload: 2_163,
            frame: 0x0001_0002,
        };
        let w = h.words();
        assert_eq!(&w[..3], &[0x1ACF, 0xFC1D, 0x011F]);
        assert_eq!(w[2..].iter().fold(0u16, |a, &b| a.wrapping_add(b)), 0xFFFF);
        assert_eq!(Header::parse(&w), Some(h));
        // a Symbols reader refuses it (version nibble 15)
        assert!(crate::format::Header::parse(&w).is_none());
        // version 2: only the version nibble (and the check) differ
        let h2 = Header {
            kind: BlockType::Sums,
            version: 2,
            ..h
        };
        let w2 = h2.words();
        assert_eq!(&w2[..3], &[0x1ACF, 0xFC1D, 0x024F]);
        assert_eq!(Header::parse(&w2), Some(h2));
        assert!(crate::format::Header::parse(&w2).is_none());
        // versions 0 and 3: not a header this reader reads
        for v in [0, 3, 15] {
            let w3 = Header { version: v, ..h }.words();
            assert_eq!(Header::parse(&w3), None, "version {v}");
        }
    }

    /// Sums with every boundary of the 48-bit range, and a pattern that
    /// tells the entries apart.
    fn sums(frame: u32, seed: i64) -> SumsBlock {
        let v = |i: usize, c: i64| {
            let i = i64::try_from(i).unwrap();
            match i {
                0 => [(1 << 47) - 1, -(1 << 47)],
                1 => [-1, 0],
                _ => [
                    (seed * 7_919 + i * 1_000_003 + c) % (1 << 46),
                    -(i * 65_537) - seed,
                ],
            }
        };
        SumsBlock {
            frame,
            sums_frame: frame,
            count: 6_720,
            r: (0..66).map(|i| v(i, 1)).collect(),
            p: (0..11).map(|i| v(i + 66, 2)).collect(),
        }
    }

    /// The sums block's words as the module documentation gives them: the
    /// frame, the count, then each sum's re and im as three words, low
    /// first, R's entries in register order then p's; 465 payload words,
    /// 476 in all. Each sum's words are its registers' (`SUMS_R + 4i`:
    /// the low 32 bits, then the high 16 sign-extended).
    #[test]
    fn spec_sums_block_layout() {
        let s = sums(0x0001_0005, 3);
        let mut words = Vec::new();
        Writer::new().write(&Block::Sums(Box::new(s.clone())), &mut words);
        assert_eq!(words.len(), 476);
        let h = Header::parse(&words).unwrap();
        assert_eq!(
            (h.kind, h.version, h.payload, h.frame),
            (BlockType::Sums, 2, 465, 0x0001_0005)
        );
        let p = &words[HEADER_WORDS..HEADER_WORDS + 465];
        assert_eq!(&p[..3], &[0x0005, 0x0001, 6_720]);
        // R[0][0] = 2^47 - 1, -2^47; R[0][1] = -1, 0
        assert_eq!(&p[3..9], &[0xFFFF, 0xFFFF, 0x7FFF, 0x0000, 0x0000, 0x8000]);
        assert_eq!(&p[9..15], &[0xFFFF, 0xFFFF, 0xFFFF, 0, 0, 0]);
        // the registers' words of the same sums, entry by entry
        let reg = crate::control::chain::sums_to_words(&s.to_training());
        assert_eq!(reg.len(), 4 * SUMS);
        for (i, r) in reg.chunks(4).enumerate() {
            let w = &p[3 + 6 * i..9 + 6 * i];
            for (c, half) in [(0, &w[..3]), (1, &w[3..])] {
                let lo = u32::from(half[0]) | u32::from(half[1]) << 16;
                let hi = i32::from(half[2].cast_signed()).cast_unsigned();
                assert_eq!([lo, hi], [r[2 * c], r[2 * c + 1]], "sum {i}, component {c}");
            }
        }
        // read back: the same block, and the registers' view of it
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&words, &mut ev);
        assert_eq!(
            ev,
            vec![Event::Block {
                seq: 0,
                block: Block::Sums(Box::new(s.clone()))
            }]
        );
        let t = s.to_training();
        assert_eq!(
            t,
            crate::control::chain::sums_from_words(s.sums_frame, 6_720, &reg)
        );
        assert_eq!(SumsBlock::from_training(&t), Some(s));
        assert_eq!(
            SumsBlock::from_training(&crate::control::chain::TrainingSums {
                count: 70_000,
                ..t.clone()
            }),
            None
        );
        assert_eq!(
            SumsBlock::from_training(&crate::control::chain::TrainingSums { p: Vec::new(), ..t }),
            None
        );
    }

    /// A version-1 stream (the images of builds 1–9) reads as before;
    /// a version-2 stream's slot, tmcc and status blocks are the same
    /// words but for the version nibble and the check.
    #[test]
    fn version_1_and_2_streams_read_alike() {
        let blocks = [slot(4, 0, 7, 9), status(), slot(5, 1, 7, 3)];
        let (mut w1, mut w2) = (Writer::with_version(1), Writer::new());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for x in &blocks {
            w1.write(x, &mut a);
            w2.write(x, &mut b);
        }
        assert_eq!((w1.version(), w2.version()), (1, 2));
        assert_eq!(a.len(), b.len());
        let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        // per block: word 2 (the version) and word 8 (the check)
        assert_eq!(differ.len(), 2 * blocks.len());
        for words in [&a, &b] {
            let mut r = Reader::new();
            let mut ev = Vec::new();
            r.feed(words, &mut ev);
            r.finish(&mut ev);
            let got: Vec<Block> = ev
                .into_iter()
                .map(|e| match e {
                    Event::Block { block, .. } => block,
                    e => panic!("{e:?}"),
                })
                .collect();
            assert_eq!(got, blocks);
        }
    }

    /// An intact block of a type the reader does not know for its
    /// version (type 4 in a version-1 header, type 7) is reported and
    /// skipped: no loss, the blocks around it delivered.
    #[test]
    fn an_unknown_block_is_reported_not_lost() {
        let mut w = Writer::with_version(1);
        let mut words = Vec::new();
        w.write(&slot(1, 0, 7, 1), &mut words);
        // hand-made: a sums payload in a version-1 header (seq 1), then
        // a type-7 block in a version-2 header (seq 2)
        let mut sw = Vec::new();
        Writer::new().write(&Block::Sums(Box::new(sums(1, 0))), &mut sw);
        let pay = sw[HEADER_WORDS..HEADER_WORDS + SUMS_PAYLOAD].to_vec();
        for (seq, kind, version) in [(1u32, BlockType::Sums, 1u16), (2, BlockType::Unknown(7), 2)] {
            let h = Header {
                kind,
                version,
                seq,
                payload: u16::try_from(pay.len()).unwrap(),
                frame: 1,
            };
            words.extend_from_slice(&h.words());
            words.extend_from_slice(&pay);
            let c = crc32c(seq, &pay);
            words.extend_from_slice(&[lo(c), hi(c)]);
        }
        words.extend_from_slice(&slot_at(3));
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&words, &mut ev);
        r.finish(&mut ev);
        assert!(matches!(ev[0], Event::Block { seq: 0, .. }));
        assert_eq!(
            ev[1],
            Event::Unknown {
                seq: 1,
                kind: 4,
                version: 1,
                frame: 1,
                payload: 465
            }
        );
        assert!(matches!(
            ev[2],
            Event::Unknown {
                seq: 2,
                kind: 7,
                version: 2,
                ..
            }
        ));
        assert!(matches!(ev[3], Event::Block { seq: 3, .. }));
        assert_eq!(ev.len(), 4);
    }

    /// A slot block's words with sequence number `seq`.
    fn slot_at(seq: u32) -> Vec<u16> {
        let mut w = Writer::new();
        let mut words = Vec::new();
        for _ in 0..=seq {
            words.clear();
            w.write(&slot(1, 1, 7, 2), &mut words);
        }
        words
    }

    /// A version-1 writer has no sums block.
    #[test]
    #[should_panic(expected = "Slots format 1")]
    fn a_version_1_writer_refuses_a_sums_block() {
        Writer::with_version(1).write(&Block::Sums(Box::new(sums(0, 0))), &mut Vec::new());
    }

    /// A sums block of the wrong length is malformed: with a valid CRC it
    /// is still not delivered (skipped as damage, a loss of one block).
    #[test]
    fn a_short_sums_block_is_malformed() {
        let mut words = Vec::new();
        let mut w = Writer::new();
        w.write(&slot(1, 0, 7, 1), &mut words);
        let pay = vec![0u16; SUMS_PAYLOAD - 6];
        let h = Header {
            kind: BlockType::Sums,
            version: 2,
            seq: 1,
            payload: u16::try_from(pay.len()).unwrap(),
            frame: 1,
        };
        words.extend_from_slice(&h.words());
        words.extend_from_slice(&pay);
        let c = crc32c(1, &pay);
        words.extend_from_slice(&[lo(c), hi(c)]);
        words.extend_from_slice(&slot_at(2));
        let mut r = Reader::new();
        let mut ev = Vec::new();
        r.feed(&words, &mut ev);
        r.finish(&mut ev);
        assert!(matches!(ev[0], Event::Block { seq: 0, .. }));
        assert!(matches!(ev[1], Event::Loss { blocks: 1, .. }));
        assert!(matches!(ev[2], Event::Block { seq: 2, .. }));
        assert_eq!(ev.len(), 3);
    }

    #[test]
    fn a_symbols_reader_rejects_a_slots_stream() {
        let mut w = Writer::new();
        let mut words = Vec::new();
        for k in 0..5 {
            w.write(&slot(1, k, 7, u64::from(k)), &mut words);
        }
        let mut r = crate::format::Reader::new();
        let mut ev = Vec::new();
        r.feed(&words, &mut ev);
        r.finish(&mut ev);
        assert!(
            !ev.iter()
                .any(|e| matches!(e, crate::format::Event::Block { .. }))
        );
    }

    #[test]
    fn slot_bits_of_the_rates() {
        assert_eq!(slot_bits(7), Some(176 + 34_408));
        assert_eq!(slot_bits(1), Some(176 + 14_960));
        assert_eq!(slot_bits(11), Some(176 + 40_392));
        assert_eq!(slot_bits(0), None);
        assert_eq!(slot_bits(12), None);
    }

    #[test]
    fn bytes_and_bits() {
        let b = [0xA5u8, 0x0F];
        let bits = bytes_to_bits(&b, 16);
        assert_eq!(&bits[..8], &[1, 0, 1, 0, 0, 1, 0, 1]);
        assert_eq!(bits_to_bytes(&bits), b);
    }

    proptest! {
        /// Any mix of blocks round-trips, in any split of the words; a
        /// removed block shows as a loss of one, the rest intact.
        #[test]
        fn prop_slots_round_trip(
            n in 1usize..12,
            seed in any::<u64>(),
            split in 1usize..5_000,
            drop in proptest::option::of(0usize..12),
        ) {
            let mut blocks = Vec::new();
            for k in 0..n {
                let b = match (seed >> k) % 5 {
                    0 => status(),
                    4 => {
                        // random 48-bit sums
                        let mut x = seed ^ (k as u64) << 32;
                        let mut v = || {
                            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                            (x.cast_signed() >> 16, (x << 16).cast_signed() >> 16)
                        };
                        let mut c = || { let (a, b) = v(); [a, b] };
                        Block::Sums(Box::new(SumsBlock {
                            frame: u32::try_from(k).unwrap(),
                            sums_frame: u32::try_from(k).unwrap(),
                            count: u16::try_from(k * 611).unwrap(),
                            r: (0..66).map(|_| c()).collect(),
                            p: (0..11).map(|_| c()).collect(),
                        }))
                    }
                    1 => Block::Tmcc(TmccBlock {
                        frame: u32::try_from(k).unwrap(),
                        bch: Some(1),
                        iterations: None,
                        applies_from: 7,
                        bits: (0..TMCC_BITS).map(|i| u8::from((i + k) % 3 == 0)).collect(),
                    }),
                    2 => Block::Slot(SlotBlock {
                        dummy: true,
                        bits: Vec::new(),
                        converged: false,
                        bch: None,
                        ..match slot(2, 3, 1, 0) { Block::Slot(s) => s, _ => unreachable!() }
                    }),
                    _ => slot(u32::try_from(k).unwrap(), u8::try_from(k).unwrap(), 1 + u8::try_from(k % 11).unwrap(), seed ^ k as u64),
                };
                blocks.push(b);
            }
            let mut w = Writer::new();
            let mut words = Vec::new();
            let mut starts = Vec::new();
            for b in &blocks {
                starts.push(words.len());
                w.write(b, &mut words);
            }
            starts.push(words.len());
            let dropped = drop.filter(|&d| d < n);
            if let Some(d) = dropped {
                words.drain(starts[d]..starts[d + 1]);
            }
            let mut r = Reader::new();
            let mut ev = Vec::new();
            if split % 2 == 0 {
                for c in words.chunks(split) {
                    r.feed(c, &mut ev);
                }
            } else {
                // as bytes, cut anywhere (odd sizes too)
                let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
                for c in bytes.chunks(split) {
                    r.feed_bytes(c, &mut ev);
                }
            }
            r.finish(&mut ev);
            let got: Vec<&Block> = ev.iter().filter_map(|e| match e {
                Event::Block { block, .. } => Some(block),
                _ => None,
            }).collect();
            let want: Vec<&Block> = blocks.iter().enumerate().filter(|(i, _)| Some(*i) != dropped).map(|(_, b)| b).collect();
            prop_assert_eq!(&got, &want);
            let lost: u64 = ev.iter().map(|e| match e {
                Event::Loss { blocks, .. } => *blocks,
                _ => 0,
            }).sum();
            // a block dropped at the very end leaves no later block to tell,
            // and one dropped at the start no earlier sequence number (a
            // stream joined there)
            let expect = u64::from(dropped.is_some_and(|d| d > 0 && d + 1 < n));
            prop_assert_eq!(lost, expect);
            // encode(decode(x)) == x: the blocks read written again give
            // the same words (no block removed: the sequence numbers run on)
            if dropped.is_none() {
                let mut w2 = Writer::new();
                let mut again = Vec::new();
                for b in &got {
                    w2.write(b, &mut again);
                }
                prop_assert_eq!(again, words);
            }
        }

        /// A damaged word is never delivered: the block is dropped (and
        /// counted) and the stream continues.
        #[test]
        fn prop_a_damaged_block_is_dropped(at in 0usize..3_000, bit in 0u16..16) {
            let mut w = Writer::new();
            let mut words = Vec::new();
            let blocks = [slot(0, 0, 7, 1), slot(0, 1, 7, 2), status()];
            for b in &blocks {
                w.write(b, &mut words);
            }
            let i = at % words.len();
            words[i] ^= 1 << bit;
            let mut r = Reader::new();
            let mut ev = Vec::new();
            r.feed(&words, &mut ev);
            r.finish(&mut ev);
            for e in &ev {
                if let Event::Block { seq, block } = e {
                    prop_assert_eq!(block, &blocks[*seq as usize]);
                }
            }
        }
    }
}
