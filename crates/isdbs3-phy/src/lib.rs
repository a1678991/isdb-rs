//! ISDB-S3 receive chain after the symbol front end (`isdb-frontend`):
//! frame and slot synchronisation, carrier recovery on the known symbols,
//! TMCC decoding and the main signal's slots. Frame layout and codes are in
//! `isdbs3-core`; BCH and LDPC in `isdb-fec`.
//!
//! | Module | Contents |
//! |---|---|
//! | [`frontend`] | IQ → symbols through `isdb-frontend` with the ISDB-S3 design and trim |
//! | [`sync`] | slot heads, frame heads, carrier phase per slot from the sync words |
//! | [`tmcc`] | the 31 680 TMCC symbols of a frame → LLRs → LDPC → BCH → information |
//! | [`main_signal`] | pilots, learned constellation, demapping, de-interleaving, LDPC, BCH, dispersal → slot data |

pub mod frontend;
pub mod main_signal;
pub mod sync;
pub mod tmcc;

/// Complex sample type.
pub type Cplx = num_complex::Complex<f64>;
