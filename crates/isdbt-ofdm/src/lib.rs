//! ISDB-T symbol-domain processing, ARIB STD-B31 v2.2 §3.9–3.16.
//!
//! Transmit chain, per hierarchical layer and OFDM frame (204 symbols):
//!
//! ```text
//! coded bits (nc·S·N·204 per layer, from isdbt-fec)
//!   → mapping::CarrierMapper   §3.9  delay adjustment (Table 3-9), bit interleaving, mapping, normalisation
//!   → combine                  §3.10 layer combining into data segments
//!   → time_interleave          §3.11.1 delay adjustment (Table 3-12) + convolutional interleaving
//!   → freq_interleave          §3.11.2 inter-segment interleaving, carrier rotation, randomising
//!   → frame::FrameLayout       §3.12–3.14 SP/CP/TMCC/AC insertion, spectrum arrangement
//!   → ofdm::OfdmTx             §3.14.1–3.14.2 IFFT and guard interval
//! ```
//!
//! [`Modulator`] runs this chain; [`Demodulator`] is the receiver
//! (synchronisation, channel estimation, TMCC decoding, inverse chain,
//! LLR output).  See [`latency`] for the frame-alignment contract with the
//! FEC layer.
//!
//! Conventions: bits are `u8` 0/1; soft bits are `f32` LLRs
//! `ln P(b=0)/P(b=1)`; complex samples are [`C32`] at 512/63 MHz.  Spec
//! interpretations are recorded in `SPEC-NOTES.md` of this crate; the
//! crate README lists the verification (spec vectors, inverse-pair
//! properties, impaired loopback, the gr-isdbt oracle) and the known gaps
//! (DQPSK and the TMCC/AC parity bit order lack an off-air check).
//!
//! # Example
//!
//! One frame of a single-layer Mode 1 signal, then demodulated:
//!
//! ```
//! use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave};
//! use isdbt_ofdm::{ac::AcFrame, latency, tmcc::TmccInfo, DemodConfig, Demodulator, Modulator};
//!
//! let a = LayerConfig {
//!     segments: Segments::new(13).unwrap(),
//!     modulation: Modulation::Qam16,
//!     code_rate: CodeRate::R1_2,
//!     time_interleave: TimeInterleave::K1,
//! };
//! let params = Params::new(Mode::M1, GuardInterval::G1_8, false, [Some(a), None, None])?;
//! let mut m = Modulator::new(params.clone());
//! assert_eq!(m.bits_per_frame(Layer::A), 96 * 4 * 13 * 204); // nc·S·N·204
//! let tmcc = TmccInfo::from_params(&params);
//! let ac = AcFrame::stuffing(m.layout());
//!
//! let mut x = 1u32; // any coded bits
//! let mut iq = Vec::new();
//! for _ in 0..latency::end_to_end_frames(&params) + 3 {
//!     let bits: Vec<u8> = (0..m.bits_per_frame(Layer::A))
//!         .map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; (x & 1) as u8 })
//!         .collect();
//!     let frame = m.modulate_frame([&bits, &[], &[]], &tmcc, &ac)?;
//!     assert_eq!(frame.len(), params.samples_per_frame());
//!     iq.extend(frame);
//! }
//!
//! let mut rx = Demodulator::new(DemodConfig::default());
//! let frames = rx.push(&iq);
//! assert!(frames.iter().any(|f| f.valid));
//! assert!(frames.iter().all(|f| f.tmcc.parity.is_ok() && f.params == params));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Libraries report through `tracing` events and return values, never by
// printing (docs/plans/errors-and-logging.md section 2.1).
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod ac;
pub mod combine;
pub mod delay;
pub mod dsc;
pub mod frame;
pub mod freq_interleave;
pub mod latency;
pub mod mapping;
pub mod modulator;
pub mod ofdm;
pub mod rx;
pub mod tables;
pub mod time_interleave;
pub mod tmcc;

pub use modulator::{Modulator, ModulatorOptions};
pub use rx::{DemodConfig, DemodFrame, Demodulator};

/// Complex baseband value.
pub type C32 = num_complex::Complex32;

/// Errors of this crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Wrong number of input bits for a layer.
    #[error("layer {layer}: expected {expected} bits, got {got}")]
    BitCount {
        /// Layer letter.
        layer: isdbt_core::Layer,
        /// Expected bit count.
        expected: usize,
        /// Supplied bit count.
        got: usize,
    },
    /// A field of a used layer's TMCC transmission parameters (Table 3-23)
    /// holds a code that names no parameter: a code Tables 3-28 … 3-31
    /// reserve, or the "unused hierarchical layer" code (all ones) in a
    /// layer whose modulation code says it is used.
    #[error(
        "invalid TMCC: {} {field} code {code:0width$b} in a used layer ({table})",
        if *code == field.unused_code() { "unused" } else { "reserved" },
        width = usize::from(field.bits()),
        table = field.table()
    )]
    Tmcc {
        /// The Table 3-23 field.
        field: tmcc::TmccField,
        /// Its code as received, in the field's width.
        code: u8,
    },
    /// Every layer's TMCC codes are assigned, but together they break a
    /// §3.11.2 constraint (segment sum, layer order, partial reception).
    #[error("invalid TMCC: the layers are not valid transmission parameters")]
    TmccParams(#[source] isdbt_core::ParamError),
    /// The AC frame does not have one value per AC carrier of the layout
    /// (§3.16).
    #[error("AC frame has {got} carriers, layout {expected}")]
    AcCarriers {
        /// AC carriers of the frame layout.
        expected: usize,
        /// Carriers in the AC frame.
        got: usize,
    },
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
