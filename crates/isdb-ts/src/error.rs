//! Error type shared by every decoder in the workspace's TS/SI layer.

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Decode/encode failure.  Every variant names the syntax element (`what`)
/// so a failure can be traced to the spec table that defines it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
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
    /// Lengths are inconsistent (a length field disagrees with the bytes
    /// present, a loop is not a whole number of entries), or fields
    /// contradict each other.  One field outside its range is
    /// [`Error::Range`]; a reserved code is [`Error::Reserved`].
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
    /// A field's value lies outside the range the spec allows for it (both
    /// bounds inclusive; a one-sided rule gives the field width's limit as
    /// the other bound).
    #[error("{what}: {value} outside {min}..={max}")]
    Range {
        /// Syntax element.
        what: &'static str,
        /// Offending value.
        value: u64,
        /// Smallest allowed value.
        min: u64,
        /// Largest allowed value.
        max: u64,
    },
    /// A field holds a code the spec reserves or forbids.
    #[error("{what}: {value:#x} is reserved")]
    Reserved {
        /// Syntax element.
        what: &'static str,
        /// Offending code.
        value: u64,
    },
    /// A field the syntax fixes to one value (a start code, a
    /// discriminator, an indicator bit) holds another.
    #[error("{what}: {value:#x}, expected {expected:#x}")]
    Mismatch {
        /// Syntax element.
        what: &'static str,
        /// Value found.
        value: u64,
        /// The one value the syntax allows.
        expected: u64,
    },
    /// A valid code that the decoder in use does not take (the `table_id`
    /// of another table).
    #[error("{what}: {value:#x} not accepted here")]
    NotAccepted {
        /// Syntax element.
        what: &'static str,
        /// Value found.
        value: u64,
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
