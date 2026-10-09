//! Error type shared by every decoder in the workspace's TS/SI layer.

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Decode/encode failure.  Every variant names the syntax element (`what`)
/// so a failure can be traced to the spec table that defines it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The input ended before the syntax element was complete.
    #[error("{what}: truncated (need {need} bytes, have {have})")]
    Truncated {
        /// Syntax element being decoded.
        what: &'static str,
        /// Bytes required.
        need: usize,
        /// Bytes available.
        have: usize,
    },
    /// A field holds a value the spec forbids, or lengths are inconsistent.
    #[error("{what}: {detail}")]
    Invalid {
        /// Syntax element being decoded.
        what: &'static str,
        /// Human-readable reason.
        detail: String,
    },
    /// CRC_32 check failed (H.222.0 Annex A).
    #[error("{what}: CRC_32 mismatch (computed residue {residue:#010x})")]
    Crc {
        /// Syntax element being decoded.
        what: &'static str,
        /// Residue of the decoder model over the whole section (0 when valid).
        residue: u32,
    },
    /// A value does not fit the field width when encoding.
    #[error("{what}: value {value} does not fit in {bits} bits")]
    Overflow {
        /// Syntax element being encoded.
        what: &'static str,
        /// Offending value.
        value: u64,
        /// Field width.
        bits: u32,
    },
}

impl Error {
    /// Convenience constructor for [`Error::Invalid`].
    pub fn invalid(what: &'static str, detail: impl Into<String>) -> Self {
        Error::Invalid {
            what,
            detail: detail.into(),
        }
    }
}
