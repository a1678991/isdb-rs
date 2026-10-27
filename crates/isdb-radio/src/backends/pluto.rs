//! ADALM-Pluto family backend (`pluto`): AD936x radios running ADI's Pluto
//! firmware or a derivative such as Tezuka (ADALM-Pluto, PlutoSky R2, …),
//! through `isdb-iio` / libiio.
//!
//! * Device arguments:
//!   * `uri`: libiio context URI (default `ip:pluto.local`), e.g.
//!     `ip:192.168.2.1` (USB RNDIS), `ip:192.0.2.30` (Ethernet), `usb:1.4.5`.
//!     The network backend streams over iiod; Gigabit Ethernet keeps up
//!     with 512/63 MHz (32.5 MB/s each way).
//!   * `channel`: `1` or `2` (TX1/RX1 or TX2/RX2 port, default 1); 2 needs
//!     the firmware's `2r2t` mode.
//!   * `buffer`: samples per IIO buffer, 1024..=4194304 in multiples of
//!     1024 (default 262144: 32 ms at 512/63 MHz); 4 such buffers are queued
//!     in the device kernel.
//!   * TX only, `loopback`: `on` routes the TX samples digitally back into
//!     the RX path inside the AD936x (its BIST data-port loopback) and
//!     powers the TX LO down, so nothing is radiated; an `isdb rx` on the
//!     same device then receives the transmission. Default `off`. Closing
//!     the TX turns the loopback off again.
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

use super::maia;
use crate::{
    Backend, Capture, CaptureFormat, CaptureGap, Cs12, IqSink, IqSource, Offload, Quantizer,
    QueuedSink, RadioConfig, RadioTx, RxOptions, StreamCounters, SymbolsCapture, TxOptions,
    TxStream,
};
use anyhow::{Context as _, Result, bail};
use isdb_frontend::control::{self, Encoder, GPIO_IN_REG, GPIO_OUT_REG, Identity, Op, reg};
use isdb_iio::Context;
use isdbt_ofdm::C32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use tracing::{debug, info, warn};

/// The `pluto` backend.
pub(crate) struct Pluto;

const NAME: &str = "pluto";
const DEFAULT_URI: &str = "ip:pluto.local";
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
/// The ADC core's interface status register (ADI `up_adc_common`
/// `REG_STATUS`, 0x0022 × 4, through the HDL-core bit 31) and its
/// overflow bit (sticky, write 1 to clear).
const ADC_STATUS_REG: u32 = 0x8000_0088;
const ADC_STATUS_OVERFLOW: u32 = 0x4;
/// `axi_ad9361` common control register (ADI `up_adc_common` / `up_dac_common`
/// REG 0x11): bit 2 is `r1_mode`. With r1_mode set on the ADC and the DAC,
/// `util_ad9361_divclk` takes its other input (l_clk / 2 instead of / 4 in
/// LVDS), a case the symbol front end's bitstream is not timed for
/// (fpga/README.md "Clock"), so the FPGA path is refused then.
const R1_MODE_REG: u32 = 0x8000_0044;
const R1_MODE_BIT: u32 = 1 << 2;
/// Settling time between programming the RX and capturing (LO lock, AGC).
const CAPTURE_SETTLE: Duration = Duration::from_millis(100);

const PHY: &str = "ad9361-phy";
const TX_DEV: &str = "cf-ad9361-dds-core-lpc";
const RX_DEV: &str = "cf-ad9361-lpc";
const RX_LO: &str = "altvoltage0";
const TX_LO: &str = "altvoltage1";

/// RX gain control modes of the AD936x driver.
const AGC_MODES: [&str; 4] = ["manual", "slow_attack", "fast_attack", "hybrid"];

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
    uri: String,
    /// 1 or 2.
    channel: u8,
    buffer: usize,
    /// `buffer` was given.
    buffer_given: bool,
    /// Kernel buffers of the symbol stream (`kbuffers`).
    kernel_buffers: u32,
    loopback: bool,
    recorder: Recorder,
    /// Bits per component of a capture: 8, 12 or 16.
    bits: u8,
}

impl DeviceArgs {
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
        _ => bail!("{NAME}: {k}={v}: want on or off"),
    }
}

/// Parse the device arguments; `loopback` only for TX, `recorder` only
/// for captures (which have no streaming `buffer`).
fn device_args(cfg: &RadioConfig, dir: Dir) -> Result<DeviceArgs> {
    let mut a = DeviceArgs {
        uri: DEFAULT_URI.into(),
        channel: 1,
        buffer: DEFAULT_BUFFER_SAMPLES,
        buffer_given: false,
        kernel_buffers: SYMBOLS_KERNEL_BUFFERS,
        loopback: false,
        recorder: Recorder::Iio,
        bits: 16,
    };
    let known = match dir {
        Dir::Tx => "uri, channel, buffer, loopback",
        Dir::Rx => "uri, channel, buffer",
        Dir::Capture => {
            "uri, channel, recorder, bits, buffer (recorder=stream and the symbol stream), kbuffers (the symbol stream)"
        }
    };
    for (k, v) in &cfg.args {
        match k.as_str() {
            "uri" if v.is_empty() => bail!("{NAME}: empty uri"),
            "uri" => a.uri = v.clone(),
            "channel" => {
                a.channel = match v.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => bail!("{NAME}: channel={v}: want 1 or 2"),
                }
            }
            "buffer" => {
                let n: usize = v
                    .parse()
                    .with_context(|| format!("{NAME}: buffer={v}: not a number of samples"))?;
                if !BUFFER_SAMPLES.contains(&n) || !n.is_multiple_of(1024) {
                    bail!(
                        "{NAME}: buffer={v}: must be {}..={} samples in multiples of 1024",
                        BUFFER_SAMPLES.start(),
                        BUFFER_SAMPLES.end()
                    );
                }
                a.buffer = n;
                a.buffer_given = true;
            }
            "kbuffers" if dir == Dir::Capture => {
                let n: u32 = v
                    .parse()
                    .with_context(|| format!("{NAME}: kbuffers={v}: not a number"))?;
                if !SYMBOLS_KERNEL_BUFFERS_RANGE.contains(&n) {
                    bail!(
                        "{NAME}: kbuffers={v}: must be {}..={}",
                        SYMBOLS_KERNEL_BUFFERS_RANGE.start(),
                        SYMBOLS_KERNEL_BUFFERS_RANGE.end()
                    );
                }
                a.kernel_buffers = n;
            }
            "loopback" if dir == Dir::Tx => a.loopback = on_off(k, v)?,
            "recorder" if dir == Dir::Capture => {
                a.recorder = match v.as_str() {
                    "iio" => Recorder::Iio,
                    "maia" => Recorder::Maia,
                    "stream" => Recorder::Stream,
                    _ => bail!("{NAME}: recorder={v}: want iio, maia or stream"),
                }
            }
            "bits" if dir == Dir::Capture => {
                a.bits = match v.as_str() {
                    "8" => 8,
                    "12" => 12,
                    "16" => 16,
                    _ => bail!("{NAME}: bits={v}: want 8, 12 or 16"),
                }
            }
            _ => bail!("{NAME}: unknown device argument {k:?} (known: {known})"),
        }
    }
    if dir == Dir::Capture {
        if a.buffer_given && a.recorder == Recorder::Maia {
            bail!(
                "{NAME}: buffer applies to recorder=stream and the symbol stream, not recorder=maia"
            );
        }
        if !a.buffer_given {
            a.buffer = DEFAULT_STREAM_BUFFER_SAMPLES;
        }
        if a.bits == 12 && a.recorder != Recorder::Maia {
            bail!("{NAME}: bits=12 needs recorder=maia");
        }
        if (a.bits == 8 || a.recorder == Recorder::Maia) && a.channel != 1 {
            bail!(
                "{NAME}: {} records RX1 only",
                if a.recorder == Recorder::Maia {
                    "the maia-sdr recorder"
                } else {
                    "bits=8"
                }
            );
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
    if !freq.contains(&cfg.center_hz) {
        bail!(
            "frequency {} Hz outside the Pluto (AD9361) range {}..{} MHz",
            cfg.center_hz,
            freq.start() / 1_000_000,
            freq.end() / 1_000_000
        );
    }
    if cfg.rate_den == 0 || !RATE_HZ.contains(&cfg.sample_rate_hz()) {
        bail!(
            "sample rate {}/{} Hz outside the Pluto (AD9361) range 2.083334..61.44 MHz",
            cfg.rate_num,
            cfg.rate_den
        );
    }
    if !bw.contains(&cfg.bandwidth_hz) {
        bail!(
            "RF bandwidth {} Hz outside the Pluto range {:.1}..{} MHz",
            cfg.bandwidth_hz,
            f64::from(*bw.start()) / 1e6,
            bw.end() / 1_000_000
        );
    }
    Ok(())
}

/// Reject repeated and unknown gain names (`known` with their help text).
fn check_gain_names(cfg: &RadioConfig, dir: &str, known: &[(&str, String)]) -> Result<()> {
    for (i, (k, _)) in cfg.gains.iter().enumerate() {
        if !known.iter().any(|(n, _)| n == k) {
            let help: Vec<&str> = known.iter().map(|(_, h)| h.as_str()).collect();
            bail!(
                "{NAME} {dir} has no gain {k:?} (gains: {})",
                help.join(", ")
            );
        }
        if cfg.gains[..i].iter().any(|(p, _)| p == k) {
            bail!("{NAME} gain {k:?} given twice");
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
        "TX",
        &[(
            "att",
            "att=0..89.75 dB of attenuation in 0.25 dB steps (default 89.75)".into(),
        )],
    )?;
    let att_qdb = match gain(cfg, "att") {
        None => MAX_ATT_QDB,
        Some(v) => {
            let db: f64 = v
                .parse()
                .with_context(|| format!("{NAME} gain att={v}: not a number of dB"))?;
            // The quarter-dB step equal to db * 4, if there is one.
            let q = db * 4.0;
            let Some(q) = (0..=MAX_ATT_QDB).find(|&n| f64::from(n) == q) else {
                bail!("{NAME} gain att={v}: must be 0..89.75 dB in 0.25 dB steps");
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
        "RX",
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
        Some(v) => Some(
            v.parse::<i32>()
                .ok()
                .filter(|g| (-10..=77).contains(g))
                .with_context(|| format!("{NAME} gain gain={v}: want whole dB in -10..77"))?,
        ),
    };
    let agc = match gain(cfg, "agc") {
        None if gain_db.is_some() => "manual",
        None => "slow_attack",
        Some(v) => match AGC_MODES.iter().find(|m| **m == v) {
            Some(m) => *m,
            None => bail!("{NAME} gain agc={v}: want {}", AGC_MODES.join(", ")),
        },
    };
    if gain_db.is_some() && agc != "manual" {
        bail!("{NAME}: gain is the manual gain; it cannot be combined with agc={agc}");
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
        dev.uri,
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
        let mut d = format!(
            "{}\n  TX atten     {} dB{}",
            describe_device(&s.dev, "TX", cfg),
            s.att_db(),
            if s.att_qdb == MAX_ATT_QDB {
                " (maximum)"
            } else {
                ""
            }
        );
        if s.dev.loopback {
            d += "\n  loopback     digital TX→RX inside the AD936x, TX LO powered down (nothing radiated)";
        }
        d += &format!(
            "\n  backoff      {}",
            Quantizer::<Cs12>::new(opts.backoff_db).describe_level()
        );
        Ok(d)
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
        let s = tx_settings(cfg)?;
        opts.validate()?;
        let ctx = open(&s.dev.uri)?;
        let mut info = device_info(&ctx);
        let tx = PlutoTx {
            ctx,
            channels: s.dev.stream_channels(),
            phy_channel: s.dev.phy_channel(),
            buffer: s.dev.buffer,
            loopback: s.dev.loopback,
        };
        // From here on, dropping `tx` restores the safe state.
        tx.configure(cfg, &s, &mut info)
            .with_context(|| format!("configure Pluto {} for TX", s.dev.uri))?;
        let gain = format!("TX{} attenuation {} dB", s.dev.channel, s.att_db());
        info!(
            backend = NAME,
            uri = s.dev.uri,
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            loopback = s.dev.loopback,
            "Pluto {} opened for TX: {}; {gain}{}",
            s.dev.uri,
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
                Recorder::Maia => format!(
                    "maia-sdr recorder at http://{}/",
                    s.dev.uri.strip_prefix("ip:").unwrap_or("?")
                ),
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
        let s = rx_settings_for(cfg, Dir::Capture)?;
        let (format, limit, why) = capture_plan(&s.dev);
        if samples == 0 || samples > limit {
            bail!("{NAME}: capture of {samples} samples: 1..={limit} ({why})");
        }
        let host = match s.dev.recorder {
            Recorder::Maia => Some(s.dev.uri.strip_prefix("ip:").with_context(|| {
                format!(
                    "{NAME}: recorder=maia needs a network uri (ip:HOST), not {}",
                    s.dev.uri
                )
            })?),
            _ => None,
        };
        let mut ctx = open(&s.dev.uri)?;
        let device = device_info(&ctx);
        if s.dev.bits == 8
            && s.dev.recorder != Recorder::Maia
            && !ctx
                .attr("fw_version")
                .is_some_and(|v| v.starts_with("tezuka"))
        {
            bail!(
                "{NAME}: bits=8 through libiio needs Tezuka's 8-bit packing (firmware {}); use recorder=maia or bits=16",
                ctx.attr("fw_version").unwrap_or_else(|| "?".into())
            );
        }
        let mut info = String::new();
        configure_rx(&ctx, cfg, &s, &mut info)
            .with_context(|| format!("configure Pluto {} for RX", s.dev.uri))?;
        std::thread::sleep(CAPTURE_SETTLE);
        let r1 = |dev: &str| -> Result<bool> {
            Ok(ctx.device(dev)?.reg_read(R1_MODE_REG)? & R1_MODE_BIT != 0)
        };
        if r1(RX_DEV)? && r1(TX_DEV)? {
            bail!(
                "{NAME}: the AD9361 core runs in r1_mode (one channel per direction): the symbol front end's clock is then l_clk / 2, which its bitstream is not timed for; use the CPU front end (--offload none)"
            );
        }
        let rate = {
            let rates = ctx.device(PHY)?.read("rx_path_rates")?;
            path_rate(&rates, "RXSAMP")
                .with_context(|| format!("{PHY} rx_path_rates {rates:?}: no RX rate"))?
        };
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri,
            rate_hz = rate,
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            samples,
            recorder = ?s.dev.recorder,
            bits = s.dev.bits,
            "Pluto {} opened for a capture of {samples} samples: {}; RX{} gain {gain}; {:?} recorder, {} bits per component",
            s.dev.uri,
            cfg.describe_tuning(),
            s.dev.channel,
            s.dev.recorder,
            s.dev.bits
        );
        debug!(backend = NAME, "{device}{info}");
        let (written, gaps, method) = match (s.dev.recorder, host) {
            (Recorder::Maia, Some(h)) => {
                let m = maia::Mode(s.dev.bits);
                maia::record(h, rate, samples, m, out)?;
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
                let n = usize::try_from(samples)?;
                let bytes = ctx
                    .capture(RX_DEV, s.dev.capture_channels(), n, 1)
                    .with_context(|| format!("capture {samples} samples in one libiio buffer"))?;
                out.write_all(&bytes[..n * format.bytes_per_sample()])
                    .context("write the capture")?;
                (samples, Vec::new(), "libiio single buffer".to_string())
            }
        };
        let phy = ctx.device(PHY)?;
        let ch = phy.channel(s.dev.phy_channel(), false)?;
        let gain_db = ch.read_f64("hardwaregain")?;
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
            lo_hz: u64::try_from(phy.channel(RX_LO, true)?.read_i64("frequency")?)?,
            rf_bandwidth_hz: u64::try_from(ch.read_i64("rf_bandwidth")?).ok(),
            gain,
            device,
            method,
            time,
        })
    }

    fn offloads(&self, cfg: &RadioConfig) -> Result<Vec<Offload>> {
        let s = rx_settings_for(cfg, Dir::Capture)?;
        let ctx = open(&s.dev.uri)?;
        Ok(probe_front_end(&ctx)?
            .map(|id| Offload::Symbols {
                version: id.version,
                taps: id.taps,
            })
            .into_iter()
            .collect())
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
        let s = rx_settings_for(cfg, Dir::Capture)?;
        let mut ctx = open(&s.dev.uri)?;
        let device = device_info(&ctx);
        let id = probe_front_end(&ctx)?.with_context(|| {
            format!(
                "{NAME}: this bitstream has no symbol front end (up_adc_gpio_in has no ID word)"
            )
        })?;
        if !id.supports(params) {
            bail!(
                "{NAME}: the device's front end is format {} with {} taps; the parameters need format {} with {} taps",
                id.version,
                id.taps,
                isdb_frontend::params::FORMAT_VERSION,
                params.taps()
            );
        }
        let mut info = String::new();
        configure_rx(&ctx, cfg, &s, &mut info)
            .with_context(|| format!("configure Pluto {} for RX", s.dev.uri))?;
        std::thread::sleep(CAPTURE_SETTLE);
        let rate = {
            let rates = ctx.device(PHY)?.read("rx_path_rates")?;
            path_rate(&rates, "RXSAMP")
                .with_context(|| format!("{PHY} rx_path_rates {rates:?}: no RX rate"))?
        };
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        // Configure through the command register; check one register.
        let t0 = std::time::Instant::now();
        let mut enc = {
            let rx = ctx.device(RX_DEV)?;
            let mut enc = Encoder::new(rx.reg_read(GPIO_OUT_REG)?);
            let cmds = enc.configure(params);
            for &w in &cmds {
                rx.reg_write(GPIO_OUT_REG, w)?;
            }
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Addr, u32::from(reg::KP)))?;
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Read, 0))?;
            let kp = rx.reg_read(GPIO_IN_REG)?;
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Nop, 0))?;
            if kp != i32::from(params.kp) as u32 {
                bail!(
                    "{NAME}: front end register kp reads {kp:#x} after writing {:#x}",
                    params.kp
                );
            }
            rx.reg_write(GPIO_OUT_REG, enc.word(Op::Ctrl, control::ctrl::START))?;
            rx.reg_write(
                GPIO_OUT_REG,
                enc.word(Op::Ctrl, control::ctrl::RUN | control::ctrl::SYMBOLS),
            )?;
            enc
        };
        let config_s = t0.elapsed().as_secs_f64();
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri,
            rate_hz = rate,
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            words,
            format = id.version,
            taps = id.taps,
            config_s,
            "Pluto {} opened for a symbol capture of {words} words: {}; RX{} gain {gain}; FPGA front end format {} with {} taps, configured in {config_s:.1} s",
            s.dev.uri,
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
        restored?;
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
        let phy = ctx.device(PHY)?;
        let ch = phy.channel(s.dev.phy_channel(), false)?;
        let gain_db = ch.read_f64("hardwaregain")?;
        Ok(SymbolsCapture {
            words: written,
            overflows,
            sample_rate_hz: rate,
            lo_hz: u64::try_from(phy.channel(RX_LO, true)?.read_i64("frequency")?)?,
            rf_bandwidth_hz: u64::try_from(ch.read_i64("rf_bandwidth")?).ok(),
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
        let s = rx_settings(cfg)?;
        opts.validate()?;
        let depth = opts.buffers(cfg.sample_rate_hz(), s.dev.buffer);
        let mut ctx = open(&s.dev.uri)?;
        let mut info = device_info(&ctx);
        configure_rx(&ctx, cfg, &s, &mut info)
            .with_context(|| format!("configure Pluto {} for RX", s.dev.uri))?;
        let stream = ctx
            .start_rx(
                RX_DEV,
                &s.dev.stream_channels(),
                s.dev.buffer,
                KERNEL_BUFFERS,
                depth,
            )
            .with_context(|| port_hint(&s.dev, "RX"))?;
        info += &format!("; RX queue {depth} buffers of {} samples", s.dev.buffer);
        let gain = describe_rx_gain(&s);
        info!(
            backend = NAME,
            uri = s.dev.uri,
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = cfg.bandwidth_hz,
            gain,
            queue = depth,
            buffer = s.dev.buffer,
            "Pluto {} opened for RX: {}; RX{} gain {gain}; RX queue {depth} buffers of {} samples",
            s.dev.uri,
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
    Ok(Identity::parse(ctx.device(RX_DEV)?.reg_read(GPIO_IN_REG)?))
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
            dev.kernel_buffers,
            STREAM_DEPTH,
        )
        .with_context(|| port_hint(dev, "RX"))?;
    if stream.sample_bytes != 2 {
        bail!(
            "{RX_DEV} delivers {} bytes per word, 2 expected",
            stream.sample_bytes
        );
    }
    ctx.device(RX_DEV)?
        .reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW)?;
    let (mut written, mut overflows) = (0u64, 0u64);
    let timeout = Duration::from_secs(10);
    let r = (|| -> Result<()> {
        while written < words && !stop.load(Ordering::SeqCst) {
            let data = match stream.receiver.recv_timeout(timeout) {
                Ok(d) => d,
                Err(RecvTimeoutError::Timeout) => bail!("no words from {RX_DEV} for {timeout:?}"),
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let rx = ctx.device(RX_DEV)?;
            if rx.reg_read(ADC_STATUS_REG)? & ADC_STATUS_OVERFLOW != 0 {
                rx.reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW)?;
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
                .context("write the words")?;
            written += n;
        }
        Ok(())
    })();
    let stopped = ctx.stop();
    r?;
    stopped?;
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
        .with_context(|| port_hint(dev, "RX"))?;
    if stream.sample_bytes != bps {
        bail!(
            "{RX_DEV} delivers {} bytes per sample, {bps} expected",
            stream.sample_bytes
        );
    }
    // The bit is set whenever no transfer runs: clear it now that one does.
    ctx.device(RX_DEV)?
        .reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW)?;
    // Buffers that may hold samples from before an unseen loss: the
    // device's kernel blocks, iiod's and the client's buffer, one in the
    // network and the host queue.
    let window = u64::from(KERNEL_BUFFERS) + 3 + STREAM_DEPTH as u64;
    let (mut written, mut received, mut discard) = (0u64, 0u64, 0u64);
    let mut gaps = Vec::<CaptureGap>::new();
    let timeout = Duration::from_secs(10);
    let r = (|| -> Result<()> {
        while received < samples && !stop.load(Ordering::SeqCst) {
            let data = match stream.receiver.recv_timeout(timeout) {
                Ok(d) => d,
                Err(RecvTimeoutError::Timeout) => bail!("no samples from {RX_DEV} for {timeout:?}"),
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let rx = ctx.device(RX_DEV)?;
            if rx.reg_read(ADC_STATUS_REG)? & ADC_STATUS_OVERFLOW != 0 {
                rx.reg_write(ADC_STATUS_REG, ADC_STATUS_OVERFLOW)?;
                if discard == 0 {
                    warn!(
                        backend = NAME,
                        at = written,
                        window,
                        gaps = gaps.len() + 1,
                        "ADC overflow after {written} samples: a gap in the capture, the next {window} buffers discarded"
                    );
                    gaps.push(CaptureGap {
                        at: written,
                        discarded: 0,
                    });
                } else {
                    debug!(
                        backend = NAME,
                        at = written,
                        "ADC overflow again inside the gap after {written} samples"
                    );
                }
                discard = window;
            }
            let n =
                (data.len() / bps).min(usize::try_from(samples - received).unwrap_or(usize::MAX));
            received += n as u64;
            if discard > 0 {
                discard -= 1;
                gaps.last_mut().expect("a gap is open").discarded += n as u64;
                continue;
            }
            out.write_all(&data[..n * bps])
                .context("write the capture")?;
            written += n as u64;
        }
        Ok(())
    })();
    let stopped = ctx.stop();
    r?;
    stopped?;
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

fn open(uri: &str) -> Result<Context> {
    Context::open(uri).with_context(|| format!("open Pluto {uri}"))
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
/// clock chain) and check the rate the device runs at.
fn set_rate(
    ctx: &Context,
    ch: &isdb_iio::Channel<'_>,
    cfg: &RadioConfig,
    rates_attr: &str,
    key: &str,
) -> Result<f64> {
    let want = cfg.sample_rate_hz();
    // round(rate_num / rate_den) in whole Hz, computed in integers.
    let (num, den) = (i64::from(cfg.rate_num), i64::from(cfg.rate_den));
    let hz = (2 * num + den)
        .checked_div(2 * den)
        .context("sample rate denominator is 0")?;
    // The driver reads back whole Hz below the BBPLL rate (8126983 for
    // 8126984): rewriting reprograms the clock chain, which would glitch a
    // stream of the other direction, so 1 Hz counts as unchanged.
    if ch.read_i64("sampling_frequency")?.abs_diff(hz) > 1 {
        ch.write_i64("sampling_frequency", hz)?;
    }
    let rates = ctx.device(PHY)?.read(rates_attr)?;
    let got = path_rate(&rates, key)
        .with_context(|| format!("{PHY} {rates_attr} {rates:?}: no BBPLL / {key} rate"))?;
    let ppm = (got / want - 1.0) * 1e6;
    if ppm.abs() > MAX_RATE_ERROR_PPM {
        bail!(
            "sample rate {got:.2} Hz programmed for {want:.2} Hz ({ppm:+.1} ppm, more than {MAX_RATE_ERROR_PPM} ppm)"
        );
    }
    Ok(got)
}

/// Program the LO (always when `force`, else only if it differs) and
/// check the frequency read back.
fn set_lo(lo: &isdb_iio::Channel<'_>, hz: u64, force: bool) -> Result<u64> {
    let want = i64::try_from(hz)?;
    if force || lo.read_i64("frequency")? != want {
        lo.write_i64("frequency", want)?;
    }
    let got = u64::try_from(lo.read_i64("frequency")?)?;
    if got.abs_diff(hz) > MAX_LO_ERROR_HZ {
        bail!("LO {got} Hz programmed for {hz} Hz (outside the device's tuning range?)");
    }
    Ok(got)
}

/// Program the RF bandwidth; returns the value read back.
fn set_bandwidth(ch: &isdb_iio::Channel<'_>, hz: u32) -> Result<i64> {
    if ch.read_i64("rf_bandwidth")? != i64::from(hz) {
        ch.write_i64("rf_bandwidth", i64::from(hz))?;
    }
    Ok(ch.read_i64("rf_bandwidth")?)
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
    let phy = ctx.device(PHY)?;
    let ch = phy.channel(s.dev.phy_channel(), false)?;
    let rate = set_rate(ctx, &ch, cfg, "rx_path_rates", "RXSAMP")?;
    let bw = set_bandwidth(&ch, cfg.bandwidth_hz)?;
    let lo = set_lo(&phy.channel(RX_LO, true)?, cfg.center_hz, false)?;
    ch.write("gain_control_mode", s.agc)?;
    if s.agc == "manual" {
        let avail = ch.read("hardwaregain_available")?;
        if let Some((min, max)) = parse_range(&avail)
            && !(min..=max).contains(&f64::from(s.gain_db))
        {
            bail!(
                "{NAME} gain gain={}: outside {min}..{max} dB at this LO",
                s.gain_db
            );
        }
        ch.write_f64("hardwaregain", f64::from(s.gain_db))?;
    }
    *info += &describe_programmed(cfg, lo, rate, bw);
    *info += &format!(
        "; RX{} gain {}",
        s.dev.channel,
        match s.agc {
            "manual" => format!("manual {} dB", ch.read_f64("hardwaregain")?),
            m => format!("AGC {m}"),
        }
    );
    if phy.read_debug("loopback").is_ok_and(|v| v.trim() != "0") {
        *info += "; digital TX→RX loopback ON (receiving the device's own TX)";
    }
    Ok(())
}

/// A Pluto configured for transmission (the radio of a [`QueuedSink`]).
struct PlutoTx {
    ctx: Context,
    channels: [&'static str; 2],
    phy_channel: &'static str,
    buffer: usize,
    loopback: bool,
}

impl PlutoTx {
    /// Program the device, attenuation last (it starts at the maximum).
    fn configure(&self, cfg: &RadioConfig, s: &TxSettings, info: &mut String) -> Result<()> {
        let phy = self.ctx.device(PHY)?;
        let ch = phy.channel(self.phy_channel, true)?;
        ch.write_f64("hardwaregain", -f64::from(MAX_ATT_QDB) / 4.0)?;
        let lo_ch = phy.channel(TX_LO, true)?;
        // Loopback: the LO stays off for the whole run. Otherwise it is
        // powered first (a previous close left it down) and tuned below,
        // which makes the synthesiser calibrate and lock.
        lo_ch.write_i64("powerdown", i64::from(self.loopback))?;
        // The DDS tone generator off: without a buffer the DAC sends zeros.
        self.ctx
            .device(TX_DEV)?
            .channel("altvoltage0", true)?
            .write_i64("raw", 0)?;
        let rate = set_rate(&self.ctx, &ch, cfg, "tx_path_rates", "TXSAMP")?;
        let bw = set_bandwidth(&ch, cfg.bandwidth_hz)?;
        let lo = set_lo(&lo_ch, cfg.center_hz, !self.loopback)?;
        // A loopback left on (by a TX that did not close) is turned off.
        if self.loopback || phy.read_debug("loopback").is_ok_and(|v| v != "0") {
            phy.write_debug("loopback", if self.loopback { "1" } else { "0" })?;
        }
        // Check the stream channels before raising the level.
        for c in self.channels {
            self.ctx
                .device(TX_DEV)?
                .channel(c, true)
                .with_context(|| port_hint(&s.dev, "TX"))?;
        }
        ch.write_f64("hardwaregain", -s.att_db())?;
        *info += &describe_programmed(cfg, lo, rate, bw);
        *info += &format!(
            "; TX{} attenuation {} dB",
            s.dev.channel,
            -ch.read_f64("hardwaregain")?
        );
        if self.loopback {
            *info += "; digital TX→RX loopback, TX LO powered down";
        }
        *info += &format!(
            "; buffers of {} samples, {KERNEL_BUFFERS} in the kernel",
            self.buffer
        );
        Ok(())
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
    /// Stop streaming and leave the transmitter in its safe state.
    fn drop(&mut self) {
        let _ = self.ctx.stop();
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
            .start_tx(TX_DEV, &self.channels, self.buffer, KERNEL_BUFFERS, depth)
            .context("start Pluto TX stream")?;
        Ok(TxStream {
            sender: s.sender,
            counters: s.counters,
        })
    }
    fn stop(&mut self) -> Result<()> {
        self.ctx.stop().context("Pluto TX stream")?;
        self.safe_state();
        Ok(())
    }
    fn is_streaming(&mut self) -> bool {
        self.ctx.is_streaming()
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
                        self.ctx.stop().context("Pluto RX stream")?;
                        bail!("Pluto RX stream ended unexpectedly");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.stream = None;
                    self.ctx.stop().context("Pluto RX stream")?;
                    bail!("Pluto RX stream closed");
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
        self.ctx.stop().context("Pluto RX stream")?;
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
            bail!("{s}");
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
                uri: "ip:pluto.local".into(),
                channel: 1,
                buffer: 262_144,
                buffer_given: false,
                kernel_buffers: SYMBOLS_KERNEL_BUFFERS,
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
                ("loopback", "on"),
            ],
            &[],
        ))
        .unwrap();
        assert_eq!(s.dev.uri, "ip:192.0.2.30");
        assert_eq!(s.dev.stream_channels(), ["voltage2", "voltage3"]);
        assert_eq!(s.dev.phy_channel(), "voltage1");
        assert_eq!((s.dev.buffer, s.dev.loopback), (65_536, true));
        for bad in [
            ("uri", ""),
            ("channel", "0"),
            ("channel", "3"),
            ("buffer", "1000"),
            ("buffer", "512"),
            ("buffer", "8388608"),
            ("buffer", "x"),
            ("loopback", "maybe"),
            ("serial", "1"),
        ] {
            assert!(tx_settings(&cfg(&[bad], &[])).is_err(), "{bad:?}");
        }
        // Loopback is a TX argument.
        let e = rx_settings(&cfg(&[("loopback", "on")], &[]))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("unknown device argument \"loopback\" (known: uri, channel, buffer)"),
            "{e}"
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
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (1_048_576, 16));
        let s = rx_settings_for(
            &cfg(&[("buffer", "262144"), ("kbuffers", "8")], &[]),
            Dir::Capture,
        )
        .unwrap();
        assert_eq!((s.dev.buffer, s.dev.kernel_buffers), (262_144, 8));
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
                "maia-sdr recorder records RX1 only",
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
        assert!(tx_settings(&cfg(&[], &[("att", "1"), ("att", "2")])).is_err());
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
            .describe_rx(&cfg(&[("channel", "2")], &[("gain", "40")]))
            .unwrap();
        assert!(d.contains("device       Pluto ip:pluto.local, RX2"), "{d}");
        assert!(d.contains("RX gain      manual 40 dB"), "{d}");
    }
}
