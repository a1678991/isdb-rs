//! ADALM-Pluto family backend (`pluto`): AD936x radios running ADI's Pluto
//! firmware or a derivative such as Tezuka (ADALM-Pluto, PlutoSky R2, …),
//! through `isdb-iio` / libiio.
//!
//! * Device arguments:
//!   * `uri`: libiio context URI, e.g. `ip:192.168.2.1` (USB RNDIS),
//!     `ip:192.0.2.30` (Ethernet), `usb:1.4.5`. Without it, the
//!     `ISDB_PLUTO_URI` environment variable; without that, the device is
//!     found by mDNS when it is opened: the one host on the link answering
//!     for libiio's DNS-SD service `_iio._tcp.local` or for `pluto.local`
//!     becomes `ip:ADDRESS` (none, or several, is an error naming them).
//!     A given `uri` or `ISDB_PLUTO_URI` sends no mDNS query. The query
//!     goes out on the interface of the IPv4 default route, so a Pluto on
//!     USB alone needs `uri=ip:192.168.2.1` (or `usb:…`); the board
//!     answers only while its `avahi-daemon` runs.
//!     The network backend streams over iiod; Gigabit Ethernet keeps up
//!     with 512/63 MHz (32.5 MB/s each way).
//!   * `channel`: `1` or `2` (TX1/RX1 or TX2/RX2 port, default 1); 2 needs
//!     the firmware's `2r2t` mode.
//!   * `buffer`: samples per IIO buffer, 1024..=4194304 in multiples of
//!     1024 (default 262144: 32 ms at 512/63 MHz; the TX symbol codes:
//!     65536 DMA samples of eight codes, 18.2 ms of ISDB-S); 4 such
//!     buffers are queued in the device kernel (`kbuffers`).
//!   * TX only, `loopback`: `on` routes the TX samples digitally back into
//!     the RX path inside the AD936x (its BIST data-port loopback) and
//!     powers the TX LO down, so nothing is radiated; an `isdb rx` on the
//!     same device then receives the transmission. Default `off`. Closing
//!     the TX turns the loopback off again.
//!   * TX, `kbuffers`: kernel buffers on the device, 2..=64 (default 4 for
//!     IQ samples; 16 for the symbol codes: 16 × 18.2 ms = 290 ms of
//!     slack in the device while the host or the link pauses).
//!   * Capture only (`isdb capture`):
//!     * `recorder`: `iio` (default) takes one libiio buffer of up to
//!       63 MiB (the largest iiod allocated on a PlutoSky R2 with Tezuka
//!       v0.3.24); `maia` drives Tezuka's maia-sdr recorder over HTTP
//!       (port 80 of the `ip:` host), up to its 128 MiB DDR buffer, RX1
//!       only. Both are contiguous by construction (one DMA transfer).
//!       `stream` streams libiio buffers for as long as wanted, with
//!       losses reported (below).
//!     * `bits`: `16` (default), `12` (`maia` only) or `8` (RX1 only;
//!       through libiio it needs Tezuka, whose bitstream packs 8-bit I and
//!       Q into one 16-bit word when `voltage0` is the only channel
//!       enabled: `{Q, I}`, each the 12-bit sample / 16 rounded with
//!       first-order error feedback and saturated, full scale 128).
//!       Samples per capture: `iio` 16 515 072 (16 bits) or 33 030 144
//!       (8 bits); `maia` 33 554 432 / 44 739 242 / 67 108 864
//!       (16 / 12 / 8 bits).
//!     * `buffer` (`recorder=stream` and the FPGA symbol stream; not
//!       `maia`): samples (words) per libiio buffer, as for streaming
//!       (default 1048576).
//!     * `kbuffers` (the FPGA symbol stream, `isdb symbols` / `isdb rx`
//!       with the front end): kernel buffers on the device, 2..=64
//!       (default 16 × 1 Mi words = 330 ms of a 50.8 MB/s ISDB-S3 stream).
//!       iiod uses ≈ 72 % of a Pluto core at that rate and the stream stalls
//!       for ≈ 10 ms every ≈ 10.2 s; with 4 buffers that overflowed the DMA
//!       every time (plan §0.18), with 8 or 16 never in 45–60 s.
//!
//!     A streamed capture checks the overflow bit of the ADC core
//!     (`cf-ad9361-lpc` register 0x80000088 bit 2, write 1 to clear)
//!     after every buffer. The bit says that samples were lost since the
//!     last check, not where: the buffers before the check that found it
//!     are contiguous, and the loss lies before the end of the buffers
//!     still queued in the device, in iiod and on the host. So the capture
//!     drops the buffer that found it and as many more as can be queued
//!     ([`CaptureGap::discarded`]), and the samples written after a gap
//!     are contiguous again.
//! * TX gains: `att` 0..89.75 dB of attenuation in 0.25 dB steps (default
//!   89.75, the minimum output power).
//! * RX gains: `agc` `manual|slow_attack|fast_attack|hybrid` (default
//!   `slow_attack`, or `manual` when `gain` is given), `gain` in whole dB
//!   (manual gain; default 30). The device's range depends on the LO
//!   (AD9361: −3..71 dB at 1.3–4 GHz) and is checked when it is opened.
//! * Range: TX 47 MHz..6 GHz, RX 70 MHz..6 GHz (AD9361; an AD9363 with
//!   stock firmware tunes 325 MHz..3.8 GHz, which is found when the LO is
//!   read back), sample rate 2.083334..61.44 MHz, RF bandwidth TX
//!   0.2..40 MHz, RX 0.2..56 MHz.
//! * Sample rate: the AD936x clocks are integer-Hz requests to its BBPLL
//!   divider chain, so a rational rate is approximated: 512/63 MHz becomes
//!   1 040 253 944 Hz / 128 = 8 126 983.94 Hz (−0.02 ppm). The rate the
//!   device runs at is read from `tx_path_rates` / `rx_path_rates` and
//!   reported; more than 10 ppm away from the request is an error.
//! * Samples: TX through a [`QueuedSink`] of [`Cs12`] (12 bits left-aligned
//!   in `i16`, scaled with [`TxOptions::backoff_db`]); RX receives 12-bit
//!   samples sign-extended in `i16`, scaled by 1/2048. RX queues
//!   [`RxOptions::queue_seconds`] of buffers. A capture returns the raw
//!   components (full scale 2048 through libiio, 32768 from maia-sdr,
//!   which left-aligns the 12 bits), 100 ms after programming the RX.
//! * Safety: the TX attenuation is set to its maximum before anything else
//!   is programmed, the DDS tone generator is switched off, and closing the
//!   TX (also on error or panic) stops the stream, sets the maximum
//!   attenuation and powers the TX LO down.
//! * TX symbol codes ([`Backend::open_tx_symbols`]): with a bitstream that
//!   has the FPGA transmit block (`SymbolsTx`, `fpga/`), whose ID word
//!   `isdb_frontend::control::tx` reads in `up_dac_gpio_in` (0x800000B8
//!   of `cf-ad9361-dds-core-lpc`; 0 without the block). The device is
//!   configured as for IQ, then the block: refused when the DAC core runs
//!   in r1_mode (0x80000048 bit 5) or `up_dac_gpio_out` bit 0 (Tezuka's ×8
//!   TX FIR) is set; `Encoder::configure_tx` (STEP, SHIFT, both banks:
//!   13 315 commands, ≈ 8 s through iiod), STEP read back, CTRL
//!   START|SELECT (the block owns the DAC and the DMA, outputs zero and
//!   requests nothing), STATUS must read "selected" (codes sent to a plain
//!   DAC would go out as samples), then the attenuation. Once the stream
//!   runs and its first buffer is in the device, CTRL RUN|SELECT starts
//!   the block (STATUS must read run and selected) and the DAC core's
//!   underflow flag (0x80000088 bit 0, set whenever no DMA word is
//!   available) is cleared; so a clean run misses no symbol. Both
//!   `voltage0` and `voltage1` carry codes (eight per DMA sample). Status
//!   lines add the block's MISSED (one READ per line); the summary its
//!   CONSUMED, MISSED, FIRST_MISS and STATUS and the underflow flag, read
//!   while the zero buffers that end the stream still play. Closing (also
//!   on error or panic) stops the stream, then CTRL 0 (the IQ
//!   pass-through), then the attenuation and the LO as above; opening for
//!   IQ sends CTRL 0 when the bitstream has the block, before the level is
//!   raised.

use super::{maia, mdns};
use crate::{
    Backend, Capture, CaptureFormat, CaptureGap, CodeEncoder, Cs12, Direction, IqSink, IqSource,
    Key, Offload, Quantizer, QueuedSink, RadioConfig, RadioError, RadioTx, Result, Rule, RxOptions,
    StreamCounters, StreamProblem, SymbolSink, SymbolsCapture, Tolerance, TxOptions, TxStream,
};
use isdb_frontend::control::tx::{self as txc, TxIdentity};
use isdb_frontend::control::{self, Encoder, GPIO_IN_REG, GPIO_OUT_REG, Identity, Op, reg};
use isdb_frontend::shape::fixed::{FULL_SCALE, TxParams};
use isdb_frontend::txformat;
use isdb_iio::Context;
use isdbt_ofdm::C32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// The `pluto` backend.
pub(crate) struct Pluto;

const NAME: &str = "pluto";
/// The device's address when `uri` is not given (and not discovered).
const URI_ENV: &str = "ISDB_PLUTO_URI";
/// What mDNS asks for: libiio's DNS-SD service (iiod announces it) and
/// the host name of the ADI firmware.
const MDNS_SERVICE: &str = "_iio._tcp.local";
const MDNS_HOST: &str = "pluto.local";
/// How long mDNS waits for an answer.
const MDNS_WAIT: Duration = Duration::from_secs(2);
const TX_FREQ_HZ: std::ops::RangeInclusive<u64> = 47_000_000..=6_000_000_000;
const RX_FREQ_HZ: std::ops::RangeInclusive<u64> = 70_000_000..=6_000_000_000;
/// AD936x sample rates without the programmable FIR (Hz).
const RATE_HZ: std::ops::RangeInclusive<f64> = 2_083_334.0..=61_440_000.0;
const TX_BANDWIDTH_HZ: std::ops::RangeInclusive<u32> = 200_000..=40_000_000;
const RX_BANDWIDTH_HZ: std::ops::RangeInclusive<u32> = 200_000..=56_000_000;
const BUFFER_SAMPLES: std::ops::RangeInclusive<usize> = 1024..=4_194_304;
const DEFAULT_BUFFER_SAMPLES: usize = 262_144;
/// Buffers queued in the device kernel (libiio's default).
const KERNEL_BUFFERS: u32 = 4;
/// Largest TX attenuation, in quarter dB (89.75 dB).
const MAX_ATT_QDB: u32 = 359;
/// Largest distance of the programmed sample rate from the request.
const MAX_RATE_ERROR_PPM: f64 = 10.0;
/// Largest distance of the LO read back from the request.
const MAX_LO_ERROR_HZ: u64 = 1000;
/// Default manual RX gain in dB.
const DEFAULT_RX_GAIN_DB: i32 = 30;
/// Largest single libiio RX buffer in bytes: larger ones fail with ENOMEM
/// in iiod (16 515 072 samples of 4 bytes, 63 MiB; measured on a PlutoSky
/// R2 with Tezuka v0.3.24, with 1 and 4 kernel blocks alike).
const MAX_IIO_CAPTURE_BYTES: u64 = 66_060_288;
/// Samples per libiio buffer of a streamed capture (`recorder=stream`).
const DEFAULT_STREAM_BUFFER_SAMPLES: usize = 1_048_576;
/// Buffers a streamed capture queues on the host.
const STREAM_DEPTH: usize = 8;
/// Kernel buffers of the symbol stream (`kbuffers`, default), each
/// `buffer` words (1 Mi: 2 MiB, 41 ms of a 50.8 MB/s ISDB-S3 stream):
/// the device's slack while iiod cannot send. iiod spends ≈ 72 % of a
/// Pluto core on that stream.
///
/// What it absorbs (plan §0.20): Tezuka sets the board's TCP congestion
/// control to BBR (`/etc/init.d/S96networkcong`), and BBR's PROBE_RTT
/// phase cuts the in-flight data to 4 packets for ≥ 200 ms every 10 s to
/// re-measure the minimum RTT. iiod's sends block meanwhile, and with
/// 4 buffers (165 ms) the DMA overflowed at the same point of every
/// stream: 10.152 s, symbol 342 704 128, in 5 of 5 runs under BBR, and in
/// none of 2 runs with `cubic` set at runtime. 16 buffers (32 MiB of DMA
/// memory, 660 ms at 50.8 MB/s) ride it out with room for host-side stalls
/// as well; setting `cubic` on the board removes the pause itself.
const SYMBOLS_KERNEL_BUFFERS: u32 = 16;
/// Accepted `kbuffers`.
const SYMBOLS_KERNEL_BUFFERS_RANGE: std::ops::RangeInclusive<u32> = 2..=64;
/// DMA samples per IIO buffer of the TX symbol codes by default: 65 536
/// samples of eight codes (256 KiB, 524 288 symbols, 18.2 ms of ISDB-S),
/// 1.6 superframes, so that the host's queue of superframes always holds
/// a buffer's worth.
const TX_SYMBOLS_BUFFER: usize = 65_536;
/// Kernel buffers of the TX symbol codes by default: 16 × 18.2 ms =
/// 290 ms (4 MiB of DMA memory), the device's slack while the host or the
/// link pauses (the RX symbol stream needed ≥ 8 to ride out a ≈ 200 ms
/// TCP pause, plan §0.20; the code stream's 14.4 MB/s is far below the
/// measured 64 MB/s host → board ceiling).
const TX_SYMBOLS_KERNEL_BUFFERS: u32 = 16;
/// The DAC core's interface status register (ADI `up_dac_common`
/// `REG_STATUS`, 0x0022 × 4, through the HDL-core bit 31) and its
/// underflow bit (sticky, write 1 to clear): set whenever the DAC (or the
/// transmit block) found no DMA word, which includes the idle state
/// between streams (VERIFICATION.md "ISDB-S transmitter in the FPGA: board
/// checks"), so it means something only cleared and read while streaming.
const DAC_STATUS_REG: u32 = 0x8000_0088;
const DAC_STATUS_UNDERFLOW: u32 = 0x1;
/// How long the transmit block waits for the stream's first buffer to
/// reach the device before it is started.
const FIRST_BUFFER_TIMEOUT: Duration = Duration::from_secs(10);
/// The ADC core's interface status register (ADI `up_adc_common`
/// `REG_STATUS`, 0x0022 × 4, through the HDL-core bit 31) and its
/// overflow bit (sticky, write 1 to clear).
const ADC_STATUS_REG: u32 = 0x8000_0088;
const ADC_STATUS_OVERFLOW: u32 = 0x4;
/// `r1_mode` of the AD9361 core's receive side (ADI `up_adc_common` REG
/// 0x11, bit 2) and of its transmit side (`up_dac_common` REG 0x12, bit 5;
/// REG 0x11 bit 2 there is `ext_sync_disarm`). With r1_mode set on both,
/// `util_ad9361_divclk` takes its other input (l_clk / 2 instead of / 4 in
/// LVDS), a case the symbol front end's bitstream is not timed for
/// (fpga/README.md "Clock"), so the FPGA path is refused then.
const ADC_R1_MODE_REG: u32 = 0x8000_0044;
const ADC_R1_MODE_BIT: u32 = 1 << 2;
/// See [`ADC_R1_MODE_REG`].
const DAC_R1_MODE_REG: u32 = 0x8000_0048;
const DAC_R1_MODE_BIT: u32 = 1 << 5;
/// Settling time between programming the RX and capturing (LO lock, AGC).
const CAPTURE_SETTLE: Duration = Duration::from_millis(100);

const PHY: &str = "ad9361-phy";
const TX_DEV: &str = "cf-ad9361-dds-core-lpc";
const RX_DEV: &str = "cf-ad9361-lpc";
const RX_LO: &str = "altvoltage0";
const TX_LO: &str = "altvoltage1";

/// RX gain control modes of the AD936x driver.
const AGC_MODES: [&str; 4] = ["manual", "slow_attack", "fast_attack", "hybrid"];

/// A libiio failure, as a [`RadioError::Device`] doing `op`.
trait At<T> {
    fn at(self, op: &str) -> Result<T>;
}

impl<T> At<T> for isdb_iio::Result<T> {
    fn at(self, op: &str) -> Result<T> {
        self.map_err(|e| RadioError::device(NAME, op, e))
    }
}

/// A device argument value this backend refuses.
fn bad_arg(key: &str, value: &str, rule: Rule) -> RadioError {
    RadioError::Argument {
        backend: NAME,
        kind: Key::Argument,
        key: key.to_string(),
        value: value.to_string(),
        rule,
    }
}

/// A gain value of direction `dir` this backend refuses.
fn bad_gain(dir: Direction, key: &str, value: &str, rule: Rule) -> RadioError {
    RadioError::Argument {
        backend: NAME,
        kind: Key::Gain(dir),
        key: key.to_string(),
        value: value.to_string(),
        rule,
    }
}

/// Two device arguments or gains that do not go together.
fn conflict(first: impl Into<String>, rule: &'static str, second: impl Into<String>) -> RadioError {
    RadioError::Conflict {
        backend: NAME,
        first: first.into(),
        rule,
        second: second.into(),
    }
}

/// A reply of the device this backend cannot use.
fn reply(what: impl Into<String>, detail: impl Into<String>) -> RadioError {
    RadioError::Reply {
        backend: NAME,
        what: what.into(),
        detail: detail.into(),
    }
}

/// The frequency of the LO channel `lo` read back, in Hz (never
/// negative).
fn read_lo(lo: &isdb_iio::Channel<'_>, op: &str) -> Result<u64> {
    let v = lo.read_i64("frequency").at(op)?;
    u64::try_from(v).map_err(|_| reply("LO frequency", format!("{v} Hz read back")))
}

/// What the device is opened for (which device arguments apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Tx,
    Rx,
    Capture,
}

/// How a capture takes its samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorder {
    /// One libiio buffer, refilled once.
    Iio,
    /// Tezuka's maia-sdr recorder over its HTTP API.
    Maia,
    /// libiio buffers streamed for as long as wanted.
    Stream,
}

/// Device arguments.
#[derive(Debug, Clone, PartialEq)]
struct DeviceArgs {
    /// The libiio URI: given (`uri`), or `None` until
    /// [`DeviceArgs::resolve`] takes `ISDB_PLUTO_URI` or finds the device.
    uri: Option<String>,
    /// 1 or 2.
    channel: u8,
    buffer: usize,
    /// `buffer` was given.
    buffer_given: bool,
    /// Kernel buffers (`kbuffers`) of the RX symbol stream or the TX;
    /// `None`: each one's default.
    kernel_buffers: Option<u32>,
    loopback: bool,
    recorder: Recorder,
    /// Bits per component of a capture: 8, 12 or 16.
    bits: u8,
}

impl DeviceArgs {
    /// The URI to open (after [`DeviceArgs::resolve`]).
    fn uri(&self) -> &str {
        self.uri.as_deref().unwrap_or("(not resolved)")
    }

    /// Where the URI comes from, for a description before the device is
    /// opened.
    fn uri_source(&self) -> String {
        match (&self.uri, env_uri()) {
            (Some(u), _) => u.clone(),
            (None, Some(u)) => format!("{u} ({URI_ENV})"),
            (None, None) => format!("found by mDNS ({MDNS_SERVICE} or {MDNS_HOST}) when opened"),
        }
    }

    /// Settle the URI: the given `uri`, else `ISDB_PLUTO_URI`, else the
    /// one device answering mDNS. Only the last sends anything.
    fn resolve(&mut self) -> Result<()> {
        if self.uri.is_some() {
            return Ok(());
        }
        if let Some(u) = env_uri() {
            debug!(backend = NAME, uri = u, "Pluto URI from {URI_ENV}");
            self.uri = Some(u);
            return Ok(());
        }
        let query = format!("{MDNS_SERVICE} or {MDNS_HOST}");
        let found = mdns::lookup(MDNS_SERVICE, MDNS_HOST, MDNS_WAIT)
            .map_err(|e| RadioError::device(NAME, format!("mDNS query for {query}"), e))?;
        let [one] = found.as_slice() else {
            return Err(RadioError::Discovery {
                backend: NAME,
                query,
                found: found
                    .iter()
                    .map(|a| format!("{} ({})", a.name, a.addr))
                    .collect(),
                hint: "give the device with uri=ip:ADDRESS (or usb:…) or ISDB_PLUTO_URI; a board answers mDNS only while its avahi-daemon runs",
            });
        };
        info!(
            backend = NAME,
            name = one.name,
            addr = %one.addr,
            "Pluto found by mDNS: {} at {}",
            one.name,
            one.addr
        );
        self.uri = Some(format!("ip:{}", one.addr));
        Ok(())
    }

    /// The phy channel of the selected port: `voltage0` / `voltage1`.
    fn phy_channel(&self) -> &'static str {
        ["voltage0", "voltage1"][usize::from(self.channel - 1)]
    }

    /// The streaming channels (I, Q) of the selected port.
    fn stream_channels(&self) -> [&'static str; 2] {
        [["voltage0", "voltage1"], ["voltage2", "voltage3"]][usize::from(self.channel - 1)]
    }

    /// The channels a capture enables: `voltage0` alone selects Tezuka's
    /// 8-bit packing.
    fn capture_channels(&self) -> &'static [&'static str] {
        match (self.bits, self.channel) {
            (8, _) => &["voltage0"],
            (_, 1) => &["voltage0", "voltage1"],
            _ => &["voltage2", "voltage3"],
        }
    }
}

fn on_off(k: &str, v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Ok(true),
        "off" | "0" | "false" | "no" => Ok(false),
        _ => Err(bad_arg(k, v, Rule::OneOf(&["on", "off"]))),
    }
}

/// Parse the device arguments; `loopback` only for TX, `recorder` only
/// for captures (which have no streaming `buffer`).
fn device_args(cfg: &RadioConfig, dir: Dir) -> Result<DeviceArgs> {
    let mut a = DeviceArgs {
        uri: None,
        channel: 1,
        buffer: DEFAULT_BUFFER_SAMPLES,
        buffer_given: false,
        kernel_buffers: None,
        loopback: false,
        recorder: Recorder::Iio,
        bits: 16,
    };
    let known: &[&str] = match dir {
        Dir::Tx => &["uri", "channel", "buffer", "kbuffers", "loopback"],
        Dir::Rx => &["uri", "channel", "buffer"],
        Dir::Capture => &[
            "uri",
            "channel",
            "recorder",
            "bits",
            "buffer (recorder=stream and the symbol stream)",
            "kbuffers (the symbol stream)",
        ],
    };
    for (k, v) in &cfg.args {
        match k.as_str() {
            "uri" if v.is_empty() => return Err(bad_arg(k, v, Rule::Empty)),
            "uri" => a.uri = Some(v.clone()),
            "channel" => {
                a.channel = match v.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => return Err(bad_arg(k, v, Rule::OneOf(&["1", "2"]))),
                }
            }
            "buffer" => {
                let n: usize = v.parse().map_err(|_| {
                    bad_arg(
                        k,
                        v,
                        Rule::NotANumber {
                            what: "number of samples",
                        },
                    )
                })?;
                if !BUFFER_SAMPLES.contains(&n) || !n.is_multiple_of(1024) {
                    return Err(bad_arg(
                        k,
                        v,
                        Rule::Multiple {
                            min: *BUFFER_SAMPLES.start() as u64,
                            max: *BUFFER_SAMPLES.end() as u64,
                            of: 1024,
                            unit: "samples",
                        },
                    ));
                }
                a.buffer = n;
                a.buffer_given = true;
            }
            "kbuffers" if dir != Dir::Rx => {
                let n: u32 = v
                    .parse()
                    .map_err(|_| bad_arg(k, v, Rule::NotANumber { what: "number" }))?;
                if !SYMBOLS_KERNEL_BUFFERS_RANGE.contains(&n) {
                    return Err(bad_arg(
                        k,
                        v,
                        Rule::Range {
                            min: f64::from(*SYMBOLS_KERNEL_BUFFERS_RANGE.start()),
                            max: f64::from(*SYMBOLS_KERNEL_BUFFERS_RANGE.end()),
                            unit: "",
                        },
                    ));
                }
                a.kernel_buffers = Some(n);
            }
            "loopback" if dir == Dir::Tx => a.loopback = on_off(k, v)?,
            "recorder" if dir == Dir::Capture => {
                a.recorder = match v.as_str() {
                    "iio" => Recorder::Iio,
                    "maia" => Recorder::Maia,
                    "stream" => Recorder::Stream,
                    _ => return Err(bad_arg(k, v, Rule::OneOf(&["iio", "maia", "stream"]))),
                }
            }
            "bits" if dir == Dir::Capture => {
                a.bits = match v.as_str() {
                    "8" => 8,
                    "12" => 12,
                    "16" => 16,
                    _ => return Err(bad_arg(k, v, Rule::OneOf(&["8", "12", "16"]))),
                }
            }
            _ => {
                return Err(RadioError::UnknownKey {
                    backend: NAME,
                    kind: Key::Argument,
                    key: k.clone(),
                    known: known.iter().map(|k| (*k).to_string()).collect(),
                });
            }
        }
    }
    if dir == Dir::Capture {
        if a.buffer_given && a.recorder == Recorder::Maia {
            return Err(conflict(
                "buffer",
                "applies to recorder=stream and the symbol stream, not",
                "recorder=maia",
            ));
        }
        if !a.buffer_given {
            a.buffer = DEFAULT_STREAM_BUFFER_SAMPLES;
        }
        if a.bits == 12 && a.recorder != Recorder::Maia {
            return Err(conflict("bits=12", "needs", "recorder=maia"));
        }
        if (a.bits == 8 || a.recorder == Recorder::Maia) && a.channel != 1 {
            return Err(conflict(
                if a.recorder == Recorder::Maia {
                    "recorder=maia"
                } else {
                    "bits=8"
                },
                "records RX1 only, not",
                format!("channel={}", a.channel),
            ));
        }
    }
    Ok(a)
}

/// What a capture with `dev` writes and how many samples it can take.
fn capture_plan(dev: &DeviceArgs) -> (CaptureFormat, u64, String) {
    let format = match dev.bits {
        8 => CaptureFormat::Cs8,
        _ => CaptureFormat::Cs16 { full_scale: 2048.0 },
    };
    let bytes = format.bytes_per_sample() as u64;
    match dev.recorder {
        Recorder::Iio => (
            format,
            MAX_IIO_CAPTURE_BYTES / bytes,
            "one libiio buffer (63 MiB, the largest iiod allocated on a PlutoSky R2 with Tezuka v0.3.24; recorder=maia takes 2x)".into(),
        ),
        Recorder::Maia => {
            let m = maia::Mode(dev.bits);
            (
                m.format(),
                m.max_samples(),
                format!(
                    "the maia-sdr recorder buffer ({} MiB in DDR, {})",
                    maia::BUFFER_BYTES >> 20,
                    m.describe()
                ),
            )
        }
        Recorder::Stream => (
            format,
            u64::MAX,
            "streamed libiio buffers, losses reported".into(),
        ),
    }
}

/// The tuning, sample rate and bandwidth ranges.
fn check_tuning(
    cfg: &RadioConfig,
    freq: &std::ops::RangeInclusive<u64>,
    bw: &std::ops::RangeInclusive<u32>,
) -> Result<()> {
    // u64 -> f64: frequencies below 2^53 Hz are exact.
    #[allow(clippy::cast_precision_loss)]
    let hz = |f: u64| f as f64;
    if !freq.contains(&cfg.center_hz) {
        return Err(RadioError::Tuning {
            backend: NAME,
            device: "Pluto (AD9361)",
            what: "frequency",
            hz: hz(cfg.center_hz),
            min_hz: Some(hz(*freq.start())),
            max_hz: hz(*freq.end()),
        });
    }
    if cfg.rate_den == 0 || !RATE_HZ.contains(&cfg.sample_rate_hz()) {
        return Err(rate_error(cfg));
    }
    if !bw.contains(&cfg.bandwidth_hz) {
        return Err(RadioError::Tuning {
            backend: NAME,
            device: "Pluto",
            what: "RF bandwidth",
            hz: f64::from(cfg.bandwidth_hz),
            min_hz: Some(f64::from(*bw.start())),
            max_hz: f64::from(*bw.end()),
        });
    }
    Ok(())
}

/// The sample rate of `cfg` is outside [`RATE_HZ`].
fn rate_error(cfg: &RadioConfig) -> RadioError {
    RadioError::Tuning {
        backend: NAME,
        device: "Pluto (AD9361)",
        what: "sample rate",
        hz: cfg.sample_rate_hz(),
        min_hz: Some(*RATE_HZ.start()),
        max_hz: *RATE_HZ.end(),
    }
}

/// Reject repeated and unknown gain names (`known` with their help text).
fn check_gain_names(cfg: &RadioConfig, dir: Direction, known: &[(&str, String)]) -> Result<()> {
    for (i, (k, _)) in cfg.gains.iter().enumerate() {
        if !known.iter().any(|(n, _)| n == k) {
            return Err(RadioError::UnknownKey {
                backend: NAME,
                kind: Key::Gain(dir),
                key: k.clone(),
                known: known.iter().map(|(_, h)| h.clone()).collect(),
            });
        }
        if cfg.gains[..i].iter().any(|(p, _)| p == k) {
            return Err(RadioError::RepeatedKey {
                backend: NAME,
                kind: Key::Gain(dir),
                key: k.clone(),
            });
        }
    }
    Ok(())
}

fn gain<'a>(cfg: &'a RadioConfig, name: &str) -> Option<&'a str> {
    cfg.gains
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Validated TX settings.
#[derive(Debug, Clone, PartialEq)]
struct TxSettings {
    dev: DeviceArgs,
    /// Attenuation in quarter dB.
    att_qdb: u32,
}

impl TxSettings {
    fn att_db(&self) -> f64 {
        f64::from(self.att_qdb) / 4.0
    }

    /// `TX atten     89.75 dB (maximum)` and, with `loopback`, its line.
    fn describe_level(&self) -> String {
        let mut d = format!(
            "\n  TX atten     {} dB{}",
            self.att_db(),
            if self.att_qdb == MAX_ATT_QDB {
                " (maximum)"
            } else {
                ""
            }
        );
        if self.dev.loopback {
            d += "\n  loopback     digital TX→RX inside the AD936x, TX LO powered down (nothing radiated)";
        }
        d
    }
}

/// The TX settings of the symbol codes: [`tx_settings`] on TX1 only (the
/// transmit block drives the TX1 data path; `channel=2` is refused), with
/// the code stream's buffer and kernel-buffer defaults.
fn tx_symbols_settings(cfg: &RadioConfig) -> Result<TxSettings> {
    let mut s = tx_settings(cfg)?;
    if s.dev.channel != 1 {
        return Err(conflict(
            format!("channel={}", s.dev.channel),
            "is not wired to the FPGA transmit block, which drives",
            "TX1 (channel=1)",
        ));
    }
    if !s.dev.buffer_given {
        s.dev.buffer = TX_SYMBOLS_BUFFER;
    }
    s.dev.kernel_buffers = Some(s.dev.kernel_buffers.unwrap_or(TX_SYMBOLS_KERNEL_BUFFERS));
    Ok(s)
}

/// The symbol rate a block with `p` sends at the sample rate of `cfg`.
fn tx_symbol_rate(cfg: &RadioConfig, p: &TxParams) -> f64 {
    cfg.sample_rate_hz() / p.sps()
}

/// The settings text of the symbol codes (what a dry run prints): the
/// device, tuning and level as for IQ, then the block, the link and the
/// digital level.
fn describe_tx_symbols(
    cfg: &RadioConfig,
    s: &TxSettings,
    opts: &TxOptions,
    p: &TxParams,
) -> String {
    let rs = tx_symbol_rate(cfg, p);
    let kbuffers = s.dev.kernel_buffers.unwrap_or(TX_SYMBOLS_KERNEL_BUFFERS);
    let buffer_s = s.dev.buffer as f64 * txformat::CODES_PER_WORD as f64 / rs;
    let commands = Encoder::new(0).configure_tx(p).len();
    format!(
        "{}{}\n  modulator    FPGA transmit block of {TX_DEV} (SymbolsTx, TX format {}): symbol codes shaped on the board, banks of {} phases × {} taps, {:.4} samples/symbol (step {:#x})\n  link         4-bit symbol codes, {} per DMA sample (voltage0 + voltage1): {:.2} MB/s at {:.3} Mbaud; buffers of {} samples ({:.1} ms), kbuffers {kbuffers} in the kernel ({:.0} ms)\n  level        RMS {:.1} dB below the 12-bit DAC full scale ({:.1} LSB), in the block's banks (shift {}); the TX attenuation sets the output power\n  configure    {commands} commands through gpio_out at open (≈ {:.0} s at 0.62 ms each)",
        describe_device(&s.dev, "TX", cfg),
        s.describe_level(),
        txformat::FORMAT_VERSION,
        p.phases(),
        p.taps(),
        p.sps(),
        p.step,
        txformat::CODES_PER_WORD,
        txformat::link_rate(rs) / 1e6,
        rs / 1e6,
        s.dev.buffer,
        buffer_s * 1e3,
        f64::from(kbuffers) * buffer_s * 1e3,
        opts.backoff_db,
        FULL_SCALE * 10f64.powf(-f64::from(opts.backoff_db) / 20.0),
        p.shift,
        commands as f64 * 0.62e-3,
    )
}

/// Validated RX settings.
#[derive(Debug, Clone, PartialEq)]
struct RxSettings {
    dev: DeviceArgs,
    /// One of [`AGC_MODES`].
    agc: &'static str,
    /// Manual gain in dB (used with `agc = manual`).
    gain_db: i32,
}

fn tx_settings(cfg: &RadioConfig) -> Result<TxSettings> {
    let dev = device_args(cfg, Dir::Tx)?;
    check_tuning(cfg, &TX_FREQ_HZ, &TX_BANDWIDTH_HZ)?;
    check_gain_names(
        cfg,
        Direction::Tx,
        &[(
            "att",
            "att=0..89.75 dB of attenuation in 0.25 dB steps (default 89.75)".into(),
        )],
    )?;
    let att_qdb = match gain(cfg, "att") {
        None => MAX_ATT_QDB,
        Some(v) => {
            let refuse = |rule| bad_gain(Direction::Tx, "att", v, rule);
            let db: f64 = v.parse().map_err(|_| {
                refuse(Rule::NotANumber {
                    what: "number of dB",
                })
            })?;
            // The quarter-dB step equal to db * 4, if there is one.
            let q = db * 4.0;
            let Some(q) = (0..=MAX_ATT_QDB).find(|&n| f64::from(n) == q) else {
                return Err(refuse(Rule::Steps {
                    min: 0.0,
                    max: f64::from(MAX_ATT_QDB) / 4.0,
                    step: 0.25,
                    unit: "dB",
                }));
            };
            q
        }
    };
    Ok(TxSettings { dev, att_qdb })
}

fn rx_settings(cfg: &RadioConfig) -> Result<RxSettings> {
    rx_settings_for(cfg, Dir::Rx)
}

fn rx_settings_for(cfg: &RadioConfig, dir: Dir) -> Result<RxSettings> {
    let dev = device_args(cfg, dir)?;
    check_tuning(cfg, &RX_FREQ_HZ, &RX_BANDWIDTH_HZ)?;
    check_gain_names(
        cfg,
        Direction::Rx,
        &[
            (
                "agc",
                "agc=manual|slow_attack|fast_attack|hybrid (default slow_attack, manual with gain)"
                    .into(),
            ),
            (
                "gain",
                format!("gain=dB (manual gain, default {DEFAULT_RX_GAIN_DB})"),
            ),
        ],
    )?;
    let gain_db = match gain(cfg, "gain") {
        None => None,
        Some(v) => {
            let refuse = |rule| bad_gain(Direction::Rx, "gain", v, rule);
            let g = v.parse::<i32>().map_err(|_| {
                refuse(Rule::NotANumber {
                    what: "whole number of dB",
                })
            })?;
            if !(-10..=77).contains(&g) {
                return Err(refuse(Rule::Range {
                    min: -10.0,
                    max: 77.0,
                    unit: "dB",
                }));
            }
            Some(g)
        }
    };
    let agc = match gain(cfg, "agc") {
        None if gain_db.is_some() => "manual",
        None => "slow_attack",
        Some(v) => match AGC_MODES.iter().find(|m| **m == v) {
            Some(m) => *m,
            None => return Err(bad_gain(Direction::Rx, "agc", v, Rule::OneOf(&AGC_MODES))),
        },
    };
    if gain_db.is_some() && agc != "manual" {
        return Err(conflict(
            "gain",
            "is the manual gain; it cannot be combined with",
            format!("agc={agc}"),
        ));
    }
    Ok(RxSettings {
        dev,
        agc,
        gain_db: gain_db.unwrap_or(DEFAULT_RX_GAIN_DB),
    })
}

fn describe_device(dev: &DeviceArgs, dir: &str, cfg: &RadioConfig) -> String {
    format!(
        "  device       Pluto {}, {dir}{}\n  tuning       {}\n  rf bandwidth {:.2} MHz requested",
        dev.uri_source(),
        dev.channel,
        cfg.describe_tuning(),
        f64::from(cfg.bandwidth_hz) / 1e6,
    )
}

impl Backend for Pluto {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<String> {
        let s = tx_settings(cfg)?;
        opts.validate()?;
        Ok(format!(
            "{}{}\n  backoff      {}",
            describe_device(&s.dev, "TX", cfg),
            s.describe_level(),
            Quantizer::<Cs12>::new(opts.backoff_db).describe_level()
        ))
    }

    fn describe_rx(&self, cfg: &RadioConfig) -> Result<String> {
        let s = rx_settings(cfg)?;
        Ok(format!(
            "{}\n  RX gain      {}",
            describe_device(&s.dev, "RX", cfg),
            describe_rx_gain(&s)
        ))
    }

    fn open_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<(Box<dyn IqSink>, String)> {
        let mut s = tx_settings(cfg)?;
        opts.validate()?;
        let ctx = open(&mut s.dev)?;
        let mut info = device_info(&ctx);
        let tx = PlutoTx {
            ctx,
            channels: s.dev.stream_channels(),
            phy_channel: s.dev.phy_channel(),
            buffer: s.dev.buffer,
            kernel_buffers: s.dev.kernel_buffers.unwrap_or(KERNEL_BUFFERS),
            loopback: s.dev.loopback,
            block: None,
        };
        // From here on, dropping `tx` restores the safe state. The
        // pass-through once the attenuation is at its maximum and the DDS
        // off, so that whatever the DAC then takes cannot go out loud.
        tx.program(cfg, &s, &mut info)?;
        tx.pass_through()?;
        tx.set_level(&s, &mut info)?;
        info += &format!(
            "; buffers of {} samples, {} in the kernel",
            tx.buffer, tx.kernel_buffers
        );
        let gain = format!("TX{} attenuation {} dB", s.dev.channel, s.att_db());
        info!(
            backend = NAME,
            uri = s.dev.uri(),
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            loopback = s.dev.loopback,
            "Pluto {} opened for TX: {}; {gain}{}",
            s.dev.uri(),
            cfg.describe_tuning(),
            if s.dev.loopback {
                "; digital TX→RX loopback"
            } else {
                ""
            }
        );
        debug!(backend = NAME, "{info}");
        Ok((Box::new(QueuedSink::new(tx, opts)?), info))
    }

    fn describe_capture(&self, cfg: &RadioConfig) -> Result<String> {
        let s = rx_settings_for(cfg, Dir::Capture)?;
        let (format, _, _) = capture_plan(&s.dev);
        Ok(format!(
            "{}\n  RX gain      {}\n  recorder     {}\n  samples      {}",
            describe_device(&s.dev, "RX", cfg),
            describe_rx_gain(&s),
            match s.dev.recorder {
                Recorder::Iio => "libiio single buffer".to_string(),
                Recorder::Maia => match s.dev.uri.as_deref() {
                    Some(u) => format!(
                        "maia-sdr recorder at http://{}/",
                        u.strip_prefix("ip:").unwrap_or("?")
                    ),
                    None => "maia-sdr recorder on port 80 of the device's host".to_string(),
                },
                Recorder::Stream => format!(
                    "libiio stream, buffers of {} samples, {KERNEL_BUFFERS} in the kernel, ADC overflow bit checked after each",
                    s.dev.buffer
                ),
            },
            match (format, s.dev.recorder) {
                (CaptureFormat::Cs8, Recorder::Maia) => "cs8 (maia-sdr IQ8bit)",
                (CaptureFormat::Cs8, _) => "cs8 (Tezuka 8-bit packing)",
                (_, Recorder::Maia) if s.dev.bits == 12 =>
                    "cs16, 12 bits sign-extended (maia-sdr IQ12bit)",
                (_, Recorder::Maia) => "cs16, 12 bits left-aligned (maia-sdr IQ16bit)",
                _ => "cs16, 12 bits sign-extended",
            }
        ))
    }

    fn capture_limit(&self, cfg: &RadioConfig) -> Result<Option<(u64, String)>> {
        let s = rx_settings_for(cfg, Dir::Capture)?;
        let (_, limit, why) = capture_plan(&s.dev);
        Ok(Some((limit, why)))
    }

    fn capture(
        &self,
        cfg: &RadioConfig,
        samples: u64,
        out: &mut dyn std::io::Write,
        stop: &AtomicBool,
    ) -> Result<Capture> {
        let mut s = rx_settings_for(cfg, Dir::Capture)?;
        let (format, limit, why) = capture_plan(&s.dev);
        let too_long = || RadioError::CaptureLength {
            backend: NAME,
            samples,
            limit,
            why: why.clone(),
        };
        if samples == 0 || samples > limit {
            return Err(too_long());
        }
        s.dev.resolve()?;
        let host = match s.dev.recorder {
            Recorder::Maia => Some(
                s.dev
                    .uri()
                    .strip_prefix("ip:")
                    .map(str::to_string)
                    .ok_or_else(|| {
                        conflict(
                            "recorder=maia",
                            "needs a network uri (ip:HOST), not",
                            format!("uri={}", s.dev.uri()),
                        )
                    })?,
            ),
            _ => None,
        };
        let mut ctx = open(&mut s.dev)?;
        let device = device_info(&ctx);
        if s.dev.bits == 8
            && s.dev.recorder != Recorder::Maia
            && !ctx
                .attr("fw_version")
                .is_some_and(|v| v.starts_with("tezuka"))
        {
            return Err(RadioError::Firmware {
                backend: NAME,
                firmware: ctx.attr("fw_version").unwrap_or_else(|| "?".into()),
                need: "bits=8 through libiio needs Tezuka's 8-bit packing; use recorder=maia or bits=16",
            });
        }
        let mut info = String::new();
        configure_rx(&ctx, cfg, &s, &mut info)?;
        std::thread::sleep(CAPTURE_SETTLE);
        let r1 = |dev: &str, reg: u32, bit: u32| -> Result<bool> {
            let mode = ctx
                .device(dev)
                .and_then(|d| d.reg_read(reg))
                .at("read the AD9361 core mode")?;
            Ok(mode & bit != 0)
        };
        if r1(RX_DEV, ADC_R1_MODE_REG, ADC_R1_MODE_BIT)?
            && r1(TX_DEV, DAC_R1_MODE_REG, DAC_R1_MODE_BIT)?
        {
            return Err(RadioError::Unsupported {
                backend: NAME,
                what: "runs its AD9361 core in r1_mode (one channel per direction): the symbol front end's clock is then l_clk / 2, which its bitstream is not timed for; use the CPU front end (--offload none)",
            });
        }
        let rate = rx_rate(&ctx)?;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri(),
            rate_hz = rate,
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            samples,
            recorder = ?s.dev.recorder,
            bits = s.dev.bits,
            "Pluto {} opened for a capture of {samples} samples: {}; RX{} gain {gain}; {:?} recorder, {} bits per component",
            s.dev.uri(),
            cfg.describe_tuning(),
            s.dev.channel,
            s.dev.recorder,
            s.dev.bits
        );
        debug!(backend = NAME, "{device}{info}");
        let (written, gaps, method) = match (s.dev.recorder, host) {
            (Recorder::Maia, Some(h)) => {
                let m = maia::Mode(s.dev.bits);
                maia::record(&h, rate, samples, m, out)?;
                (
                    samples,
                    Vec::new(),
                    format!("maia-sdr recorder {}", m.describe()),
                )
            }
            (Recorder::Stream, _) => {
                let (n, gaps) = stream_capture(&mut ctx, &s.dev, samples, format, out, stop)?;
                (
                    n,
                    gaps,
                    format!(
                        "libiio stream (buffers of {} samples, {KERNEL_BUFFERS} kernel blocks; ADC overflow bit checked after every buffer)",
                        s.dev.buffer
                    ),
                )
            }
            _ => {
                let n = usize::try_from(samples).map_err(|_| too_long())?;
                let bytes = ctx
                    .capture(RX_DEV, s.dev.capture_channels(), n, 1)
                    .at(&format!("capture {samples} samples in one libiio buffer"))?;
                out.write_all(&bytes[..n * format.bytes_per_sample()])
                    .map_err(|e| RadioError::io("write the capture", e))?;
                (samples, Vec::new(), "libiio single buffer".to_string())
            }
        };
        let read_back = "read back the RX settings";
        let phy = ctx.device(PHY).at(read_back)?;
        let ch = phy.channel(s.dev.phy_channel(), false).at(read_back)?;
        let gain_db = ch.read_f64("hardwaregain").at(read_back)?;
        let gain = match s.agc {
            "manual" => format!("manual {gain_db} dB"),
            m => format!("AGC {m}, {gain_db} dB at the end"),
        };
        if !gaps.is_empty() {
            warn!(
                backend = NAME,
                gaps = gaps.len(),
                discarded = gaps.iter().map(|g| g.discarded).sum::<u64>(),
                "capture has {} gap(s): {} samples discarded around ADC overflows",
                gaps.len(),
                gaps.iter().map(|g| g.discarded).sum::<u64>()
            );
        }
        debug!(
            backend = NAME,
            samples = written,
            gain,
            method,
            "capture done: {written} samples by {method}; gain {gain}"
        );
        Ok(Capture {
            format,
            samples: written,
            gaps,
            sample_rate_hz: rate,
            lo_hz: read_lo(&phy.channel(RX_LO, true).at(read_back)?, read_back)?,
            rf_bandwidth_hz: u64::try_from(ch.read_i64("rf_bandwidth").at(read_back)?).ok(),
            gain,
            device,
            method,
            time,
        })
    }

    fn offloads(&self, cfg: &RadioConfig) -> Result<Vec<Offload>> {
        let mut s = rx_settings_for(cfg, Dir::Capture)?;
        let ctx = open(&mut s.dev)?;
        Ok(probe_front_end(&ctx)?
            .map(|id| Offload::Symbols {
                version: id.version,
                taps: id.taps,
            })
            .into_iter()
            .collect())
    }

    fn tx_offloads(&self, cfg: &RadioConfig) -> Result<Vec<Offload>> {
        let mut s = tx_settings(cfg)?;
        let ctx = open(&mut s.dev)?;
        let dac = ctx.device(TX_DEV).at("probe the FPGA transmit block")?;
        Ok(TxBlock::probe(&dac)?
            .map(|id| Offload::TxSymbols {
                version: id.version,
                phases: id.phases(),
                taps: id.taps,
            })
            .into_iter()
            .collect())
    }

    fn describe_tx_symbols(
        &self,
        cfg: &RadioConfig,
        opts: &TxOptions,
        params: &TxParams,
    ) -> Result<String> {
        let s = tx_symbols_settings(cfg)?;
        opts.validate()?;
        params.validate()?;
        Ok(describe_tx_symbols(cfg, &s, opts, params))
    }

    fn open_tx_symbols(
        &self,
        cfg: &RadioConfig,
        opts: &TxOptions,
        params: &TxParams,
    ) -> Result<(Box<dyn SymbolSink>, String)> {
        let mut s = tx_symbols_settings(cfg)?;
        opts.validate()?;
        params.validate()?;
        let ctx = open(&mut s.dev)?;
        let mut info = device_info(&ctx);
        // The block first, before anything is programmed: one read.
        let need = TxIdentity::of(params);
        let found = {
            let dac = ctx.device(TX_DEV).at("probe the FPGA transmit block")?;
            TxBlock::probe(&dac)?
        };
        let geometry = |id: TxIdentity| (id.version, id.phases(), id.taps);
        match found {
            Some(id) if id.supports(params) => {}
            other => {
                return Err(RadioError::Modulator {
                    backend: NAME,
                    found: other.map(geometry),
                    need: geometry(need),
                });
            }
        }
        let mut tx = PlutoTx {
            ctx,
            // Both words of a DMA sample are codes (with voltage0 alone
            // the Q word would come from the DDS).
            channels: ["voltage0", "voltage1"],
            phy_channel: s.dev.phy_channel(),
            buffer: s.dev.buffer,
            kernel_buffers: s.dev.kernel_buffers.unwrap_or(TX_SYMBOLS_KERNEL_BUFFERS),
            loopback: s.dev.loopback,
            block: None,
        };
        // From here on, dropping `tx` restores the safe state (CTRL 0 once
        // the block is configured).
        tx.program(cfg, &s, &mut info)?;
        let commands = Encoder::new(0).configure_tx(params).len();
        info!(
            backend = NAME,
            commands,
            "configuring the FPGA transmit block: {commands} commands through {TX_DEV} gpio_out (≈ {:.0} s through iiod)",
            commands as f64 * 0.62e-3
        );
        let t0 = Instant::now();
        let block = {
            let dac = tx
                .ctx
                .device(TX_DEV)
                .at("configure the FPGA transmit block")?;
            TxBlock::open(&dac, params)?
        };
        let config_s = t0.elapsed().as_secs_f64();
        tx.block = Some(block);
        tx.set_level(&s, &mut info)?;
        let rs = tx_symbol_rate(cfg, params);
        info += &format!(
            "; FPGA transmit block TX format {}, {} phases × {} taps, configured in {config_s:.1} s ({commands} commands), selected; {:.3} Mbaud of symbol codes, {:.2} MB/s; buffers of {} DMA samples, {} in the kernel",
            need.version,
            need.phases(),
            need.taps,
            rs / 1e6,
            txformat::link_rate(rs) / 1e6,
            tx.buffer,
            tx.kernel_buffers
        );
        let gain = format!("TX{} attenuation {} dB", s.dev.channel, s.att_db());
        info!(
            backend = NAME,
            uri = s.dev.uri(),
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            loopback = s.dev.loopback,
            config_s,
            "Pluto {} opened for TX of symbol codes through the FPGA transmit block (configured in {config_s:.1} s): {}; {gain}{}",
            s.dev.uri(),
            cfg.describe_tuning(),
            if s.dev.loopback {
                "; digital TX→RX loopback"
            } else {
                ""
            }
        );
        debug!(backend = NAME, "{info}");
        let sink = QueuedSink::with_encoder(tx, opts, CodeEncoder::new())?;
        Ok((Box::new(sink), info))
    }

    fn capture_symbols(
        &self,
        cfg: &RadioConfig,
        params: &isdb_frontend::Params,
        words: u64,
        out: &mut dyn std::io::Write,
        stop: &AtomicBool,
    ) -> Result<SymbolsCapture> {
        params.validate()?;
        let mut s = rx_settings_for(cfg, Dir::Capture)?;
        let mut ctx = open(&mut s.dev)?;
        let device = device_info(&ctx);
        let need = (isdb_frontend::params::FORMAT_VERSION, params.taps());
        let Some(id) = probe_front_end(&ctx)? else {
            return Err(RadioError::FrontEnd {
                backend: NAME,
                found: None,
                need,
            });
        };
        if !id.supports(params) {
            return Err(RadioError::FrontEnd {
                backend: NAME,
                found: Some((id.version, id.taps)),
                need,
            });
        }
        let mut info = String::new();
        configure_rx(&ctx, cfg, &s, &mut info)?;
        std::thread::sleep(CAPTURE_SETTLE);
        let rate = rx_rate(&ctx)?;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        // Configure through the command register; check one register.
        let t0 = std::time::Instant::now();
        let mut enc = {
            let op = "configure the symbol front end";
            let rx = ctx.device(RX_DEV).at(op)?;
            let mut enc = Encoder::new(rx.reg_read(GPIO_OUT_REG).at(op)?);
            let cmds = enc.configure(params);
            for &w in &cmds {
                rx.reg_write(GPIO_OUT_REG, w).at(op)?;
            }
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Addr, u32::from(reg::KP)))
                .at(op)?;
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Read, 0)).at(op)?;
            let kp = rx.reg_read(GPIO_IN_REG).at(op)?;
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Nop, 0)).at(op)?;
            if kp != i32::from(params.kp) as u32 {
                return Err(reply(
                    "front end register kp",
                    format!("reads {kp:#x} after writing {:#x}", params.kp),
                ));
            }
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Ctrl, control::ctrl::START))
                .at(op)?;
            rx.reg_write(
                GPIO_OUT_REG,
                enc.word(Op::Ctrl, control::ctrl::RUN | control::ctrl::SYMBOLS),
            )
            .at(op)?;
            enc
        };
        let config_s = t0.elapsed().as_secs_f64();
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri(),
            rate_hz = rate,
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            words,
            format = id.version,
            taps = id.taps,
            config_s,
            "Pluto {} opened for a symbol capture of {words} words: {}; RX{} gain {gain}; FPGA front end format {} with {} taps, configured in {config_s:.1} s",
            s.dev.uri(),
            cfg.describe_tuning(),
            s.dev.channel,
            id.version,
            id.taps
        );
        debug!(backend = NAME, "{device}{info}");
        let streamed = stream_words(&mut ctx, &s.dev, words, out, stop);
        // Back to IQ samples, whatever happened.
        let restored = ctx
            .device(RX_DEV)
            .and_then(|d| d.reg_write(GPIO_OUT_REG, enc.word(Op::Ctrl, 0)));
        let (written, overflows) = streamed?;
        restored.at("switch the RX back to IQ samples")?;
        if overflows > 0 {
            warn!(
                backend = NAME,
                overflows,
                words = written,
                "symbol capture: the ADC core reported {overflows} overflow(s) in {written} words (symbols lost)"
            );
        }
        debug!(
            backend = NAME,
            words = written,
            overflows,
            "symbol capture done: {written} words, {overflows} overflow(s)"
        );
        let read_back = "read back the RX settings";
        let phy = ctx.device(PHY).at(read_back)?;
        let ch = phy.channel(s.dev.phy_channel(), false).at(read_back)?;
        let gain_db = ch.read_f64("hardwaregain").at(read_back)?;
        Ok(SymbolsCapture {
            words: written,
            overflows,
            sample_rate_hz: rate,
            lo_hz: read_lo(&phy.channel(RX_LO, true).at(read_back)?, read_back)?,
            rf_bandwidth_hz: u64::try_from(ch.read_i64("rf_bandwidth").at(read_back)?).ok(),
            gain: match s.agc {
                "manual" => format!("manual {gain_db} dB"),
                m => format!("AGC {m}, {gain_db} dB at the end"),
            },
            device,
            method: format!(
                "FPGA symbol front end (format {}, {} taps), configured in {config_s:.1} s; libiio stream of 16-bit words",
                id.version, id.taps
            ),
            time,
        })
    }

    fn open_rx(&self, cfg: &RadioConfig, opts: &RxOptions) -> Result<(Box<dyn IqSource>, String)> {
        let mut s = rx_settings(cfg)?;
        opts.validate()?;
        let depth = opts.buffers(cfg.sample_rate_hz(), s.dev.buffer);
        let mut ctx = open(&mut s.dev)?;
        let mut info = device_info(&ctx);
        configure_rx(&ctx, cfg, &s, &mut info)?;
        let stream = ctx
            .start_rx(
                RX_DEV,
                &s.dev.stream_channels(),
                s.dev.buffer,
                KERNEL_BUFFERS,
                depth,
            )
            .at(&port_hint(&s.dev, "RX"))?;
        info += &format!("; RX queue {depth} buffers of {} samples", s.dev.buffer);
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri(),
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            queue = depth,
            buffer = s.dev.buffer,
            "Pluto {} opened for RX: {}; RX{} gain {gain}; RX queue {depth} buffers of {} samples",
            s.dev.uri(),
            cfg.describe_tuning(),
            s.dev.channel,
            s.dev.buffer
        );
        debug!(backend = NAME, "{info}");
        Ok((
            Box::new(PlutoIqSource {
                ctx,
                stream: Some(stream),
            }),
            info,
        ))
    }
}

/// The symbol front end the bitstream reports in `up_adc_gpio_in`, if any
/// (one register read; nothing is written).
fn probe_front_end(ctx: &Context) -> Result<Option<Identity>> {
    let word = ctx
        .device(RX_DEV)
        .and_then(|d| d.reg_read(GPIO_IN_REG))
        .at("probe the symbol front end")?;
    Ok(Identity::parse(word))
}

/// The RX sample rate the AD936x runs at (`rx_path_rates`).
fn rx_rate(ctx: &Context) -> Result<f64> {
    let rates = ctx
        .device(PHY)
        .and_then(|d| d.read("rx_path_rates"))
        .at("read the RX sample rate")?;
    path_rate(&rates, "RXSAMP").ok_or_else(|| {
        reply(
            format!("{PHY} rx_path_rates"),
            format!("{rates:?}: no RX rate"),
        )
    })
}

/// A [`RadioError::Stream`] of the RX device.
fn rx_stream(problem: StreamProblem) -> RadioError {
    RadioError::Stream {
        stream: RX_DEV,
        problem,
    }
}

/// Clear the ADC core's overflow bit.
fn clear_adc_overflow(ctx: &Context) -> Result<()> {
    ctx.device(RX_DEV)
        .and_then(|rx| rx.reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW))
        .at("clear the ADC overflow bit")
}

/// Clear the ADC core's overflow bit if it is set; true if it was.
fn adc_overflowed(ctx: &Context) -> Result<bool> {
    let op = "check the ADC overflow bit";
    let rx = ctx.device(RX_DEV).at(op)?;
    if rx.reg_read(ADC_STATUS_REG).at(op)? & ADC_STATUS_OVERFLOW == 0 {
        return Ok(false);
    }
    rx.reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW).at(op)?;
    Ok(true)
}

/// Stream `words` 16-bit words from `voltage0` of the RX (the front end's
/// output while it is selected) into `out` until done or `stop`; returns
/// the words written and the buffers after which the ADC core reported an
/// overflow.
fn stream_words(
    ctx: &mut Context,
    dev: &DeviceArgs,
    words: u64,
    out: &mut dyn std::io::Write,
    stop: &AtomicBool,
) -> Result<(u64, u64)> {
    let stream = ctx
        .start_rx_raw(
            RX_DEV,
            &["voltage0"],
            dev.buffer,
            dev.kernel_buffers.unwrap_or(SYMBOLS_KERNEL_BUFFERS),
            STREAM_DEPTH,
        )
        .at(&port_hint(dev, "RX"))?;
    if stream.sample_bytes != 2 {
        return Err(rx_stream(StreamProblem::SampleSize {
            what: "word",
            got: stream.sample_bytes,
            want: 2,
        }));
    }
    clear_adc_overflow(ctx)?;
    let (mut written, mut overflows) = (0u64, 0u64);
    let timeout = Duration::from_secs(10);
    let r = (|| -> Result<()> {
        while written < words && !stop.load(Ordering::SeqCst) {
            let data = match stream.receiver.recv_timeout(timeout) {
                Ok(d) => d,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(rx_stream(StreamProblem::Silent {
                        what: "words",
                        seconds: timeout.as_secs_f64(),
                    }));
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if adc_overflowed(ctx)? {
                overflows += 1;
                // the first is a warning, the rest are counted in the
                // summary event at the end of the capture
                if overflows == 1 {
                    warn!(
                        backend = NAME,
                        overflows,
                        words = written,
                        "ADC overflow after {written} words: symbols lost; further ones are counted"
                    );
                } else {
                    debug!(
                        backend = NAME,
                        overflows,
                        words = written,
                        "ADC overflow {overflows} after {written} words: symbols lost"
                    );
                }
            }
            let n = ((data.len() / 2) as u64).min(words - written);
            out.write_all(&data[..n as usize * 2])
                .map_err(|e| RadioError::io("write the words", e))?;
            written += n;
        }
        Ok(())
    })();
    let stopped = ctx.stop();
    r?;
    stopped.at("stop the RX stream")?;
    Ok((written, overflows))
}

/// Stream `samples` samples of `format` from the RX into `out` until
/// done or `stop`; returns the samples written and the gaps (see the module
/// documentation for how a loss is placed). The samples discarded around
/// a loss count towards `samples`, so the capture spans `samples` sample
/// periods whatever it loses.
fn stream_capture(
    ctx: &mut Context,
    dev: &DeviceArgs,
    samples: u64,
    format: CaptureFormat,
    out: &mut dyn std::io::Write,
    stop: &AtomicBool,
) -> Result<(u64, Vec<CaptureGap>)> {
    let bps = format.bytes_per_sample();
    let stream = ctx
        .start_rx_raw(
            RX_DEV,
            dev.capture_channels(),
            dev.buffer,
            KERNEL_BUFFERS,
            STREAM_DEPTH,
        )
        .at(&port_hint(dev, "RX"))?;
    if stream.sample_bytes != bps {
        return Err(rx_stream(StreamProblem::SampleSize {
            what: "sample",
            got: stream.sample_bytes,
            want: bps,
        }));
    }
    // The bit is set whenever no transfer runs: clear it now that one does.
    clear_adc_overflow(ctx)?;
    // Buffers that may hold samples from before an unseen loss: the
    // device's kernel blocks, iiod's and the client's buffer, one in the
    // network and the host queue.
    let window = u64::from(KERNEL_BUFFERS) + 3 + STREAM_DEPTH as u64;
    let (mut written, mut received) = (0u64, 0u64);
    let mut gaps = Vec::<CaptureGap>::new();
    // The gap being discarded and the buffers still to discard into it.
    let mut open: Option<(CaptureGap, u64)> = None;
    let timeout = Duration::from_secs(10);
    let r = (|| -> Result<()> {
        while received < samples && !stop.load(Ordering::SeqCst) {
            let data = match stream.receiver.recv_timeout(timeout) {
                Ok(d) => d,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(rx_stream(StreamProblem::Silent {
                        what: "samples",
                        seconds: timeout.as_secs_f64(),
                    }));
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if adc_overflowed(ctx)? {
                match &mut open {
                    None => {
                        warn!(
                            backend = NAME,
                            at = written,
                            window,
                            gaps = gaps.len() + 1,
                            "ADC overflow after {written} samples: a gap in the capture, the next {window} buffers discarded"
                        );
                        open = Some((
                            CaptureGap {
                                at: written,
                                discarded: 0,
                            },
                            window,
                        ));
                    }
                    Some((_, left)) => {
                        debug!(
                            backend = NAME,
                            at = written,
                            "ADC overflow again inside the gap after {written} samples"
                        );
                        *left = window;
                    }
                }
            }
            let n =
                (data.len() / bps).min(usize::try_from(samples - received).unwrap_or(usize::MAX));
            received += n as u64;
            if let Some((mut gap, left)) = open.take() {
                gap.discarded += n as u64;
                if left > 1 {
                    open = Some((gap, left - 1));
                } else {
                    gaps.push(gap);
                }
                continue;
            }
            out.write_all(&data[..n * bps])
                .map_err(|e| RadioError::io("write the capture", e))?;
            written += n as u64;
        }
        // A capture that ends inside a gap.
        gaps.extend(open.take().map(|(gap, _)| gap));
        Ok(())
    })();
    let stopped = ctx.stop();
    r?;
    stopped.at("stop the RX stream")?;
    Ok((written, gaps))
}

fn describe_rx_gain(s: &RxSettings) -> String {
    if s.agc == "manual" {
        format!("manual {} dB", s.gain_db)
    } else {
        format!("AGC {}", s.agc)
    }
}

/// What to say when the port's channels are missing.
fn port_hint(dev: &DeviceArgs, dir: &str) -> String {
    if dev.channel == 2 {
        format!("{dir}2 streaming (needs the firmware's 2r2t mode: fw_setenv mode 2r2t)")
    } else {
        format!("{dir}1 streaming")
    }
}

/// Open the device, settling its URI first ([`DeviceArgs::resolve`]).
fn open(dev: &mut DeviceArgs) -> Result<Context> {
    dev.resolve()?;
    let uri = dev.uri();
    Context::open(uri).at(&format!("open Pluto {uri}"))
}

/// `ISDB_PLUTO_URI`, if set and not empty.
fn env_uri() -> Option<String> {
    std::env::var(URI_ENV).ok().filter(|u| !u.is_empty())
}

/// `libiio 0.26 (v0.26), Analog Devices PlutoSDR Rev.C (Z7020-AD9361),
/// ad9361, firmware v0.38, serial 1044…`.
fn device_info(ctx: &Context) -> String {
    let attr = |k: &str| ctx.attr(k).unwrap_or_else(|| "?".into());
    format!(
        "libiio {}, {}, {}, firmware {}, serial {}",
        isdb_iio::library_version(),
        attr("hw_model"),
        attr("ad9361-phy,model"),
        attr("fw_version"),
        attr("hw_serial")
    )
}

/// The rate the AD936x runs at from its `tx_path_rates` / `rx_path_rates`
/// text (`BBPLL:983040000 DAC:122880000 … TXSAMP:30720000`): the BBPLL
/// frequency over its integer divider to `key`.
fn path_rate(rates: &str, key: &str) -> Option<f64> {
    let get = |k: &str| {
        rates.split_whitespace().find_map(|f| {
            f.strip_prefix(k)?
                .strip_prefix(':')?
                .parse::<f64>()
                .ok()
                .filter(|v| *v > 0.0)
        })
    };
    let (bbpll, samp) = (get("BBPLL")?, get(key)?);
    Some(bbpll / (bbpll / samp).round())
}

/// Program the sample rate into `ch` (a phy channel; TX and RX share the
/// clock chain) and check the rate the device runs at; libiio failures
/// are reported as doing `op`.
fn set_rate(
    ctx: &Context,
    ch: &isdb_iio::Channel<'_>,
    cfg: &RadioConfig,
    rates_attr: &str,
    key: &str,
    op: &str,
) -> Result<f64> {
    let want = cfg.sample_rate_hz();
    // round(rate_num / rate_den) in whole Hz, computed in integers.
    let (num, den) = (i64::from(cfg.rate_num), i64::from(cfg.rate_den));
    let hz = (2 * num + den)
        .checked_div(2 * den)
        .ok_or_else(|| rate_error(cfg))?;
    // The driver reads back whole Hz below the BBPLL rate (8126983 for
    // 8126984): rewriting reprograms the clock chain, which would glitch a
    // stream of the other direction, so 1 Hz counts as unchanged.
    if ch.read_i64("sampling_frequency").at(op)?.abs_diff(hz) > 1 {
        ch.write_i64("sampling_frequency", hz).at(op)?;
    }
    let rates = ctx.device(PHY).and_then(|d| d.read(rates_attr)).at(op)?;
    let got = path_rate(&rates, key).ok_or_else(|| {
        reply(
            format!("{PHY} {rates_attr}"),
            format!("{rates:?}: no BBPLL / {key} rate"),
        )
    })?;
    let ppm = (got / want - 1.0) * 1e6;
    if ppm.abs() > MAX_RATE_ERROR_PPM {
        return Err(RadioError::Readback {
            backend: NAME,
            what: "sample rate",
            requested_hz: want,
            programmed_hz: got,
            tolerance: Tolerance::Ppm(MAX_RATE_ERROR_PPM),
        });
    }
    Ok(got)
}

/// Program the LO (always when `force`, else only if it differs) and
/// check the frequency read back; libiio failures are reported as doing
/// `op`.
fn set_lo(lo: &isdb_iio::Channel<'_>, hz: u64, force: bool, op: &str) -> Result<u64> {
    // u64 -> f64: frequencies below 2^53 Hz are exact.
    #[allow(clippy::cast_precision_loss)]
    let as_f64 = |f: u64| f as f64;
    // Above i64::MAX Hz: refused by check_tuning long before.
    let want = i64::try_from(hz).map_err(|_| RadioError::Tuning {
        backend: NAME,
        device: "Pluto (AD9361)",
        what: "LO",
        hz: as_f64(hz),
        min_hz: Some(0.0),
        max_hz: as_f64(i64::MAX.unsigned_abs()),
    })?;
    if force || lo.read_i64("frequency").at(op)? != want {
        lo.write_i64("frequency", want).at(op)?;
    }
    let got = read_lo(lo, op)?;
    if got.abs_diff(hz) > MAX_LO_ERROR_HZ {
        return Err(RadioError::Readback {
            backend: NAME,
            what: "LO",
            requested_hz: as_f64(hz),
            programmed_hz: as_f64(got),
            tolerance: Tolerance::Hz(as_f64(MAX_LO_ERROR_HZ)),
        });
    }
    Ok(got)
}

/// Program the RF bandwidth; returns the value read back.
fn set_bandwidth(ch: &isdb_iio::Channel<'_>, hz: u32) -> isdb_iio::Result<i64> {
    if ch.read_i64("rf_bandwidth")? != i64::from(hz) {
        ch.write_i64("rf_bandwidth", i64::from(hz))?;
    }
    ch.read_i64("rf_bandwidth")
}

/// `; LO 473142857 Hz; sample rate 8126983.94 Hz (-0.02 ppm); RF bandwidth 6.00 MHz`.
fn describe_programmed(cfg: &RadioConfig, lo: u64, rate: f64, bw: i64) -> String {
    format!(
        "; LO {lo} Hz; sample rate {rate:.2} Hz ({:+.2} ppm); RF bandwidth {:.2} MHz",
        (rate / cfg.sample_rate_hz() - 1.0) * 1e6,
        bw as f64 / 1e6
    )
}

/// `[min step max]` (an `*_available` range) → (min, max).
fn parse_range(s: &str) -> Option<(f64, f64)> {
    let v: Vec<f64> = s
        .trim()
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_whitespace()
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    match v[..] {
        [min, _, max] => Some((min, max)),
        _ => None,
    }
}

fn configure_rx(ctx: &Context, cfg: &RadioConfig, s: &RxSettings, info: &mut String) -> Result<()> {
    let op = &format!("configure Pluto {} for RX", s.dev.uri());
    let phy = ctx.device(PHY).at(op)?;
    let ch = phy.channel(s.dev.phy_channel(), false).at(op)?;
    let rate = set_rate(ctx, &ch, cfg, "rx_path_rates", "RXSAMP", op)?;
    let bw = set_bandwidth(&ch, cfg.bandwidth_hz).at(op)?;
    let lo = set_lo(&phy.channel(RX_LO, true).at(op)?, cfg.center_hz, false, op)?;
    ch.write("gain_control_mode", s.agc).at(op)?;
    if s.agc == "manual" {
        let avail = ch.read("hardwaregain_available").at(op)?;
        if let Some((min, max)) = parse_range(&avail)
            && !(min..=max).contains(&f64::from(s.gain_db))
        {
            return Err(bad_gain(
                Direction::Rx,
                "gain",
                &s.gain_db.to_string(),
                Rule::Range {
                    min,
                    max,
                    unit: "dB at this LO",
                },
            ));
        }
        ch.write_f64("hardwaregain", f64::from(s.gain_db)).at(op)?;
    }
    *info += &describe_programmed(cfg, lo, rate, bw);
    *info += &format!(
        "; RX{} gain {}",
        s.dev.channel,
        match s.agc {
            "manual" => format!("manual {} dB", ch.read_f64("hardwaregain").at(op)?),
            m => format!("AGC {m}"),
        }
    );
    if phy.read_debug("loopback").is_ok_and(|v| v.trim() != "0") {
        *info += "; digital TX→RX loopback ON (receiving the device's own TX)";
    }
    Ok(())
}

/// Register access to the DAC core (`cf-ad9361-dds-core-lpc`), where the
/// FPGA transmit block's control pair and the core's status live: libiio's
/// register access on the device, a model of the block in the tests.
trait Regs {
    /// Read the register at debug address `addr`.
    fn read(&self, addr: u32) -> Result<u32>;
    /// Write `value` to the register at debug address `addr`.
    fn write(&self, addr: u32, value: u32) -> Result<()>;
}

impl Regs for isdb_iio::Device<'_> {
    fn read(&self, addr: u32) -> Result<u32> {
        self.reg_read(addr)
            .at("read a register of the DAC core (the FPGA transmit block)")
    }
    fn write(&self, addr: u32, value: u32) -> Result<()> {
        self.reg_write(addr, value)
            .at("write a register of the DAC core (the FPGA transmit block)")
    }
}

/// The FPGA transmit block as the host drives it (`control::tx`): the
/// command encoder (the toggle bit and `gpio_out` bit 0 carried on),
/// whether it runs, and the MISSED the last status line read.
#[derive(Debug)]
struct TxBlock {
    enc: Encoder,
    running: bool,
    missed: u32,
}

/// What [`TxBlock::counters`] reads back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TxCounters {
    /// STATUS: bit 0 run, bit 1 selected, bit 2 underrun (sticky).
    status: u32,
    /// Codes entered from delivered words since START (low 32 bits).
    consumed: u32,
    /// Zero symbols entered for stalled requests since START.
    missed: u32,
    /// CONSUMED at the first stalled request (0 before one).
    first_miss: u32,
    /// The DAC core's underflow flag, cleared when the block started.
    underflow: bool,
}

impl TxCounters {
    /// `CONSUMED 319488, MISSED 0, FIRST_MISS -, STATUS 0x3; DAC underflow
    /// flag clear`.
    fn describe(&self) -> String {
        format!(
            "CONSUMED {}, MISSED {}, FIRST_MISS {}, STATUS {:#x}; DAC underflow flag {}",
            self.consumed,
            self.missed,
            if self.status & txc::status::UNDERRUN != 0 {
                self.first_miss.to_string()
            } else {
                "-".into()
            },
            self.status,
            if self.underflow { "set" } else { "clear" }
        )
    }
}

impl TxBlock {
    /// The block the bitstream reports in `up_dac_gpio_in`, if any (one
    /// read; nothing is written).
    fn probe(r: &dyn Regs) -> Result<Option<TxIdentity>> {
        Ok(TxIdentity::parse(r.read(txc::GPIO_IN_REG)?))
    }

    /// The block with its encoder continuing from `gpio_out` (not
    /// configured).
    fn attach(r: &dyn Regs) -> Result<Self> {
        Ok(Self {
            enc: Encoder::new(r.read(txc::GPIO_OUT_REG)?),
            running: false,
            missed: 0,
        })
    }

    /// Check the DAC core (refused in r1_mode and with `gpio_out` bit 0,
    /// Tezuka's ×8 TX FIR, set), configure the block for `p`
    /// ([`Encoder::configure_tx`]), read STEP back, CTRL START|SELECT, and
    /// require STATUS "selected, not running": from here on the block owns
    /// the DAC and the DMA, outputs zero and requests nothing, so the
    /// stream may start. After a failed step the block is sent CTRL 0.
    fn open(r: &dyn Regs, p: &TxParams) -> Result<Self> {
        if r.read(DAC_R1_MODE_REG)? & DAC_R1_MODE_BIT != 0 {
            return Err(RadioError::Unsupported {
                backend: NAME,
                what: "runs its AD9361 DAC core in r1_mode (one TX channel): the FPGA transmit block's bitstream is not timed for that clock case; use --offload cpu",
            });
        }
        let out = r.read(txc::GPIO_OUT_REG)?;
        if out & 1 != 0 {
            return Err(RadioError::Unsupported {
                backend: NAME,
                what: "has bit 0 of the DAC core's gpio_out set (Tezuka's ×8 TX FIR enable, also written by the kernel's interpolation attribute): the FPGA transmit block takes the FIR's place, so the bit must be clear; use --offload cpu",
            });
        }
        let mut b = Self {
            enc: Encoder::new(out),
            running: false,
            missed: 0,
        };
        match b.configure(r, p) {
            Ok(()) => Ok(b),
            Err(e) => {
                // best effort: back to the IQ pass-through
                let _ = b.release(r);
                Err(e)
            }
        }
    }

    fn configure(&mut self, r: &dyn Regs, p: &TxParams) -> Result<()> {
        for w in self.enc.configure_tx(p) {
            r.write(txc::GPIO_OUT_REG, w)?;
        }
        let step = self.read(r, txc::reg::STEP)?;
        if step != p.step {
            return Err(reply(
                "FPGA transmit block register STEP",
                format!("reads {step:#x} after writing {:#x}", p.step),
            ));
        }
        self.ctrl(r, txc::ctrl::START | txc::ctrl::SELECT)?;
        let st = self.read(r, txc::reg::STATUS)?;
        let mode = txc::status::RUN | txc::status::SELECTED;
        if st & mode != txc::status::SELECTED {
            return Err(reply(
                "FPGA transmit block STATUS",
                format!(
                    "reads {st:#x} after CTRL START|SELECT, not selected and stopped: the block did not take the DAC, and codes sent to a plain DAC would go out as samples"
                ),
            ));
        }
        Ok(())
    }

    /// Write one command word.
    fn command(&mut self, r: &dyn Regs, op: Op, arg: u32) -> Result<()> {
        r.write(txc::GPIO_OUT_REG, self.enc.word(op, arg))
    }

    /// CTRL with `bits` ([`txc::ctrl`]).
    fn ctrl(&mut self, r: &dyn Regs, bits: u32) -> Result<()> {
        self.command(r, Op::Ctrl, bits)
    }

    /// Registers `regs`, in order (ADDR, READ and the input register each),
    /// then a NOP, so that `gpio_in` shows the ID word again.
    fn read_regs<const N: usize>(&mut self, r: &dyn Regs, regs: [u16; N]) -> Result<[u32; N]> {
        let mut v = [0; N];
        for (x, &a) in v.iter_mut().zip(&regs) {
            self.command(r, Op::Addr, u32::from(a))?;
            self.command(r, Op::Read, 0)?;
            *x = r.read(txc::GPIO_IN_REG)?;
        }
        self.command(r, Op::Nop, 0)?;
        Ok(v)
    }

    /// One register ([`TxBlock::read_regs`]).
    fn read(&mut self, r: &dyn Regs, reg: u16) -> Result<u32> {
        Ok(self.read_regs(r, [reg])?[0])
    }

    /// Start, once the DMA holds the stream's first buffer: CTRL
    /// RUN|SELECT (the requests begin a few clocks later and find words),
    /// STATUS must read run and selected, then the DAC core's underflow
    /// flag is cleared, so that it reports what happens from here on.
    fn run(&mut self, r: &dyn Regs) -> Result<()> {
        self.ctrl(r, txc::ctrl::RUN | txc::ctrl::SELECT)?;
        self.running = true;
        let st = self.read(r, txc::reg::STATUS)?;
        let mode = txc::status::RUN | txc::status::SELECTED;
        if st & mode != mode {
            return Err(reply(
                "FPGA transmit block STATUS",
                format!("reads {st:#x} after CTRL RUN|SELECT, not running and selected"),
            ));
        }
        if st & txc::status::UNDERRUN != 0 {
            warn!(
                backend = NAME,
                status = st,
                "the FPGA transmit block missed symbols at its start: the stream's first buffer was not in the DMA when it started"
            );
        }
        r.write(DAC_STATUS_REG, DAC_STATUS_UNDERFLOW)
    }

    /// MISSED, for a status line; warns when it grew since the last one.
    fn status(&mut self, r: &dyn Regs) -> Result<u32> {
        let m = self.read(r, txc::reg::MISSED)?;
        let d = m.wrapping_sub(self.missed);
        if d > 0 {
            warn!(
                backend = NAME,
                missed = m,
                new_missed = d,
                "the FPGA transmit block missed {d} symbol(s) since the last status ({m} since its start): the DMA ran dry and zero symbols went on air"
            );
        }
        self.missed = m;
        Ok(m)
    }

    /// The counters, STATUS and the DAC core's underflow flag.
    fn counters(&mut self, r: &dyn Regs) -> Result<TxCounters> {
        let [status, consumed, missed, first_miss] = self.read_regs(
            r,
            [
                txc::reg::STATUS,
                txc::reg::CONSUMED,
                txc::reg::MISSED,
                txc::reg::FIRST_MISS,
            ],
        )?;
        let underflow = r.read(DAC_STATUS_REG)? & DAC_STATUS_UNDERFLOW != 0;
        Ok(TxCounters {
            status,
            consumed,
            missed,
            first_miss,
            underflow,
        })
    }

    /// CTRL 0: the IQ pass-through again.
    fn release(&mut self, r: &dyn Regs) -> Result<()> {
        self.ctrl(r, 0)?;
        self.running = false;
        Ok(())
    }
}

/// Components a TX stream takes ahead of real time
/// ([`RadioTx::ahead`]): its `kernel_buffers` kernel buffers and the one
/// being filled, of `buffer` samples, two components per sample. libiio
/// pushes that much at the link's speed before a push waits for the DMA.
fn queue_components(kernel_buffers: u32, buffer: usize) -> usize {
    (kernel_buffers as usize + 1) * buffer * 2
}

/// Wait until the stream has handed the device its first buffer (the
/// transmit block may then start without missing a word).
fn wait_first_buffer(counters: &dyn StreamCounters, timeout: Duration) -> Result<()> {
    let t = Instant::now();
    while counters.buffers() == 0 {
        if counters.finished() {
            return Err(RadioError::Stream {
                stream: TX_DEV,
                problem: StreamProblem::Ended,
            });
        }
        if t.elapsed() > timeout {
            return Err(RadioError::Stream {
                stream: TX_DEV,
                problem: StreamProblem::Stalled {
                    seconds: timeout.as_secs_f64(),
                },
            });
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// A Pluto configured for transmission (the radio of a [`QueuedSink`]):
/// of IQ samples, or of symbol codes through the FPGA transmit block.
struct PlutoTx {
    ctx: Context,
    channels: [&'static str; 2],
    phy_channel: &'static str,
    buffer: usize,
    kernel_buffers: u32,
    loopback: bool,
    /// The FPGA transmit block, once configured and selected (symbol
    /// codes); `None` for IQ samples.
    block: Option<TxBlock>,
}

impl PlutoTx {
    /// Program the device up to its level (the attenuation starts and stays
    /// at the maximum): LO powerdown with `loopback`, DDS off, rate,
    /// bandwidth, LO, loopback, and check the stream channels.
    fn program(&self, cfg: &RadioConfig, s: &TxSettings, info: &mut String) -> Result<()> {
        let op = &format!("configure Pluto {} for TX", s.dev.uri());
        let phy = self.ctx.device(PHY).at(op)?;
        let ch = phy.channel(self.phy_channel, true).at(op)?;
        ch.write_f64("hardwaregain", -f64::from(MAX_ATT_QDB) / 4.0)
            .at(op)?;
        let lo_ch = phy.channel(TX_LO, true).at(op)?;
        // Loopback: the LO stays off for the whole run. Otherwise it is
        // powered first (a previous close left it down) and tuned below,
        // which makes the synthesiser calibrate and lock.
        lo_ch
            .write_i64("powerdown", i64::from(self.loopback))
            .at(op)?;
        // The DDS tone generator off: without a buffer the DAC sends zeros.
        self.ctx
            .device(TX_DEV)
            .and_then(|d| d.channel("altvoltage0", true)?.write_i64("raw", 0))
            .at(op)?;
        let rate = set_rate(&self.ctx, &ch, cfg, "tx_path_rates", "TXSAMP", op)?;
        let bw = set_bandwidth(&ch, cfg.bandwidth_hz).at(op)?;
        let lo = set_lo(&lo_ch, cfg.center_hz, !self.loopback, op)?;
        // A loopback left on (by a TX that did not close) is turned off.
        if self.loopback || phy.read_debug("loopback").is_ok_and(|v| v != "0") {
            phy.write_debug("loopback", if self.loopback { "1" } else { "0" })
                .at(op)?;
        }
        // Check the stream channels before raising the level.
        for c in self.channels {
            self.ctx
                .device(TX_DEV)
                .and_then(|d| d.channel(c, true).map(|_| ()))
                .at(&port_hint(&s.dev, "TX"))?;
        }
        *info += &describe_programmed(cfg, lo, rate, bw);
        Ok(())
    }

    /// The attenuation, last.
    fn set_level(&self, s: &TxSettings, info: &mut String) -> Result<()> {
        let op = &format!("set the TX attenuation of Pluto {}", s.dev.uri());
        let ch = self
            .ctx
            .device(PHY)
            .and_then(|phy| phy.channel(self.phy_channel, true))
            .at(op)?;
        ch.write_f64("hardwaregain", -s.att_db()).at(op)?;
        *info += &format!(
            "; TX{} attenuation {} dB",
            s.dev.channel,
            -ch.read_f64("hardwaregain").at(op)?
        );
        if self.loopback {
            *info += "; digital TX→RX loopback, TX LO powered down";
        }
        Ok(())
    }

    /// With a bitstream that has the FPGA transmit block, CTRL 0 (the IQ
    /// pass-through) before IQ samples are sent: a block left selected (by
    /// a process that did not close) would take them as codes. Without the
    /// block (it reads 0) nothing is written; a register access the device
    /// refuses is logged and ignored, as the IQ path does not need it.
    fn pass_through(&self) -> Result<()> {
        let Ok(dac) = self.ctx.device(TX_DEV) else {
            return Ok(());
        };
        match TxBlock::probe(&dac) {
            Ok(Some(id)) => {
                TxBlock::attach(&dac)?.release(&dac)?;
                debug!(
                    backend = NAME,
                    version = id.version,
                    "the bitstream has the FPGA transmit block (TX format {}): CTRL 0, the IQ pass-through",
                    id.version
                );
            }
            Ok(None) => {}
            Err(e) => debug!(
                backend = NAME,
                "no register access to {TX_DEV} ({e}): the FPGA transmit block is not probed"
            ),
        }
        Ok(())
    }

    /// CTRL 0 to the block, if it is configured (best effort; kept for a
    /// retry when it fails).
    fn release_block(&mut self) {
        let Some(b) = &mut self.block else {
            return;
        };
        let op = "switch the FPGA transmit block back to the IQ pass-through";
        match self.ctx.device(TX_DEV).at(op).and_then(|d| b.release(&d)) {
            Ok(()) => self.block = None,
            Err(e) => warn!(backend = NAME, "could not {op}: {e}"),
        }
    }

    /// Maximum attenuation, TX LO off, loopback off (best effort).
    fn safe_state(&self) {
        let Ok(phy) = self.ctx.device(PHY) else {
            return;
        };
        if let Ok(ch) = phy.channel(self.phy_channel, true) {
            let _ = ch.write_f64("hardwaregain", -f64::from(MAX_ATT_QDB) / 4.0);
        }
        if let Ok(lo) = phy.channel(TX_LO, true) {
            let _ = lo.write_i64("powerdown", 1);
        }
        if self.loopback {
            let _ = phy.write_debug("loopback", "0");
        }
    }
}

impl Drop for PlutoTx {
    /// Stop streaming, switch the transmit block back to the IQ
    /// pass-through and leave the transmitter in its safe state.
    fn drop(&mut self) {
        let _ = self.ctx.stop();
        self.release_block();
        self.safe_state();
    }
}

impl StreamCounters for isdb_iio::Counters {
    fn buffers(&self) -> u64 {
        self.buffers.load(Ordering::Relaxed)
    }
    fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}

impl RadioTx for PlutoTx {
    type Format = Cs12;

    fn start_tx(&mut self, depth: usize) -> Result<TxStream<i16>> {
        let s = self
            .ctx
            .start_tx(
                TX_DEV,
                &self.channels,
                self.buffer,
                self.kernel_buffers,
                depth,
            )
            .at("start the Pluto TX stream")?;
        Ok(TxStream {
            sender: s.sender,
            counters: s.counters,
        })
    }

    /// Stop the stream, then (symbol codes) CTRL 0, then the safe state.
    fn stop(&mut self) -> Result<()> {
        let stopped = self.ctx.stop().at("stop the Pluto TX stream");
        self.release_block();
        self.safe_state();
        stopped
    }

    fn is_streaming(&mut self) -> bool {
        self.ctx.is_streaming()
    }

    fn ahead(&self) -> usize {
        queue_components(self.kernel_buffers, self.buffer)
    }

    /// Symbol codes: once the first buffer is in the device, start the
    /// transmit block ([`TxBlock::run`]).
    fn started(&mut self, counters: &dyn StreamCounters) -> Result<()> {
        let Some(b) = &mut self.block else {
            return Ok(());
        };
        wait_first_buffer(counters, FIRST_BUFFER_TIMEOUT)?;
        let dac = self
            .ctx
            .device(TX_DEV)
            .at("start the FPGA transmit block")?;
        b.run(&dac)?;
        info!(
            backend = NAME,
            "FPGA transmit block running (CTRL RUN|SELECT) on the stream's first buffer"
        );
        Ok(())
    }

    /// Symbol codes: the block's MISSED (one READ).
    fn status(&mut self) -> String {
        let Some(b) = self.block.as_mut().filter(|b| b.running) else {
            return String::new();
        };
        match self
            .ctx
            .device(TX_DEV)
            .at("read the FPGA transmit block's MISSED")
            .and_then(|d| b.status(&d))
        {
            Ok(m) => format!(" | TX block MISSED {m}"),
            Err(e) => format!(" | TX block MISSED ? ({e})"),
        }
    }

    /// Symbol codes: CONSUMED, MISSED, FIRST_MISS, STATUS and the DAC
    /// core's underflow flag, read while the stream's last zero buffers
    /// still play (after the drain, before the stop).
    fn summary(&mut self) -> String {
        let Some(b) = &mut self.block else {
            return String::new();
        };
        if !b.running {
            return "; FPGA transmit block selected, never started".into();
        }
        match self
            .ctx
            .device(TX_DEV)
            .at("read the FPGA transmit block's counters")
            .and_then(|d| b.counters(&d))
        {
            Ok(c) => {
                if c.missed > 0 || c.underflow {
                    warn!(
                        backend = NAME,
                        missed = c.missed,
                        first_miss = c.first_miss,
                        underflow = c.underflow,
                        "the FPGA transmit block missed {} symbol(s), the first at code {} since its start: zero symbols went on air",
                        c.missed,
                        c.first_miss
                    );
                }
                format!("; FPGA TX block {}", c.describe())
            }
            Err(e) => format!("; FPGA TX block counters not read: {e}"),
        }
    }
}

/// 12-bit samples sign-extended in `i16` → samples scaled to ±1 (÷2048).
fn samples_from_cs12(b: &[i16]) -> Vec<C32> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|&[i, q]| C32::new(f32::from(i) / 2048.0, f32::from(q) / 2048.0))
        .collect()
}

/// Pluto receive stream.
struct PlutoIqSource {
    ctx: Context,
    stream: Option<isdb_iio::RxStream>,
}

impl IqSource for PlutoIqSource {
    fn read(&mut self, stop: &AtomicBool) -> Result<Option<Vec<C32>>> {
        let Some(st) = &self.stream else {
            return Ok(None);
        };
        loop {
            match st.receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(b) => return Ok(Some(samples_from_cs12(&b))),
                Err(RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => {
                    return Ok(None);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if st.counters.finished() {
                        self.stream = None;
                        self.ctx.stop().at("stop the Pluto RX stream")?;
                        return Err(RadioError::Stream {
                            stream: "Pluto RX",
                            problem: StreamProblem::Ended,
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.stream = None;
                    self.ctx.stop().at("stop the Pluto RX stream")?;
                    return Err(RadioError::Stream {
                        stream: "Pluto RX",
                        problem: StreamProblem::Closed,
                    });
                }
            }
        }
    }

    fn status(&self) -> String {
        self.stream.as_ref().map_or(String::new(), |s| {
            format!(
                " | overruns {}",
                s.counters.overruns.load(Ordering::Relaxed)
            )
        })
    }

    fn finish(&mut self) -> Result<String> {
        let overruns = self
            .stream
            .as_ref()
            .map_or(0, |s| s.counters.overruns.load(Ordering::Relaxed));
        self.stream = None;
        self.ctx.stop().at("stop the Pluto RX stream")?;
        let streaming = self.ctx.is_streaming();
        if overruns > 0 {
            warn!(
                backend = NAME,
                overruns,
                "Pluto RX dropped {overruns} buffer(s) (the reader fell behind the radio)"
            );
        }
        debug!(
            backend = NAME,
            overruns,
            streaming,
            "Pluto RX stream stopped: {overruns} overrun(s){}",
            if streaming {
                "; the device still reports streaming"
            } else {
                ""
            }
        );
        let s = format!(
            "radio: overruns {overruns}; device stopped streaming: {}",
            if streaming { "NO" } else { "yes" }
        );
        if streaming {
            return Err(RadioError::StillStreaming { summary: s });
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[(&str, &str)], gains: &[(&str, &str)]) -> RadioConfig {
        let kv = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        RadioConfig {
            center_hz: 473_142_857,
            nominal_hz: 473_142_857,
            rate_num: 512_000_000,
            rate_den: 63,
            bandwidth_hz: 6_000_000,
            args: kv(args),
            gains: kv(gains),
        }
    }

    const OPTS: TxOptions = TxOptions {
        prefill_frames: 2,
        frame_seconds: 0.231,
        backoff_db: 10.0,
    };

    #[test]
    fn device_arguments() {
        let s = tx_settings(&cfg(&[], &[])).unwrap();
        assert_eq!(
            s.dev,
            DeviceArgs {
                uri: None,
                channel: 1,
                buffer: 262_144,
                buffer_given: false,
                kernel_buffers: None,
                loopback: false,
                recorder: Recorder::Iio,
                bits: 16,
            }
        );
        let s = tx_settings(&cfg(
            &[
                ("uri", "ip:192.0.2.30"),
                ("channel", "2"),
                ("buffer", "65536"),
                ("kbuffers", "8"),
                ("loopback", "on"),
            ],
            &[],
        ))
        .unwrap();
        assert_eq!(s.dev.uri(), "ip:192.0.2.30");
        assert_eq!(s.dev.stream_channels(), ["voltage2", "voltage3"]);
        assert_eq!(s.dev.phy_channel(), "voltage1");
        assert_eq!((s.dev.buffer, s.dev.loopback), (65_536, true));
        assert_eq!(s.dev.kernel_buffers, Some(8));
        for bad in [
            ("uri", ""),
            ("channel", "0"),
            ("channel", "3"),
            ("buffer", "1000"),
            ("buffer", "512"),
            ("buffer", "8388608"),
            ("buffer", "x"),
            ("kbuffers", "1"),
            ("kbuffers", "65"),
            ("kbuffers", "x"),
            ("loopback", "maybe"),
            ("serial", "1"),
        ] {
            assert!(tx_settings(&cfg(&[bad], &[])).is_err(), "{bad:?}");
        }
        // Loopback and kbuffers are TX (and capture) arguments.
        let e = rx_settings(&cfg(&[("loopback", "on")], &[]))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("unknown device argument \"loopback\" (known: uri, channel, buffer)"),
            "{e}"
        );
        assert!(rx_settings(&cfg(&[("kbuffers", "8")], &[])).is_err());
        let e = tx_settings(&cfg(&[("recorder", "iio")], &[]))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("(known: uri, channel, buffer, kbuffers, loopback)"),
            "{e}"
        );
        // The variants, not only the messages.
        let rule = |arg| match tx_settings(&cfg(&[arg], &[])) {
            Err(RadioError::Argument {
                backend: "pluto",
                kind: Key::Argument,
                rule,
                ..
            }) => rule,
            other => panic!("{arg:?}: {other:?}"),
        };
        assert_eq!(rule(("uri", "")), Rule::Empty);
        assert_eq!(rule(("channel", "3")), Rule::OneOf(&["1", "2"]));
        assert!(matches!(rule(("buffer", "x")), Rule::NotANumber { .. }));
        assert!(matches!(
            rule(("buffer", "1000")),
            Rule::Multiple { of: 1024, .. }
        ));
        assert!(matches!(
            rx_settings(&cfg(&[("loopback", "on")], &[])),
            Err(RadioError::UnknownKey {
                kind: Key::Argument,
                ..
            })
        ));
        let e = tx_settings(&cfg(&[("buffer", "1000")], &[]))
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "pluto: buffer=1000: must be 1024..=4194304 samples in multiples of 1024"
        );
    }

    #[test]
    fn capture_arguments_and_limits() {
        let limit = |args: &[(&str, &str)]| Pluto.capture_limit(&cfg(args, &[]));
        let c = cfg(&[("uri", "ip:192.0.2.30"), ("recorder", "maia")], &[]);
        let s = rx_settings_for(&c, Dir::Capture).unwrap();
        assert_eq!((s.dev.recorder, s.dev.bits), (Recorder::Maia, 16));
        let (n, why) = Pluto.capture_limit(&c).unwrap().unwrap();
        assert_eq!(n, 33_554_432);
        assert!(why.contains("maia-sdr") && why.contains("128 MiB"), "{why}");
        // Samples per capture by recorder and bits.
        for (args, n) in [
            (vec![], 16_515_072),
            (vec![("bits", "8")], 33_030_144),
            (vec![("recorder", "maia"), ("bits", "12")], 44_739_242),
            (vec![("recorder", "maia"), ("bits", "8")], 67_108_864),
            (vec![("recorder", "stream")], u64::MAX),
            (vec![("recorder", "stream"), ("bits", "8")], u64::MAX),
        ] {
            assert_eq!(limit(&args).unwrap().unwrap().0, n, "{args:?}");
        }
        let s = rx_settings_for(&cfg(&[("bits", "8")], &[]), Dir::Capture).unwrap();
        assert_eq!(s.dev.capture_channels(), ["voltage0"]);
        assert_eq!(capture_plan(&s.dev).0, CaptureFormat::Cs8);
        let s = rx_settings_for(&cfg(&[("channel", "2")], &[]), Dir::Capture).unwrap();
        assert_eq!(s.dev.capture_channels(), ["voltage2", "voltage3"]);
        assert_eq!(
            capture_plan(&s.dev).0,
            CaptureFormat::Cs16 { full_scale: 2048.0 }
        );
        let s = rx_settings_for(
            &cfg(&[("recorder", "stream"), ("buffer", "65536")], &[]),
            Dir::Capture,
        )
        .unwrap();
        assert_eq!(s.dev.buffer, 65_536);
        // The symbol stream: 1 Mi-word buffers, 16 in the kernel by default
        // (plan §0.18); `buffer` and `kbuffers` set them, not with maia.
        let s = rx_settings_for(&cfg(&[], &[]), Dir::Capture).unwrap();
        assert_eq!(
            (
                s.dev.buffer,
                s.dev.kernel_buffers.unwrap_or(SYMBOLS_KERNEL_BUFFERS)
            ),
            (1_048_576, 16)
        );
        let s = rx_settings_for(
            &cfg(&[("buffer", "262144"), ("kbuffers", "8")], &[]),
            Dir::Capture,
        )
        .unwrap();
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (262_144, Some(8)));
        for bad in ["1", "65", "x"] {
            assert!(rx_settings_for(&cfg(&[("kbuffers", bad)], &[]), Dir::Capture).is_err());
        }
        assert!(rx_settings_for(&cfg(&[("kbuffers", "8")], &[]), Dir::Rx).is_err());
        assert!(
            rx_settings_for(
                &cfg(&[("recorder", "maia"), ("buffer", "65536")], &[]),
                Dir::Capture
            )
            .is_err()
        );
        let s = rx_settings_for(&cfg(&[("recorder", "stream")], &[]), Dir::Capture).unwrap();
        assert_eq!(s.dev.buffer, DEFAULT_STREAM_BUFFER_SAMPLES);
        // Refused combinations, and streaming arguments that do not apply.
        for (args, msg) in [
            (
                vec![("recorder", "maia"), ("buffer", "65536")],
                "buffer applies to recorder=stream and the symbol stream, not recorder=maia",
            ),
            (
                vec![("loopback", "on")],
                "uri, channel, recorder, bits, buffer",
            ),
            (vec![("bits", "12")], "bits=12 needs recorder=maia"),
            (
                vec![("bits", "12"), ("recorder", "stream")],
                "bits=12 needs recorder=maia",
            ),
            (vec![("bits", "10")], "want 8, 12 or 16"),
            (
                vec![("bits", "8"), ("channel", "2")],
                "bits=8 records RX1 only",
            ),
            (
                vec![("recorder", "maia"), ("channel", "2")],
                "recorder=maia records RX1 only, not channel=2",
            ),
            (vec![("recorder", "x")], "want iio, maia or stream"),
        ] {
            let e = limit(&args).unwrap_err().to_string();
            assert!(e.contains(msg), "{args:?}: {e}");
        }
        assert!(rx_settings(&cfg(&[("recorder", "iio")], &[])).is_err());
        assert!(rx_settings(&cfg(&[("bits", "8")], &[])).is_err());
        let d = Pluto.describe_capture(&c).unwrap();
        assert!(
            d.contains("recorder     maia-sdr recorder at http://192.0.2.30/"),
            "{d}"
        );
        assert!(
            d.contains("samples      cs16, 12 bits left-aligned (maia-sdr IQ16bit)"),
            "{d}"
        );
        let d = Pluto
            .describe_capture(&cfg(&[("recorder", "stream"), ("bits", "8")], &[]))
            .unwrap();
        assert!(
            d.contains("libiio stream, buffers of 1048576 samples"),
            "{d}"
        );
        assert!(d.contains("cs8 (Tezuka 8-bit packing)"), "{d}");
        assert!(Pluto.describe_rx(&c).is_err());
        // Refused before touching a device: too large, empty, maia over USB.
        let stop = AtomicBool::new(false);
        for (args, n) in [
            (vec![], 16_515_073),
            (vec![], 0),
            (vec![("recorder", "maia"), ("uri", "ip:h")], 33_554_433),
            (
                vec![("recorder", "maia"), ("uri", "ip:h"), ("bits", "8")],
                67_108_865,
            ),
            (vec![("recorder", "maia"), ("uri", "usb:1.2.3")], 1024),
        ] {
            assert!(
                Pluto
                    .capture(&cfg(&args, &[]), n, &mut std::io::sink(), &stop)
                    .is_err(),
                "{args:?} {n}"
            );
        }
        assert!(matches!(
            Pluto.capture(&cfg(&[], &[]), 16_515_073, &mut std::io::sink(), &stop),
            Err(RadioError::CaptureLength {
                limit: 16_515_072,
                ..
            })
        ));
        assert!(matches!(
            Pluto.capture(
                &cfg(&[("recorder", "maia"), ("uri", "usb:1.2.3")], &[]),
                1024,
                &mut std::io::sink(),
                &stop
            ),
            Err(RadioError::Conflict { second, .. }) if second == "uri=usb:1.2.3"
        ));
        assert!(matches!(
            limit(&[("bits", "12")]),
            Err(RadioError::Conflict { rule: "needs", .. })
        ));
        assert!(matches!(
            rx_settings_for(&cfg(&[("kbuffers", "65")], &[]), Dir::Capture),
            Err(RadioError::Argument {
                rule: Rule::Range { max: 64.0, .. },
                ..
            })
        ));
        // The maia-sdr recorder refuses more than its buffer before any
        // request.
        let m = maia::Mode(16);
        assert!(matches!(
            maia::record(
                "127.0.0.1",
                1e6,
                m.max_samples() + 1,
                m,
                &mut std::io::sink()
            ),
            Err(RadioError::CaptureLength { .. })
        ));
    }

    #[test]
    fn symbol_capture_checks_the_parameters_first() {
        let design = isdb_frontend::Design::new(40e6, 33.7561e6, 0.03);
        let mut p = isdb_frontend::Params::design(&design).unwrap();
        p.h.clear();
        let stop = AtomicBool::new(false);
        assert!(matches!(
            Pluto.capture_symbols(&cfg(&[], &[]), &p, 1, &mut std::io::sink(), &stop),
            Err(RadioError::FrontEndParams(_))
        ));
    }

    #[test]
    fn a_given_uri_is_used_as_is_and_sends_no_mdns_query() {
        let mut s = rx_settings(&cfg(&[("uri", "usb:1.4.5")], &[])).unwrap();
        assert_eq!(s.dev.uri_source(), "usb:1.4.5");
        // returns at once: the URI is given
        s.dev.resolve().unwrap();
        assert_eq!(s.dev.uri(), "usb:1.4.5");
    }

    #[test]
    fn without_a_uri_the_description_says_how_it_is_found() {
        let s = rx_settings(&cfg(&[], &[])).unwrap();
        let d = s.dev.uri_source();
        if env_uri().is_some() {
            assert!(d.ends_with("(ISDB_PLUTO_URI)"), "{d}");
        } else {
            assert_eq!(
                d,
                "found by mDNS (_iio._tcp.local or pluto.local) when opened"
            );
        }
    }

    #[test]
    fn discovery_errors_name_what_answered() {
        let e = |found: &[&str]| {
            RadioError::Discovery {
                backend: NAME,
                query: "_iio._tcp.local or pluto.local".into(),
                found: found.iter().map(ToString::to_string).collect(),
                hint: "give uri",
            }
            .to_string()
        };
        assert_eq!(
            e(&[]),
            "pluto: nothing answered the mDNS query for _iio._tcp.local or pluto.local; give uri"
        );
        assert_eq!(
            e(&["a (192.0.2.1)", "b (192.0.2.2)"]),
            "pluto: 2 devices answered the mDNS query for _iio._tcp.local or pluto.local (a (192.0.2.1), b (192.0.2.2)); give uri"
        );
    }

    #[test]
    fn library_failures_keep_the_library_error() {
        use std::error::Error as _;
        // No libiio backend accepts this URI; nothing touches the network.
        let mut s = rx_settings(&cfg(&[("uri", "bogus:nothing")], &[])).unwrap();
        let e = open(&mut s.dev).unwrap_err();
        assert!(
            matches!(&e, RadioError::Device { backend: "pluto", op, .. } if op == "open Pluto bogus:nothing")
        );
        let source = e.source().unwrap().downcast_ref::<isdb_iio::Error>();
        assert!(source.is_some(), "{e:?}");
    }

    #[test]
    fn tx_attenuation() {
        // The default is the minimum output power.
        assert_eq!(tx_settings(&cfg(&[], &[])).unwrap().att_db(), 89.75);
        for (v, q) in [("0", 0), ("10", 40), ("10.25", 41), ("89.75", 359)] {
            assert_eq!(
                tx_settings(&cfg(&[], &[("att", v)])).unwrap().att_qdb,
                q,
                "{v}"
            );
        }
        for bad in ["-1", "90", "10.1", "x", "NaN"] {
            assert!(tx_settings(&cfg(&[], &[("att", bad)])).is_err(), "{bad}");
        }
        let e = tx_settings(&cfg(&[], &[("txvga", "20")]))
            .unwrap_err()
            .to_string();
        assert!(
            e.ends_with(
                "has no gain \"txvga\" (gains: att=0..89.75 dB of attenuation in 0.25 dB steps (default 89.75))"
            ),
            "{e}"
        );
        assert!(matches!(
            tx_settings(&cfg(&[], &[("att", "1"), ("att", "2")])),
            Err(RadioError::RepeatedKey {
                kind: Key::Gain(Direction::Tx),
                ..
            })
        ));
        assert!(matches!(
            tx_settings(&cfg(&[], &[("att", "90")])),
            Err(RadioError::Argument {
                rule: Rule::Steps { step: 0.25, .. },
                ..
            })
        ));
        assert!(matches!(
            tx_settings(&cfg(&[], &[("txvga", "20")])),
            Err(RadioError::UnknownKey {
                kind: Key::Gain(Direction::Tx),
                ..
            })
        ));
    }

    #[test]
    fn rx_gains() {
        let s = rx_settings(&cfg(&[], &[])).unwrap();
        assert_eq!((s.agc, s.gain_db), ("slow_attack", 30));
        let s = rx_settings(&cfg(&[], &[("gain", "40")])).unwrap();
        assert_eq!((s.agc, s.gain_db), ("manual", 40));
        let s = rx_settings(&cfg(&[], &[("agc", "fast_attack")])).unwrap();
        assert_eq!(s.agc, "fast_attack");
        assert_eq!(describe_rx_gain(&s), "AGC fast_attack");
        assert!(rx_settings(&cfg(&[], &[("agc", "manual"), ("gain", "-3")])).is_ok());
        for bad in [
            vec![("gain", "80")],
            vec![("gain", "1.5")],
            vec![("agc", "auto")],
            vec![("agc", "slow_attack"), ("gain", "20")],
            vec![("att", "10")],
        ] {
            assert!(rx_settings(&cfg(&[], &bad)).is_err(), "{bad:?}");
        }
        let gain_rule = |gains: &[(&str, &str)]| match rx_settings(&cfg(&[], gains)) {
            Err(RadioError::Argument {
                kind: Key::Gain(Direction::Rx),
                rule,
                ..
            }) => rule,
            other => panic!("{gains:?}: {other:?}"),
        };
        assert!(matches!(gain_rule(&[("gain", "80")]), Rule::Range { .. }));
        assert!(matches!(
            gain_rule(&[("gain", "1.5")]),
            Rule::NotANumber { .. }
        ));
        assert_eq!(gain_rule(&[("agc", "auto")]), Rule::OneOf(&AGC_MODES));
        let e = rx_settings(&cfg(&[], &[("agc", "slow_attack"), ("gain", "20")])).unwrap_err();
        assert_eq!(
            e.to_string(),
            "pluto: gain is the manual gain; it cannot be combined with agc=slow_attack"
        );
        assert!(matches!(e, RadioError::Conflict { .. }));
    }

    #[test]
    fn tuning_ranges() {
        let mut c = cfg(&[], &[]);
        for (f, tx, rx) in [
            (46_999_999, false, false),
            (47_000_000, true, false),
            (70_000_000, true, true),
            (6_000_000_000, true, true),
            (6_000_000_001, false, false),
        ] {
            c.center_hz = f;
            assert_eq!(tx_settings(&c).is_ok(), tx, "{f}");
            assert_eq!(rx_settings(&c).is_ok(), rx, "{f}");
        }
        let mut c = cfg(&[], &[]);
        for (num, den, ok) in [(2_000_000, 1, false), (61_440_000, 1, true), (1, 0, false)] {
            (c.rate_num, c.rate_den) = (num, den);
            assert_eq!(tx_settings(&c).is_ok(), ok, "{num}/{den}");
        }
        let mut c = cfg(&[], &[]);
        for (bw, tx, rx) in [
            (199_999, false, false),
            (200_000, true, true),
            (40_000_000, true, true),
            (56_000_000, false, true),
        ] {
            c.bandwidth_hz = bw;
            assert_eq!(tx_settings(&c).is_ok(), tx, "{bw}");
            assert_eq!(rx_settings(&c).is_ok(), rx, "{bw}");
        }
        c.bandwidth_hz = 50_000_000;
        let e = tx_settings(&c).unwrap_err();
        assert!(matches!(
            e,
            RadioError::Tuning {
                what: "RF bandwidth",
                ..
            }
        ));
        assert_eq!(
            e.to_string(),
            "RF bandwidth 50000000 Hz outside the Pluto range 0.2..40 MHz"
        );
        let mut c = cfg(&[], &[]);
        c.center_hz = 46_999_999;
        assert_eq!(
            tx_settings(&c).unwrap_err().to_string(),
            "frequency 46999999 Hz outside the Pluto (AD9361) range 47..6000 MHz"
        );
        let mut c = cfg(&[], &[]);
        c.rate_num = 2_000_000;
        c.rate_den = 1;
        assert_eq!(
            tx_settings(&c).unwrap_err().to_string(),
            "sample rate 2000000 Hz outside the Pluto (AD9361) range 2.083334..61.44 MHz"
        );
    }

    #[test]
    fn path_rates_and_ranges() {
        // Read from a PlutoSDR set to 8126984 Hz: BBPLL / 128.
        let r = "BBPLL:1040253944 DAC:65015871 T2:32507935 T1:16253967 TF:8126983 TXSAMP:8126983";
        let got = path_rate(r, "TXSAMP").unwrap();
        assert!((got - 8_126_983.937_5).abs() < 1e-6, "{got}");
        assert!(((got / (512e6 / 63.0) - 1.0) * 1e6 + 0.0234).abs() < 1e-3);
        let r =
            "BBPLL:983040000 ADC:245760000 R2:122880000 R1:61440000 RF:30720000 RXSAMP:30720000";
        assert_eq!(path_rate(r, "RXSAMP"), Some(30_720_000.0));
        assert_eq!(path_rate(r, "TXSAMP"), None);
        assert_eq!(path_rate("", "RXSAMP"), None);
        assert_eq!(parse_range("[-3 1 71]"), Some((-3.0, 71.0)));
        assert_eq!(parse_range("[-89.750000]"), None);
        assert_eq!(parse_range("A B"), None);
    }

    #[test]
    fn rx_samples_are_scaled_by_2048() {
        let v = samples_from_cs12(&[2047, -2048, 0, 1024, 7]);
        assert_eq!(v, vec![C32::new(2047.0 / 2048.0, -1.0), C32::new(0.0, 0.5)]);
    }

    #[test]
    fn descriptions() {
        let d = Pluto
            .describe_tx(&cfg(&[("uri", "ip:192.0.2.30")], &[]), &OPTS)
            .unwrap();
        assert!(d.contains("device       Pluto ip:192.0.2.30, TX1"), "{d}");
        assert!(
            d.contains("sample rate 512000000/63 Hz = 8.126984 MHz"),
            "{d}"
        );
        assert!(d.contains("rf bandwidth 6.00 MHz requested"), "{d}");
        assert!(d.contains("TX atten     89.75 dB (maximum)"), "{d}");
        assert!(!d.contains("loopback"), "{d}");
        assert!(
            d.contains("10.0 dB below cs12 full scale (RMS 647.3 of 2047)"),
            "{d}"
        );
        let d = Pluto
            .describe_tx(&cfg(&[("loopback", "on")], &[("att", "20")]), &OPTS)
            .unwrap();
        assert!(d.contains("TX atten     20 dB\n"), "{d}");
        assert!(d.contains("nothing radiated"), "{d}");
        let o = TxOptions {
            backoff_db: 41.0,
            ..OPTS
        };
        assert!(Pluto.describe_tx(&cfg(&[], &[]), &o).is_err());
        let d = Pluto
            .describe_rx(&cfg(
                &[("uri", "ip:192.0.2.30"), ("channel", "2")],
                &[("gain", "40")],
            ))
            .unwrap();
        assert!(d.contains("device       Pluto ip:192.0.2.30, RX2"), "{d}");
        assert!(d.contains("RX gain      manual 40 dB"), "{d}");
    }

    /// The BS1 IF at 40 Msps with the 40 MHz filter (`isdb tx --system
    /// isdbs`).
    fn sat_cfg(args: &[(&str, &str)]) -> RadioConfig {
        RadioConfig {
            center_hz: 1_049_480_000,
            nominal_hz: 1_049_480_000,
            rate_num: 40_000_000,
            rate_den: 1,
            bandwidth_hz: 40_000_000,
            ..cfg(args, &[])
        }
    }

    /// The default ISDB-S block's parameters (40 Msps, 10 dB).
    fn tx_params() -> TxParams {
        TxParams::design(&isdb_frontend::shape::fixed::TxDesign::new(
            40e6, 28.86e6, 0.35,
        ))
        .unwrap()
    }

    const SAT_OPTS: TxOptions = TxOptions {
        prefill_frames: 2,
        frame_seconds: 0.011_07,
        backoff_db: 10.0,
    };

    /// TX symbol codes: TX1 only, the code stream's buffers by default,
    /// `buffer` and `kbuffers` as given; the description names the block,
    /// the link rate, the buffers and the level in the banks.
    #[test]
    fn tx_symbols_settings_and_description() {
        let s = tx_symbols_settings(&sat_cfg(&[])).unwrap();
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (65_536, Some(16)));
        let s = tx_symbols_settings(&sat_cfg(&[("buffer", "32768"), ("kbuffers", "8")])).unwrap();
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (32_768, Some(8)));
        // the IQ path keeps its defaults
        let s = tx_settings(&sat_cfg(&[])).unwrap();
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (262_144, None));
        let e = tx_symbols_settings(&sat_cfg(&[("channel", "2")])).unwrap_err();
        assert!(matches!(e, RadioError::Conflict { .. }), "{e:?}");
        assert_eq!(
            e.to_string(),
            "pluto: channel=2 is not wired to the FPGA transmit block, which drives TX1 (channel=1)"
        );
        let p = tx_params();
        let d = Pluto
            .describe_tx_symbols(
                &sat_cfg(&[("uri", "ip:192.0.2.30"), ("loopback", "on")]),
                &SAT_OPTS,
                &p,
            )
            .unwrap();
        for want in [
            "device       Pluto ip:192.0.2.30, TX1",
            "sample rate 40000000/1 Hz = 40.000000 MHz",
            "TX atten     89.75 dB (maximum)",
            "nothing radiated",
            "modulator    FPGA transmit block of cf-ad9361-dds-core-lpc (SymbolsTx, TX format 1): symbol codes shaped on the board, banks of 512 phases × 24 taps, 1.3860 samples/symbol (step 0xb8b439)",
            "link         4-bit symbol codes, 8 per DMA sample (voltage0 + voltage1): 14.43 MB/s at 28.860 Mbaud; buffers of 65536 samples (18.2 ms), kbuffers 16 in the kernel (291 ms)",
            "level        RMS 10.0 dB below the 12-bit DAC full scale (647.6 LSB), in the block's banks (shift 7)",
            "configure    13315 commands",
        ] {
            assert!(d.contains(want), "{want:?} not in\n{d}");
        }
        assert!(!d.contains("cs12"), "no quantiser level:\n{d}");
        // refused without a device: a channel the block does not drive,
        // parameters that do not validate, options out of range
        let stop_here = |r: Result<(Box<dyn SymbolSink>, String)>| r.map(|_| ()).unwrap_err();
        assert!(matches!(
            stop_here(Pluto.open_tx_symbols(&sat_cfg(&[("channel", "2")]), &SAT_OPTS, &p)),
            RadioError::Conflict { .. }
        ));
        let mut bad = p.clone();
        bad.a[0][0] += 1;
        assert!(matches!(
            Pluto.describe_tx_symbols(&sat_cfg(&[]), &SAT_OPTS, &bad),
            Err(RadioError::ModulatorParams(_))
        ));
        assert!(matches!(
            stop_here(Pluto.open_tx_symbols(&sat_cfg(&[]), &SAT_OPTS, &bad)),
            RadioError::ModulatorParams(_)
        ));
        let o = TxOptions {
            backoff_db: 41.0,
            ..SAT_OPTS
        };
        assert!(Pluto.describe_tx_symbols(&sat_cfg(&[]), &o, &p).is_err());
    }

    /// The DAC core with the transmit block, as the HDL decodes its
    /// commands (fpga/README.md "The transmit block"): a command executes
    /// when bit 31 of `gpio_out` toggles, READ shows a register in
    /// `gpio_in` and any other command the ID word; `r1_mode` (0x48) and
    /// the status register (0x88, write 1 to clear) beside it.
    #[derive(Default)]
    struct Dac {
        id: u32,
        gpio_out: u32,
        gpio_in: u32,
        r1: u32,
        status: u32,
        addr: u16,
        staged: u32,
        regs: std::collections::BTreeMap<u16, u32>,
        ctrl: Vec<u32>,
        run: bool,
        selected: bool,
        /// A block that ignores SELECT.
        deaf: bool,
        /// What STEP reads (`None`: what was written).
        step_reads: Option<u32>,
        consumed: u32,
        missed: u32,
        first_miss: u32,
        underrun: bool,
        writes: usize,
    }

    struct FakeDac(std::cell::RefCell<Dac>);

    impl FakeDac {
        fn new(id: u32) -> Self {
            Self(std::cell::RefCell::new(Dac {
                id,
                gpio_in: id,
                ..Dac::default()
            }))
        }
    }

    impl Dac {
        fn execute(&mut self, w: u32) {
            let arg = (w >> 1) & 0x07FF_FFFF;
            let lo = arg & 0xFFFF;
            self.gpio_in = self.id;
            match (w >> 28) & 7 {
                0 => {}
                1 => self.addr = u16::try_from(lo).unwrap(),
                2 => self.staged = lo,
                3 => {
                    self.regs.insert(self.addr, self.staged << 16 | lo);
                    self.addr += 1;
                }
                4 => {
                    self.gpio_in = match self.addr {
                        txc::reg::STEP => self.step_reads.unwrap_or(self.regs[&txc::reg::STEP]),
                        txc::reg::STATUS => {
                            u32::from(self.run)
                                | u32::from(self.selected) << 1
                                | u32::from(self.underrun) << 2
                        }
                        txc::reg::CONSUMED => self.consumed,
                        txc::reg::MISSED => self.missed,
                        txc::reg::FIRST_MISS => self.first_miss,
                        a => self.regs.get(&a).copied().unwrap_or(0),
                    }
                }
                5 => {
                    self.ctrl.push(arg);
                    if arg & txc::ctrl::START != 0 {
                        (self.consumed, self.missed, self.first_miss) = (0, 0, 0);
                        self.underrun = false;
                    }
                    self.run = arg & txc::ctrl::RUN != 0;
                    self.selected = arg & txc::ctrl::SELECT != 0 && !self.deaf;
                }
                6 => {
                    let v = ((arg << 5).cast_signed() >> 5).cast_unsigned();
                    self.regs.insert(self.addr, v);
                    self.addr += 1;
                }
                op => panic!("{w:#010x}: op {op}"),
            }
        }
    }

    impl Regs for FakeDac {
        fn read(&self, addr: u32) -> Result<u32> {
            let d = self.0.borrow();
            Ok(match addr {
                txc::GPIO_IN_REG => d.gpio_in,
                txc::GPIO_OUT_REG => d.gpio_out,
                DAC_R1_MODE_REG => d.r1,
                DAC_STATUS_REG => d.status,
                _ => panic!("read of {addr:#010x}"),
            })
        }
        fn write(&self, addr: u32, value: u32) -> Result<()> {
            let mut d = self.0.borrow_mut();
            d.writes += 1;
            match addr {
                txc::GPIO_OUT_REG => {
                    assert_eq!(value & 1, d.gpio_out & 1, "bit 0 of gpio_out changed");
                    let toggled = (value ^ d.gpio_out) >> 31 != 0;
                    d.gpio_out = value;
                    if toggled {
                        d.execute(value);
                    }
                }
                DAC_STATUS_REG => d.status &= !value,
                _ => panic!("write of {addr:#010x}"),
            }
            Ok(())
        }
    }

    /// The host's sequence against the block's command decoding: probe,
    /// configuration with STEP read back, CTRL START|SELECT (selected, not
    /// running), RUN|SELECT once the stream runs with the DAC underflow
    /// flag cleared, MISSED per status line, the counters, CTRL 0; the ID
    /// word in `gpio_in` after every read.
    #[test]
    fn transmit_block_sequence() {
        let p = tx_params();
        let dac = FakeDac::new(p.id_word());
        assert_eq!(
            TxBlock::probe(&dac).unwrap(),
            TxIdentity::parse(0xB5D1_1918)
        );
        assert_eq!(TxBlock::probe(&FakeDac::new(0)).unwrap(), None);
        let mut b = TxBlock::open(&dac, &p).unwrap();
        let both = txc::ctrl::START | txc::ctrl::SELECT;
        {
            let d = dac.0.borrow();
            assert_eq!(d.ctrl, [both]);
            assert!(d.selected && !d.run);
            assert_eq!(d.regs[&txc::reg::STEP], 0xB8_B439);
            assert_eq!(d.regs[&txc::reg::SHIFT], u32::from(p.shift));
            assert_eq!(d.regs[&txc::reg::A_BASE].cast_signed(), p.a[0][0]);
            let last_b = txc::reg::B_BASE + 511 * txc::reg::ROW_STRIDE + 11;
            assert_eq!(d.regs[&last_b].cast_signed(), p.b[511][11]);
            assert_eq!(d.gpio_in, p.id_word());
            assert_eq!(
                d.writes,
                Encoder::new(0).configure_tx(&p).len() + 2 * 3 + 1,
                "configuration, two reads (ADDR, READ, NOP) and CTRL"
            );
        }
        // the flag the idle DAC set before the stream
        dac.0.borrow_mut().status = DAC_STATUS_UNDERFLOW;
        b.run(&dac).unwrap();
        {
            let d = dac.0.borrow();
            assert_eq!(d.ctrl, [both, txc::ctrl::RUN | txc::ctrl::SELECT]);
            assert!(d.run && d.selected);
            assert_eq!(d.status, 0, "the DAC underflow flag cleared");
        }
        dac.0.borrow_mut().missed = 16;
        assert_eq!(b.status(&dac).unwrap(), 16);
        assert_eq!(b.status(&dac).unwrap(), 16);
        {
            let mut d = dac.0.borrow_mut();
            (d.consumed, d.first_miss, d.underrun) = (319_488, 640, true);
            d.status = DAC_STATUS_UNDERFLOW;
        }
        let c = b.counters(&dac).unwrap();
        assert_eq!(
            c,
            TxCounters {
                status: 7,
                consumed: 319_488,
                missed: 16,
                first_miss: 640,
                underflow: true
            }
        );
        assert_eq!(
            c.describe(),
            "CONSUMED 319488, MISSED 16, FIRST_MISS 640, STATUS 0x7; DAC underflow flag set"
        );
        assert_eq!(dac.0.borrow().gpio_in, p.id_word());
        b.release(&dac).unwrap();
        let d = dac.0.borrow();
        assert_eq!(d.ctrl.last(), Some(&0));
        assert!(!d.selected && !d.run);
        let clean = TxCounters {
            status: 3,
            consumed: 8,
            missed: 0,
            first_miss: 0,
            underflow: false,
        };
        assert_eq!(
            clean.describe(),
            "CONSUMED 8, MISSED 0, FIRST_MISS -, STATUS 0x3; DAC underflow flag clear"
        );
    }

    /// Refused before a command: r1_mode, `gpio_out` bit 0. Refused after
    /// the configuration, with CTRL 0 sent: a block that does not take the
    /// DAC on SELECT, a STEP that does not read back; and a block that
    /// does not run on RUN|SELECT.
    #[test]
    fn transmit_block_refusals() {
        let p = tx_params();
        let dac = FakeDac::new(p.id_word());
        dac.0.borrow_mut().r1 = DAC_R1_MODE_BIT;
        let e = TxBlock::open(&dac, &p).unwrap_err().to_string();
        assert!(e.contains("r1_mode"), "{e}");
        assert_eq!(dac.0.borrow().writes, 0);
        let dac = FakeDac::new(p.id_word());
        dac.0.borrow_mut().gpio_out = 0x8000_0001;
        let e = TxBlock::open(&dac, &p).unwrap_err().to_string();
        assert!(e.contains("gpio_out set") && e.contains("TX FIR"), "{e}");
        assert_eq!(dac.0.borrow().writes, 0);
        let dac = FakeDac::new(p.id_word());
        dac.0.borrow_mut().deaf = true;
        let e = TxBlock::open(&dac, &p).unwrap_err().to_string();
        assert!(
            e.contains("reads 0x0 after CTRL START|SELECT")
                && e.contains("codes sent to a plain DAC would go out as samples"),
            "{e}"
        );
        assert_eq!(dac.0.borrow().ctrl, [6, 0]);
        let dac = FakeDac::new(p.id_word());
        dac.0.borrow_mut().step_reads = Some(0x80_0000);
        let e = TxBlock::open(&dac, &p).unwrap_err().to_string();
        assert!(
            e.contains("STEP: reads 0x800000 after writing 0xb8b439"),
            "{e}"
        );
        assert_eq!(dac.0.borrow().ctrl, [0]);
        let dac = FakeDac::new(p.id_word());
        let mut b = TxBlock::open(&dac, &p).unwrap();
        dac.0.borrow_mut().deaf = true;
        let e = b.run(&dac).unwrap_err().to_string();
        assert!(e.contains("not running and selected"), "{e}");
    }

    #[test]
    fn the_block_starts_on_the_first_buffer() {
        use crate::mock::MockCounters;
        let c = MockCounters::default();
        c.buffers.store(1, Ordering::Relaxed);
        wait_first_buffer(&c, Duration::from_millis(10)).unwrap();
        let c = MockCounters::default();
        assert!(matches!(
            wait_first_buffer(&c, Duration::from_millis(10)),
            Err(RadioError::Stream {
                problem: StreamProblem::Stalled { .. },
                ..
            })
        ));
        c.finished.store(true, Ordering::SeqCst);
        assert!(matches!(
            wait_first_buffer(&c, Duration::from_millis(10)),
            Err(RadioError::Stream {
                problem: StreamProblem::Ended,
                ..
            })
        ));
    }

    /// The prefill covers the device's queue: the kernel buffers and the
    /// one being filled, two components per sample.
    #[test]
    fn what_the_device_takes_ahead() {
        let s = tx_symbols_settings(&sat_cfg(&[])).unwrap();
        let ahead = queue_components(s.dev.kernel_buffers.unwrap(), s.dev.buffer);
        assert_eq!(ahead, 17 * 65_536 * 2);
        // 28 superframes of codes (39 936 DMA samples each) cover it
        assert_eq!((ahead / 2).div_ceil(39_936), 28);
        // IQ at 512/63 MHz: 5 buffers, less than the 2 frames of the prefill
        assert_eq!(queue_components(KERNEL_BUFFERS, 262_144), 5 * 262_144 * 2);
    }
}
