//! ISDB-S3 receive chain after the symbol front end (`isdb-frontend`):
//! frame and slot synchronisation, carrier recovery on the known symbols,
//! TMCC decoding and the main signal's slots. Frame layout and codes are in
//! `isdbs3-core`; BCH and LDPC in `isdb-fec`.
//!
//! | Module | Contents |
//! |---|---|
//! | [`frontend`] | IQ → symbols through `isdb-frontend` with the ISDB-S3 design and trim |
//! | [`eq`] | least-squares symbol-spaced equaliser trained on each frame's sync and pilot symbols |
//! | [`sync`] | slot heads, frame heads, carrier phase per slot from the sync words |
//! | [`tmcc`] | the 31 680 TMCC symbols of a frame → LLRs → LDPC → BCH → information |
//! | [`rx`] | `receive`: frames, TMCC, decoded slots in order with their stream ids |
//! | [`main_signal`] | pilots, learned constellation, demapping, de-interleaving, LDPC, BCH, dispersal → slot data |

pub mod eq;
pub mod frontend;
pub mod main_signal;
pub mod rx;
pub mod sync;
pub mod tmcc;

/// Complex sample type.
pub type Cplx = num_complex::Complex<f64>;

/// A soft metric (LLR) as the `f32` the LDPC decoder takes.
// Soft-bit quantisation: f32 is the decoder's precision, so the cast
// rounds the metric to it; a metric past the f32 range (a noise variance
// near zero) becomes the infinity it stands for.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn soft(x: f64) -> f32 {
    x as f32
}
