//! ISDB-T transmission parameters, ARIB STD-B31 v2.2 Chapter 3.
//!
//! Only quantities stated in the spec text are encoded here; each item
//! cites its table.  Derived quantities are checked against the spec's own
//! tables in this crate's tests.
//!
//! * [`params`] (re-exported here) — [`Mode`], [`GuardInterval`],
//!   [`Modulation`], [`CodeRate`], [`TimeInterleave`], [`Segments`], [`LayerConfig`] and
//!   the validated [`Params`] set (§2.3, Tables 3-1..3-5, §3.11.2), with
//!   frame length, samples and 27 MHz ticks per OFDM frame.
//! * [`fec_params`] — per-layer figures for the channel coding (Tables
//!   3-6, 3-7, 3-8): delay adjustment, coded / information bits per frame.
//! * [`ofdm_params`] — symbol-domain figures (Tables 3-9, 3-12, Fig. 3-23,
//!   Fig. 3-32, §3.14.1): bit/time-interleave delays, segment arrangement,
//!   carrier counts, centre carrier.
//!
//! The crate is the shared vocabulary of `isdbt-fec`, `isdbt-ofdm`,
//! `isdbt-phy`, `isdbt-mux` and `isdb-cli`; it does no signal processing.
//! See `README.md` next to this crate for the verification table and
//! limitations.
//!
//! # Example
//!
//! ```
//! use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave};
//!
//! // Mode 3, GI 1/8, partial reception: A = 1 seg QPSK 2/3, B = 12 seg 64QAM 3/4.
//! let ti = TimeInterleave::K2;
//! let a = LayerConfig { segments: Segments::new(1).unwrap(), modulation: Modulation::Qpsk, code_rate: CodeRate::R2_3, time_interleave: ti };
//! let b = LayerConfig { segments: Segments::new(12).unwrap(), modulation: Modulation::Qam64, code_rate: CodeRate::R3_4, time_interleave: ti };
//! let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [Some(a), Some(b), None]).unwrap();
//! assert_eq!((a.tsp_per_frame(p.mode()), b.tsp_per_frame(p.mode())), (64, 2592)); // Table 3-3
//! assert_eq!(p.multiplex_frame_tsps(), 4608); // Table 3-5
//! assert_eq!(p.frame_ticks_27mhz(), 6_246_072); // 231.336 ms × 27 MHz
//! ```

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
pub mod fec_params;
pub mod ofdm_params;
pub mod params;

pub use params::{
    CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, ParamError, Params,
    SAMPLE_RATE_DEN, SAMPLE_RATE_NUM, SEGMENTS, SYMBOLS_PER_FRAME, Segments, TRANSMISSION_TSP, TSP,
    TimeInterleave, sample_rate_hz,
};

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
