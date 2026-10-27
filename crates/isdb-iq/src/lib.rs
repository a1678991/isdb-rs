//! System-independent IQ recordings for the `isdb-rs` receivers.
//!
//! The `Iq` boundary of the receive chain (complex samples at the radio's
//! rate) as files on disk, independent of the broadcast system:
//!
//! * [`SampleFormat`]: how samples are stored (`cs8`, `cs16` with its full
//!   scale, `cf32`) and how they decode to [`C32`].
//! * [`sigmf`]: the [SigMF](https://sigmf.org) metadata written next to a
//!   capture (`NAME.sigmf-meta` beside `NAME.sigmf-data`) and read back, with
//!   the `isdb:` extension fields this project adds (full scale, programmed
//!   LO, bandwidth, gain). Each SigMF capture segment of a recording is one
//!   contiguous stretch of samples; a new segment marks a gap.
//! * [`Recording`]: a data file with its format, rate and centre frequency
//!   (from the SigMF metadata or given), read as [`C32`].
//! * [`survey`]: what a single-carrier signal in a recording looks like
//!   without demodulating it: power spectrum and band edges, the symbol-rate
//!   line of |z|², the carrier lines of z², z⁴, z⁸, and the known system
//!   (ISDB-S, ISDB-S3) whose symbol rate matches.
//!
//! No broadcast specification is implemented here; the symbol rates in
//! [`survey::KNOWN_SYSTEMS`] are quoted from the transmission standards
//! (crate README).
//!
//! # Example
//!
//! ```
//! use isdb_iq::{C32, SampleFormat};
//!
//! // AD936x samples from libiio: 12 bits sign-extended in i16.
//! let f = SampleFormat::Cs16 { full_scale: 2048.0 };
//! let mut out = Vec::new();
//! f.decode(&[0x00, 0x04, 0x00, 0xfc], &mut out); // I = 1024, Q = -1024
//! assert_eq!(out, vec![C32::new(0.5, -0.5)]);
//! assert_eq!(f.sigmf_datatype(), "ci16_le");
//! ```

// Libraries report through `tracing` events and return values, never by
// printing (docs/plans/errors-and-logging.md section 2.1).
#![warn(clippy::print_stdout, clippy::print_stderr)]

pub mod sigmf;
pub mod survey;

mod format;
mod recording;

pub use format::SampleFormat;
pub use num_complex::Complex32 as C32;
pub use recording::{Recording, data_path_for, meta_path_for};

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// File I/O.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: String,
        /// The I/O error.
        source: std::io::Error,
    },
    /// Malformed or unsupported SigMF metadata or sample format.
    #[error("{0}")]
    Format(String),
}

/// Result of this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Format the time `secs` seconds after the Unix epoch as an ISO 8601 UTC
/// timestamp with milliseconds (`2026-10-20T08:15:30.125Z`), the form of
/// SigMF `core:datetime`.
pub fn iso8601_utc(secs: f64) -> String {
    // Milliseconds since the epoch; `as` saturates far outside any clock
    // reading and std has no fallible float-to-integer conversion.
    #[allow(clippy::cast_possible_truncation)]
    let ms_total = (secs * 1000.0).round() as i64;
    let (days, ms) = (
        ms_total.div_euclid(86_400_000),
        ms_total.rem_euclid(86_400_000),
    );
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601() {
        assert_eq!(iso8601_utc(0.0), "1970-01-01T00:00:00.000Z");
        // 2000-02-29 (leap day), 2026-10-20 08:15:30.125.
        assert_eq!(iso8601_utc(951_782_400.0), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_792_484_130.125), "2026-10-20T08:15:30.125Z");
        assert_eq!(iso8601_utc(-1.0), "1969-12-31T23:59:59.000Z");
    }
}
