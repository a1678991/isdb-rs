//! Transport stream analysis and experiment tooling on top of the spec
//! decoders (`isdb-ts` for ITU-T H.222.0, `isdb-si` for ARIB STD-B10,
//! `isdb-text` for ARIB STD-B24 strings).
//!
//! * [`inspect`] — full report of a TS (packets, PIDs, continuity, PCR
//!   timing, programs/PES, every section with repetition statistics, the
//!   decoded SI), as text or JSON.
//! * [`validate`] — conformance findings with clause citations and
//!   severities, plus optional bench expectations.
//! * [`diff`] — table-level and field-level comparison of two captures.
//! * [`edit`] — experiment transforms producing a valid TS (null PIDs,
//!   remove/replace/insert sections, typed field mutation, extraction).
//!
//! [`aac`] reassembles ADTS frames of stream_type 0x0F PIDs during the
//! scan and counts the ARIB STD-B32 Part 2 §5.2 header violations that
//! [`validate`] reports as `aac.*` findings.
//!
//! Supporting modules: [`scan`] (the single pass every report starts from:
//! [`scan_path`] / [`scan_bytes`] / [`scan_packets`] → [`Capture`]),
//! [`mutate`] (typed field mutation by path expression), [`decode`],
//! [`tree`] (generic field tree for JSON, dumps and diffs), [`si`] (human
//! SI summary), [`names`] (spec name/allocation tables) and [`timebase`]
//! (packet index → time from PCRs).
//!
//! Validation uses only the decoders; it shares no packing logic with any
//! multiplexer.  Severities: error = a "shall" of H.222.0 / B10 violated;
//! warning = reserved bits, substitutable rules, operational criteria,
//! guidelines and estimates; info = observations.  The crate `README.md`
//! lists the checks' spec scope, the tests (synthetic one-rule streams and
//! the real-capture tests that run when `ISDB_RS_CAPTURE_TS` names a capture)
//! and the known limits; `SPEC-NOTES.md` records how each check reads the
//! spec text.  Used by `isdb-cli` (`isdb ts …`).
//!
//! # Example
//!
//! ```
//! use isdb_analyze::edit::{self, EditOp, EditOptions};
//! use isdb_analyze::mutate::Expr;
//! use isdb_analyze::validate::{Severity, validate};
//! use isdb_ts::cc::Counter;
//! use isdb_ts::packetize::{Packing, packetize_sections};
//! use isdb_ts::psi::{Pat, PatEntry};
//! use isdb_ts::Pid;
//!
//! // A stream that carries only a PAT (H.222.0 Table 2-30).
//! let pmt_pid = Pid::new(0x01F0).unwrap();
//! let pat = Pat::section(0x79EE, 1, vec![PatEntry::new(0x0408, pmt_pid)]).encode()?;
//! let pkts = packetize_sections(Pid::PAT, &[pat], Packing::OnePerPacket, &mut Counter::new(0))?;
//! let cap = isdb_analyze::scan_packets(&pkts, &Default::default());
//! assert_eq!(cap.pat().unwrap().header.table_id_extension, 0x79EE);
//!
//! // B10 Table 5-2 makes CAT/PMT/NIT/SDT/EIT p/f/TDT-or-TOT mandatory.
//! let report = validate(&cap, &Default::default());
//! assert!(report.findings.iter().any(|f| f.severity == Severity::Error
//!     && f.check == "tables.mandatory" && f.clause.contains("Table 5-2")));
//!
//! // Typed mutation re-encodes the section; the diff shows exactly that field.
//! let mut edited = pkts.clone();
//! edit::apply(&mut edited, &[EditOp::Mutate(Expr::parse("pat.version=2")?)], &EditOptions::default())?;
//! let d = isdb_analyze::diff::diff(&cap, &isdb_analyze::scan_packets(&edited, &Default::default()), &Default::default());
//! let changes: Vec<_> = d.sections.iter().flat_map(|s| &s.changes).collect();
//! assert_eq!(changes.len(), 1);
//! assert_eq!(changes[0].path, "header.version_number");
//! # Ok::<(), anyhow::Error>(())
//! ```

pub mod aac;
pub mod decode;
pub mod diff;
pub mod edit;
pub mod inspect;
pub mod mutate;
pub mod names;
pub mod scan;
pub mod si;
pub mod timebase;
pub mod tree;
pub mod validate;

pub use scan::{Capture, ScanOptions, scan_bytes, scan_packets, scan_path};

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
