//! Device-neutral SDR layer: ISDB-T streaming and raw captures.
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
//!   ([`Backend::open_tx`] / [`Backend::open_rx`]). A backend can
//!   also take a [`Capture`] of raw samples ([`Backend::capture`], up to
//!   [`Backend::capture_limit`]) at any rate the device supports, in one
//!   contiguous piece or streamed with its losses reported
//!   ([`CaptureGap`]): the way to record signals of other systems
//!   (ISDB-S, ISDB-S3), in one piece when they are faster than the host
//!   link can stream.
//! * [`Offload`]: a receive stage a radio can run itself ([`Backend::offloads`],
//!   probed without changing anything), so that less data crosses the
//!   link. The first one is the single-carrier front end of
//!   `isdb_frontend` (IQ to `Symbols`), whose output
//!   [`Backend::capture_symbols`] streams; the host runs the same stage on
//!   the CPU when the radio does not offer it.
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
        f64::from(self.rate_num) / f64::from(self.rate_den)
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
        // f64 -> usize saturates: a count of buffers past usize::MAX could
        // not be allocated anyway, and n < 1 (or NaN) becomes 1.
        #[allow(clippy::cast_possible_truncation)]
        let n = if n >= 1.0 { n as usize } else { 1 };
        n
    }
}

/// How the bytes [`Backend::capture`] writes encode the samples:
/// interleaved I, Q components.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureFormat {
    /// Signed 8-bit components; 128 is full scale.
    Cs8,
    /// Signed 16-bit little-endian components.
    Cs16 {
        /// The component value of 1.0: 2048 for 12-bit samples
        /// sign-extended in 16 bits (Pluto through libiio), 32768 for
        /// samples left-aligned in 16 bits (Tezuka's maia-sdr recorder).
        full_scale: f32,
    },
}

impl CaptureFormat {
    /// Bytes per complex sample.
    pub fn bytes_per_sample(&self) -> usize {
        match self {
            Self::Cs8 => 2,
            Self::Cs16 { .. } => 4,
        }
    }
}

/// Samples lost inside one [`Capture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureGap {
    /// Samples written before the gap.
    pub at: u64,
    /// Samples received around the loss but not written, because the
    /// device reports a loss without its exact position: the samples
    /// before `at` and from `at` on are each contiguous, and an unknown
    /// number of samples (at least one) is missing in between.
    pub discarded: u64,
}

/// One capture taken by [`Backend::capture`]: what was written and the
/// settings read back from the device.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    /// How the written bytes encode the samples.
    pub format: CaptureFormat,
    /// Samples written.
    pub samples: u64,
    /// Where samples were lost, in order (empty for a capture taken in one
    /// piece).
    pub gaps: Vec<CaptureGap>,
    /// Sample rate the device ran at, Hz (read back).
    pub sample_rate_hz: f64,
    /// LO frequency read back, Hz.
    pub lo_hz: u64,
    /// Analog bandwidth read back, Hz, if the device reports it.
    pub rf_bandwidth_hz: Option<u64>,
    /// Gain mode and value as the device reported them after the capture,
    /// e.g. `AGC slow_attack, 33 dB at the end` or `manual 30 dB`.
    pub gain: String,
    /// One line identifying the device (model, firmware, serial).
    pub device: String,
    /// How the samples were taken, e.g. `libiio single buffer`.
    pub method: String,
    /// Host time (seconds since the Unix epoch) when the capture was
    /// requested; the first sample follows within the device's set-up
    /// time.
    pub time: f64,
}

/// A receive stage a radio can run itself instead of the host CPU, with
/// the same output bit for bit (more will follow: soft bits, packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Offload {
    /// The single-carrier front end of `isdb_frontend` (IQ to `Symbols`),
    /// format `version`, `taps` taps per phase.
    Symbols {
        /// Format version (`isdb_frontend::params::FORMAT_VERSION`).
        version: u16,
        /// Taps per phase of the device's filter banks.
        taps: usize,
    },
}

/// One stream of `Symbols` words taken by [`Backend::capture_symbols`].
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolsCapture {
    /// 16-bit words written.
    pub words: u64,
    /// Device buffers after which the DMA reported an overflow (the words
    /// carry their own sequence numbers; `isdb_frontend::format::Reader`
    /// finds what was lost).
    pub overflows: u64,
    /// Sample rate the device ran at, Hz (read back).
    pub sample_rate_hz: f64,
    /// LO frequency read back, Hz.
    pub lo_hz: u64,
    /// Analog bandwidth read back, Hz, if the device reports it.
    pub rf_bandwidth_hz: Option<u64>,
    /// Gain mode and value after the capture.
    pub gain: String,
    /// One line identifying the device.
    pub device: String,
    /// How the words were taken and how long the configuration took.
    pub method: String,
    /// Host time (seconds since the Unix epoch) of the request.
    pub time: f64,
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
    /// As [`Backend::describe_rx`], for [`Backend::capture`] (whose
    /// device arguments may differ from streaming ones).
    fn describe_capture(&self, cfg: &RadioConfig) -> Result<String> {
        self.describe_rx(cfg)
    }
    /// The largest number of samples [`Backend::capture`] takes with
    /// `cfg`, with a line saying where the limit comes from ([`u64::MAX`]
    /// for a capture streamed for as long as wanted); a backend that
    /// captures validates `cfg` as [`Backend::describe_capture`] does.
    /// `None` (the default): the backend cannot capture.
    fn capture_limit(&self, cfg: &RadioConfig) -> Result<Option<(u64, String)>> {
        let _ = cfg;
        Ok(None)
    }
    /// Receive `samples` samples and write their bytes to `out` as they
    /// arrive, then release the device. Refuses more than
    /// [`Backend::capture_limit`]. A capture taken in one piece (one DMA
    /// transfer) is contiguous. A streamed one spans `samples` sample
    /// periods: it reports where the device lost samples
    /// ([`Capture::gaps`]), writes fewer samples when it does, and ends
    /// early once `stop` is set.
    fn capture(
        &self,
        cfg: &RadioConfig,
        samples: u64,
        out: &mut dyn std::io::Write,
        stop: &AtomicBool,
    ) -> Result<Capture> {
        let _ = (cfg, samples, out, stop);
        bail!("radio backend {:?} cannot capture", self.name())
    }
    /// The stages the device can run itself with `cfg`, found without
    /// changing its state (default: none).
    fn offloads(&self, cfg: &RadioConfig) -> Result<Vec<Offload>> {
        let _ = cfg;
        Ok(Vec::new())
    }
    /// Run the device's [`Offload::Symbols`] stage with `params` and write
    /// `words` 16-bit `Symbols` words (little-endian) to `out` as they
    /// arrive, or until `stop`. Fails when the device does not offer the
    /// stage with `params`' format and taps.
    fn capture_symbols(
        &self,
        cfg: &RadioConfig,
        params: &isdb_frontend::Params,
        words: u64,
        out: &mut dyn std::io::Write,
        stop: &AtomicBool,
    ) -> Result<SymbolsCapture> {
        let _ = (cfg, params, words, out, stop);
        bail!("radio backend {:?} has no symbol front end", self.name())
    }
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
