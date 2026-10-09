//! `isdb`: ISDB-T streamer (ARIB STD-B31) and TS tools.
//!
//! * `isdb tx` — transport streams → ISDB-T baseband → IQ file or HackRF
//! * `isdb rx` — IQ file or HackRF → demodulated TS + reception report
//! * `isdb loopback` — software TX → impaired channel → RX, byte-exact check
//! * `isdb ts`, `isdb mux` — TS analysis / service multiplexing
//! * `--relay FILE` (`tx`, `loopback`, `mux`) — rebroadcast a recorded
//!   program without a TOML file
//!
//! The binary adds I/O, threading, pacing, radio control and reporting on
//! top of the library crates (`isdbt-phy`, `isdbt-mux`, `isdb-analyze`,
//! `isdb-hackrf`); it holds no spec logic.  Options, the bench recipe, the
//! thread architecture and the test inventory are in this crate's
//! `README.md`.

use clap::{Parser, Subcommand};

mod channel;
mod input;
mod loopback;
#[allow(missing_docs)]
mod mux;
mod phyargs;
mod producer;
mod radio;
mod relay;
mod rx;
mod sink;
mod source;
mod stop;
#[allow(missing_docs)]
mod ts;
mod tx;

/// ISDB-T (ARIB STD-B31) streamer and TS tools.
#[derive(Parser, Debug)]
#[command(name = "isdb", version, about, long_about = None, after_help = tx::LEGAL_NOTICE)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Transmit: per-layer TS (or one TS split by PID) → ISDB-T → IQ file / HackRF.
    Tx(tx::TxArgs),
    /// Receive: IQ file / HackRF → ISDB-T demodulation → TS, with status.
    Rx(rx::RxArgs),
    /// Software loopback TX → channel impairments → RX, verifying delivery.
    Loopback(loopback::LoopbackArgs),
    /// Transport-stream tools.
    Ts(ts::TsArgs),
    /// Service multiplexer.
    Mux(mux::MuxArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Tx(a) => {
            stop::install()?;
            tx::run(a)
        }
        Command::Rx(a) => {
            stop::install()?;
            rx::run(a)
        }
        Command::Loopback(a) => {
            stop::install()?;
            loopback::run(a)
        }
        Command::Ts(a) => ts::run(a),
        Command::Mux(a) => mux::run(a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
