//! The content of ISDB-S3 / advanced BS TLV streams (ARIB STD-B60 v1.14):
//! the MMT-SI and TLV-SI tables a receiver uses to find services, events
//! and assets, and the media of each asset reassembled from its MFUs into
//! an elementary stream. Built on `isdb-tlv`, which delivers the TLV, IP
//! and MMTP layers.
//!
//! | Module | Contents |
//! |---|---|
//! | [`desc`] | MMT-SI descriptors (16-bit tags, per-descriptor length width) |
//! | [`table`] | MPT, PLT, MH-EIT, MH-SDT, MH-TOT, MH-BIT, TLV-NIT, AMT |
//! | [`message`] | control-message reassembly (divided, aggregated) into tables |
//! | [`mpu`] | MPU payloads (fragment type and fragmentation indicator as coded enums), MFU reassembly per asset, loss counters |
//! | [`es`] | HEVC Annex B, LOAS audio, caption MFUs |
//! | [`decoder`] | the streaming decoder over TLV packets |
//!
//! ISO/IEC 23008-1 (MMT), ITU-T H.265 and ISO/IEC 14496-3 are not
//! available: what B60 defines is implemented from B60, and the few
//! framing details from the codec standards are marked "from knowledge"
//! (SPEC-NOTES.md). Scrambled payloads (ACAS) are recognised and counted;
//! they are never descrambled (project policy).

// Libraries report through `tracing` events and return values, never by
// printing (docs/plans/errors-and-logging.md section 2.1).
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod decoder;
pub mod desc;
pub mod es;
pub mod message;
pub mod mpu;
pub mod table;

pub use decoder::Decoder;

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
