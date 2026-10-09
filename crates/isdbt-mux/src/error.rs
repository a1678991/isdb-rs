//! Error type.

/// Multiplexer error.
#[derive(Debug, thiserror::Error)]
pub enum MuxError {
    /// Invalid or inconsistent configuration.
    #[error("config: {0}")]
    Config(String),
    /// Elementary stream could not be parsed.
    #[error("media {path}: {msg}")]
    Media {
        /// Source file.
        path: String,
        /// What went wrong.
        msg: String,
    },
    /// A `ts-program` relay needs more than its layer carries
    /// (`rate_check = "error"`); `message` lists the layouts that fit
    /// ([`crate::capacity::suggest`] with `layer` and `need_bps`).
    #[error("config: {message}")]
    OverCapacity {
        /// The layer carrying the relay.
        layer: isdbt_core::Layer,
        /// Mean rate of the relayed streams + PCR/PSI/SI on that layer (bit/s).
        need_bps: f64,
        /// The full refusal, with the suggested layouts.
        message: String,
    },
    /// Encoding error from the TS/SI codecs.
    #[error("encode: {0}")]
    Encode(#[from] isdb_ts::Error),
    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, MuxError>;
