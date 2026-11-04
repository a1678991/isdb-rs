//! The host's view of the ISDB-S3 receive chain after the symbol front end
//! (docs/plans/isdbs3-fpga-rx.md, the chain's model `isdbs3_phy::chain`,
//! its output the `Slots` words of [`crate::slots`]): discovery, the
//! registers the host writes at open, and the ones its slow loop reads.
//!
//! **The map is the HDL's** (`fpga/isdb_fpga/chain/top.py`, `SlotsRx`,
//! 2026-11-02): the HDL took these addresses and adds [`reg::MAX_ITER`],
//! [`reg::AGC`] and [`reg::TMCC_OVERRUNS`]. What the first image offers
//! ([`features`]): the host's taps ([`features::TAPS`]) and the training
//! sums ([`features::SUMS`]); not a constellation or an n0 at START
//! ([`reg::CONST`] reads 0, [`reg::N0`] is read only), and the layout lag
//! is 2 only ([`reg::LAYOUT_LAG`] reads 2). START also restarts the chain
//! (the model's `Chain::loss`) and the blocks' sequence numbers. The host
//! (`isdb-radio`'s `ChainControl`) uses only the names of this file.
//!
//! **The map's one source** is `fpga/isdb_fpga/chain/registers.toml`
//! (issue #30 item 5): every address, bit field, width and reset constant
//! here is checked against it by `tests/chain_registers.rs`, as are the
//! HDL's constants (`fpga/tests/test_registers.py`, which also simulates
//! each register's documented behaviour) and the host's register sequences
//! (`isdb-radio`'s tests); change the map with the copies.
//!
//! The chain shares the front end's command protocol ([`super::Encoder`],
//! the `gpio_out` / `gpio_in` pair of `cf-ad9361-lpc`) and its address
//! space: the front end has 0x00–0x11 and the banks from 0x1000, the chain
//! takes 0x400–0x6FF.
//!
//! **Discovery.** Bits 11–8 of the ID word ([`super::Identity`]) are 0 on
//! every front end so far; an image with the chain puts the `Slots` format
//! version there ([`slots_version`]), so finding the chain is the same one
//! register read as finding the front end. [`reg::VERSION`] repeats it and
//! [`reg::FEATURES`] says what the host may do ([`features`]).
//!
//! **Selection.** CTRL [`super::ctrl::SLOTS`] with
//! [`super::ctrl::SYMBOLS`] sends the chain's `Slots` words to the DMA
//! instead of the symbols; the front end's STATUS ([`super::reg::STATUS`])
//! then reads [`STATUS_SLOTS`] as well.
//!
//! **Open.** The registers written before CTRL START are the state START
//! loads (the model's `ChainOptions`): [`reg::CARRIER`] (the carrier
//! register's initial value), [`reg::LAYOUT_LAG`], and, as
//! [`reg::INIT`] selects, the taps ([`reg::TAPS`]), the constellation
//! ([`reg::CONST`]) and n0 ([`reg::N0`]); what [`reg::INIT`] does not
//! select starts at its default (identity taps, the nominal points of
//! §3.8, n0 = 52 556 in Q.20).
//!
//! **Slow loop** (the equaliser's training, plan stage 4). At the end of
//! each frame with a layout the block latches the frame's normal-equation
//! sums ([`reg::SUMS_FRAME`], [`reg::SUMS_COUNT`], [`reg::SUMS_R`],
//! [`reg::SUMS_P`]; 66 + 11 complex values of 48 bits, two registers per
//! component: [`sum_registers`], [`sums_from_words`]). With
//! [`features::SUMS_CAPTURE`] the host writes [`reg::SUMS_CAPTURE`], polls
//! it for [`capture::VALID`] (the next frame end latched and holds the
//! sums: later frames are summed but not latched until the next write),
//! reads them (308 registers: about 15 frames through iiod) and
//! [`reg::SUMS_FRAME`] again (the same, or the hold failed); without it
//! the latch moves every frame and the host re-reads [`reg::SUMS_FRAME`]
//! to find a torn read (read again later). Then it solves in `f64` and writes the 11
//! taps ([`reg::TAPS`], [`pack_point`]) and then [`reg::TAPS_FROM`] with
//! the frame N they were solved from, which commits them: the block takes
//! them at the start of frame N + 2 (the model's rule). Reading [`reg::TAPS`] and
//! [`reg::TAPS_FROM`] gives the taps in use.
//!
//! **Sum reads** ([`features::SUMS_SEQ`], issues #27/#29): a READ of a sum
//! register is answered 3 symbol clocks after the block decodes it, and
//! the readback keeps the previous READ's value until then (never a 0 in
//! between); [`reg::SUMS_CAPTURE`] reads [`capture::seq`], the latches so
//! far: equal before and after the 308 reads, no latch moved in between.
//! Only a frame with a layout is summed and latched: with
//! [`capture::NO_LAYOUT`] (the same as [`status::DECODING`] clear) an
//! armed capture waits until TMCC decodes again.
//!
//! **Sums in the stream** ([`features::SUMS_STREAM`], `Slots` format 2):
//! the block also sends each latched set in the `Slots` stream, as a sums
//! block right after its frame's status block
//! ([`crate::slots::SumsBlock`]: the same values as these registers, in
//! [`all_sum_registers`]' order, under the block's CRC). A host reading
//! the stream solves from those and reads none of the 308 registers (and
//! arms no capture): no read is exposed to another client of
//! `direct_reg_access`. The registers, [`reg::SUMS_CAPTURE`] and its hold
//! stay (a host of an older binary, and the proven path); a capture held
//! keeps the latch on its frame, and a frame whose sums did not latch at
//! its end gets no sums block.
//!
//! **START and the decoder** (issue #28, [`features::DECODER_HEALTH`]):
//! START, a lost sync or a dropped frame gives up the slot codeword being
//! loaded and the block frees its LDPC decoder bank (an abort); committed
//! codewords decode and leave as usual. After a stop (CTRL without
//! [`super::ctrl::SLOTS`]) and START every bank is free:
//! [`DecoderHealth::held`] 0. Without [`init::TAPS`] START leaves the taps in
//! use (and a set armed by [`reg::TAPS_FROM`]) as they are; with it the
//! taps last written to [`reg::TAPS`] go in use at once. [`reg::DECODER`]
//! and [`reg::RECLAIMED`] ([`DecoderHealth`]) show the decoder's state.
//!
//! **Without a layout** (TMCC not decoding: [`status::DECODING`] clear)
//! the block still sends, per whole frame (1 115 520 symbols, 33.05 ms at
//! 33.756 Mbaud), its TMCC block (604 words with the header and trailer)
//! and its status block (79 words, plus 2 · 2^(m+1) for each learned
//! modulation m, up to 203): 683 to 807 words a frame, at least 20 668
//! words/s (a 1 Mi-word buffer fills in 50.7 s); nothing while the sync
//! is not locked or no frame ends whole.

use super::{ID_MAGIC, Identity};

/// The newest `Slots` format version this host reads
/// ([`crate::slots::SLOTS_VERSION`]; [`SLOTS_VERSION_MIN`] the oldest:
/// [`crate::slots::readable`]).
pub const SLOTS_VERSION: u16 = crate::slots::SLOTS_VERSION;
/// The oldest `Slots` format version this host reads
/// ([`crate::slots::SLOTS_VERSION_MIN`]).
pub const SLOTS_VERSION_MIN: u16 = crate::slots::SLOTS_VERSION_MIN;

/// The equaliser's taps ([`TAPS`] complex values, Q2.14, `[re, im]`).
pub type Taps = [[i16; 2]; TAPS];

/// Taps of the equaliser.
pub const TAPS: usize = 11;
/// Entries of the upper triangle of R (r ≤ c, row by row).
pub const R_ENTRIES: usize = TAPS * (TAPS + 1) / 2;
/// Modulations with a constellation register set: π/2-BPSK, QPSK, 8PSK,
/// 16APSK, 32APSK (the `Slots` status block's order).
pub const MODULATIONS: usize = 5;
/// Points per modulation in [`reg::CONST`] (the most, 32APSK's).
pub const MAX_POINTS: usize = 32;
/// "None" in a frame register (`0xFFFF_FFFF`, as in the `Slots` status
/// block).
pub const NONE: u32 = u32::MAX;
/// [`reg::MAX_ITER`] at reset, the LDPC iteration limit the chain decodes
/// with unless the host writes another: the HDL's `ldpc.DEFAULT_MAX_ITER`
/// and the model's `isdbs3_phy::chain::ldpc::HDL_MAX_ITER` (the decoder's
/// queue has no overrun at the chain's pace with it; at 16 it would).
pub const MAX_ITER_RESET: u32 = 14;
/// The front end's STATUS ([`super::reg::STATUS`]) bit that says the
/// chain's `Slots` words go to the DMA.
pub const STATUS_SLOTS: u32 = 4;
/// n0 before the first estimate (Q.20; Es/N0 13 dB), the model's default.
pub const N0_DEFAULT: u32 = 52_556;

/// The `Slots` format version of the chain an ID word announces (bits
/// 11–8), `None` without the front end's magic or without a chain.
pub fn slots_version(id_word: u32) -> Option<u16> {
    Identity::parse(id_word)?;
    let v = u16::try_from((id_word >> 8) & 0xF).expect("4 bits");
    (v != 0).then_some(v)
}

/// The ID word of a front end `id` followed by a chain of `Slots` format
/// `version` (for tests and the HDL's own checks).
pub fn id_word(id: &Identity, version: u16) -> u32 {
    debug_assert_eq!(id.word() >> 16, u32::from(ID_MAGIC));
    id.word() | u32::from(version & 0xF) << 8
}

/// Register addresses (module documentation).
pub mod reg {
    /// Read only: the `Slots` format version the chain writes.
    pub const VERSION: u16 = 0x400;
    /// Read only: [`super::features`] bits.
    pub const FEATURES: u16 = 0x401;
    /// The carrier register, 2⁻³² turn per symbol (signed, the model's
    /// `ChainOptions::carrier` when written before START; reads the value
    /// in use: the block tracks the carrier itself).
    pub const CARRIER: u16 = 0x402;
    /// The layout lag, 1 or 2 frames (`ChainOptions::layout_lag`): the
    /// host writes it at open; an image that builds lag 2 only (the first,
    /// `SlotsRx`) reads 2 and ignores the write.
    pub const LAYOUT_LAG: u16 = 0x403;
    /// [`super::init`] bits: which host values START loads.
    pub const INIT: u16 = 0x404;
    /// Read only: [`super::status`] bits.
    pub const STATUS: u16 = 0x405;
    /// Read only: the frame counter of the last frame out.
    pub const FRAME: u16 = 0x406;
    /// Read only: the sequence number of the next `Slots` block.
    pub const SEQ: u16 = 0x407;
    /// n0, Q.20: written, the initial value ([`super::init::N0`], an image
    /// with [`super::features::N0`]); read, the value in force.
    pub const N0: u16 = 0x408;
    /// Read only: the LLR scale in force (`round(2³² / n0)`).
    pub const LLR_SCALE: u16 = 0x409;
    /// Read only: the frame the constellation and n0 in force were learned
    /// on ([`super::NONE`]: defaults).
    pub const LEARNED_FROM: u16 = 0x40A;
    /// Bit m: modulation m's points in [`CONST`] are learned (or the
    /// host's, [`super::init::CONSTELLATION`]), not the nominal ones;
    /// written with the points before START (an image with
    /// [`super::features::CONSTELLATION`]; read only on one without).
    pub const CONST_MASK: u16 = 0x40B;
    /// The LDPC iteration limit each codeword is committed with (8 bits,
    /// reset [`super::MAX_ITER_RESET`]; [`super::features::ldpc_limit`]
    /// reads it too).
    pub const MAX_ITER: u16 = 0x40C;
    /// The input's AGC gain (Q6.10) the status blocks report.
    pub const AGC: u16 = 0x40D;
    /// Tap k at `TAPS + k` (k < 11, [`super::pack_point`]; an image with
    /// [`super::features::TAPS`]): written, the next taps (committed by
    /// [`TAPS_FROM`], or loaded by START with [`super::init::TAPS`]); read,
    /// the taps in use.
    pub const TAPS: u16 = 0x410;
    /// Written after [`TAPS`] ([`super::features::TAPS`]): the frame N the
    /// taps were solved from
    /// (commits them; the block loads them at the start of frame N + 2,
    /// the model's rule, or of the next frame if that has passed).
    /// Read: the frame the taps in use were solved from
    /// ([`super::NONE`]: identity, or the host's at open).
    pub const TAPS_FROM: u16 = 0x41B;
    /// Read only ([`super::features::SUMS`]): the frame of the latched
    /// training sums ([`super::NONE`]: none yet).
    pub const SUMS_FRAME: u16 = 0x420;
    /// Read only ([`super::features::SUMS`]): training symbols in the
    /// latched sums (16 bits).
    pub const SUMS_COUNT: u16 = 0x421;
    /// Written (any value): the next frame end latches the training sums
    /// and holds them until the next write ([`super::features::SUMS_CAPTURE`]);
    /// read: [`super::capture`] bits.
    pub const SUMS_CAPTURE: u16 = 0x422;
    /// Read only: the counters of the `Slots` status block, 32 bits each,
    /// at `COUNTERS + i` in [`super::Counters::NAMES`] order (the
    /// iteration sum as two registers, low first).
    pub const COUNTERS: u16 = 0x430;
    /// Read only: TMCC codewords the LDPC decoder dropped (its queue was
    /// full; also counted as TMCC failed).
    pub const TMCC_OVERRUNS: u16 = 0x438;
    /// Read only ([`super::features::DECODER_HEALTH`]): the LDPC decoder's
    /// state, a snapshot from its clock under 1 µs old: bits 15–0 its
    /// overruns (codewords dropped for want of a free bank, slots and
    /// TMCC, since configuration, wrapping), bit 16 its sticky queue
    /// overflow, bits 23–20 the banks allocated now, bits 27–24 its banks
    /// (3) ([`super::DecoderHealth`]).
    pub const DECODER: u16 = 0x439;
    /// Read only ([`super::features::DECODER_HEALTH`]): bits 15–0 the
    /// decoder banks freed by the abort after a frame drop or restart,
    /// bits 31–16 the banks a codeword's begin took back from its
    /// context's abandoned codeword, since configuration, wrapping
    /// ([`super::DecoderHealth`]).
    pub const RECLAIMED: u16 = 0x43A;
    /// Read only ([`super::features::SUMS`]): R's 66 upper-triangle
    /// entries ([`super::sum_registers`]).
    pub const SUMS_R: u16 = 0x500;
    /// Read only ([`super::features::SUMS`]): p's 11 entries
    /// ([`super::sum_registers`]).
    pub const SUMS_P: u16 = 0x610;
    /// Point t of modulation m at `CONST + 32·m + t`
    /// ([`super::pack_point`], Q.10): written, the initial points
    /// ([`super::init::CONSTELLATION`], an image with
    /// [`super::features::CONSTELLATION`]); read, the points in force (an
    /// image without the feature has not built them: they read 0).
    pub const CONST: u16 = 0x640;
}

/// [`reg::FEATURES`] bits.
pub mod features {
    /// The host may write taps ([`super::reg::TAPS`],
    /// [`super::reg::TAPS_FROM`]).
    pub const TAPS: u32 = 1;
    /// The training sums are readable: the host's slow loop can run.
    pub const SUMS: u32 = 2;
    /// The host may load a constellation at START.
    pub const CONSTELLATION: u32 = 4;
    /// The host may load n0 at START.
    pub const N0: u32 = 8;
    /// The training sums are held from a host-armed frame end
    /// ([`super::reg::SUMS_CAPTURE`]).
    pub const SUMS_CAPTURE: u32 = 16;
    /// [`super::reg::DECODER`] and [`super::reg::RECLAIMED`] are there, and
    /// the block frees the decoder bank of a codeword it gives up (a stop
    /// and START leaves every bank free).
    pub const DECODER_HEALTH: u32 = 32;
    /// [`super::reg::SUMS_CAPTURE`] reads [`super::capture::NO_LAYOUT`] and
    /// [`super::capture::seq`]; a sum register's READ is answered in 3
    /// symbol clocks, the readback keeping its last value until then.
    pub const SUMS_SEQ: u32 = 64;
    /// The latched training sums of each frame also leave in the `Slots`
    /// stream, a sums block after its frame's status block (`Slots`
    /// format 2, [`crate::slots::SumsBlock`]; module documentation, "Sums
    /// in the stream").
    pub const SUMS_STREAM: u32 = 128;
    /// Every flag this host knows.
    pub const ALL: u32 =
        TAPS | SUMS | CONSTELLATION | N0 | SUMS_CAPTURE | DECODER_HEALTH | SUMS_SEQ | SUMS_STREAM;
    /// Bits 15–8: the LDPC decoder's iteration limit.
    pub fn ldpc_limit(f: u32) -> u32 {
        (f >> 8) & 0xFF
    }
    /// Bits 27–16: the LDPC schedule's step (0: sequential; plan "The
    /// LDPC schedule contract").
    pub fn ldpc_step(f: u32) -> u32 {
        (f >> 16) & 0xFFF
    }
}

/// [`reg::SUMS_CAPTURE`] bits (read).
pub mod capture {
    /// The sums latched since the last write, and held.
    pub const VALID: u32 = 1;
    /// Waiting for a frame end.
    pub const ARMED: u32 = 2;
    /// The last frame end had no layout ([`super::status::DECODING`]
    /// clear): nothing is latched, an armed capture waits
    /// ([`super::features::SUMS_SEQ`]).
    pub const NO_LAYOUT: u32 = 4;
    /// Bits 15–8: the latches since configuration, wrapping
    /// ([`super::features::SUMS_SEQ`]).
    pub fn seq(w: u32) -> u8 {
        u8::try_from((w >> 8) & 0xFF).expect("8 bits")
    }
}

/// [`reg::INIT`] bits.
pub mod init {
    /// START loads [`super::reg::TAPS`] (else the identity taps).
    pub const TAPS: u32 = 1;
    /// START loads [`super::reg::CONST`] for the modulations of
    /// [`super::reg::CONST_MASK`] (else the nominal points).
    pub const CONSTELLATION: u32 = 2;
    /// START loads [`super::reg::N0`] (else [`super::N0_DEFAULT`]).
    pub const N0: u32 = 4;
}

/// [`reg::STATUS`] bits.
pub mod status {
    /// The slot sync is locked.
    pub const LOCKED: u32 = 1;
    /// A frame head has been found since the last restart.
    pub const FRAMED: u32 = 2;
    /// A layout is in force (frames are decoded).
    pub const DECODING: u32 = 4;
}

/// A point or tap as one register: re in bits 15–0, im in bits 31–16.
pub fn pack_point(p: [i16; 2]) -> u32 {
    u32::from(p[0].cast_unsigned()) | u32::from(p[1].cast_unsigned()) << 16
}

/// [`pack_point`] inverted.
pub fn unpack_point(w: u32) -> [i16; 2] {
    let [a, b, c, d] = w.to_le_bytes();
    [i16::from_le_bytes([a, b]), i16::from_le_bytes([c, d])]
}

/// The four registers of a complex 48-bit sum: entry `i` of R
/// (`which` 0, i < [`R_ENTRIES`]) or p (`which` 1, i < [`TAPS`]); for re
/// and im, the low 32 bits and then the high 16 sign-extended to 32.
pub fn sum_registers(which: usize, i: usize) -> [u16; 4] {
    let base = if which == 0 { reg::SUMS_R } else { reg::SUMS_P };
    let a = base + u16::try_from(4 * i).expect("< 4 · 66");
    [a, a + 1, a + 2, a + 3]
}

/// Every sum register in reading order: R's entries, then p's.
pub fn all_sum_registers() -> Vec<u16> {
    (0..R_ENTRIES)
        .map(|i| sum_registers(0, i))
        .chain((0..TAPS).map(|i| sum_registers(1, i)))
        .flatten()
        .collect()
}

/// A 48-bit signed sum from its low and high registers.
pub fn sum_value(lo: u32, hi: u32) -> i64 {
    i64::from(hi.cast_signed()) << 32 | i64::from(lo)
}

/// One frame's training sums as the host reads them (the model's
/// `isdbs3_phy::chain::eq::Sums`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainingSums {
    /// The frame summed.
    pub frame: u32,
    /// Training symbols summed.
    pub count: u32,
    /// `R[r][c] = Σ conj(x_r) · x_c` for r ≤ c, row by row (66).
    pub r: Vec<[i64; 2]>,
    /// `p[r] = Σ conj(x_r) · d` (11).
    pub p: Vec<[i64; 2]>,
}

/// The sums from the values of [`all_sum_registers`], in that order.
///
/// # Panics
///
/// Unless `words` has `4 · (66 + 11)` values.
pub fn sums_from_words(frame: u32, count: u32, words: &[u32]) -> TrainingSums {
    assert_eq!(words.len(), 4 * (R_ENTRIES + TAPS));
    let c = |w: &[u32]| [sum_value(w[0], w[1]), sum_value(w[2], w[3])];
    let v: Vec<[i64; 2]> = words.chunks(4).map(c).collect();
    TrainingSums {
        frame,
        count,
        r: v[..R_ENTRIES].to_vec(),
        p: v[R_ENTRIES..].to_vec(),
    }
}

/// The register values of `s` in [`all_sum_registers`] order (what an
/// image's latch reads; for tests).
pub fn sums_to_words(s: &TrainingSums) -> Vec<u32> {
    let split = |v: i64| -> [u32; 2] {
        let b = v.to_le_bytes();
        let lo = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        // the high 16 bits of a 48-bit value, sign-extended
        let hi = i32::from(i16::from_le_bytes([b[4], b[5]])).cast_unsigned();
        [lo, hi]
    };
    s.r.iter()
        .chain(&s.p)
        .flat_map(|c| {
            let [a, b] = split(c[0]);
            let [x, y] = split(c[1]);
            [a, b, x, y]
        })
        .collect()
}

/// What the host loads at START (module documentation, "Open").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainInit {
    /// The carrier register's initial value (2⁻³² turn per symbol).
    pub carrier: i32,
    /// The layout lag (1 or 2).
    pub layout_lag: u32,
    /// Taps to start with (`None`: identity).
    pub taps: Option<Taps>,
    /// n0 to start with, Q.20 (`None`: [`N0_DEFAULT`]).
    pub n0: Option<u32>,
    /// Points to start with per modulation, Q.10 (`None`: nominal).
    pub constellation: [Option<Vec<[i16; 2]>>; MODULATIONS],
}

impl Default for ChainInit {
    /// Carrier 0, lag 2, identity taps, default n0, nominal points.
    fn default() -> Self {
        Self {
            carrier: 0,
            layout_lag: 2,
            taps: None,
            n0: None,
            constellation: Default::default(),
        }
    }
}

/// The chain's counters ([`reg::COUNTERS`]; the `Slots` status block's).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// Whole frames.
    pub frames: u32,
    /// Frames without a layout (not decoded).
    pub frames_undecoded: u32,
    /// TMCC codewords that failed.
    pub tmcc_failed: u32,
    /// Valid slots decoded.
    pub slots: u32,
    /// Slots whose LDPC did not converge.
    pub ldpc_failed: u32,
    /// Slots whose BCH failed.
    pub bch_failed: u32,
    /// LDPC iterations (sum).
    pub iterations: u64,
}

impl Counters {
    /// The registers from [`reg::COUNTERS`] on, in order.
    pub const NAMES: [&'static str; 8] = [
        "frames",
        "frames_undecoded",
        "tmcc_failed",
        "slots",
        "ldpc_failed",
        "bch_failed",
        "iterations (low)",
        "iterations (high)",
    ];

    /// The registers [`Counters::from_words`] takes.
    pub fn registers() -> [u16; 8] {
        std::array::from_fn(|i| reg::COUNTERS + u16::try_from(i).expect("< 8"))
    }

    /// From the values of [`Counters::registers`].
    pub fn from_words(w: [u32; 8]) -> Self {
        Self {
            frames: w[0],
            frames_undecoded: w[1],
            tmcc_failed: w[2],
            slots: w[3],
            ldpc_failed: w[4],
            bch_failed: w[5],
            iterations: u64::from(w[7]) << 32 | u64::from(w[6]),
        }
    }
}

/// The LDPC decoder's state ([`reg::DECODER`], [`reg::RECLAIMED`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecoderHealth {
    /// Codewords dropped for want of a free bank (wrapping).
    pub overruns: u16,
    /// A commit the result queue could not take (sticky).
    pub queue_overflow: bool,
    /// Banks allocated now (0 when the chain is stopped and idle).
    pub held: u8,
    /// The decoder's banks.
    pub banks: u8,
    /// Banks freed by the abort after a frame drop or restart (wrapping).
    pub aborted: u16,
    /// Banks a begin took back from an abandoned codeword (wrapping).
    pub reused: u16,
}

impl DecoderHealth {
    /// The registers [`DecoderHealth::from_words`] takes.
    pub const REGISTERS: [u16; 2] = [reg::DECODER, reg::RECLAIMED];

    /// From the values of [`DecoderHealth::REGISTERS`].
    pub fn from_words(w: [u32; 2]) -> Self {
        let field = |v: u32, at: u32, bits: u32| v >> at & ((1 << bits) - 1);
        Self {
            overruns: u16::try_from(field(w[0], 0, 16)).expect("16 bits"),
            queue_overflow: field(w[0], 16, 1) != 0,
            held: u8::try_from(field(w[0], 20, 4)).expect("4 bits"),
            banks: u8::try_from(field(w[0], 24, 4)).expect("4 bits"),
            aborted: u16::try_from(field(w[1], 0, 16)).expect("16 bits"),
            reused: u16::try_from(field(w[1], 16, 16)).expect("16 bits"),
        }
    }
}

/// The carrier register in Hz at `symbol_rate_hz` (2⁻³² turn per symbol).
pub fn carrier_hz(carrier: i32, symbol_rate_hz: f64) -> f64 {
    f64::from(carrier) * symbol_rate_hz / 2f64.powi(32)
}

/// The MER, dB, of n0 in Q.20 (the mean squared distance to the nearest
/// point, the main signal at unit RMS): −10 log10(n0 / 2²⁰).
pub fn mer_db(n0: u32) -> f64 {
    -10.0 * (f64::from(n0.max(1)) / f64::from(1u32 << 20)).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_word_announces_the_chain() {
        let fe = Identity {
            version: 4,
            taps: 32,
        };
        assert_eq!(slots_version(fe.word()), None);
        let w = id_word(&fe, 1);
        assert_eq!(w, 0x15DB_4120);
        assert_eq!(slots_version(w), Some(1));
        // still the front end it was
        assert_eq!(Identity::parse(w), Some(fe));
        assert_eq!(slots_version(0x0000_0100), None);
    }

    #[test]
    fn the_registers_do_not_overlap() {
        let mut all: Vec<u16> = vec![
            reg::VERSION,
            reg::FEATURES,
            reg::CARRIER,
            reg::LAYOUT_LAG,
            reg::INIT,
            reg::STATUS,
            reg::FRAME,
            reg::SEQ,
            reg::N0,
            reg::LLR_SCALE,
            reg::LEARNED_FROM,
            reg::CONST_MASK,
            reg::MAX_ITER,
            reg::AGC,
            reg::TMCC_OVERRUNS,
            reg::DECODER,
            reg::RECLAIMED,
            reg::TAPS_FROM,
            reg::SUMS_FRAME,
            reg::SUMS_COUNT,
            reg::SUMS_CAPTURE,
        ];
        all.extend((0..TAPS).map(|k| reg::TAPS + u16::try_from(k).unwrap()));
        all.extend(Counters::registers());
        all.extend(all_sum_registers());
        all.extend((0..MODULATIONS * MAX_POINTS).map(|k| reg::CONST + u16::try_from(k).unwrap()));
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "two registers share an address");
        // within the chain's window, clear of the front end's registers
        // (0x00–0x11) and banks (0x1000…)
        assert!(all.iter().all(|&a| (0x400..0x700).contains(&a)));
        // the plan's "≈ 300 words" of sums
        assert_eq!(all_sum_registers().len(), 308);
    }

    #[test]
    fn sums_round_trip_through_their_registers() {
        let s = TrainingSums {
            frame: 7,
            count: 6_720,
            r: (0..R_ENTRIES)
                .map(|i| {
                    let i = i64::try_from(i).unwrap();
                    [(1 << 44) - i * 1_000_003, -(1 << 43) + i]
                })
                .collect(),
            p: (0..TAPS)
                .map(|i| [-i64::try_from(i).unwrap() * 77, 5])
                .collect(),
        };
        let w = sums_to_words(&s);
        assert_eq!(w.len(), all_sum_registers().len());
        assert_eq!(sums_from_words(7, 6_720, &w), s);
        assert_eq!(sum_value(0xFFFF_FFFF, 0xFFFF_FFFF), -1);
    }

    #[test]
    fn the_decoder_health_and_the_capture_word_parse() {
        // the HDL's words (fpga/isdb_fpga/chain/top.py, SlotsRxRegisterTest)
        let h = DecoderHealth::from_words([0x1234 | 1 << 16 | 2 << 20 | 3 << 24, 7 | 9 << 16]);
        assert_eq!(
            h,
            DecoderHealth {
                overruns: 0x1234,
                queue_overflow: true,
                held: 2,
                banks: 3,
                aborted: 7,
                reused: 9,
            }
        );
        let w = capture::VALID | capture::NO_LAYOUT | 0xA5 << 8;
        assert_eq!(capture::seq(w), 0xA5);
        assert_eq!(w & capture::ARMED, 0);
        assert_eq!(features::ALL, 0xFF);
    }

    #[test]
    fn points_pack_into_one_register() {
        for p in [[0, 0], [16_384, -3], [-32_768, 32_767], [-1, 1]] {
            assert_eq!(unpack_point(pack_point(p)), p);
        }
        assert_eq!(pack_point([1, -1]), 0xFFFF_0001);
    }

    #[test]
    fn carrier_and_mer() {
        // 127.2 units per Hz at 33.756 Mbaud (crate isdbs3-phy chain docs)
        let rs = 33.7561e6;
        assert!((carrier_hz(1272, rs) - 10.0).abs() < 0.01);
        assert!((mer_db(1 << 20)).abs() < 1e-12);
        assert!((mer_db(N0_DEFAULT) - 13.0).abs() < 0.01);
    }
}
