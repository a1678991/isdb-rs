//! ISDB-S receive chain after the symbol front end (`isdb-frontend`):
//! frame and superframe synchronisation, carrier recovery on the known
//! symbols, TMCC decoding, and the derotated main signal of every frame.
//! Frame layout and codes are in `isdbs-core`.
//!
//! | Module | Contents |
//! |---|---|
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
//! [`isdbs_core::slots::runs`].

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

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

#[cfg(test)]
pub(crate) mod testsig;

pub use rx::{Frame, Reception, RxOptions, Superframe, main_mer, receive};
