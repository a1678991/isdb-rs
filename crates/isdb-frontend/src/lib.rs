//! Single-carrier receive front end for ISDB-S and ISDB-S3: IQ samples at
//! the radio's rate in, symbols at one sample per symbol out (the `Iq` →
//! `Symbols` stage of the receive chain).
//!
//! The stage exists twice with the same output, bit for bit: in the CPU
//! (this crate) and in the FPGA of a PlutoSky R2 (`fpga/` in this
//! repository), so that a radio can send symbols instead of samples when
//! its link cannot carry the samples. This crate is the reference:
//!
//! * [`fixed::Frontend`]: the bit-exact fixed-point model of format
//!   version 1 (mixer, polyphase root-raised-cosine matched filter with
//!   its derivative, non-data-aided timing recovery, AGC, output
//!   quantiser), whose arithmetic the module documents step by step;
//! * [`float::FloatFrontend`]: the same structure in floating point, to
//!   measure what the fixed-point arithmetic costs;
//! * [`format`](mod@format): the `Symbols` boundary, version 1 (16-bit words in blocks
//!   with sync, sequence number, gain and period), its [`format::Packer`]
//!   and the host-side [`format::Reader`] that reports losses;
//! * [`params`]: the run-time parameters (the FPGA's registers) and their
//!   design from symbol rate, roll-off and sample rate ([`params::Design`]);
//! * [`tables`]: the cosine table and the filter banks;
//! * [`metrics`]: ring MER, PSK MER and SQNR, which need no frame sync;
//! * [`control`]: the FPGA's register map and command words (through the
//!   AD9361 core's general purpose registers) and its discovery;
//! * [`vectors`]: the test vectors the FPGA simulation must reproduce.
//!
//! The front end removes a carrier offset given by the host (from
//! `isdb_iq::survey`) but does not recover the carrier phase: its symbols
//! may rotate slowly, and carrier recovery runs on the host, after the
//! boundary, where it needs the frame structure (TMCC, bursts) anyway.
//!
//! No broadcast specification is implemented here: the symbol rates and
//! roll-offs come from the callers (`isdb_iq::survey::KNOWN_SYSTEMS`).
//!
//! # Example
//!
//! ```
//! use isdb_frontend::fixed::Frontend;
//! use isdb_frontend::format::{Packer, Reader, Event};
//! use isdb_frontend::params::{Design, Params};
//!
//! let mut d = Design::new(40e6, 28.86e6, 0.35); // ISDB-S at 40 Msps
//! d.taps = 16;
//! d.bits = 5;
//! d.block_log2 = 8;
//! let p = Params::design(&d).unwrap();
//! let mut fe = Frontend::new(p.clone()).unwrap();
//! let mut pk = Packer::new(&p);
//! let mut words = Vec::new();
//! for n in 0..4000 {
//!     let x = if n % 3 == 0 { [600, -600] } else { [-600, 600] };
//!     if let Some(s) = fe.push(x) {
//!         pk.push(s.q, s.gain, s.period, &mut words);
//!     }
//! }
//! let mut events = Vec::new();
//! Reader::new().feed(&words, &mut events);
//! assert!(matches!(events[0], Event::Block { .. }));
//! ```

pub mod control;
pub mod fixed;
pub mod float;
pub mod format;
pub mod metrics;
pub mod params;
pub mod tables;
pub mod vectors;

pub use fixed::{Frontend, Symbol};
pub use params::{Design, Params};
