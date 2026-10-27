//! `isdb capture`: receive raw IQ samples from a radio at any rate it
//! supports, in one contiguous piece or streamed with its losses marked,
//! and save them with SigMF metadata. Receive only; nothing is transmitted.

use crate::bands::{Freq, parse_count, parse_hz};
use crate::phyargs::parse_gain;
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_iq::sigmf::{Meta, Segment};
use isdb_iq::{SampleFormat, meta_path_for};
use isdb_radio::{Capture, CaptureFormat, DeviceSpec, RadioConfig};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::info;

/// `isdb capture` arguments.
#[derive(Debug, Args)]
pub struct CaptureArgs {
    /// The radio: BACKEND[:key=value,...] (e.g.
    /// `pluto:uri=ip:192.0.2.30`, `pluto:uri=ip:192.0.2.30,recorder=maia,bits=8`,
    /// `pluto:uri=ip:192.0.2.30,recorder=stream`).
    #[arg(long, value_name = "SPEC")]
    pub radio: DeviceSpec,
    /// Centre frequency: Hz (`1049480000`, `1049.48M`) or a channel name:
    /// `T13`..`T62` (terrestrial UHF), `BS1`..`BS24` and `ND1`..`ND24`
    /// (110° CS) at their satellite IF: RF − 10.678 GHz for the
    /// right-hand-circular channels (odd BS, even ND), RF − 9.505 GHz for
    /// the left-hand ones (even BS, odd ND; IF 2224–3224 MHz).
    #[arg(long, value_name = "HZ|CHANNEL")]
    pub freq: Freq,
    /// Sample rate in samples per second (`57.72M`, `61440000`). The rate
    /// the device runs at is read back and recorded.
    #[arg(long, value_name = "SPS", value_parser = parse_hz)]
    pub rate: f64,
    /// Samples per segment (`4Mi`, `16515072`); default: the largest the
    /// radio captures in one piece. A streamed capture needs this or
    /// `--seconds`.
    #[arg(long, value_name = "N", value_parser = parse_count, conflicts_with = "seconds")]
    pub samples: Option<usize>,
    /// Seconds of signal per segment at the requested rate, instead of
    /// `--samples`.
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
    /// Analog bandwidth request in Hz; default 0.8 × the sample rate.
    #[arg(long, value_name = "HZ", value_parser = parse_hz)]
    pub bw: Option<f64>,
    /// Radio gain NAME=VALUE (repeatable; the backend's names, e.g.
    /// `agc=slow_attack` or `gain=30` for a Pluto).
    #[arg(long, value_name = "NAME=VALUE", value_parser = parse_gain)]
    pub gain: Vec<(String, String)>,
    /// Number of captures written one after another into the same file.
    /// Samples between them are lost: each capture is its own SigMF
    /// capture segment, marked `isdb:gap_before` from the second on (as is
    /// every loss a streamed capture reports).
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub segments: usize,
    /// Free text for the SigMF `core:description` (do not put station
    /// names in recordings you share).
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,
    /// Output data file (interleaved I/Q: cs16 little-endian, or cs8 with
    /// `bits=8`; the SigMF datatype says which). The SigMF metadata goes
    /// next to it: `NAME.sigmf-meta` for `NAME.sigmf-data`, else the file
    /// name with `.sigmf-meta` appended.
    #[arg(short, long, value_name = "PATH", required_unless_present = "discard")]
    pub out: Option<PathBuf>,
    /// Receive without writing anything: a test of the radio link (rate
    /// sustained, losses), which prints what a capture would record.
    #[arg(long, conflicts_with_all = ["out", "overwrite", "survey"])]
    pub discard: bool,
    /// Replace existing output files.
    #[arg(long)]
    pub overwrite: bool,
    /// Print the survey of the capture (`isdb iq survey`) when done.
    #[arg(long)]
    pub survey: bool,
    /// Validate and print the settings without opening the radio.
    #[arg(long)]
    pub dry_run: bool,
}

impl CaptureArgs {
    /// The radio configuration.
    pub fn radio_config(&self) -> Result<RadioConfig> {
        if !(self.rate.is_finite() && self.rate >= 1.0 && self.rate <= f64::from(u32::MAX)) {
            bail!("--rate {}: out of range", self.rate);
        }
        let bw = self.bw.unwrap_or(0.8 * self.rate).round();
        if !(bw >= 1.0 && bw <= f64::from(u32::MAX)) {
            bail!("--bw {bw}: out of range");
        }
        Ok(RadioConfig {
            center_hz: self.freq.hz,
            nominal_hz: self.freq.hz,
            // The radio takes whole Hz; the rate it runs at is read back.
            rate_num: crate::phyargs::whole_u32(self.rate, "--rate")?,
            rate_den: 1,
            bandwidth_hz: crate::phyargs::whole_u32(bw, "--bw")?,
            args: self.radio.args.clone(),
            gains: self.gain.clone(),
        })
    }
}

/// The SigMF sample format of a capture.
fn sample_format(f: CaptureFormat) -> SampleFormat {
    match f {
        CaptureFormat::Cs8 => SampleFormat::Cs8,
        CaptureFormat::Cs16 { full_scale } => SampleFormat::Cs16 { full_scale },
    }
}

/// The SigMF metadata of `caps` (one or more segments each: one per
/// capture, and one more after each loss inside a capture) taken with `a`.
pub fn metadata(a: &CaptureArgs, caps: &[Capture]) -> Result<Meta> {
    let first = caps.first().context("no capture")?;
    let mut m = Meta::new(sample_format(first.format), first.sample_rate_hz);
    m.recorder = Some(format!("isdb capture {}", env!("CARGO_PKG_VERSION")));
    m.hw = Some(format!("{} ({})", first.device, a.radio.backend));
    m.description.clone_from(&a.description);
    let mut start = 0u64;
    for (i, c) in caps.iter().enumerate() {
        if c.format != first.format || c.sample_rate_hz != first.sample_rate_hz {
            bail!("captures with different formats or rates cannot share a file");
        }
        let mut extra = serde_json::Map::new();
        extra.insert("isdb:lo_hz".into(), json!(c.lo_hz));
        if let Some(bw) = c.rf_bandwidth_hz {
            extra.insert("isdb:rf_bandwidth_hz".into(), json!(bw));
        }
        extra.insert("isdb:gain".into(), json!(c.gain));
        extra.insert("isdb:capture_method".into(), json!(c.method));
        if let Some(ch) = a.freq.channel {
            extra.insert("isdb:channel".into(), json!(ch.to_string()));
            extra.insert("isdb:channel_rf_hz".into(), json!(ch.rf_hz()?));
            if let (Some(lo), Some(pol)) = (ch.lnb_lo_hz(), ch.polarisation()) {
                extra.insert("isdb:lnb_lo_hz".into(), json!(lo));
                extra.insert("isdb:polarisation".into(), json!(pol.name()));
            }
        }
        let segment = |at: u64, gap: Option<u64>| {
            let mut extra = extra.clone();
            if let Some(discarded) = gap {
                extra.insert("isdb:gap_before".into(), json!(true));
                if discarded > 0 {
                    extra.insert("isdb:samples_discarded".into(), json!(discarded));
                }
            }
            Segment {
                sample_start: start + at,
                frequency: Some(a.freq.hz as f64),
                datetime: (at == 0).then(|| isdb_iq::iso8601_utc(c.time)),
                extra,
            }
        };
        m.captures.push(segment(0, (i > 0).then_some(0)));
        for g in c.gaps.iter().filter(|g| g.at > 0 && g.at < c.samples) {
            m.captures.push(segment(g.at, Some(g.discarded)));
        }
        // A loss found after the last sample written (the capture ended in
        // the samples discarded around it) has no segment to start: its
        // count goes on the capture's last segment.
        let after: u64 = c
            .gaps
            .iter()
            .filter(|g| g.at >= c.samples)
            .map(|g| g.discarded)
            .sum();
        if after > 0 {
            let last = m.captures.last_mut().expect("pushed above");
            last.extra
                .insert("isdb:samples_discarded_after".into(), json!(after));
        }
        start += c.samples;
    }
    Ok(m)
}

/// A writer that prints the progress of a long capture every few seconds.
struct Progress<'a> {
    inner: &'a mut dyn Write,
    bytes: u64,
    start: Instant,
    last: Instant,
}

impl Write for Progress<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        if self.last.elapsed() >= Duration::from_secs(5) {
            self.last = Instant::now();
            let t = self.start.elapsed().as_secs_f64();
            info!(
                target: "isdb::capture",
                bytes = self.bytes,
                seconds = t,
                mb_per_s = self.bytes as f64 / t / 1e6,
                "{:.1} MB written in {t:.1} s ({:.1} MB/s)",
                self.bytes as f64 / 1e6,
                self.bytes as f64 / t / 1e6
            );
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Create `path`, refusing to replace a file unless `overwrite`.
pub(crate) fn create(path: &Path, overwrite: bool) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    if overwrite {
        o.create(true).truncate(true);
    } else {
        o.create_new(true);
    }
    o.open(path).with_context(|| {
        format!(
            "create {}{}",
            path.display(),
            if overwrite {
                ""
            } else {
                " (--overwrite replaces it)"
            }
        )
    })
}

/// Capture the segments into `data` (nothing with `None`) and return
/// them.
fn take_captures(
    a: &CaptureArgs,
    backend: &dyn isdb_radio::Backend,
    cfg: &RadioConfig,
    samples: u64,
    data: Option<std::fs::File>,
) -> Result<Vec<Capture>> {
    let stop = crate::stop::flag();
    let path = a.out.as_deref().unwrap_or(Path::new("-"));
    let mut sink = std::io::sink();
    let mut file;
    let inner: &mut dyn Write = match data {
        Some(f) => {
            file = std::io::BufWriter::with_capacity(8 << 20, f);
            &mut file
        }
        None => &mut sink,
    };
    let mut caps = Vec::with_capacity(a.segments);
    for i in 0..a.segments {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let t = Instant::now();
        let mut out = Progress {
            inner: &mut *inner,
            bytes: 0,
            start: t,
            last: t,
        };
        let c = backend
            .capture(cfg, samples, &mut out, &stop)
            .with_context(|| format!("capture into {}", path.display()))?;
        let secs = t.elapsed().as_secs_f64();
        let lost: u64 = c.gaps.iter().map(|g| g.discarded).sum();
        info!(
            target: "isdb::capture",
            segment = i + 1,
            segments = a.segments,
            samples = c.samples,
            sample_rate_hz = c.sample_rate_hz,
            lo_hz = c.lo_hz,
            seconds = secs,
            mb_per_s = out.bytes as f64 / secs / 1e6,
            losses = c.gaps.len(),
            samples_discarded = lost,
            "segment {}/{}: {} samples ({}) at {:.2} Hz, LO {} Hz, {}, {} ({secs:.2} s, {:.1} MB/s); {}",
            i + 1,
            a.segments,
            c.samples,
            sample_format(c.format),
            c.sample_rate_hz,
            c.lo_hz,
            c.gain,
            c.method,
            out.bytes as f64 / secs / 1e6,
            if c.gaps.is_empty() {
                "no loss reported".to_string()
            } else {
                format!(
                    "{} loss(es) reported (after sample {}), {lost} samples discarded around them",
                    c.gaps.len(),
                    c.gaps
                        .iter()
                        .map(|g| g.at.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        caps.push(c);
    }
    inner
        .flush()
        .with_context(|| format!("write {}", path.display()))?;
    if caps.is_empty() {
        bail!("stopped before the first capture");
    }
    Ok(caps)
}

/// Run `isdb capture`.
pub fn run(a: &CaptureArgs) -> Result<()> {
    if a.segments == 0 {
        bail!("--segments must be at least 1");
    }
    let backend = isdb_radio::backend(&a.radio.backend)?;
    let cfg = a.radio_config()?;
    let (limit, why) = backend
        .capture_limit(&cfg)?
        .with_context(|| format!("radio backend {:?} cannot capture", backend.name()))?;
    let settings = backend.describe_capture(&cfg)?;
    let samples = match (a.samples, a.seconds) {
        (Some(n), _) => n as u64,
        (None, Some(t)) if t.is_finite() && t > 0.0 => {
            crate::phyargs::sample_count(t, cfg.sample_rate_hz())
        }
        (None, Some(t)) => bail!("--seconds {t}: must be > 0"),
        (None, None) if limit == u64::MAX => {
            bail!("a streamed capture needs --samples or --seconds")
        }
        (None, None) => limit,
    };
    if samples == 0 || samples > limit {
        bail!(
            "--samples {samples}: {} captures at most {limit} contiguous samples ({why}); use --segments for more (with gaps)",
            backend.name()
        );
    }
    let label = a.freq.channel.map(|c| c.label()).unwrap_or_default();
    info!(
        target: "isdb::capture",
        backend = backend.name(),
        "{} capture settings{label}:\n{settings}",
        backend.name()
    );
    info!(
        target: "isdb::capture",
        segments = a.segments,
        samples,
        milliseconds = samples as f64 / cfg.sample_rate_hz() * 1e3,
        limit,
        "capture      {} segment(s) of {samples} samples ({:.1} ms each at the requested rate); {}",
        a.segments,
        samples as f64 / cfg.sample_rate_hz() * 1e3,
        if limit == u64::MAX {
            why
        } else {
            format!("at most {limit} ({why})")
        }
    );
    let Some(out) = a.out.as_deref() else {
        info!(target: "isdb::capture", "output       none (--discard)");
        if a.dry_run {
            info!(target: "isdb::capture", "dry run: radio not opened");
            return Ok(());
        }
        take_captures(a, backend, &cfg, samples, None)?;
        return Ok(());
    };
    let meta_path = meta_path_for(out);
    info!(
        target: "isdb::capture",
        file = %out.display(),
        meta = %meta_path.display(),
        "output       {} + {}",
        out.display(),
        meta_path.display()
    );
    if a.dry_run {
        info!(target: "isdb::capture", "dry run: radio not opened");
        return Ok(());
    }
    let data = create(out, a.overwrite)?;
    let mut meta_file = match create(&meta_path, a.overwrite) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(out);
            return Err(e);
        }
    };
    // A failed capture leaves no partial recording behind.
    let written = take_captures(a, backend, &cfg, samples, Some(data)).and_then(|caps| {
        let m = metadata(a, &caps)?;
        meta_file
            .write_all(m.to_json().as_bytes())
            .with_context(|| format!("write {}", meta_path.display()))?;
        let total: u64 = caps.iter().map(|c| c.samples).sum();
        info!(
            target: "isdb::capture",
            samples = total,
            milliseconds = total as f64 / m.sample_rate * 1e3,
            file = %out.display(),
            segments = m.captures.len(),
            device = caps[0].device,
            "done: {total} samples ({:.1} ms) in {}, {} SigMF segment(s); device {}",
            total as f64 / m.sample_rate * 1e3,
            out.display(),
            m.captures.len(),
            caps[0].device
        );
        Ok(())
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(out);
        let _ = std::fs::remove_file(&meta_path);
        return Err(e);
    }
    if a.survey {
        crate::iq::survey_file(out, None, None, None, 0, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use isdb_radio::CaptureGap;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        a: CaptureArgs,
    }

    fn args(v: &[&str]) -> Result<CaptureArgs, clap::Error> {
        T::try_parse_from(std::iter::once("t").chain(v.iter().copied())).map(|t| t.a)
    }

    #[test]
    fn arguments_and_radio_config() {
        let a = args(&[
            "--radio",
            "pluto:uri=ip:192.0.2.1",
            "--freq",
            "bs1",
            "--rate",
            "57.72M",
            "--samples",
            "4Mi",
            "--gain",
            "agc=slow_attack",
            "-o",
            "x.sigmf-data",
        ])
        .unwrap();
        assert_eq!(a.freq.hz, 1_049_480_000);
        assert_eq!(a.samples, Some(4 << 20));
        let c = a.radio_config().unwrap();
        assert_eq!(
            (c.center_hz, c.rate_num, c.rate_den),
            (1_049_480_000, 57_720_000, 1)
        );
        assert_eq!(c.bandwidth_hz, 46_176_000);
        assert_eq!(c.args, vec![("uri".to_string(), "ip:192.0.2.1".to_string())]);
        assert_eq!(
            c.gains,
            vec![("agc".to_string(), "slow_attack".to_string())]
        );
        let a = args(&[
            "--radio", "pluto", "--freq", "1613M", "--rate", "61440000", "--bw", "56M", "-o", "y",
        ])
        .unwrap();
        let c = a.radio_config().unwrap();
        assert_eq!((c.center_hz, c.bandwidth_hz), (1_613_000_000, 56_000_000));
        assert_eq!(a.freq.channel, None);
        for bad in [
            vec![
                "--radio", "pluto", "--freq", "bs25", "--rate", "1M", "-o", "y",
            ],
            vec![
                "--radio", "pluto", "--freq", "bs1", "--rate", "x", "-o", "y",
            ],
            vec!["--radio", "pluto", "--freq", "bs1", "--rate", "1M"],
            vec![
                "--radio",
                "pluto",
                "--freq",
                "bs1",
                "--rate",
                "1M",
                "--samples",
                "1.5",
                "-o",
                "y",
            ],
        ] {
            assert!(args(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn metadata_marks_gaps_and_settings() {
        let a = args(&[
            "--radio", "pluto", "--freq", "nd2", "--rate", "61.44M", "-o", "z",
        ])
        .unwrap();
        let cap = |t: f64, gaps: Vec<CaptureGap>| Capture {
            format: CaptureFormat::Cs16 { full_scale: 2048.0 },
            samples: 4,
            gaps,
            sample_rate_hz: 61_440_000.0,
            lo_hz: 1_612_999_998,
            rf_bandwidth_hz: Some(49_152_000),
            gain: "AGC slow_attack, 33 dB at the end".into(),
            device: "PlutoSky".into(),
            method: "libiio single buffer".into(),
            time: t,
        };
        let m = metadata(
            &a,
            &[cap(1_792_484_130.0, vec![]), cap(1_792_484_131.0, vec![])],
        )
        .unwrap();
        assert_eq!(m.format, SampleFormat::AD936X_CS16);
        assert_eq!(m.captures.len(), 2);
        assert_eq!(m.captures[1].sample_start, 4);
        assert_eq!(m.captures[0].frequency, Some(1_613_000_000.0));
        assert_eq!(m.captures[0].extra["isdb:channel"], json!("ND2"));
        assert_eq!(
            m.captures[0].extra["isdb:channel_rf_hz"],
            json!(12_291_000_000u64)
        );
        assert_eq!(
            m.captures[0].extra["isdb:lnb_lo_hz"],
            json!(10_678_000_000u64)
        );
        assert_eq!(m.captures[0].extra["isdb:polarisation"], json!("right"));
        assert_eq!(m.captures[0].extra["isdb:lo_hz"], json!(1_612_999_998u64));
        assert!(!m.captures[0].extra.contains_key("isdb:gap_before"));
        assert_eq!(m.captures[1].extra["isdb:gap_before"], json!(true));
        assert!(!m.captures[1].extra.contains_key("isdb:samples_discarded"));
        assert_eq!(
            m.captures[1].datetime.as_deref(),
            Some("2026-10-20T08:15:31.000Z")
        );
        let back = Meta::from_json(&m.to_json()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.segment_ranges(8), vec![0..4, 4..8]);

        // A streamed cs8 capture with a loss after 3 samples (and one
        // reported after the last sample, which leaves nothing to mark).
        let mut c = cap(1_792_484_130.0, vec![]);
        c.format = CaptureFormat::Cs8;
        c.samples = 10;
        c.gaps = vec![
            CaptureGap {
                at: 3,
                discarded: 1000,
            },
            CaptureGap {
                at: 10,
                discarded: 7,
            },
        ];
        let m = metadata(&a, &[c.clone()]).unwrap();
        assert_eq!(m.format, SampleFormat::Cs8);
        assert_eq!(m.captures.len(), 2);
        assert_eq!(
            m.captures[1].extra["isdb:samples_discarded_after"],
            json!(7)
        );
        assert!(
            !m.captures[0]
                .extra
                .contains_key("isdb:samples_discarded_after")
        );
        assert_eq!(m.captures[1].sample_start, 3);
        assert_eq!(m.captures[1].extra["isdb:gap_before"], json!(true));
        assert_eq!(m.captures[1].extra["isdb:samples_discarded"], json!(1000));
        assert_eq!(m.captures[1].datetime, None);
        assert_eq!(m.segment_ranges(10), vec![0..3, 3..10]);
        // Formats cannot mix in one file.
        assert!(metadata(&a, &[c, cap(0.0, vec![])]).is_err());

        // A left-hand-circular channel records the 9.505 GHz LO, so the
        // recording leads back to its satellite channel.
        let a = args(&[
            "--radio", "pluto", "--freq", "BS14", "--rate", "40M", "-o", "z",
        ])
        .unwrap();
        assert_eq!(a.radio_config().unwrap().center_hz, 2_471_820_000);
        let m = metadata(&a, &[cap(0.0, vec![])]).unwrap();
        let x = &m.captures[0].extra;
        assert_eq!(m.captures[0].frequency, Some(2_471_820_000.0));
        assert_eq!(x["isdb:channel"], json!("BS14"));
        assert_eq!(x["isdb:channel_rf_hz"], json!(11_976_820_000u64));
        assert_eq!(x["isdb:lnb_lo_hz"], json!(9_505_000_000u64));
        assert_eq!(x["isdb:polarisation"], json!("left"));
        // A frequency, or a terrestrial channel, has no LNB.
        for f in ["2471.82M", "T27"] {
            let a = args(&["--radio", "pluto", "--freq", f, "--rate", "40M", "-o", "z"]).unwrap();
            let m = metadata(&a, &[cap(0.0, vec![])]).unwrap();
            let x = &m.captures[0].extra;
            assert!(!x.contains_key("isdb:lnb_lo_hz") && !x.contains_key("isdb:polarisation"));
            assert_eq!(x.contains_key("isdb:channel_rf_hz"), f == "T27");
        }
    }

    #[test]
    fn discard_and_seconds() {
        let a = args(&[
            "--radio",
            "pluto:recorder=stream,bits=8",
            "--freq",
            "bs1",
            "--rate",
            "40M",
            "--seconds",
            "2.5",
            "--discard",
        ])
        .unwrap();
        assert!(a.discard && a.out.is_none());
        assert_eq!(a.seconds, Some(2.5));
        // --discard and --out exclude each other; one of them is needed;
        // --samples and --seconds exclude each other.
        for bad in [
            vec![
                "--radio",
                "pluto",
                "--freq",
                "bs1",
                "--rate",
                "1M",
                "--discard",
                "-o",
                "x",
            ],
            vec!["--radio", "pluto", "--freq", "bs1", "--rate", "1M"],
            vec![
                "--radio",
                "pluto",
                "--freq",
                "bs1",
                "--rate",
                "1M",
                "--samples",
                "1Mi",
                "--seconds",
                "1",
                "-o",
                "x",
            ],
        ] {
            assert!(args(&bad).is_err(), "{bad:?}");
        }
    }
}
