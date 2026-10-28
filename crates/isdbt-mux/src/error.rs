//! Error type.
//!
//! A configuration error names the value it is about by `key`, the dotted
//! TOML path of the value with 0-based array indices in file order
//! (`service[0].stream[2].bml.block_size`, `nit.add_descriptors[1].hex`,
//! `clock.start`); a rule on a table as a whole names the table
//! (`network`, `service[1]`). The message (`Display`) is derived from the
//! fields.

use isdb_ts::Pid;
use isdbt_core::Layer;

/// Multiplexer error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MuxError {
    /// A configuration value breaks a rule that no other variant names;
    /// `problem` states the rule (and how to satisfy it).
    #[error("config: {key}: {problem}")]
    Config {
        /// TOML path of the value or table.
        key: String,
        /// The rule the value breaks.
        problem: String,
    },
    /// The TOML text does not parse into a [`crate::Config`] (syntax, an
    /// unknown key, a wrong type or name), or a configuration does not
    /// serialize. The toml crate's message names the key and position.
    #[error("config: {message}")]
    Toml {
        /// The toml crate's message.
        message: String,
    },
    /// A hex string (`hex = "…"`, `sections = […]`) that is not whole pairs
    /// of hex digits.
    #[error("config: {key}: hex {value:?}: {}", hex_problem(.bad.as_deref()))]
    Hex {
        /// TOML path of the string.
        key: String,
        /// The string as given.
        value: String,
        /// The first pair that is not two hex digits; `None` when the
        /// number of digits is odd.
        bad: Option<String>,
    },
    /// A string not in the syntax its key takes: a JST time
    /// `YYYY-MM-DDTHH:MM:SS`, a duration `HH:MM:SS`, an offset `HH:MM`, a
    /// version `major.minor`, a country code.
    #[error("config: {key}: {value:?}: want {want}")]
    Format {
        /// TOML path of the string.
        key: String,
        /// The string as given.
        value: String,
        /// The syntax the key takes.
        want: &'static str,
    },
    /// A number outside the range of its field (a bit width of the coded
    /// field, or a limit of the specification or of this multiplexer).
    #[error("config: {key}: {value} outside {min}..={max}{note}")]
    Range {
        /// TOML path of the number.
        key: String,
        /// The value given.
        value: i64,
        /// Smallest value accepted.
        min: i64,
        /// Largest value accepted.
        max: i64,
        /// Where the range comes from (empty, or ` (…)` with the clause).
        note: &'static str,
    },
    /// A time outside the MJD date range ARIB STD-B10 Annex C codes
    /// (16-bit MJD: §5.2.8 "present date can be indicated up to April 22,
    /// 2038").
    #[error(
        "config: {key}: time {seconds} (s since 1970-01-01 JST) outside the MJD range (B10 Annex C)"
    )]
    TimeRange {
        /// TOML path of the time (`clock.start` for the stream clock).
        key: String,
        /// The time ([`crate::jst::JstSeconds`]).
        seconds: i64,
    },
    /// A code the specification reserves (or forbids at this place).
    #[error("config: {key}: {} is reserved{note}", code(*.value))]
    Reserved {
        /// TOML path of the value.
        key: String,
        /// The code given.
        value: u32,
        /// The clause (empty, or ` (…)`).
        note: &'static str,
    },
    /// A PID outside the range a component may use.
    #[error("config: {key}: PID {pid:#06x} outside 0x0010–0x1FFE (H.222.0 Table 2-3)")]
    PidRange {
        /// TOML path of the PID.
        key: String,
        /// The PID given.
        pid: Pid,
    },
    /// A PID that another component of the multiplex already uses.
    #[error(
        "config: {key}: PID {pid:#06x} already used by {by}{}",
        if *.other_layer { " on another layer (would need B31 Part 2 §3.2.2 duplicate packets)" } else { "" }
    )]
    PidInUse {
        /// TOML path of the PID (or the table that carries it).
        key: String,
        /// The PID.
        pid: Pid,
        /// The component already on it (a TOML path or a table name).
        by: String,
        /// The other component is on another hierarchical layer.
        other_layer: bool,
    },
    /// A component on a hierarchical layer the transmission parameters
    /// leave without segments.
    #[error("config: {key}: layer {layer} is not active")]
    InactiveLayer {
        /// TOML path of the layer key (`service[0].layer`, `nit.layer`).
        key: String,
        /// The layer.
        layer: Layer,
    },
    /// A key that another setting needs is not given.
    #[error("config: {key} is missing ({needed_by} needs it)")]
    Missing {
        /// TOML path of the missing key.
        key: String,
        /// The setting that needs it (`kind = "ts-program"`).
        needed_by: String,
    },
    /// A key given where it has no meaning.
    #[error("config: {key} applies only to {applies_to}")]
    NotApplicable {
        /// TOML path of the key given.
        key: String,
        /// Where it applies (`kind = "bml-carousel"`).
        applies_to: String,
    },
    /// Two keys that exclude each other are both given.
    #[error("config: {key} and {other} both given; give one")]
    Conflict {
        /// TOML path of the first key.
        key: String,
        /// The other key (path relative to the same table).
        other: String,
    },
    /// A value listed twice where each entry must be unique.
    #[error("config: {key}: {value} is listed twice")]
    Duplicate {
        /// TOML path of the list.
        key: String,
        /// The repeated value as the configuration gives it.
        value: String,
    },
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
    /// A method called out of its order (the caller's contract, stated in
    /// the method's `# Errors`).
    #[error("{what}")]
    CallOrder {
        /// The rule.
        what: &'static str,
    },
    /// An invariant the multiplexer establishes itself does not hold: a
    /// bug in isdbt-mux, reported instead of a panic.
    #[error("internal error in isdbt-mux: {what}")]
    Internal {
        /// The invariant.
        what: &'static str,
    },
    /// Encoding error from the TS/SI codecs.
    #[error("encode: {0}")]
    Encode(#[from] isdb_ts::Error),
    /// I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A code as the configuration would give it: small ones in decimal,
/// bit fields in hex.
fn code(value: u32) -> String {
    if value < 10 {
        value.to_string()
    } else {
        format!("{value:#x}")
    }
}

fn hex_problem(bad: Option<&str>) -> String {
    match bad {
        None => "odd number of digits".to_string(),
        Some(d) => format!("{d:?} is not two hex digits"),
    }
}

impl MuxError {
    /// [`MuxError::Config`] at `key`.
    pub fn config(key: impl Into<String>, problem: impl Into<String>) -> Self {
        Self::Config {
            key: key.into(),
            problem: problem.into(),
        }
    }

    /// The TOML path a configuration error names, if it names one.
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Config { key, .. }
            | Self::Hex { key, .. }
            | Self::Format { key, .. }
            | Self::Range { key, .. }
            | Self::TimeRange { key, .. }
            | Self::Reserved { key, .. }
            | Self::PidRange { key, .. }
            | Self::PidInUse { key, .. }
            | Self::InactiveLayer { key, .. }
            | Self::Missing { key, .. }
            | Self::NotApplicable { key, .. }
            | Self::Conflict { key, .. }
            | Self::Duplicate { key, .. } => Some(key),
            _ => None,
        }
    }
}

/// TOML paths of the configuration entries.
pub(crate) mod key {
    use crate::config::Service;

    /// `service[i]`.
    pub(crate) fn service(i: usize) -> String {
        format!("service[{i}]")
    }

    /// `service[i].stream[n]` of stream `k` of `s`, `n` counting the
    /// entries of the file: the streams the multiplexer expands one
    /// `ts-program` entry into (consecutive, same relay index) count once.
    pub(crate) fn stream(i: usize, s: &Service, k: usize) -> String {
        let mut n = 0;
        for j in 1..=k.min(s.streams.len().saturating_sub(1)) {
            let same_entry = matches!(
                (s.streams[j - 1].relay_es, s.streams[j].relay_es),
                (Some((a, _)), Some((b, _))) if a == b
            );
            if !same_entry {
                n += 1;
            }
        }
        format!("service[{i}].stream[{n}]")
    }

    /// `service[i].event[j]`.
    pub(crate) fn event(i: usize, j: usize) -> String {
        format!("service[{i}].event[{j}]")
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, MuxError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Service, StreamCfg};

    #[test]
    fn stream_key_counts_file_entries() {
        let mut s: Service = toml::from_str(
            "service_id = 1\nprovider = \"\"\nname = \"\"\nlayer = \"A\"\npmt_pid = 0x1F0",
        )
        .unwrap();
        let st: StreamCfg =
            toml::from_str("kind = \"declared\"\npid = 0x111\nstream_type = 2").unwrap();
        let relayed = |ri, ei| StreamCfg {
            relay_es: Some((ri, ei)),
            ..st.clone()
        };
        // file: [declared, ts-program (3 ES), declared, ts-program (2 ES)]
        s.streams = vec![
            st.clone(),
            relayed(0, 0),
            relayed(0, 1),
            relayed(0, 2),
            st.clone(),
            relayed(1, 0),
            relayed(1, 1),
        ];
        let keys: Vec<String> = (0..7).map(|k| key::stream(2, &s, k)).collect();
        assert_eq!(
            keys,
            [0, 1, 1, 1, 2, 3, 3].map(|n| format!("service[2].stream[{n}]"))
        );
    }
}
