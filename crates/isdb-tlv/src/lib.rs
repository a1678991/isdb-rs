//! TLV streams of ISDB-S3 / advanced BS: TLV packets (ARIB STD-B32 v3.11
//! Part 3 §3.5), their reassembly from slots (ARIB STD-B44 v2.1 §3.2 (1)),
//! IPv4 / IPv6 / header-compressed IP (B32 §3.4, §3.7), MMTP (B32 §3.9)
//! and the MMT-SI messages and tables they carry (ARIB STD-B60 v1.14), and
//! TLV-SI sections in transmission-control packets (B32 §3.10, B60 §5).
//!
//! | Module | Contents |
//! |---|---|
//! | [`tlv`] | TLV packet, slot-to-packet assembler using the head-TLV indication |
//! | [`ip`] | IPv4 / IPv6 / HCfB → UDP flow and payload |
//! | [`mmtp`] | MMTP header, control messages, MMT-SI message and table names |
//! | [`si`] | TLV-SI sections (TLV-NIT, AMT) of a control packet |
//! | [`stats`] | per-stream counters: packets by type, flows, MMTP ids, signalling |
//!
//! Payload scrambling (ACAS) is recognised, never removed (project policy).

pub mod ip;
pub mod mmtp;
pub mod si;
pub mod stats;
pub mod tlv;

pub use tlv::{Assembler, Packet, PacketType};
