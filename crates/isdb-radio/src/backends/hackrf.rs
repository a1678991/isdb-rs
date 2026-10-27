//! HackRF One backend (`hackrf`), through `isdb-hackrf` / libhackrf.
//!
//! * Device arguments: `serial` (default: the first HackRF found).
//! * TX gains: `txvga` 0..47 dB (default 0), `amp` on|off (RF amplifier,
//!   ≈ +11 dB; default off): the defaults are the minimum output power.
//! * RX gains: `lna` 0..40 dB in 8 dB steps (default 16), `vga` 0..62 dB in
//!   2 dB steps (default 20), `amp` on|off (default off).
//! * Range: 1 MHz..6 GHz, 2..20 Msps (exact rational rate through
//!   `set_sample_rate_manual`).
//! * Baseband filter: libhackrf's `hackrf_compute_baseband_filter_bw`
//!   picks the largest MAX2837 bandwidth (1.75, 2.5, 3.5, 5, 5.5, 6, 7, 8,
//!   9, 10, 12, 14, 15, 20, 24 or 28 MHz) ≤ the request, or 1.75 MHz below
//!   that; its header says "nearest", but it rounds down (6.1 MHz → 6 MHz,
//!   5.9 MHz → 5.5 MHz). Above 28 MHz it returns 0, so such requests are
//!   refused here. The bandwidth programmed is reported when the device is
//!   opened.
//! * Samples: cs8; TX through a [`QueuedSink`] (scaled with
//!   [`TxOptions::backoff_db`] by a [`Quantizer<Cs8>`]) in libhackrf USB
//!   transfers of 262 144 bytes; RX queues [`RxOptions::queue_seconds`] of
//!   such transfers.

use crate::{
    Backend, Cs8, IqSink, IqSource, Quantizer, QueuedSink, RadioConfig, RadioTx, RxOptions,
    StreamCounters, TxOptions, TxStream,
};
use anyhow::{Context, Result, bail};
use isdbt_ofdm::C32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use tracing::{debug, info, warn};

/// The `hackrf` backend.
pub(crate) struct HackRf;

const NAME: &str = "hackrf";
const FREQ_HZ: std::ops::RangeInclusive<u64> = 1_000_000..=6_000_000_000;
const RATE_HZ: std::ops::RangeInclusive<f64> = 2e6..=20e6;
/// Largest MAX2837 baseband filter bandwidth.
const MAX_BANDWIDTH_HZ: u32 = 28_000_000;
/// Samples per libhackrf USB transfer (262 144 bytes of cs8).
const TRANSFER_SAMPLES: usize = 131_072;

/// The values a gain takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainKind {
    /// Whole dB in `0..=max`, multiples of `step`.
    Db { max: u32, step: u32 },
    /// on / off (1 / 0).
    OnOff,
}

/// One named gain: what the user may set and its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Gain {
    name: &'static str,
    kind: GainKind,
    /// Default value: dB, or 0/1 for off/on.
    default: u32,
}

impl Gain {
    /// Help text: `lna=0..40 dB in 8 dB steps (default 16)`.
    fn help(&self) -> String {
        match self.kind {
            GainKind::Db { max, step: 1 } => {
                format!("{}=0..{max} dB (default {})", self.name, self.default)
            }
            GainKind::Db { max, step } => format!(
                "{}=0..{max} dB in {step} dB steps (default {})",
                self.name, self.default
            ),
            GainKind::OnOff => format!(
                "{}=on|off (default {})",
                self.name,
                if self.default != 0 { "on" } else { "off" }
            ),
        }
    }

    /// Parse and check a value.
    fn parse(&self, v: &str) -> Result<u32> {
        let name = self.name;
        match self.kind {
            GainKind::Db { max, step } => {
                let x: u32 = v
                    .parse()
                    .with_context(|| format!("{NAME} gain {name}={v}: not a whole number of dB"))?;
                if x > max || !x.is_multiple_of(step) {
                    if step == 1 {
                        bail!("{NAME} gain {name}={v}: outside 0..{max} dB");
                    }
                    bail!("{NAME} gain {name}={v}: must be 0..{max} dB in {step} dB steps");
                }
                Ok(x)
            }
            GainKind::OnOff => match v.to_ascii_lowercase().as_str() {
                "on" | "1" | "true" | "yes" => Ok(1),
                "off" | "0" | "false" | "no" => Ok(0),
                _ => bail!("{NAME} gain {name}={v}: want on or off"),
            },
        }
    }
}

/// TX gains, in [`TxSettings`] order. The defaults are the minimum output
/// power.
const TX_GAINS: [Gain; 2] = [
    Gain {
        name: "txvga",
        kind: GainKind::Db { max: 47, step: 1 },
        default: 0,
    },
    Gain {
        name: "amp",
        kind: GainKind::OnOff,
        default: 0,
    },
];

/// RX gains, in [`RxSettings`] order.
const RX_GAINS: [Gain; 3] = [
    Gain {
        name: "lna",
        kind: GainKind::Db { max: 40, step: 8 },
        default: 16,
    },
    Gain {
        name: "vga",
        kind: GainKind::Db { max: 62, step: 2 },
        default: 20,
    },
    Gain {
        name: "amp",
        kind: GainKind::OnOff,
        default: 0,
    },
];

/// The values of the gains in `table` (its order): those in `cfg.gains`,
/// the defaults for the others. Unknown and repeated names are errors.
fn gains<const N: usize>(cfg: &RadioConfig, dir: &str, table: &[Gain; N]) -> Result<[u32; N]> {
    let mut values = table.map(|g| g.default);
    for (i, (k, v)) in cfg.gains.iter().enumerate() {
        let Some(j) = table.iter().position(|g| g.name == k) else {
            let known: Vec<String> = table.iter().map(Gain::help).collect();
            bail!(
                "{NAME} {dir} has no gain {k:?} (gains: {})",
                known.join(", ")
            );
        };
        if cfg.gains[..i].iter().any(|(p, _)| p == k) {
            bail!("{NAME} gain {k:?} given twice");
        }
        values[j] = table[j].parse(v)?;
    }
    Ok(values)
}

/// Validated TX settings.
#[derive(Debug, Clone, PartialEq)]
struct TxSettings {
    serial: Option<String>,
    txvga_db: u32,
    amp: bool,
}

/// Validated RX settings.
#[derive(Debug, Clone, PartialEq)]
struct RxSettings {
    serial: Option<String>,
    lna_db: u32,
    vga_db: u32,
    amp: bool,
}

/// The device arguments (only `serial`) and the range of the tuning and
/// of the baseband filter.
fn check_common(cfg: &RadioConfig) -> Result<Option<String>> {
    let mut serial = None;
    for (k, v) in &cfg.args {
        match k.as_str() {
            "serial" if v.is_empty() => bail!("{NAME}: empty serial number"),
            "serial" => serial = Some(v.clone()),
            _ => bail!("{NAME}: unknown device argument {k:?} (known: serial)"),
        }
    }
    if !FREQ_HZ.contains(&cfg.center_hz) {
        bail!(
            "frequency {} Hz outside the HackRF range 1 MHz..6 GHz",
            cfg.center_hz
        );
    }
    if cfg.rate_den == 0 || !RATE_HZ.contains(&cfg.sample_rate_hz()) {
        bail!(
            "sample rate {}/{} Hz outside the HackRF range 2..20 MHz",
            cfg.rate_num,
            cfg.rate_den
        );
    }
    if cfg.bandwidth_hz > MAX_BANDWIDTH_HZ {
        bail!(
            "baseband filter {} Hz above the largest HackRF (MAX2837) bandwidth, 28 MHz",
            cfg.bandwidth_hz
        );
    }
    Ok(serial)
}

fn tx_settings(cfg: &RadioConfig) -> Result<TxSettings> {
    let serial = check_common(cfg)?;
    let [txvga_db, amp] = gains(cfg, "TX", &TX_GAINS)?;
    Ok(TxSettings {
        serial,
        txvga_db,
        amp: amp != 0,
    })
}

fn rx_settings(cfg: &RadioConfig) -> Result<RxSettings> {
    let serial = check_common(cfg)?;
    let [lna_db, vga_db, amp] = gains(cfg, "RX", &RX_GAINS)?;
    Ok(RxSettings {
        serial,
        lna_db,
        vga_db,
        amp: amp != 0,
    })
}

fn describe_device(serial: Option<&str>, cfg: &RadioConfig) -> String {
    format!(
        "  device       HackRF {}\n  tuning       {}\n  bb filter    {:.2} MHz requested",
        serial.unwrap_or("(first found)"),
        cfg.describe_tuning(),
        f64::from(cfg.bandwidth_hz) / 1e6,
    )
}

impl Backend for HackRf {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<String> {
        let s = tx_settings(cfg)?;
        opts.validate()?;
        Ok(format!(
            "{}\n  TX VGA       {} dB\n  RF amp       {}\n  backoff      {}",
            describe_device(s.serial.as_deref(), cfg),
            s.txvga_db,
            if s.amp { "ON (+~11 dB)" } else { "off" },
            Quantizer::<Cs8>::new(opts.backoff_db).describe_level()
        ))
    }

    fn describe_rx(&self, cfg: &RadioConfig) -> Result<String> {
        let s = rx_settings(cfg)?;
        Ok(format!(
            "{}\n  LNA / VGA    {} / {} dB\n  RF amp       {}",
            describe_device(s.serial.as_deref(), cfg),
            s.lna_db,
            s.vga_db,
            if s.amp { "ON" } else { "off" }
        ))
    }

    fn open_tx(&self, cfg: &RadioConfig, opts: &TxOptions) -> Result<(Box<dyn IqSink>, String)> {
        let s = tx_settings(cfg)?;
        opts.validate()?;
        let mut d = isdb_hackrf::Device::open(s.serial.as_deref()).context("open HackRF")?;
        let info = device_info(&mut d);
        d.set_antenna_enable(false)?;
        d.set_amp_enable(false)?;
        d.set_sample_rate_manual(cfg.rate_num, cfg.rate_den)?;
        let bw = d.set_baseband_filter(cfg.bandwidth_hz)?;
        d.set_freq(cfg.center_hz)?;
        d.set_txvga_gain(s.txvga_db)?;
        d.set_amp_enable(s.amp)?;
        let gain = format!(
            "TX VGA {} dB, RF amp {}",
            s.txvga_db,
            if s.amp { "on" } else { "off" }
        );
        info!(
            backend = NAME,
            serial = s.serial.as_deref().unwrap_or("first found"),
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = bw,
            gain,
            "HackRF {} opened for TX: {}; baseband filter {:.2} MHz; {gain}",
            s.serial.as_deref().unwrap_or("(first found)"),
            cfg.describe_tuning(),
            f64::from(bw) / 1e6
        );
        debug!(backend = NAME, "{info}");
        Ok((
            Box::new(QueuedSink::new(d, opts)?),
            format!("{info}; baseband filter {:.2} MHz", f64::from(bw) / 1e6),
        ))
    }

    fn open_rx(&self, cfg: &RadioConfig, opts: &RxOptions) -> Result<(Box<dyn IqSource>, String)> {
        let s = rx_settings(cfg)?;
        opts.validate()?;
        let depth = rx_transfers(cfg, opts);
        let mut d = isdb_hackrf::Device::open(s.serial.as_deref()).context("open HackRF")?;
        let info = device_info(&mut d);
        d.set_antenna_enable(false)?;
        d.set_sample_rate_manual(cfg.rate_num, cfg.rate_den)?;
        let bw = d.set_baseband_filter(cfg.bandwidth_hz)?;
        d.set_freq(cfg.center_hz)?;
        d.set_lna_gain(s.lna_db)?;
        d.set_vga_gain(s.vga_db)?;
        d.set_amp_enable(s.amp)?;
        let stream = d.start_rx(depth)?;
        let gain = format!(
            "LNA {} dB, VGA {} dB, RF amp {}",
            s.lna_db,
            s.vga_db,
            if s.amp { "on" } else { "off" }
        );
        info!(
            backend = NAME,
            serial = s.serial.as_deref().unwrap_or("first found"),
            rate_hz = cfg.sample_rate_hz(),
            freq_hz = cfg.center_hz,
            bandwidth_hz = bw,
            gain,
            queue = depth,
            "HackRF {} opened for RX: {}; baseband filter {:.2} MHz; {gain}; RX queue {depth} transfers",
            s.serial.as_deref().unwrap_or("(first found)"),
            cfg.describe_tuning(),
            f64::from(bw) / 1e6
        );
        debug!(backend = NAME, "{info}");
        Ok((
            Box::new(HackrfIqSource {
                dev: d,
                stream: Some(stream),
            }),
            format!(
                "{info}; baseband filter {:.2} MHz; RX queue {depth} transfers",
                f64::from(bw) / 1e6
            ),
        ))
    }
}

/// The RX queue length in USB transfers.
fn rx_transfers(cfg: &RadioConfig, opts: &RxOptions) -> usize {
    opts.buffers(cfg.sample_rate_hz(), TRANSFER_SAMPLES)
}

fn device_info(d: &mut isdb_hackrf::Device) -> String {
    let fw = d.firmware_version().unwrap_or_else(|e| format!("? ({e})"));
    let board = d
        .board_id()
        .map(|b| b.to_string())
        .unwrap_or_else(|e| format!("? ({e})"));
    format!(
        "libhackrf {} (release {}), board id {board}, firmware {fw}",
        isdb_hackrf::library_version(),
        isdb_hackrf::library_release()
    )
}

impl StreamCounters for isdb_hackrf::Counters {
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

impl RadioTx for isdb_hackrf::Device {
    type Format = Cs8;

    fn start_tx(&mut self, depth: usize) -> Result<TxStream<i8>> {
        let s = isdb_hackrf::Device::start_tx(self, depth)?;
        Ok(TxStream {
            sender: s.sender,
            counters: s.counters,
        })
    }
    fn stop(&mut self) -> Result<()> {
        Ok(isdb_hackrf::Device::stop(self)?)
    }
    fn is_streaming(&mut self) -> bool {
        isdb_hackrf::Device::is_streaming(self)
    }
}

/// HackRF receive stream.
struct HackrfIqSource {
    dev: isdb_hackrf::Device,
    stream: Option<isdb_hackrf::RxStream>,
}

impl IqSource for HackrfIqSource {
    fn read(&mut self, stop: &AtomicBool) -> Result<Option<Vec<C32>>> {
        let Some(st) = &self.stream else {
            return Ok(None);
        };
        loop {
            match st.receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(b) => return Ok(Some(isdbt_phy::iq::samples_from_cs8(&b))),
                Err(RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => {
                    return Ok(None);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if st.counters.finished() {
                        bail!("HackRF RX stream ended unexpectedly");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => bail!("HackRF RX stream closed"),
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
        self.dev.stop()?;
        let streaming = self.dev.is_streaming();
        if overruns > 0 {
            warn!(
                backend = NAME,
                overruns,
                "HackRF RX dropped {overruns} transfer(s) (the reader fell behind the radio)"
            );
        }
        debug!(
            backend = NAME,
            overruns,
            streaming,
            "HackRF RX stream stopped: {overruns} overrun(s){}",
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

    fn cfg(gains: &[(&str, &str)]) -> RadioConfig {
        RadioConfig {
            center_hz: 473_142_857,
            nominal_hz: 473_142_857,
            rate_num: 512_000_000,
            rate_den: 63,
            bandwidth_hz: 6_000_000,
            args: Vec::new(),
            gains: gains
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    const OPTS: TxOptions = TxOptions {
        prefill_frames: 2,
        frame_seconds: 0.231,
        backoff_db: 10.0,
    };

    #[test]
    fn tx_gains_defaults_and_ranges() {
        let s = tx_settings(&cfg(&[])).unwrap();
        assert_eq!((s.serial, s.txvga_db, s.amp), (None, 0, false));
        let s = tx_settings(&cfg(&[("txvga", "47"), ("amp", "on")])).unwrap();
        assert_eq!((s.txvga_db, s.amp), (47, true));
        assert!(tx_settings(&cfg(&[("txvga", "48")])).is_err());
        assert!(tx_settings(&cfg(&[("txvga", "-1")])).is_err());
        assert!(tx_settings(&cfg(&[("txvga", "20dB")])).is_err());
        assert!(tx_settings(&cfg(&[("amp", "maybe")])).is_err());
        // RX gains are not TX gains; unknown names list the known ones.
        let e = tx_settings(&cfg(&[("lna", "16")])).unwrap_err().to_string();
        assert!(
            e.contains("no gain \"lna\"") && e.contains("txvga=0..47"),
            "{e}"
        );
        let e = tx_settings(&cfg(&[("bogus", "1")]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("no gain \"bogus\""), "{e}");
        let e = tx_settings(&cfg(&[("txvga", "1"), ("txvga", "2")]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("twice"), "{e}");
        // The backoff is a TX option, checked by describe_tx.
        let o = TxOptions {
            backoff_db: 41.0,
            ..OPTS
        };
        let e = HackRf.describe_tx(&cfg(&[]), &o).unwrap_err().to_string();
        assert!(e.contains("backoff 41 dB outside 0..40"), "{e}");
    }

    #[test]
    fn rx_gains_defaults_and_steps() {
        let s = rx_settings(&cfg(&[])).unwrap();
        assert_eq!((s.lna_db, s.vga_db, s.amp), (16, 20, false));
        let s = rx_settings(&cfg(&[("lna", "40"), ("vga", "62"), ("amp", "off")])).unwrap();
        assert_eq!((s.lna_db, s.vga_db), (40, 62));
        assert!(rx_settings(&cfg(&[("lna", "12")])).is_err());
        assert!(rx_settings(&cfg(&[("lna", "48")])).is_err());
        assert!(rx_settings(&cfg(&[("vga", "21")])).is_err());
        assert!(rx_settings(&cfg(&[("vga", "64")])).is_err());
        assert!(rx_settings(&cfg(&[("txvga", "20")])).is_err());
        let e = rx_settings(&cfg(&[("gain", "1")])).unwrap_err().to_string();
        assert!(
            e.ends_with(
                "(gains: lna=0..40 dB in 8 dB steps (default 16), \
                 vga=0..62 dB in 2 dB steps (default 20), amp=on|off (default off))"
            ),
            "{e}"
        );
    }

    #[test]
    fn gain_tables() {
        // The TX defaults are the minimum output power.
        for g in TX_GAINS {
            assert_eq!(g.default, 0, "{}", g.name);
        }
        // Every default passes its own validation.
        for g in TX_GAINS.iter().chain(&RX_GAINS) {
            let v = match g.kind {
                GainKind::Db { .. } => g.default.to_string(),
                GainKind::OnOff => ["off", "on"][g.default as usize].to_string(),
            };
            assert_eq!(g.parse(&v).unwrap(), g.default, "{}", g.name);
        }
        let e = tx_settings(&cfg(&[("lna", "16")])).unwrap_err().to_string();
        assert!(
            e.ends_with("(gains: txvga=0..47 dB (default 0), amp=on|off (default off))"),
            "{e}"
        );
    }

    #[test]
    fn baseband_filter_and_rx_queue() {
        let mut c = cfg(&[]);
        for (bw, ok) in [(1_000_000, true), (28_000_000, true), (28_000_001, false)] {
            c.bandwidth_hz = bw;
            assert_eq!(tx_settings(&c).is_ok(), ok, "{bw}");
            assert_eq!(rx_settings(&c).is_ok(), ok, "{bw}");
        }
        // 1 s at 512/63 MHz = 62.004 transfers of 131 072 samples: 63.
        let c = cfg(&[]);
        assert_eq!(rx_transfers(&c, &RxOptions::default()), 63);
        assert_eq!(
            rx_transfers(
                &c,
                &RxOptions {
                    queue_seconds: 1e-6
                }
            ),
            1
        );
        assert!(
            RxOptions {
                queue_seconds: f64::NAN
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn device_args_and_ranges() {
        let mut c = cfg(&[]);
        c.args = vec![("serial".into(), "0000abcd".into())];
        assert_eq!(tx_settings(&c).unwrap().serial.as_deref(), Some("0000abcd"));
        assert!(HackRf.describe_rx(&c).unwrap().contains("HackRF 0000abcd"));
        c.args = vec![("uri".into(), "x".into())];
        let e = tx_settings(&c).unwrap_err().to_string();
        assert!(e.contains("unknown device argument \"uri\""), "{e}");
        c.args = vec![("serial".into(), String::new())];
        assert!(tx_settings(&c).is_err());
        // The HackRF covers 1 MHz..6 GHz.
        let mut c = cfg(&[]);
        for (f, ok) in [
            (999_999, false),
            (1_000_000, true),
            (6_000_000_000, true),
            (7_000_000_000, false),
        ] {
            c.center_hz = f;
            assert_eq!(rx_settings(&c).is_ok(), ok, "{f}");
            assert_eq!(tx_settings(&c).is_ok(), ok, "{f}");
        }
        let mut c = cfg(&[]);
        c.rate_num = 1_000_000;
        c.rate_den = 1;
        assert!(tx_settings(&c).is_err());
        c.rate_den = 0;
        assert!(tx_settings(&c).is_err());
    }

    #[test]
    fn descriptions() {
        let d = HackRf.describe_tx(&cfg(&[("txvga", "20")]), &OPTS).unwrap();
        assert!(d.contains("device       HackRF (first found)"), "{d}");
        assert!(
            d.contains("centre 473.142857 MHz, programmed 473142857 Hz"),
            "{d}"
        );
        assert!(
            d.contains("sample rate 512000000/63 Hz = 8.126984 MHz"),
            "{d}"
        );
        assert!(d.contains("bb filter    6.00 MHz requested"), "{d}");
        assert!(d.contains("TX VGA       20 dB"), "{d}");
        assert!(d.contains("RF amp       off"), "{d}");
        assert!(
            d.contains("10.0 dB below cs8 full scale (RMS 40.2 of 127)"),
            "{d}"
        );
        let d = HackRf.describe_rx(&cfg(&[("amp", "on")])).unwrap();
        assert!(d.contains("LNA / VGA    16 / 20 dB"), "{d}");
        assert!(d.contains("RF amp       ON"), "{d}");
    }
}
