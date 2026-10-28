//! The error of this crate: [`RadioError`].
//!
//! Its variants say what went wrong in terms a caller can match on: a
//! device selection that does not parse, a device argument or gain the
//! backend does not accept, a setting or tuning outside a backend's range,
//! an operation the backend or the device cannot do, a stream that
//! stopped, a reply the device should not have given, or a device library
//! or I/O failure with the backend and the operation named. A failure of a
//! device library keeps that library's error (`isdb_hackrf::Error`,
//! `isdb_iio::Error`) as its [`source`](std::error::Error::source), so the
//! chain a caller prints names the library call; the variants' own text
//! does not repeat it.

use std::fmt;

/// Result of this crate.
pub type Result<T> = std::result::Result<T, RadioError>;

/// The error of every fallible call of this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RadioError {
    /// A device selection (`NAME[:key=value,...]`, [`crate::DeviceSpec`])
    /// that does not parse.
    #[error("device {spec:?}: {problem}")]
    Spec {
        /// The text given.
        spec: String,
        /// What is wrong with it.
        problem: SpecProblem,
    },
    /// A `NAME=VALUE` string ([`crate::parse_key_value`]) that does not
    /// parse.
    #[error("{text:?}: {problem}")]
    KeyValue {
        /// The text given.
        text: String,
        /// What is wrong with it.
        problem: KeyValueProblem,
    },
    /// No backend of this name in this build ([`crate::backend`]). The
    /// message lists the compiled-in backends and the cargo feature of
    /// each known one.
    #[error("{}", unknown_backend(name, *feature))]
    UnknownBackend {
        /// The name asked for.
        name: String,
        /// The `isdb-radio` feature enabling it, for a known backend left
        /// out of this build; `None` for a name no backend has.
        feature: Option<&'static str>,
    },
    /// A device argument or gain name the backend does not know.
    #[error("{}", unknown_key(backend, *kind, key, known))]
    UnknownKey {
        /// The backend.
        backend: &'static str,
        /// Device argument or gain.
        kind: Key,
        /// The name given.
        key: String,
        /// The names it knows, each with its values and default.
        known: Vec<String>,
    },
    /// A device argument or gain given more than once.
    #[error("{}", repeated_key(backend, *kind, key))]
    RepeatedKey {
        /// The backend.
        backend: &'static str,
        /// Device argument or gain.
        kind: Key,
        /// The name repeated.
        key: String,
    },
    /// A device argument or gain value the backend does not accept.
    #[error("{}", argument(backend, *kind, key, value, rule))]
    Argument {
        /// The backend.
        backend: &'static str,
        /// Device argument or gain.
        kind: Key,
        /// Its name.
        key: String,
        /// The value given.
        value: String,
        /// The rule it breaks.
        rule: Rule,
    },
    /// Two device arguments or gains that do not go together: `first`
    /// `rule` `second` reads as a sentence (`bits=12` `needs`
    /// `recorder=maia`).
    #[error("{backend}: {first} {rule} {second}")]
    Conflict {
        /// The backend.
        backend: &'static str,
        /// The first argument (`key` or `key=value`).
        first: String,
        /// How it conflicts with the second.
        rule: &'static str,
        /// The second argument.
        second: String,
    },
    /// A [`crate::TxOptions::backoff_db`] outside
    /// [`crate::TxOptions::BACKOFF_DB`].
    #[error(
        "backoff {db} dB outside {}..{}",
        crate::TxOptions::BACKOFF_DB.start(),
        crate::TxOptions::BACKOFF_DB.end()
    )]
    Backoff {
        /// The backoff given, dB.
        db: f32,
    },
    /// A duration option that is not finite and > 0
    /// ([`crate::TxOptions::frame_seconds`],
    /// [`crate::RxOptions::queue_seconds`]).
    #[error("{what} {seconds} s: must be finite and > 0")]
    Duration {
        /// Which option (`frame duration`, `RX queue`).
        what: &'static str,
        /// The value given, seconds.
        seconds: f64,
    },
    /// A frequency, sample rate or bandwidth outside what the backend's
    /// device tunes.
    #[error("{}", tuning(what, *hz, device, *min_hz, *max_hz))]
    Tuning {
        /// The backend.
        backend: &'static str,
        /// The device (or its part) whose range it is
        /// (`Pluto (AD9361)`, `HackRF (MAX2837)`).
        device: &'static str,
        /// The quantity (`frequency`, `sample rate`, `RF bandwidth`).
        what: &'static str,
        /// The value asked for, Hz.
        hz: f64,
        /// The smallest value, Hz; `None` for a bandwidth with only an
        /// upper bound.
        min_hz: Option<f64>,
        /// The largest value, Hz.
        max_hz: f64,
    },
    /// The device runs at another frequency or rate than the one
    /// programmed (read back after programming it).
    #[error("{}", readback(what, *requested_hz, *programmed_hz, *tolerance))]
    Readback {
        /// The backend.
        backend: &'static str,
        /// The quantity (`sample rate`, `LO`).
        what: &'static str,
        /// The value programmed, Hz.
        requested_hz: f64,
        /// The value read back, Hz.
        programmed_hz: f64,
        /// How far the two may be apart.
        tolerance: Tolerance,
    },
    /// A capture of more samples than the backend takes with this
    /// configuration ([`crate::Backend::capture_limit`]), or of none.
    #[error("{backend}: capture of {samples} samples: 1..={limit} ({why})")]
    CaptureLength {
        /// The backend.
        backend: &'static str,
        /// The samples asked for.
        samples: u64,
        /// The largest capture.
        limit: u64,
        /// Where the limit comes from (the text of
        /// [`crate::Backend::capture_limit`]).
        why: String,
    },
    /// An operation the backend, or the device in its present state,
    /// cannot do: `radio backend "<backend>" <what>`.
    #[error("radio backend {backend:?} {what}")]
    Unsupported {
        /// The backend.
        backend: &'static str,
        /// What it cannot do (`cannot capture`, `has no symbol front
        /// end`).
        what: &'static str,
    },
    /// An operation the device's firmware does not support.
    #[error("{backend}: {need} (firmware {firmware})")]
    Firmware {
        /// The backend.
        backend: &'static str,
        /// The firmware version the device reports.
        firmware: String,
        /// What the operation needs.
        need: &'static str,
    },
    /// The device has no symbol front end, or one of another format or
    /// tap count than the parameters ([`crate::Backend::capture_symbols`]).
    #[error("{}", front_end(backend, *found, *need))]
    FrontEnd {
        /// The backend.
        backend: &'static str,
        /// (format version, taps) of the device's front end; `None`: it
        /// has none.
        found: Option<(u16, usize)>,
        /// (format version, taps) the parameters need.
        need: (u16, usize),
    },
    /// Front-end parameters that do not validate
    /// ([`crate::Backend::capture_symbols`]).
    #[error(transparent)]
    FrontEndParams(#[from] isdb_frontend::params::ParamError),
    /// A stream that stopped or misbehaves while running.
    #[error("{stream} stream {problem}")]
    Stream {
        /// The stream (`radio`, `HackRF RX`, the IIO device).
        stream: &'static str,
        /// What happened.
        problem: StreamProblem,
    },
    /// The device still reports streaming after it was told to stop.
    #[error("{summary} — power-cycle the device")]
    StillStreaming {
        /// The summary line of the stream that would have been returned.
        summary: String,
    },
    /// A reply of the device (or of a service on it) this crate cannot use:
    /// a missing or malformed field, a state that forbids the operation.
    #[error("{backend}: {what}: {detail}")]
    Reply {
        /// The backend.
        backend: &'static str,
        /// What replied (`maia-sdr GET http://HOST/api`, an attribute).
        what: String,
        /// What is wrong with the reply (it quotes the device's text).
        detail: String,
    },
    /// The device's address was not given and mDNS found no device, or
    /// several: nothing, or more than one, answered the query on the local
    /// link.
    #[error("{}", discovery(backend, query, found, hint))]
    Discovery {
        /// The backend.
        backend: &'static str,
        /// What was asked for (`_iio._tcp.local or pluto.local`).
        query: String,
        /// The devices that answered, `NAME (ADDRESS)`: none, or more
        /// than one.
        found: Vec<String>,
        /// How to give the address instead.
        hint: &'static str,
    },
    /// A device library or protocol call failed; `source` is the
    /// library's error (`isdb_hackrf::Error`, `isdb_iio::Error`), the I/O
    /// error of a network request, or the parser's error of a reply.
    #[error("{backend}: {op}")]
    Device {
        /// The backend.
        backend: &'static str,
        /// What was being done (`open HackRF`, `configure Pluto ip:… for
        /// RX`).
        op: String,
        /// The underlying error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    /// Writing samples to, or reading them from, the caller's I/O.
    #[error("{op}")]
    Io {
        /// What was being done (`write the capture`, `write out.cs8`).
        op: String,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl RadioError {
    /// A [`RadioError::Device`] of `backend` doing `op`.
    pub fn device(
        backend: &'static str,
        op: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
    ) -> Self {
        Self::Device {
            backend,
            op: op.into(),
            source: source.into(),
        }
    }

    /// A [`RadioError::Io`] doing `op`.
    pub fn io(op: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            op: op.into(),
            source,
        }
    }
}

/// What is wrong with a device selection ([`RadioError::Spec`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpecProblem {
    /// Nothing before the `:`.
    #[error("empty backend name (want NAME[:key=value,...])")]
    EmptyName,
    /// A backend name with characters outside `[A-Za-z0-9_-]`.
    #[error("backend name {0:?} is not [A-Za-z0-9_-]+")]
    BadName(String),
    /// A `:` with nothing after it.
    #[error("nothing after ':' (want NAME:key=value,...)")]
    NoArguments,
    /// An argument that is not `key=value`.
    #[error("{text:?}: {problem}")]
    Argument {
        /// The argument.
        text: String,
        /// What is wrong with it.
        problem: KeyValueProblem,
    },
    /// An argument given twice.
    #[error("argument {0:?} given twice")]
    Repeated(String),
}

/// What is wrong with a `NAME=VALUE` string ([`RadioError::KeyValue`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyValueProblem {
    /// No `=`.
    #[error("want NAME=VALUE")]
    NoEquals,
    /// Nothing (but whitespace) before the `=`.
    #[error("empty name before '='")]
    EmptyName,
}

/// Device argument or gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Key {
    /// A device argument ([`crate::RadioConfig::args`]).
    Argument,
    /// A gain ([`crate::RadioConfig::gains`]) of one direction.
    Gain(Direction),
}

/// Transmission or reception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Transmission.
    Tx,
    /// Reception (streamed or captured).
    Rx,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tx => "TX",
            Self::Rx => "RX",
        })
    }
}

/// The rule a device argument or gain value breaks
/// ([`RadioError::Argument`]).
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Rule {
    /// Not a number of the kind `what` (`whole number of dB`).
    #[error("not a {what}")]
    NotANumber {
        /// The kind of number wanted.
        what: &'static str,
    },
    /// Not one of these values.
    #[error("want {}", one_of(.0))]
    OneOf(&'static [&'static str]),
    /// Outside `min..=max`, in `unit` (may be empty).
    #[error("outside {min}..{max}{}", spaced(unit))]
    Range {
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
        /// The unit and any qualifier (`dB at this LO`).
        unit: &'static str,
    },
    /// Not a multiple of `step` in `min..=max`, in `unit`.
    #[error("must be {min}..{max} {unit} in {step} {unit} steps")]
    Steps {
        /// The smallest value.
        min: f64,
        /// The largest value.
        max: f64,
        /// The step.
        step: f64,
        /// The unit.
        unit: &'static str,
    },
    /// Not a multiple of `of` in `min..=max`, in `unit`.
    #[error("must be {min}..={max} {unit} in multiples of {of}")]
    Multiple {
        /// The smallest value.
        min: u64,
        /// The largest value.
        max: u64,
        /// The multiple.
        of: u64,
        /// The unit.
        unit: &'static str,
    },
    /// Empty.
    #[error("empty value")]
    Empty,
}

/// How far a value read back may be from the one programmed
/// ([`RadioError::Readback`]).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Tolerance {
    /// Parts per million of the value programmed.
    Ppm(f64),
    /// Hz.
    Hz(f64),
}

/// What happened to a running stream ([`RadioError::Stream`]).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StreamProblem {
    /// The device's stream thread ended on its own.
    #[error("ended unexpectedly")]
    Ended,
    /// The device's queue closed.
    #[error("closed")]
    Closed,
    /// No buffer transferred for this long.
    #[error("stalled: no buffer transferred for {seconds:.1} s")]
    Stalled {
        /// Seconds without progress.
        seconds: f64,
    },
    /// Nothing received for this long.
    #[error("sent no {what} for {seconds} s")]
    Silent {
        /// What was expected (`samples`, `words`).
        what: &'static str,
        /// Seconds waited.
        seconds: f64,
    },
    /// The device's samples are of another size than expected.
    #[error("delivers {got} bytes per {what}, {want} expected")]
    SampleSize {
        /// `sample` or `word`.
        what: &'static str,
        /// Bytes per sample the device delivers.
        got: usize,
        /// Bytes per sample expected.
        want: usize,
    },
}

/// The message of [`RadioError::UnknownBackend`]: the build's backends
/// come from the registry.
fn unknown_backend(name: &str, feature: Option<&'static str>) -> String {
    crate::backends::unknown_backend_message(name, feature)
}

fn unknown_key(backend: &str, kind: Key, key: &str, known: &[String]) -> String {
    match kind {
        Key::Argument => format!(
            "{backend}: unknown device argument {key:?} (known: {})",
            known.join(", ")
        ),
        Key::Gain(dir) => format!(
            "{backend} {dir} has no gain {key:?} (gains: {})",
            known.join(", ")
        ),
    }
}

fn repeated_key(backend: &str, kind: Key, key: &str) -> String {
    match kind {
        Key::Argument => format!("{backend}: device argument {key:?} given twice"),
        Key::Gain(_) => format!("{backend} gain {key:?} given twice"),
    }
}

fn argument(backend: &str, kind: Key, key: &str, value: &str, rule: &Rule) -> String {
    match kind {
        Key::Argument => format!("{backend}: {key}={value}: {rule}"),
        Key::Gain(_) => format!("{backend} gain {key}={value}: {rule}"),
    }
}

fn discovery(backend: &str, query: &str, found: &[String], hint: &str) -> String {
    if found.is_empty() {
        format!("{backend}: nothing answered the mDNS query for {query}; {hint}")
    } else {
        format!(
            "{backend}: {} devices answered the mDNS query for {query} ({}); {hint}",
            found.len(),
            found.join(", ")
        )
    }
}

fn tuning(what: &str, hz: f64, device: &str, min_hz: Option<f64>, max_hz: f64) -> String {
    match min_hz {
        Some(min) => format!(
            "{what} {hz} Hz outside the {device} range {}..{} MHz",
            min / 1e6,
            max_hz / 1e6
        ),
        None => format!(
            "{what} {hz} Hz above the largest {device} bandwidth, {} MHz",
            max_hz / 1e6
        ),
    }
}

fn readback(what: &str, requested: f64, programmed: f64, tolerance: Tolerance) -> String {
    match tolerance {
        Tolerance::Ppm(limit) => format!(
            "{what} {programmed:.2} Hz programmed for {requested:.2} Hz ({:+.1} ppm, more than {limit} ppm)",
            (programmed / requested - 1.0) * 1e6
        ),
        Tolerance::Hz(limit) => format!(
            "{what} {programmed} Hz programmed for {requested} Hz (more than {limit} Hz off: outside the device's tuning range?)"
        ),
    }
}

fn front_end(backend: &str, found: Option<(u16, usize)>, need: (u16, usize)) -> String {
    match found {
        None => format!("{backend}: this bitstream has no symbol front end"),
        Some((version, taps)) => format!(
            "{backend}: the device's front end is format {version} with {taps} taps; the parameters need format {} with {} taps",
            need.0, need.1
        ),
    }
}

/// `a`, `a or b`, `a, b or c`.
fn one_of(values: &[&str]) -> String {
    match values {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// ` unit`, or nothing for an empty unit.
fn spaced(unit: &str) -> String {
    if unit.is_empty() {
        String::new()
    } else {
        format!(" {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_read_as_sentences() {
        assert_eq!(Rule::OneOf(&["1", "2"]).to_string(), "want 1 or 2");
        assert_eq!(
            Rule::OneOf(&["iio", "maia", "stream"]).to_string(),
            "want iio, maia or stream"
        );
        assert_eq!(
            Rule::Range {
                min: 0.0,
                max: 47.0,
                unit: "dB"
            }
            .to_string(),
            "outside 0..47 dB"
        );
        assert_eq!(
            Rule::Range {
                min: 2.0,
                max: 64.0,
                unit: ""
            }
            .to_string(),
            "outside 2..64"
        );
        assert_eq!(
            Rule::Steps {
                min: 0.0,
                max: 89.75,
                step: 0.25,
                unit: "dB"
            }
            .to_string(),
            "must be 0..89.75 dB in 0.25 dB steps"
        );
    }

    #[test]
    fn device_errors_keep_their_source() {
        use std::error::Error as _;
        let e = RadioError::device("pluto", "open Pluto ip:x", std::io::Error::other("refused"));
        assert_eq!(e.to_string(), "pluto: open Pluto ip:x");
        assert_eq!(e.source().unwrap().to_string(), "refused");
        let e = RadioError::io("write the capture", std::io::Error::other("full"));
        assert!(matches!(&e, RadioError::Io { op, .. } if op == "write the capture"));
        assert_eq!(e.source().unwrap().to_string(), "full");
    }

    #[test]
    fn readback_and_tuning_messages() {
        let e = RadioError::Readback {
            backend: "pluto",
            what: "LO",
            requested_hz: 300e6,
            programmed_hz: 325e6,
            tolerance: Tolerance::Hz(1000.0),
        };
        assert!(
            e.to_string()
                .starts_with("LO 325000000 Hz programmed for 300000000 Hz"),
            "{e}"
        );
        let e = RadioError::Tuning {
            backend: "hackrf",
            device: "HackRF (MAX2837)",
            what: "baseband filter",
            hz: 30e6,
            min_hz: None,
            max_hz: 28e6,
        };
        assert_eq!(
            e.to_string(),
            "baseband filter 30000000 Hz above the largest HackRF (MAX2837) bandwidth, 28 MHz"
        );
    }
}
