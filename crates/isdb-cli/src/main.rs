//! `isdb`: ISDB-T streamer (ARIB STD-B31) and TS tools.
//!
//! * `isdb tx` — transport streams → ISDB-T (or, `--system isdbs`,
//!   ISDB-S) baseband → IQ file or radio
//! * `isdb rx` — IQ file or radio → demodulated TS + reception report
//! * `isdb loopback` — software TX → impaired channel → RX, byte-exact
//!   check (ISDB-T, or ISDB-S with `--system isdbs`)
//! * `isdb ts`, `isdb mux` — TS analysis / service multiplexing
//! * `isdb capture` — radio → raw IQ file with SigMF metadata (receive
//!   only, any system: ISDB-T, ISDB-S, ISDB-S3)
//! * `isdb iq survey` — symbol rate, carrier offset, band edges and system
//!   of a single-carrier recording
//! * `--relay FILE` (`tx`, `loopback`, `mux`) — rebroadcast a recorded
//!   program without a TOML file
//! * `isdb completions SHELL` — shell completion script (bash, zsh, …)
//!
//! The binary adds I/O, threading, pacing, radio control and reporting on
//! top of the library crates (`isdbt-phy`, `isdbt-mux`, `isdb-analyze`,
//! `isdb-radio`); it holds no spec logic and no device code.  Options, the bench recipe, the
//! thread architecture and the test inventory are in this crate's
//! `README.md`.

use std::io::Write;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

mod bands;
mod capture;
mod channel;
mod input;
mod iq;
mod iqplay;
mod keys;
mod logging;
mod loopback;
mod loopbacks;
#[allow(missing_docs)]
mod mux;
// Not wired into `tx --system isdbs` yet: piece P4 of
// docs/plans/isdbs-next/pcr-pacing.md connects it to `TsInputs`; the unit
// tests exercise all of it.
#[cfg_attr(not(test), allow(dead_code))]
mod pace;
mod phyargs;
mod producer;
mod relay;
mod rx;
mod rxs;
mod rxs3;
mod s3sync;
mod s3tmcc;
mod satpipe;
mod sink;
mod source;
mod stop;
mod symbols;
mod symstream;
mod tlv;
mod tmcc;
#[allow(missing_docs)]
mod ts;
mod tx;
mod txs;

/// ISDB-T (ARIB STD-B31) streamer and TS tools.
#[derive(Parser, Debug)]
#[command(name = "isdb", version, about, long_about = None, after_help = tx::LEGAL_NOTICE)]
struct Cli {
    #[command(flatten)]
    log: logging::LogArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Transmit: per-layer TS (or one TS split by PID) → ISDB-T, or TSs
    /// per TS_ID → ISDB-S (--system isdbs) → IQ file / radio.
    Tx(Box<tx::TxArgs>),
    /// Receive: IQ file / radio → ISDB-T / ISDB-S / ISDB-S3 → TS or TLV streams, with status.
    Rx(rx::RxArgs),
    /// Software loopback TX → channel impairments → RX, verifying delivery
    /// (ISDB-T, or ISDB-S with --system isdbs).
    Loopback(Box<loopback::LoopbackArgs>),
    /// Transport-stream tools.
    Ts(ts::TsArgs),
    /// Service multiplexer.
    Mux(mux::MuxArgs),
    /// Capture raw IQ (receive only): one contiguous block per segment at
    /// any rate the radio supports, with SigMF metadata.
    Capture(capture::CaptureArgs),
    /// IQ recording tools (survey, CPU symbol front end).
    Iq(iq::IqArgs),
    /// TLV stream tools (ISDB-S3 / advanced BS).
    Tlv(tlv::TlvArgs),
    /// Symbols of a single-carrier satellite signal (ISDB-S, ISDB-S3),
    /// receive only: from the radio's FPGA front end when it has one, else
    /// from the CPU front end on one contiguous IQ capture.
    Symbols(symbols::SymbolsArgs),
    /// Shell completions: print the completion script for SHELL to stdout.
    ///
    /// The script is generated from this binary's own command line, so
    /// regenerate it after upgrading `isdb`. It completes subcommands,
    /// options and fixed option values; file names come from the shell.
    Completions {
        /// Shell to generate the script for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn main() -> anyhow::Result<()> {
    // `Cli::parse()`, keeping the matches: `tx` and `loopback` tell the
    // options given on the command line from defaults.
    let matches = Cli::command().get_matches();
    let cli =
        Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.format(&mut Cli::command()).exit());
    let given = |name: &str| txs::Given::of(matches.subcommand_matches(name));
    logging::init(&cli.log);
    match cli.command {
        Command::Tx(a) => {
            stop::install()?;
            tx::run(*a, &given("tx"))
        }
        Command::Rx(a) => {
            stop::install()?;
            rx::run(a)
        }
        Command::Loopback(a) => {
            stop::install()?;
            loopback::run(*a, &given("loopback"))
        }
        Command::Ts(a) => ts::run(a),
        Command::Mux(a) => mux::run(a),
        Command::Capture(a) => {
            stop::install()?;
            capture::run(&a)
        }
        Command::Iq(a) => iq::run(&a),
        Command::Tlv(a) => tlv::run(&a),
        Command::Symbols(a) => {
            stop::install()?;
            symbols::run(&a)
        }
        Command::Completions { shell } => {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "isdb", &mut script);
            // Written here rather than by `generate`, which panics on a
            // write error such as a closed pipe.
            std::io::stdout().lock().write_all(&script)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
