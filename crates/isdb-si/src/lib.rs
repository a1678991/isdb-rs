//! ARIB STD-B10 v5.13 service information: SI tables, descriptors and
//! MJD/BCD time coding.
//!
//! Ground truth is ARIB STD-B10 v5.13 (Parts 1 and 2) and, for the section
//! envelope and the conditional access descriptor, ITU-T H.222.0 (05/2006).
//! Every public item cites its clause/table.
//!
//! * [`time`] — MJD + BCD date/time, BCD durations/offsets, Annex C
//!   conversions.
//! * [`descriptor`] — typed, lossless descriptors (Part 2 §6.2) with
//!   [`descriptor::Descriptor::Unknown`] for tags without a typed decoder.
//! * [`tables`] — table bodies (Part 2 §5.2) plugged into the `isdb-ts`
//!   section envelope ([`isdb_ts::section::Long`] / [`isdb_ts::section::Short`]).
//! * [`any`] — `AnyTable::decode(pid, section)` dispatcher.
//! * [`conformance`] — spec-value checks, separate from the codecs.
//! * [`values`] — typed views of table-coded field values (B10 Tables
//!   6-5, 6-39, 6-43, 6-45, 6-60, 6-60-1; Annex F), next to the raw fields.
//! * [`dsmcc`] — the DSM-CC data carousel sections of ARIB data
//!   broadcasting (DII / DDB, ARIB STD-B24 Vol. 3 Ch. 6) and the DII module
//!   descriptors.
//! * [`ait`] — the Application Information Table of Hybridcast (ARIB
//!   STD-B24 Vol. 4 Ch. 5; IPTVFJ STD-0010 / STD-0013).
//! * [`bxml`] — the BML signalling structures inside the data component /
//!   data content descriptors and the AIT identifier info (B24 Vol. 2
//!   §9.3, Vol. 3 Annex C, Vol. 4 Table 7-1).
//!
//! Codecs are lossless: reserved bits are kept, lengths and CRC_32 are
//! recomputed, and `encode(decode(x)) == x` for every valid section.
//! Value conformance (reserved bits '1', length limits, reserved code
//! points, mandatory descriptors, table_id/PID placement) is reported by
//! [`conformance::check`] and never rejected by the decoders.
//!
//! Position in the workspace: built on `isdb-ts` (section envelope, bit
//! I/O) and `isdb-text` (strings); used by `isdb-analyze`, `isdbt-mux` and
//! `isdb-cli`.  The crate `README.md` lists the spec coverage, the tests
//! (spec vectors, property round trips, the synthetic multiplex) and the known
//! gaps; `SPEC-NOTES.md` records every interpretation of the B10 text.
//!
//! # Example
//!
//! ```
//! use isdb_si::{AnyTable, conformance};
//! use isdb_ts::Pid;
//!
//! // time_date_section (B10 Part 2 Table 5-8) carrying the §5.2.7 example
//! // time 0xC079124500 = 1993-10-13 12:45:00 JST, on the TDT PID of Part 1
//! // Table 5-1 (a `Pid` is 13 bits; `Pid::new` checks the range once).
//! let pid = Pid::new(0x0014).unwrap();
//! let tdt = [0x70, 0x70, 0x05, 0xC0, 0x79, 0x12, 0x45, 0x00];
//! let AnyTable::Tdt(t) = AnyTable::decode(pid, &tdt)? else { unreachable!() };
//! assert_eq!(t.body.jst_time.to_jst().unwrap().to_string(), "1993-10-13 12:45:00");
//! assert_eq!(AnyTable::Tdt(t.clone()).encode()?, tdt); // lossless
//! assert!(conformance::check(pid, &AnyTable::Tdt(t)).is_empty());
//!
//! // The 2 reserved bits set to '00' still decode (and re-encode
//! // identically); conformance reports them separately.
//! let odd = [0x40, 0x05, 0xC0, 0x79, 0x12, 0x45, 0x00];
//! let t = AnyTable::decode(pid, &[&[0x70][..], &odd].concat())?;
//! let dev = conformance::check(pid, &t);
//! assert!(dev.iter().any(|d| d.kind == conformance::Kind::ReservedBits));
//! # Ok::<(), isdb_ts::Error>(())
//! ```

// Narrowing casts are proven or fallible here (docs/plans/type-safety.md section 2).
#![warn(clippy::cast_possible_truncation)]

pub mod ait;
pub mod any;
pub mod bxml;
pub mod conformance;
pub mod descriptor;
pub mod dsmcc;
pub mod tables;
pub mod time;
pub mod values;

pub use any::{AnyTable, RawSection};
pub use descriptor::Descriptor;

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
