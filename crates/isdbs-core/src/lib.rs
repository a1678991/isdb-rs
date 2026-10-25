//! ISDB-S transmission parameters: the system of ITU-R BO.1408-1 Annex 1
//! (ARIB STD-B20), with the frame layout of MIC Ordinance No. 87 of 2011,
//! Chapter 5 §2, annexed tables 45–51.
//!
//! | Module | Source | Contents |
//! |---|---|---|
//! | [`frame`] | Table 3; annexed tables 45, 50, 51 | geometry (N = 48, M = 8, 39 936 symbols), W1/W2/W3 and their coded tails, symbol positions |
//! | [`combination`] | Table 1, Table 2; annexed tables 48, 49 | modulation-code combinations, slot units, puncturing |
//! | [`slots`] | §3.2, §8, Fig. 4 | where each combination's run of slots sits in the main signal |
//! | [`tmcc`] | §6, Figs. 7–11, App. 1; annexed table 50 | the TMCC information codec, energy dispersal and RS(64,48) of a superframe |
//! | [`burst`] | §7; annexed table 51 | the phase-reference burst bits |
//! | [`mainsig`] | §§2–5, §8; annexed table 48 別記 1–3 | sent slots, energy dispersal, inter-frame interleaver, TS codewords of a superframe |
//!
//! Interpretations are in `SPEC-NOTES.md` next to this crate. The receive
//! chain that uses these is `isdbs-phy`.

pub mod burst;
pub mod combination;
pub mod frame;
pub mod mainsig;
pub mod slots;
pub mod tmcc;

pub use combination::{Combination, Modulation};
pub use tmcc::{TmccDecode, TmccInfo};
