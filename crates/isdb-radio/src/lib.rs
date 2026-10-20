//! Device-neutral SDR layer for ISDB-T streaming.
//!
//! The signal crates (`isdbt-phy` and below) produce and consume complex
//! `f32` baseband ([`C32`]) at 512/63 MHz and know nothing about radios.
//! This crate is the seam between that baseband and a device:
//!
//! * [`IqSink`] / [`IqSource`]: where a transmitter sends frames of
//!   samples and where a receiver reads them from. `isdb-cli` implements
//!   them for IQ files; every radio backend implements them for its device.
//! * [`DeviceSpec`]: a device selection `NAME[:key=value[,key=value...]]`
//!   (e.g. `sdr` or `sdr:serial=0000abcd`).
//! * [`RadioConfig`]: what to program into the device (frequency,
//!   rational sample rate, analog bandwidth, device arguments, named gains).
//!   [`TxOptions`] (digital backoff, transmit-queue sizing) and
//!   [`RxOptions`] (receive-queue length) say how to stream.
//! * [`Backend`]: one kind of radio. It validates a configuration without
//!   touching hardware ([`Backend::describe_tx`] / [`Backend::describe_rx`],
//!   which is what a dry run prints) and opens the device
//!   ([`Backend::open_tx`] / [`Backend::open_rx`]).
//! * [`backends`] / [`backend`]: the backends compiled into this build.
//! * [`Quantizer`]: scaling of the unit-RMS OFDM signal into integer
//!   samples (e.g. [`Cs8`]) with a backoff below full scale and a count of
//!   saturated components.
//! * [`QueuedSink`]: the device-neutral transmit path of a radio fed with
//!   buffers of integer samples ([`RadioTx`]): quantisation, prefill
//!   before streaming, back-pressure, stall detection, drain on finish and
//!   underrun counting. A backend implements [`RadioTx`] for its device and
//!   returns a [`QueuedSink`] from [`Backend::open_tx`].
//!
//! # Backends
//!
//! Each backend is behind its own cargo feature; [`backends`] lists the
//! ones compiled in, and [`backend`] names the feature of a known backend
//! that is not. The crate README lists the backends with their device
//! arguments and gains. Gains are passed as `name=value` strings and
//! validated by the backend, so adding a device adds no option to its
//! callers. A build without any backend feature still compiles and runs.
//!
//! # Errors
//!
//! The interface uses [`anyhow::Result`]: errors come from many layers
//! (USB libraries, argument validation, I/O) and are reported to a person,
//! not matched on, as in `isdb-analyze`.
//!
//! # Safety
//!
//! This crate has no RF gate: an opened TX sink radiates whatever it is
//! given. Transmitting on broadcast frequencies needs a licence (in Japan
//! under the Radio Act); use a conducted or shielded setup. `isdb-cli`
//! adds the `--rf-enable` / `--dry-run` gate.
//!
//! # Example
//!
//! ```
//! use isdb_radio::{DeviceSpec, RadioConfig, TxOptions};
//!
//! let spec: DeviceSpec = "sdr:serial=0000abcd".parse()?;
//! assert_eq!((spec.backend.as_str(), spec.get("serial")), ("sdr", Some("0000abcd")));
//! // Unknown here: the error lists the compiled-in backends.
//! assert!(isdb_radio::backend(&spec.backend).is_err());
//!
//! let cfg = RadioConfig {
//!     center_hz: 473_142_857,
//!     nominal_hz: 473_142_857,
//!     rate_num: 512_000_000,
//!     rate_den: 63,
//!     bandwidth_hz: 6_000_000,
//!     args: Vec::new(),
//!     gains: Vec::new(), // every gain at the backend's default
//! };
//! let opts = TxOptions { prefill_frames: 2, frame_seconds: 0.231, backoff_db: 10.0 };
//! // Validation and the settings text need no device.
//! for b in isdb_radio::backends() {
//!     println!("{}:\n{}", b.name(), b.describe_tx(&cfg, &opts)?);
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```

mod backends;
#[cfg(any(test, feature = "mock"))]
pub mod mock;
mod quant;
mod queued;
mod spec;

pub use backends::{backend, backends};
pub use isdbt_ofdm::C32;
pub use quant::{Cs8, Cs12, IntFormat, Quantizer};
pub use queued::{QueuedSink, RadioTx, StreamCounters, TxStream};
pub use spec::{DeviceSpec, parse_key_value};

use anyhow::{Result, bail};
use std::ops::RangeInclusive;
use std::sync::atomic::AtomicBool;

/// Where a transmitter sends the baseband (an IQ file, a radio, …).
pub trait IqSink {
    /// Write one frame of samples, blocking as long as the device needs.
    /// A real-time sink returns early (dropping the samples) once `stop`
    /// is set.
    fn write(&mut self, samples: &[C32], stop: &AtomicBool) -> Result<()>;
    /// Status fragment appended to periodic status lines.
    fn status(&mut self) -> String {
        String::new()
    }
    /// End of transmission; `aborted` = stopped by signal or error (do not
    /// wait for queued samples). Returns a summary line.
    fn finish(&mut self, aborted: bool) -> Result<String>;
    /// True if the sink consumes in real time (it paces the producer).
    fn realtime(&self) -> bool;
}

/// A source of baseband chunks (an IQ file, a radio, …).
pub trait IqSource {
    /// Next chunk; `None` at the end (or once `stop` is set).
    fn read(&mut self, stop: &AtomicBool) -> Result<Option<Vec<C32>>>;
    /// Status fragment appended to periodic status lines.
    fn status(&self) -> String {
        String::new()
    }
    /// Stop the device and return a summary (empty for a plain file).
    fn finish(&mut self) -> Result<String> {
        Ok(String::new())
    }
}

/// What to program into a radio. Built by the caller; validated by the
/// backend ([`Backend::describe_tx`] / [`Backend::describe_rx`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RadioConfig {
    /// Centre frequency programmed into the device in Hz, already corrected
    /// by the caller for the device's reference-clock error.
    pub center_hz: u64,
    /// Nominal centre frequency in Hz (for descriptions).
    pub nominal_hz: u64,
    /// Sample rate numerator: the rate is `rate_num / rate_den` Hz, already
    /// corrected for the reference-clock error (ISDB-T: 512 MHz / 63).
    pub rate_num: u32,
    /// Sample rate denominator.
    pub rate_den: u32,
    /// Analog (baseband filter) bandwidth request in Hz. The backend
    /// chooses how to honour it (its documentation says how) and reports
    /// the bandwidth it programmed in the description returned by
    /// [`Backend::open_tx`] / [`Backend::open_rx`].
    pub bandwidth_hz: u32,
    /// Device arguments ([`DeviceSpec::args`], e.g. `serial`), validated by
    /// the backend.
    pub args: Vec<(String, String)>,
    /// Gains as `(name, value)` strings, validated by the backend; names it
    /// does not know are errors, names not given take its defaults.
    pub gains: Vec<(String, String)>,
}

impl RadioConfig {
    /// The sample rate in Hz.
    pub fn sample_rate_hz(&self) -> f64 {
        self.rate_num as f64 / self.rate_den as f64
    }

    /// One line describing the frequency and the sample clock, for backend
    /// descriptions: `centre 473.142857 MHz, programmed 473142857 Hz;
    /// sample rate 512000000/63 Hz = 8.126984 MHz`.
    pub fn describe_tuning(&self) -> String {
        format!(
            "centre {:.6} MHz, programmed {} Hz; sample rate {}/{} Hz = {:.6} MHz",
            self.nominal_hz as f64 / 1e6,
            self.center_hz,
            self.rate_num,
            self.rate_den,
            self.sample_rate_hz() / 1e6
        )
    }
}

/// How to transmit: digital level and transmit-queue sizing.
///
/// Checked by [`TxOptions::validate`], which [`QueuedSink::new`] and the
/// backends' [`Backend::describe_tx`] / [`Backend::open_tx`] call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TxOptions {
    /// Frames converted before the device starts streaming, so it never
    /// starts on an empty queue (0 counts as 1).
    pub prefill_frames: usize,
    /// Duration of one frame in seconds (queue drain and stall timeouts);
    /// finite and > 0.
    pub frame_seconds: f64,
    /// Digital backoff in dB of the OFDM RMS below integer full scale
    /// ([`Quantizer`]), within [`TxOptions::BACKOFF_DB`]. Backends with
    /// floating-point samples ignore it.
    pub backoff_db: f32,
}

impl TxOptions {
    /// Accepted [`TxOptions::backoff_db`] values in dB.
    pub const BACKOFF_DB: RangeInclusive<f32> = 0.0..=40.0;

    /// Check `frame_seconds` (finite, > 0) and `backoff_db`
    /// ([`TxOptions::BACKOFF_DB`]).
    pub fn validate(&self) -> Result<()> {
        if !(self.frame_seconds.is_finite() && self.frame_seconds > 0.0) {
            bail!(
                "frame duration {} s: must be finite and > 0",
                self.frame_seconds
            );
        }
        if !Self::BACKOFF_DB.contains(&self.backoff_db) {
            bail!(
                "backoff {} dB outside {}..{}",
                self.backoff_db,
                Self::BACKOFF_DB.start(),
                Self::BACKOFF_DB.end()
            );
        }
        Ok(())
    }
}

/// How to receive: the length of the queue between the device and the
/// reader.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RxOptions {
    /// Signal duration in seconds the receive queue holds before the
    /// backend drops buffers (counted as overruns); finite and > 0. The
    /// backend converts it to its own buffer count at the configured
    /// sample rate, rounding up, at least one buffer.
    pub queue_seconds: f64,
}

impl Default for RxOptions {
    /// One second of signal.
    fn default() -> Self {
        Self { queue_seconds: 1.0 }
    }
}

impl RxOptions {
    /// Check `queue_seconds` (finite, > 0).
    pub fn validate(&self) -> Result<()> {
        if !(self.queue_seconds.is_finite() && self.queue_seconds > 0.0) {
            bail!("RX queue {} s: must be finite and > 0", self.queue_seconds);
        }
        Ok(())
    }

    /// The queue length in buffers of `buffer_samples` samples at
    /// `sample_rate_hz`: rounded up, at least 1.
    pub fn buffers(&self, sample_rate_hz: f64, buffer_samples: usize) -> usize {
        let n = (self.queue_seconds * sample_rate_hz / buffer_samples as f64).ceil();
        if n >= 1.0 { n as usize } else { 1 }
    }
}

/// One kind of radio.
///
/// Gains not given in [`RadioConfig::gains`] take the backend's defaults;
/// the TX defaults must be the device's minimum output power, so that a
/// transmission that sets no gains radiates as little as possible.
pub trait Backend: Sync {
    /// The name selecting it in a [`DeviceSpec`].
    fn name(&self) -> &'static str;
    /// Validate `cfg` and `opts` for transmission without opening a device
    /// and return the multi-line settings text (indented lines) shown
    /// before transmitting.
    fn describe_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<String>;
    /// As [`Backend::describe_tx`], for reception.
    fn describe_rx(&self, cfg: &RadioConfig) -> Result<String>;
    /// Open and configure the device for transmission. Returns the sink
    /// (streaming starts after [`TxOptions::prefill_frames`] frames) and a
    /// one-line device description.
    fn open_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<(Box<dyn IqSink>, String)>;
    /// Open and configure the device for reception and start streaming,
    /// queueing up to [`RxOptions::queue_seconds`] of signal before
    /// dropping. Returns the source and a one-line device description.
    fn open_rx(&self, cfg: &RadioConfig, opts: &RxOptions) -> Result<(Box<dyn IqSource>, String)>;
}

impl std::fmt::Debug for dyn Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("name", &self.name())
            .finish()
    }
}

// Compile the README's code blocks as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
