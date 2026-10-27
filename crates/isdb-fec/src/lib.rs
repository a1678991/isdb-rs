//! Channel-coding blocks shared by the ISDB systems.
//!
//! | Module | ISDB-T (ARIB STD-B31 v2.2) | ISDB-S (ITU-R BO.1408-1; MIC Ordinance No. 87 of 2011) | Contents |
//! |---|---|---|---|
//! | [`rs`] | §3.3 | Annex 1 §2, App. 1 Table 3 item 10; annexed table 49 items 1–2 | shortened RS(n, n−16) of RS(255,239): RS(204,188), RS(64,48) |
//! | [`conv`] | §3.8, Fig. 3-10 | Annex 1 §9, Fig. 13; annexed table 49 item 3 | K = 7, 171₈/133₈ encoder and soft Viterbi |
//! | [`bch`] | — | — (ISDB-S3: ARIB STD-B44 v2.1 §3.4 (1), §3.5) | binary BCH over GF(2¹⁶), t = 12, any shortening |
//! | [`ldpc`] | — | — (ISDB-S3: B44 §3.4 (2), §3.5) | quasi-cyclic IRA LDPC from an address table: encoder and layered min-sum decoder |
//! | [`prbs`] | §3.5, Fig. 3-8 | Annex 1 §4, §6.4; annexed tables 48, 50, 51 | X¹⁵+X¹⁴+1 and the burst's X⁹+X⁴+1 |
//!
//! What differs between the systems (puncturing, where a PRBS restarts,
//! byte interleaving) stays in the system crates: `isdbt-fec` and
//! `isdbs-core`. Coded bits are `u8` 0/1 values; soft bits are `f32` LLRs
//! ln P(b=0)/P(b=1).

#![cfg_attr(isdb_fec_portable_simd, feature(portable_simd))]

pub mod bch;
pub mod conv;
pub mod ldpc;
pub mod prbs;
pub mod rs;
pub mod simd;

#[cfg(test)]
pub(crate) mod testutil;

pub use conv::{ConvEncoder, ViterbiDecoder};
pub use prbs::{B44Prbs, Prbs9, Prbs15};
pub use rs::{RS_64_48, RS_204_188, Rs, Uncorrectable};
