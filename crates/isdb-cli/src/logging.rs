//! Diagnostics output: the `tracing` subscriber of the `isdb` binary.
//!
//! Libraries emit [`tracing`] events and never print; this module decides
//! where they go. Reports and data stay on stdout (and are not events);
//! everything a human reads while a command runs, and every warning, is an
//! event on stderr (docs/plans/errors-and-logging.md).
//!
//! Two formats:
//!
//! * `text` (default): one line per event, `[<component>] <message>`,
//!   where the component is the last segment of the event's target
//!   (`isdb_cli::tx` → `[tx]`) and the level is shown only when it is not
//!   `INFO`. The structured fields are **not** printed: the message of
//!   every event is written to stand on its own (the values it needs are
//!   in its text), and the fields repeat those values for machines. That
//!   is the line shape the CLI printed before it emitted events, so
//!   scripts and tests that match on stderr keep working.
//! * `json`: one JSON object per line (`tracing_subscriber`'s JSON
//!   format, event fields flattened into the object), for log collectors
//!   and for tests that assert on fields rather than text.
//!
//! The level comes from `--log-level` (default `info`), or from `RUST_LOG`
//! when set, which takes the full `EnvFilter` directive syntax
//! (`RUST_LOG=isdbt_mux=debug,info`).

use std::fmt;
use std::io::IsTerminal;

use clap::{Args, ValueEnum};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Global diagnostics options, accepted before any subcommand.
#[derive(Args, Debug, Clone, Default)]
pub struct LogArgs {
    /// Lowest level of diagnostics to print on stderr (`RUST_LOG`, when
    /// set, overrides this with an `EnvFilter` directive).
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,
    /// Line format of the diagnostics on stderr.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    pub log_format: LogFormat,
}

/// `--log-level` values, most to least verbose.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogLevel {
    /// Every event, per-packet detail included.
    Trace,
    /// Per-frame and per-section detail.
    Debug,
    /// Progress and state changes (default).
    #[default]
    Info,
    /// Only what needs attention.
    Warn,
    /// Only failures.
    Error,
    /// Nothing.
    Off,
}

impl LogLevel {
    fn directive(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
            LogLevel::Off => "off",
        }
    }
}

/// `--log-format` values.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// `[component] message key=value …`, one line per event.
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

/// Install the global subscriber. Call once, before the subcommand runs.
///
/// # Panics
///
/// If a subscriber is already installed (a programming error: `main`
/// calls this once).
pub fn init(args: &LogArgs) {
    let filter = match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.is_empty() => EnvFilter::new(spec),
        _ => EnvFilter::new(args.log_level.directive()),
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());
    match args.log_format {
        LogFormat::Text => builder
            .event_format(TextFormat)
            .fmt_fields(MessageOnly)
            .init(),
        LogFormat::Json => builder.json().flatten_event(true).init(),
    }
}

/// Field formatter of the text format: the `message` field alone.
struct MessageOnly;

impl<'w> FormatFields<'w> for MessageOnly {
    fn format_fields<R: RecordFields>(&self, mut w: Writer<'w>, fields: R) -> fmt::Result {
        let mut v = MessageVisitor {
            w: &mut w,
            res: Ok(()),
        };
        fields.record(&mut v);
        v.res
    }
}

struct MessageVisitor<'a, 'w> {
    w: &'a mut Writer<'w>,
    res: fmt::Result,
}

impl Visit for MessageVisitor<'_, '_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.res.is_ok() {
            self.res = write!(self.w, "{value:?}");
        }
    }
}

/// `[component] message key=value …`; see the module doc.
struct TextFormat;

impl<S, N> FormatEvent<S, N> for TextFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut w: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let component = meta.target().rsplit("::").next().unwrap_or("isdb");
        write!(w, "[{component}] ")?;
        match *meta.level() {
            Level::INFO => {}
            Level::WARN => write!(w, "WARNING: ")?,
            Level::ERROR => write!(w, "ERROR: ")?,
            Level::DEBUG => write!(w, "debug: ")?,
            Level::TRACE => write!(w, "trace: ")?,
        }
        ctx.field_format().format_fields(w.by_ref(), event)?;
        writeln!(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_env_filter_directives() {
        for (l, d) in [
            (LogLevel::Trace, "trace"),
            (LogLevel::Info, "info"),
            (LogLevel::Off, "off"),
        ] {
            assert_eq!(l.directive(), d);
            EnvFilter::new(d);
        }
    }
}
