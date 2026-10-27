//! ARIB ISDB-T service multiplexer.
//!
//! * [`config`] — declarative TOML description of a terrestrial TS
//!   (network, services, events, broadcasters, timing) with first-class
//!   experiment overrides per table.
//! * [`si`] — PSI/SI generation (PAT, PMT, CAT, NIT, SDT, EIT p/f and
//!   schedule, TOT/TDT, BIT) with the typed codecs of `isdb-ts`/`isdb-si`.
//! * [`carousel`], [`hybridcast`] — BML data broadcasting (files → DSM-CC
//!   data carousel, ARIB STD-B24 Vol. 2/3) and the Hybridcast AIT naming
//!   the HTML5 application to start (B24 Vol. 4, IPTVFJ STD-0010/0013).
//! * [`signalling`] — typed receiver-facing signalling values (video
//!   decode control, EIT component types, digital copy control, remote
//!   control key) with spec-table names.
//! * [`video`], [`audio`] — MPEG-2 video and AAC ADTS elementary-stream
//!   parsing (access units, PTS/DTS).
//! * [`tstd`] — H.222.0 T-STD buffer model used for pacing.
//! * [`mux`] — the OFDM-frame-paced CBR multiplexer: exactly
//!   `tsp_per_frame` packets per active layer per frame, timed on the
//!   ARIB STD-B31 §3.2 multiplex-frame clock.
//! * [`output`] — per-layer / multiplex TS writers.
//! * [`analysis`] — measurement of produced streams.
//!
//! Timing model: packet *j* of OFDM frame *k* arrives at clock
//! `k·samples_per_frame + 408·j` of 512/63 MHz (the TS a 13-segment
//! receiver reproduces, B31 §3.2.2), so PCR and PTS/DTS are consistent
//! with the transmitted rate by construction. Typed signalling and raw
//! descriptors that would duplicate them are checked when the
//! configuration is loaded ([`Config::validate`]).
//!
//! Position in the stack: builds sections with `isdb-ts` / `isdb-si` /
//! `isdb-text` and the slot order of `isdbt_fec::MultiplexPattern`; its
//! frames feed `isdbt_phy::Transmitter::transmit_frame` (via
//! [`layer_slices`]) in `isdb mux` / `isdb tx --mux-config`.
//!
//! Spec interpretations and the numbered operational choices are listed in
//! `SPEC-NOTES.md` of this crate; spec scope, test inventory and
//! limitations in its `README.md`.
//!
//! # Example
//!
//! One service with a PMT-declared stream (no media file), Mode 3 bench
//! parameters; every frame has exactly `tsp_per_frame` packets per layer.
//!
//! ```
//! use isdbt_mux::{Config, Multiplexer};
//! use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave};
//!
//! let layer = |segments: u8, modulation, code_rate| {
//!     Some(LayerConfig { segments: Segments::new(segments).unwrap(), modulation, code_rate, time_interleave: TimeInterleave::K2 })
//! };
//! let params = Params::new(
//!     Mode::M3,
//!     GuardInterval::G1_8,
//!     true,
//!     [layer(1, Modulation::Qpsk, CodeRate::R2_3), layer(12, Modulation::Qam64, CodeRate::R3_4), None],
//! )
//! .unwrap();
//! let cfg = Config::from_toml(r#"
//! [network]
//! network_id = 0x79EE
//! name = "ベンチ"
//! area_code = 0x5A5
//! remote_control_key_id = 9
//! channels = [27]
//!
//! [transport]
//! transport_stream_id = 0x79EE
//!
//! [clock]
//! source = "fixed"
//! start = "2026-01-01T12:00:00"
//!
//! [[service]]
//! service_id = 0x0400
//! name = "ベンチHD"
//! provider = "ベンチ"
//! layer = "B"
//! pmt_pid = 0x01F0
//! pcr_pid = 0x01FF
//!
//!   [[service.stream]]
//!   kind = "declared"
//!   pid = 0x0111
//!   stream_type = 0x02
//! "#)
//! .unwrap();
//!
//! let mut mux = Multiplexer::new(&cfg, &params, std::path::Path::new(".")).unwrap();
//! let (full, layers) = mux.next_frame_full();
//! assert_eq!(full.len(), mux.pattern().len());
//! for l in 0..3 {
//!     let want = params.layers()[l].map_or(0, |c| c.tsp_per_frame(params.mode()));
//!     assert_eq!(layers[l].len(), want);
//! }
//! ```

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]
pub mod adts;
pub mod analysis;
pub mod audio;
pub mod capacity;
pub mod carousel;
pub mod config;
pub mod error;
mod float;
pub mod hybridcast;
pub mod jst;
pub mod mux;
pub mod output;
pub mod pcrclock;
pub mod playback;
pub mod relay;
pub mod relay_rate;
pub mod relay_template;
pub mod si;
pub mod signalling;
pub mod tstd;
mod validate;
pub mod video;

pub use config::Config;
pub use error::{MuxError, Result};
pub use mux::{Multiplexer, MuxStats, Packet, layer_slices, null_packet};
pub use output::TsWriter;

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
