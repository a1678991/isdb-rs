//! ISDB-S3 transmission parameters: ARIB STD-B44 v2.1 (ITU-R BO.2098-0
//! Annex 1, "System F"; MIC Ordinance No. 87 of 2011 Chapter 5 §3).
//!
//! | Module | B44 clause | Contents |
//! |---|---|---|
//! | [`params`] | §3.2–§3.4, Table 3.2-1, 3.4-2, 3.8-2/3 | frame and slot geometry, modulations, code rates, γ |
//! | [`ldpc`], [`ldpc_tables`] | §3.4 (2) Appendix 1 | the eleven LDPC codes from the transcribed address tables |
//! | [`constellation`] | §3.8, Fig. 3.8-1 | signal points and bit labels |
//! | [`interleave`] | §3.7 | bit interleaver of 8PSK / 16APSK / 32APSK slots |
//! | [`dispersal`] | §3.6 | slot, TMCC and pilot energy dispersal |
//! | [`tmcc`] | §3.2 (3), §3.5, §3.11 | TMCC information codec and signal coding |
//! | [`slots`] | §3.2 (2) | slot allocation and codeword placement in the frame |
//!
//! The outer BCH code and the LDPC machinery are system-neutral and live in
//! `isdb-fec`. Interpretations of the standard are in SPEC-NOTES.md.

pub mod constellation;
pub mod dispersal;
pub mod interleave;
pub mod ldpc;
#[allow(clippy::unreadable_literal)]
pub mod ldpc_tables;
pub mod params;
pub mod slots;
pub mod tmcc;

pub use params::{CodeRate, Modulation};
pub use tmcc::TmccInfo;
