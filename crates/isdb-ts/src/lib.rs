//! MPEG-2 transport stream layer per ITU-T H.222.0 (05/2006) | ISO/IEC
//! 13818-1, restricted to what ARIB ISDB-T (STD-B10, STD-B32) uses.
//!
//! Ground truth is the specification text only (see `specs/` and the
//! workspace README); every public item cites the clause/table it
//! implements.  Decoders are lossless (reserved bits are kept), encoders
//! recompute lengths and CRCs, and conformance of field *values* is the job
//! of validators layered on top ([`packet::check`], [`pes::check`],
//! [`psi::check_pmt`], … returning [`Violation`]s with the clause).
//!
//! * [`packet`] — TS packet and adaptation field (Table 2-2, Table 2-6);
//!   [`pcr`] — PCR and PTS/DTS coding; [`cc`] — continuity_counter rules.
//! * [`section`] — long/short section envelope with the
//!   [`section::LongBody`] / [`section::ShortBody`] traits that `isdb-si`
//!   implements; [`psi`] — PAT, CAT, PMT; [`descriptor`] — raw loops.
//! * [`pes`] — PES packets, packetiser and reassembler.
//! * [`demux`] / [`packetize`] — sections ⇄ packets (pointer_field,
//!   stuffing); [`reader`] — sync acquisition (188/192/204-byte units);
//!   [`stats`] — per-PID counters.
//!
//! The crate README covers the stack position, verification (property tests
//! and the live-capture test) and known limitations; spec interpretations
//! are in `SPEC-NOTES.md`.
//!
//! # Example
//!
//! Build a PAT, packetise it, read it back and check that decoding is
//! lossless:
//!
//! ```
//! use isdb_ts::cc::Counter;
//! use isdb_ts::demux::{Assembled, SectionDemux};
//! use isdb_ts::packetize::{Packing, packetize_sections};
//! use isdb_ts::psi::{Pat, PatEntry, check_pat};
//! use isdb_ts::reader::SliceReader;
//! use isdb_ts::section::Long;
//!
//! // PAT (H.222.0 Table 2-30): NIT on PID 0x0010, one programme on 0x01F0.
//! let pat = Pat::section(0x7FE8, 0, vec![PatEntry::new(0, 0x0010), PatEntry::new(0x0408, 0x01F0)]);
//! assert!(check_pat(&pat).is_empty());
//! let bytes = pat.encode()?; // section_length and CRC_32 computed
//!
//! let mut cc = Counter::new(0);
//! let packets = packetize_sections(0x0000, &[bytes.clone()], Packing::OnePerPacket, &mut cc)?;
//! let stream: Vec<u8> = packets.concat();
//!
//! let mut demux = SectionDemux::new();
//! demux.add_pid(0x0000);
//! let mut sections = Vec::new();
//! for p in SliceReader::new(&stream) {
//!     for a in demux.push(p.index, p.packet) {
//!         if let Assembled::Section(s) = a {
//!             sections.push(s.data);
//!         }
//!     }
//! }
//! let back = Long::<Pat>::decode(&sections[0])?;
//! assert_eq!(back.body.network_pid(), Some(0x0010));
//! assert_eq!(back.encode()?, bytes); // encode(decode(x)) == x
//! # Ok::<(), isdb_ts::Error>(())
//! ```

pub mod bits;
pub mod cc;
pub mod crc;
pub mod demux;
pub mod descriptor;
pub mod error;
pub mod packet;
pub mod packetize;
pub mod pcr;
pub mod pes;
pub mod psi;
pub mod reader;
pub mod section;
pub mod stats;
pub mod validate;

pub use error::{Error, Result};
pub use validate::Violation;

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
