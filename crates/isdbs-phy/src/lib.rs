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
//!
//! The boundary to the main-signal decoder (Phase 3) is [`rx::Frame`]: the
//! 38 976 main symbols of a frame, derotated and scaled to unit amplitude,
//! with the frame's index in its superframe; the slot layout comes from
//! the superframe's [`isdbs_core::TmccInfo`] through
//! [`isdbs_core::slots::runs`].

/// Complex symbol.
pub type Cplx = num_complex::Complex<f64>;

pub mod carrier;
pub mod frontend;
pub mod rx;
pub mod sync;
pub mod tmcc;

#[cfg(test)]
pub(crate) mod testsig;

pub use rx::{Frame, Reception, RxOptions, Superframe, main_mer, receive};
