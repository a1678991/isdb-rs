//! Per-layer channel-coding pipeline for one OFDM frame (ARIB STD-B31
//! §3.4–3.8, Fig. 3-2), transmit and receive.
//!
//! # Frame semantics
//!
//! A *layer frame* is the sequence of T = `tsp_per_frame` transmission TSPs
//! (Table 3-3 × N) that the hierarchical divider assigns to the layer from
//! one multiplex frame (§3.2, §3.4). Following §3.4 ("OFDM-frame
//! synchronization shifts by one byte, to the beginning of the information
//! bytes") the layer byte stream is cut into 204-byte **slots** that start
//! at the byte after a sync byte: slot *m* = bytes 1..203 of TSP *m*, then
//! a sync byte. Since every sync byte is 0x47 and is never dispersed, the
//! slot is built from TSP *m*'s own sync byte (equivalent to using TSP
//! *m*+1's). The order of operations (Fig. 3-2) is
//!
//! ```text
//! TX: slots ─► energy dispersal (PRBS reset at slot 0 byte 0, §3.5)
//!           ─► delay adjustment, T − 11 slots (§3.6, Table 3-7)
//!           ─► byte interleaving, path 0 = slot byte 0 (§3.7)
//!           ─► bits MSB first ─► K=7 encoder (continuous) ─► puncturing,
//!              pattern reset at frame start (§3.8)   ⇒ nc·S·N·204 bits
//! RX: LLRs ─► depuncture (reset at frame start) ─► Viterbi (flush at frame
//!     end) ─► bytes ─► byte deinterleaving ─► energy de-dispersal (PRBS
//!     reset at frame start) ─► slots → TSPs ─► RS(204,188) decoding
//! ```
//!
//! # Latency / alignment contract
//!
//! The delay adjustment (T − 11 slots) plus the interleaver/deinterleaver
//! pair (11 slots, §3.6) delay every byte by exactly T slots = **one
//! frame**. Hence, with frames numbered by the call order on each side and
//! the coded-bit vector of TX call *k* delivered (as LLRs, in the same
//! order) to RX call *k*:
//!
//! * `LayerDecoder::decode_frame` call *k* returns the TSPs that were given
//!   to `LayerEncoder::encode_frame` call *k − 1*, in the same order and
//!   frame-aligned (TSP 0 of the output = TSP 0 of TX frame *k − 1*).
//! * RX call 0 returns T warm-up packets, not data: a fresh encoder is
//!   primed with a virtual frame −1 of dispersed null transmission TSPs
//!   ([`null_transmission_tsp`]), so with a fresh decoder RX call 0 yields
//!   those null packets in slots 11..T−1 while slots 0..10 are corrupt
//!   (zeroed deinterleaver memory) and normally fail RS.
//! * Inside TX frame *k*'s coded bits, byte *b* of slot *m* (interleaver
//!   path *j* = *b* mod 12) carries byte *b* of pre-delay slot *m* + 11 − *j*
//!   of TX frame *k* − 1 when that index is < T, otherwise of slot
//!   *m* + 11 − *j* − T of TX frame *k*. So most of OFDM frame *k* carries
//!   the previous layer frame, and at most slots 0..10 of frame *k* itself
//!   (they reappear at RX call *k* + 1). A receiver that joins the
//!   stream at frame *k* (fresh `LayerDecoder`) gets correct bytes in RX
//!   call *k*'s output slots 11..T−1; slots 0..10 need deinterleaver memory
//!   from frame *k*−1 and are corrupt; from RX call *k*+1 on everything is
//!   correct.
//! * The energy-dispersal PRBS frame on both sides is the *pre-delay* layer
//!   frame; because the end-to-end delay is exactly one frame, the RX
//!   de-dispersal simply resets at each RX frame start.
//! * The convolutional encoder state carries across frames; only the
//!   puncturing pattern restarts at frame start (§3.8). The Viterbi decoder
//!   keeps its path metrics across frames but releases all decisions at
//!   each frame end (traceback from the best state), so it adds no frame
//!   latency; the last ~traceback bits of each frame are decided without
//!   look-ahead into the next frame.
//!
//! The OFDM-side delays (bit interleaving delay correction §3.9.2, time
//! interleaving §3.11) are outside this crate: the integrator must deliver
//! to RX call *k* exactly the LLRs of the coded-bit vector of TX call *k*.

use isdbt_core::{LayerConfig, Mode, TRANSMISSION_TSP, TSP};

use crate::byte_interleave::{ByteDeinterleaver, ByteInterleaver};
use crate::conv::{
    ConvEncoder, DEFAULT_TRACEBACK, Puncturing, ViterbiDecoder, depuncture, encode_punctured,
};
use crate::delay::DelayLine;
use crate::energy::{disperse_frame, frame_sequence};
use crate::{FecError, rs};

/// Frame latency of `LayerEncoder` → `LayerDecoder` in frames (§3.6:
/// delay adjustment + byte interleaving delays total one frame).
pub const LATENCY_FRAMES: usize = 1;

/// Offset of the sync byte inside a §3.4 shifted slot.
pub const SLOT_SYNC_OFFSET: usize = TRANSMISSION_TSP - 1;

fn tsps_to_slots(tsps: &[[u8; TRANSMISSION_TSP]], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(tsps.len() * TRANSMISSION_TSP);
    for t in tsps {
        out.extend_from_slice(&t[1..]);
        out.push(t[0]);
    }
}

fn slot_to_tsp(slot: &[u8]) -> [u8; TRANSMISSION_TSP] {
    let mut t = [0u8; TRANSMISSION_TSP];
    t[0] = slot[SLOT_SYNC_OFFSET];
    t[1..].copy_from_slice(&slot[..SLOT_SYNC_OFFSET]);
    t
}

fn unpack_bits(bytes: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(bytes.len() * 8);
    for &b in bytes {
        for i in (0..8).rev() {
            out.push((b >> i) & 1);
        }
    }
}

fn pack_bits(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| c.iter().fold(0u8, |acc, &b| (acc << 1) | (b & 1)))
        .collect()
}

/// RS-protected null transmission TSP (H.222.0 null packet, see
/// [`crate::remux::null_tsp`], + RS(204,188) parity, §3.3) used as the
/// virtual frame that primes a fresh [`LayerEncoder`].
pub fn null_transmission_tsp() -> [u8; TRANSMISSION_TSP] {
    rs::encode(&crate::remux::null_tsp())
}

/// Transmit-side channel coding of one layer (§3.5–3.8), frame by frame.
#[derive(Debug, Clone)]
pub struct LayerEncoder {
    tsps: usize,
    coded_bits: usize,
    prbs: Vec<u8>,
    delay: DelayLine,
    interleaver: ByteInterleaver,
    encoder: ConvEncoder,
    punct: Puncturing,
    bytes: Vec<u8>,
    bits: Vec<u8>,
}

impl LayerEncoder {
    /// Encoder for `layer` in `mode`, with the Table 3-7 delay adjustment.
    ///
    /// Start-up content (not specified by B31): the delay line and the
    /// byte-interleaver memories are primed as if a frame of energy-dispersed
    /// null transmission TSPs ([`null_transmission_tsp`]) had been encoded
    /// before the first call (the convolutional encoder stays in the zero
    /// state). The first frame's coded bits are therefore statistically like
    /// any other frame (no long zero runs from unprimed memories), and the
    /// one-frame latency contract is unchanged: TX "frame −1" is that null
    /// frame.
    pub fn new(mode: Mode, layer: &LayerConfig) -> Self {
        let tsps = layer.tsp_per_frame(mode);
        let mut e = Self {
            tsps,
            coded_bits: layer.coded_bits_per_frame(mode),
            prbs: frame_sequence(tsps),
            delay: DelayLine::new(layer.delay_adjustment_tsps(mode)),
            interleaver: ByteInterleaver::new(),
            encoder: ConvEncoder::new(),
            punct: Puncturing::new(layer.code_rate),
            bytes: Vec::new(),
            bits: Vec::new(),
        };
        // One virtual frame: its last T − 11 slots fill the delay line and
        // the delay line's output in its last 11 slots (= virtual slots
        // 0..10) fills the interleaver paths (≤ 11 slots deep), so no
        // DelayLine filler or zeroed interleaver memory survives.
        let null = vec![null_transmission_tsp(); tsps];
        let mut bytes = Vec::new();
        tsps_to_slots(&null, &mut bytes);
        disperse_frame(&mut bytes, &e.prbs);
        e.delay.process(&mut bytes);
        e.interleaver.process(&mut bytes);
        e
    }

    /// Transmission TSPs consumed per frame (T, Table 3-3 × N).
    pub fn tsps_per_frame(&self) -> usize {
        self.tsps
    }

    /// Coded bits produced per frame (nc·S·N·204).
    pub fn coded_bits_per_frame(&self) -> usize {
        self.coded_bits
    }

    /// Encode one OFDM frame: `tsps` are exactly T RS-protected 204-byte
    /// transmission TSPs (sync byte first). Returns the nc·S·N·204 coded
    /// bits (values 0/1) in the order fed to the carrier modulator (§3.9).
    pub fn encode_frame(&mut self, tsps: &[[u8; TRANSMISSION_TSP]]) -> Result<Vec<u8>, FecError> {
        if tsps.len() != self.tsps {
            return Err(FecError::FrameLength {
                expected: self.tsps,
                got: tsps.len(),
            });
        }
        let mut bytes = std::mem::take(&mut self.bytes);
        tsps_to_slots(tsps, &mut bytes);
        disperse_frame(&mut bytes, &self.prbs);
        self.delay.process(&mut bytes);
        self.interleaver.process(&mut bytes);
        let mut bits = std::mem::take(&mut self.bits);
        unpack_bits(&bytes, &mut bits);
        let mut out = Vec::with_capacity(self.coded_bits);
        encode_punctured(&mut self.encoder, &self.punct, &bits, &mut out);
        debug_assert_eq!(out.len(), self.coded_bits);
        self.bytes = bytes;
        self.bits = bits;
        Ok(out)
    }

    /// RS-encode 188-byte TSPs (§3.3) and encode them as one frame.
    pub fn encode_frame_188(&mut self, tsps: &[[u8; TSP]]) -> Result<Vec<u8>, FecError> {
        let t: Vec<_> = tsps.iter().map(rs::encode).collect();
        self.encode_frame(&t)
    }
}

/// RS outcome of one decoded TSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsStatus {
    /// Decoded; number of corrected bytes (0..=8).
    Corrected(usize),
    /// More than 8 byte errors detected; bytes left as received.
    Uncorrectable,
}

/// Per-frame RS statistics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RsStats {
    /// TSPs decoded without errors.
    pub clean: usize,
    /// TSPs with 1..=8 corrected bytes.
    pub corrected_packets: usize,
    /// Total corrected bytes.
    pub corrected_bytes: usize,
    /// TSPs with uncorrectable errors.
    pub uncorrectable: usize,
}

/// Output of one decoded frame.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// The T transmission TSPs after RS correction (204 bytes each, sync
    /// byte first, parity kept).
    pub tsps: Vec<[u8; TRANSMISSION_TSP]>,
    /// RS outcome per TSP.
    pub status: Vec<RsStatus>,
    /// Totals over the frame.
    pub stats: RsStats,
}

impl DecodedFrame {
    /// The 188-byte TSPs (parity stripped).
    pub fn tsps_188(&self) -> Vec<[u8; TSP]> {
        self.tsps
            .iter()
            .map(|t| t[..TSP].try_into().expect("188"))
            .collect()
    }
}

/// Receive-side decoding of one layer (§3.8 → §3.3 inverses), frame by
/// frame. See the module docs for the one-frame latency.
#[derive(Debug, Clone)]
pub struct LayerDecoder {
    tsps: usize,
    coded_bits: usize,
    prbs: Vec<u8>,
    deinterleaver: ByteDeinterleaver,
    viterbi: ViterbiDecoder,
    punct: Puncturing,
    pairs: Vec<f32>,
    bits: Vec<u8>,
}

impl LayerDecoder {
    /// Decoder for `layer` in `mode` with the default Viterbi traceback,
    /// assuming the transmitter's encoder started from the zero state.
    pub fn new(mode: Mode, layer: &LayerConfig) -> Self {
        Self::with_viterbi(mode, layer, ViterbiDecoder::new(DEFAULT_TRACEBACK))
    }

    /// Decoder with a caller-configured Viterbi decoder (traceback depth,
    /// unknown start state for mid-stream acquisition).
    pub fn with_viterbi(mode: Mode, layer: &LayerConfig, viterbi: ViterbiDecoder) -> Self {
        let tsps = layer.tsp_per_frame(mode);
        Self {
            tsps,
            coded_bits: layer.coded_bits_per_frame(mode),
            prbs: frame_sequence(tsps),
            deinterleaver: ByteDeinterleaver::new(),
            viterbi,
            punct: Puncturing::new(layer.code_rate),
            pairs: Vec::new(),
            bits: Vec::new(),
        }
    }

    /// LLRs consumed per frame (nc·S·N·204).
    pub fn coded_bits_per_frame(&self) -> usize {
        self.coded_bits
    }

    /// TSPs produced per frame (T).
    pub fn tsps_per_frame(&self) -> usize {
        self.tsps
    }

    /// Decode one frame of LLRs (ln P(0)/P(1), in coded-bit order; 0.0 for
    /// erased bits). Returns the T TSPs of the previous TX frame.
    pub fn decode_frame(&mut self, llrs: &[f32]) -> Result<DecodedFrame, FecError> {
        if llrs.len() != self.coded_bits {
            return Err(FecError::FrameLength {
                expected: self.coded_bits,
                got: llrs.len(),
            });
        }
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.clear();
        depuncture(&self.punct, llrs, &mut pairs);
        let mut bits = std::mem::take(&mut self.bits);
        bits.clear();
        self.viterbi.push(&pairs, &mut bits);
        self.viterbi.flush(&mut bits);
        debug_assert_eq!(bits.len(), self.tsps * TRANSMISSION_TSP * 8);
        let mut bytes = pack_bits(&bits);
        self.pairs = pairs;
        self.bits = bits;
        self.deinterleaver.process(&mut bytes);
        disperse_frame(&mut bytes, &self.prbs);
        let mut tsps = Vec::with_capacity(self.tsps);
        let mut status = Vec::with_capacity(self.tsps);
        let mut stats = RsStats::default();
        for slot in bytes.chunks_exact(TRANSMISSION_TSP) {
            let mut t = slot_to_tsp(slot);
            match rs::decode(&mut t) {
                Ok(c) if c.errors == 0 => {
                    stats.clean += 1;
                    status.push(RsStatus::Corrected(0));
                }
                Ok(c) => {
                    stats.corrected_packets += 1;
                    stats.corrected_bytes += c.errors;
                    status.push(RsStatus::Corrected(c.errors));
                }
                Err(_) => {
                    stats.uncorrectable += 1;
                    status.push(RsStatus::Uncorrectable);
                }
            }
            tsps.push(t);
        }
        Ok(DecodedFrame {
            tsps,
            status,
            stats,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::XorShift;
    use isdbt_core::{CodeRate, Modulation, TimeInterleave};

    fn rand_frame(rng: &mut XorShift, n: usize) -> Vec<[u8; TSP]> {
        (0..n)
            .map(|_| {
                let mut p = [0u8; TSP];
                rng.fill(&mut p);
                p[0] = 0x47;
                p
            })
            .collect()
    }

    /// Module-doc claim: in TX frame k, byte b of slot m (path j = b mod 12)
    /// is byte b of pre-delay slot m + 11 − j of frame k − 1 (or of slot
    /// m + 11 − j − T of frame k). Checked on the byte stream entering the
    /// convolutional encoder.
    #[test]
    fn tx_slot_provenance() {
        let mode = Mode::M1;
        let l = LayerConfig {
            segments: 2,
            modulation: Modulation::Qpsk,
            code_rate: CodeRate::R2_3,
            time_interleave: TimeInterleave(0),
        };
        let t = l.tsp_per_frame(mode);
        let mut delay = DelayLine::new(l.delay_adjustment_tsps(mode));
        let mut il = ByteInterleaver::new();
        let mut rng = XorShift::new(4);
        let mut pre: Vec<Vec<u8>> = Vec::new();
        let mut post: Vec<Vec<u8>> = Vec::new();
        for _ in 0..3 {
            let mut f = vec![0u8; t * TRANSMISSION_TSP];
            rng.fill(&mut f);
            pre.push(f.clone());
            delay.process(&mut f);
            il.process(&mut f);
            post.push(f);
        }
        for k in 1..3 {
            for m in 0..t {
                for b in 0..TRANSMISSION_TSP {
                    let j = b % 12;
                    let src = m + 11 - j;
                    let want = if src < t {
                        pre[k - 1][src * 204 + b]
                    } else {
                        pre[k][(src - t) * 204 + b]
                    };
                    assert_eq!(post[k][m * 204 + b], want, "k {k} m {m} b {b}");
                }
            }
        }
    }

    fn max_run(bits: &[u8]) -> usize {
        let mut best = 0;
        let mut run = 0;
        let mut last = 2u8;
        for &b in bits {
            run = if b == last { run + 1 } else { 1 };
            last = b;
            best = best.max(run);
        }
        best
    }

    /// Start-up priming: a fresh encoder's first two frames contain no run
    /// of equal coded bits longer than 64 (unprimed zeroed memories used to
    /// produce ~39 000-bit zero runs), and RX call 0 yields the null packets
    /// of the virtual priming frame in slots 11..T−1.
    #[test]
    fn first_frame_has_no_long_runs() {
        let cfgs = [
            (Mode::M1, 1, Modulation::Dqpsk, CodeRate::R1_2),
            (Mode::M3, 1, Modulation::Qpsk, CodeRate::R2_3),
            (Mode::M3, 12, Modulation::Qam64, CodeRate::R3_4),
            (Mode::M2, 5, Modulation::Qam16, CodeRate::R7_8),
            (Mode::M3, 13, Modulation::Qam64, CodeRate::R7_8),
        ];
        let mut rng = XorShift::new(8);
        for (mode, segments, modulation, code_rate) in cfgs {
            let l = LayerConfig {
                segments,
                modulation,
                code_rate,
                time_interleave: TimeInterleave(0),
            };
            let mut enc = LayerEncoder::new(mode, &l);
            let mut dec = LayerDecoder::new(mode, &l);
            let t = enc.tsps_per_frame();
            for k in 0..2 {
                let bits = enc.encode_frame_188(&rand_frame(&mut rng, t)).unwrap();
                let r = max_run(&bits);
                assert!(r <= 64, "{mode:?} {l:?} frame {k}: run of {r} equal bits");
                if k == 0 {
                    let llr: Vec<f32> = bits
                        .iter()
                        .map(|&b| if b == 0 { 10.0 } else { -10.0 })
                        .collect();
                    let out = dec.decode_frame(&llr).unwrap();
                    let null = null_transmission_tsp();
                    assert!(out.tsps[11..].iter().all(|p| *p == null), "{mode:?} {l:?}");
                }
            }
        }
    }

    /// The §3.6 claim, checked at the byte level: TX frame k's slots appear
    /// at RX frame k+1 with the same slot index, for a layer with T = 12.
    #[test]
    fn one_frame_latency_small_layer() {
        let l = LayerConfig {
            segments: 1,
            modulation: Modulation::Dqpsk,
            code_rate: CodeRate::R1_2,
            time_interleave: TimeInterleave(0),
        };
        let mut enc = LayerEncoder::new(Mode::M1, &l);
        let mut dec = LayerDecoder::new(Mode::M1, &l);
        assert_eq!(enc.tsps_per_frame(), 12);
        let mut rng = XorShift::new(2);
        let mut prev: Option<Vec<[u8; TSP]>> = None;
        for _ in 0..4 {
            let f = rand_frame(&mut rng, 12);
            let bits = enc.encode_frame_188(&f).unwrap();
            assert_eq!(bits.len(), 96 * 2 * 204);
            let llr: Vec<f32> = bits
                .iter()
                .map(|&b| if b == 0 { 10.0 } else { -10.0 })
                .collect();
            let out = dec.decode_frame(&llr).unwrap();
            if let Some(p) = prev {
                assert_eq!(out.tsps_188(), p);
                assert_eq!(out.stats.clean, 12);
            }
            prev = Some(f);
        }
    }
}
