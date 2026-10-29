//! ISDB-S receive and transmit chains on the symbol side of the front end
//! (`isdb-frontend`). Receive: frame and superframe synchronisation,
//! carrier recovery on the known symbols, TMCC decoding, the derotated
//! main signal of every frame and its TS packets per TS_ID. Transmit
//! ([`tx`]): TS packets per TS → the symbols of a superframe. Frame layout
//! and codes are in `isdbs-core`.
//!
//! | Module | Contents |
//! |---|---|
//! | [`tx`] | the transmitter: TS packets per relative TS → RS(204,188), dispersal and interleaving, inner code and mapping, header and bursts → a superframe's 319 488 symbols |
//! | [`sync`] | differential search on the coded W1 tail, W2/W3 check, tracking at 39 936 symbols, slips |
//! | [`carrier`] | phase of the header tails and the 192 bursts, unwrapped, smoothed and interpolated |
//! | [`tmcc`] | Viterbi decoding of the 192 header symbols, sync-word check |
//! | [`rx`] | `receive`: frames, superframes with their TMCC, `main_mer` |
//! | [`frontend`] | IQ → symbols through `isdb-frontend` with the ISDB-S design |
//! | [`demod`] | a frame's main signal → bytes: soft demapping (BPSK, QPSK, TC8PSK set-partition metrics), depuncturing, Viterbi |
//! | [`ts`] | a superframe → TS packets: de-interleave, energy dispersal, RS(204,188), TS_ID per slot |
//! | [`par`] | independent work on all cores, results in order |
//!
//! The boundary between frame recovery and the main-signal decoder is [`rx::Frame`]: the
//! 38 976 main symbols of a frame, derotated and scaled to unit amplitude,
//! with the frame's index in its superframe; the slot layout comes from
//! the superframe's [`isdbs_core::TmccInfo`] through
//! [`isdbs_core::slots::runs`]. The transmitter's output is
//! [`tx::Superframe`]: unit-modulus symbols at one sample per symbol, and
//! where each packet went, in the order the receiver returns them.

// Libraries report through `tracing` events and return values, never by
// printing (docs/plans/errors-and-logging.md section 2.1).
#![warn(clippy::print_stdout, clippy::print_stderr)]

/// Complex symbol.
pub type Cplx = num_complex::Complex<f64>;

/// A soft metric (LLR) as the `f32` the Viterbi decoder takes.
// Soft-bit quantisation: f32 is the decoder's precision, so the cast
// rounds the metric to it; a metric past the f32 range (a noise variance
// near zero) becomes the infinity it stands for.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn soft(x: f64) -> f32 {
    x as f32
}

pub mod carrier;
pub mod demod;
pub mod frontend;
pub mod par;
pub mod rx;
pub mod sync;
pub mod tmcc;
pub mod ts;
pub mod tx;

#[cfg(test)]
pub(crate) mod testsig;

pub use rx::{Frame, Reception, RxOptions, Superframe, main_mer, receive};
