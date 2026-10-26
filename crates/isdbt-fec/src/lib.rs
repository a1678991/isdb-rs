//! ISDB-T bit-domain channel coding, ARIB STD-B31 v2.2 §3.2–3.8.
//!
//! | Module | Clause | Contents |
//! |---|---|---|
//! | [`remux`] | §3.2, §3.4, Tables 3-5/3-6, Figs. 3-3..3-5 | model-receiver multiplex-frame pattern, TX assembly / hierarchical division, RX TS reproduction |
//! | [`rs`] | §3.3 | shortened RS(204,188) encoder / t = 8 decoder |
//! | [`energy`] | §3.5, Fig. 3-8 | PRBS X¹⁵+X¹⁴+1 energy dispersal |
//! | [`delay`] | §3.6, Table 3-7 | byte-interleave delay adjustment |
//! | [`byte_interleave`] | §3.7, Fig. 3-9 | 12-path convolutional byte (de)interleaver |
//! | [`conv`] | §3.8, Fig. 3-10, Table 3-8 | K=7 171/133 encoder, puncturing, soft Viterbi |
//! | [`layer`] | §3.4–3.8 | per-layer frame pipeline: TSPs ⇄ coded bits / LLRs |
//!
//! Coded bits are `u8` 0/1 values; soft bits are `f32` LLRs
//! ln P(b=0)/P(b=1). Interpretations of the spec text are recorded in
//! `SPEC-NOTES.md` next to this crate; `README.md` lists the tests and the
//! known limitations.
//!
//! [`LayerEncoder`] / [`LayerDecoder`] are the entry points: one call per
//! OFDM frame, with a fixed one-frame latency between them
//! ([`layer::LATENCY_FRAMES`], §3.6; see the [`layer`] module docs for the
//! full alignment contract). The symbol-domain stages (§3.9 onwards) are in
//! `isdbt-ofdm`; `isdbt-phy` connects the two.
//!
//! # Example
//!
//! ```
//! use isdbt_core::{CodeRate, LayerConfig, Mode, Modulation, TimeInterleave, TSP};
//! use isdbt_fec::{LayerDecoder, LayerEncoder};
//!
//! let layer = LayerConfig {
//!     segments: 1,
//!     modulation: Modulation::Qpsk,
//!     code_rate: CodeRate::R1_2,
//!     time_interleave: TimeInterleave::K0,
//! };
//! let (mut enc, mut dec) = (LayerEncoder::new(Mode::M1, &layer), LayerDecoder::new(Mode::M1, &layer));
//! let frame: Vec<[u8; TSP]> = (0..enc.tsps_per_frame() as u8)
//!     .map(|i| { let mut p = [i; TSP]; p[0] = 0x47; p })
//!     .collect();
//! let mut out = Vec::new();
//! for _ in 0..2 {
//!     let bits = enc.encode_frame_188(&frame)?;
//!     let llr: Vec<f32> = bits.iter().map(|&b| if b == 0 { 1.0 } else { -1.0 }).collect();
//!     out.push(dec.decode_frame(&llr)?);
//! }
//! assert_eq!(out[1].tsps_188(), frame); // decoder call 1 = encoder call 0
//! # Ok::<(), isdbt_fec::FecError>(())
//! ```

pub mod byte_interleave;
pub mod conv;
pub mod delay;
pub mod energy;
pub mod layer;
pub mod remux;
pub mod rs;

#[cfg(test)]
pub(crate) mod testutil;

pub use layer::{
    DecodedFrame, LayerDecoder, LayerEncoder, RsStats, RsStatus, null_transmission_tsp,
};
pub use remux::MultiplexPattern;

use isdbt_core::Layer;

/// Errors of the channel-coding stages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FecError {
    /// More than t = 8 byte errors in an RS(204,188) word (§3.3).
    #[error("RS(204,188): uncorrectable")]
    RsUncorrectable,
    /// Wrong number of items for one frame.
    #[error("frame length: expected {expected}, got {got}")]
    FrameLength {
        /// Required count.
        expected: usize,
        /// Supplied count.
        got: usize,
    },
    /// Wrong number of TSPs for a layer in a multiplex frame (§3.2).
    #[error("layer {layer}: expected {expected} TSPs, got {got}")]
    LayerCount {
        /// Layer.
        layer: Layer,
        /// Required count.
        expected: usize,
        /// Supplied count.
        got: usize,
    },
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
