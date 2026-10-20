//! `isdb capture`: receive one contiguous block of raw IQ samples from a
//! radio at any rate it supports and save it with SigMF metadata. Receive
//! only; nothing is transmitted.

use crate::bands::{Freq, parse_count, parse_hz};
use crate::phyargs::parse_gain;
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_iq::sigmf::{Meta, Segment};
use isdb_iq::{SampleFormat, meta_path_for};
use isdb_radio::{Capture, DeviceSpec, RadioConfig};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `isdb capture` arguments.
#[derive(Debug, Args)]
pub struct CaptureArgs {
    /// The radio: BACKEND[:key=value,...] (e.g.
    /// `pluto:uri=ip:192.0.2.30`, `pluto:uri=ip:192.0.2.30,recorder=maia`).
    #[arg(long, value_name = "SPEC")]
    pub radio: DeviceSpec,
    /// Centre frequency: Hz (`1049480000`, `1049.48M`) or a channel name:
    /// `chN` (UHF 13..62), `bsN` (BS, odd 1..23) and `ndN` (110° CS, even
    /// 2..24) at their satellite IF (RF − 10.678 GHz).
    #[arg(long, value_name = "HZ|CHANNEL")]
    pub freq: Freq,
    /// Sample rate in samples per second (`57.72M`, `61440000`). The rate
    /// the device runs at is read back and recorded.
    #[arg(long, value_name = "SPS", value_parser = parse_hz)]
    pub rate: f64,
    /// Samples per segment (`4Mi`, `16515072`); default: the largest the
    /// radio captures contiguously.
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub samples: Option<usize>,
    /// Analog bandwidth request in Hz; default 0.8 × the sample rate.
    #[arg(long, value_name = "HZ", value_parser = parse_hz)]
    pub bw: Option<f64>,
    /// Radio gain NAME=VALUE (repeatable; the backend's names, e.g.
    /// `agc=slow_attack` or `gain=30` for a Pluto).
    #[arg(long, value_name = "NAME=VALUE", value_parser = parse_gain)]
    pub gain: Vec<(String, String)>,
    /// Number of captures written one after another into the same file.
    /// Samples between them are lost: each capture is its own SigMF
    /// capture segment, marked `isdb:gap_before` from the second on.
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub segments: usize,
    /// Free text for the SigMF `core:description` (do not put station
    /// names in recordings you share).
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,
    /// Output data file (cs16, little-endian I/Q). The SigMF metadata goes
    /// next to it: `NAME.sigmf-meta` for `NAME.sigmf-data`, else the file
    /// name with `.sigmf-meta` appended.
    #[arg(short, long, value_name = "PATH")]
    pub out: PathBuf,
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
            rate_num: self.rate.round() as u32,
            rate_den: 1,
            bandwidth_hz: bw as u32,
            args: self.radio.args.clone(),
            gains: self.gain.clone(),
        })
    }
}

/// The SigMF metadata of `caps` (one segment each, with its number of
/// samples; the samples themselves are not used) taken with `a`.
pub fn metadata(a: &CaptureArgs, caps: &[(Capture, u64)]) -> Result<Meta> {
    let (first, _) = caps.first().context("no capture")?;
    let mut m = Meta::new(
        SampleFormat::Cs16 {
            full_scale: first.full_scale,
        },
        first.sample_rate_hz,
    );
    m.recorder = Some(format!("isdb capture {}", env!("CARGO_PKG_VERSION")));
    m.hw = Some(format!("{} ({})", first.device, a.radio.backend));
    m.description.clone_from(&a.description);
    let mut start = 0u64;
    for (i, (c, n)) in caps.iter().enumerate() {
        if c.full_scale != first.full_scale || c.sample_rate_hz != first.sample_rate_hz {
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
        }
        if i > 0 {
            extra.insert("isdb:gap_before".into(), json!(true));
        }
        m.captures.push(Segment {
            sample_start: start,
            frequency: Some(a.freq.hz as f64),
            datetime: Some(isdb_iq::iso8601_utc(c.time)),
            extra,
        });
        start += n;
    }
    Ok(m)
}

fn create(path: &Path, overwrite: bool) -> Result<std::fs::File> {
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

/// Capture the segments into `data` and write their metadata.
fn write_captures(
    a: &CaptureArgs,
    backend: &dyn isdb_radio::Backend,
    cfg: &RadioConfig,
    samples: usize,
    data: std::fs::File,
    mut meta_file: std::fs::File,
    meta_path: &Path,
) -> Result<()> {
    let mut data = std::io::BufWriter::new(data);
    let mut caps = Vec::with_capacity(a.segments);
    for i in 0..a.segments {
        let t = std::time::Instant::now();
        let mut c = backend.capture(cfg, samples)?;
        let n = c.iq.len() / 2;
        eprintln!(
            "[capture] segment {}/{}: {} samples at {:.2} Hz, LO {} Hz, {}, {} ({:.2} s)",
            i + 1,
            a.segments,
            n,
            c.sample_rate_hz,
            c.lo_hz,
            c.gain,
            c.method,
            t.elapsed().as_secs_f64()
        );
        let bytes: Vec<u8> = c.iq.iter().flat_map(|v| v.to_le_bytes()).collect();
        data.write_all(&bytes)
            .with_context(|| format!("write {}", a.out.display()))?;
        // Keep the metadata only: the samples are on disk.
        c.iq = Vec::new();
        caps.push((c, n as u64));
    }
    data.flush()
        .with_context(|| format!("write {}", a.out.display()))?;
    let total: u64 = caps.iter().map(|(_, n)| n).sum();
    let m = metadata(a, &caps)?;
    meta_file
        .write_all(m.to_json().as_bytes())
        .with_context(|| format!("write {}", meta_path.display()))?;
    eprintln!(
        "[capture] done: {total} samples ({:.1} ms) in {}; device {}",
        total as f64 / m.sample_rate * 1e3,
        a.out.display(),
        caps[0].0.device
    );
    Ok(())
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
    let samples = a.samples.unwrap_or(limit);
    if samples == 0 || samples > limit {
        bail!(
            "--samples {samples}: {} captures at most {limit} contiguous samples ({why}); use --segments for more (with gaps)",
            backend.name()
        );
    }
    let label = a.freq.channel.map(|c| c.label()).unwrap_or_default();
    eprintln!("{} capture settings{label}:\n{settings}", backend.name());
    eprintln!(
        "  capture      {} segment(s) of {samples} samples ({:.1} ms each at the requested rate); at most {limit} ({why})",
        a.segments,
        samples as f64 / cfg.sample_rate_hz() * 1e3
    );
    let meta_path = meta_path_for(&a.out);
    eprintln!(
        "  output       {} + {}",
        a.out.display(),
        meta_path.display()
    );
    if a.dry_run {
        eprintln!("dry run: radio not opened");
        return Ok(());
    }
    let data = create(&a.out, a.overwrite)?;
    let meta_file = match create(&meta_path, a.overwrite) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&a.out);
            return Err(e);
        }
    };
    // A failed capture leaves no partial recording behind.
    if let Err(e) = write_captures(a, backend, &cfg, samples, data, meta_file, &meta_path) {
        let _ = std::fs::remove_file(&a.out);
        let _ = std::fs::remove_file(&meta_path);
        return Err(e);
    }
    if a.survey {
        crate::iq::survey_file(&a.out, None, None, None, 0, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
                "--radio", "pluto", "--freq", "bs2", "--rate", "1M", "-o", "y",
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
        let cap = |t: f64| Capture {
            iq: vec![0; 8],
            full_scale: 2048.0,
            sample_rate_hz: 61_440_000.0,
            lo_hz: 1_612_999_998,
            rf_bandwidth_hz: Some(49_152_000),
            gain: "AGC slow_attack, 33 dB at the end".into(),
            device: "PlutoSky".into(),
            method: "libiio single buffer".into(),
            time: t,
        };
        let m = metadata(&a, &[(cap(1_792_484_130.0), 4), (cap(1_792_484_131.0), 4)]).unwrap();
        assert_eq!(m.format, SampleFormat::AD936X_CS16);
        assert_eq!(m.captures.len(), 2);
        assert_eq!(m.captures[1].sample_start, 4);
        assert_eq!(m.captures[0].frequency, Some(1_613_000_000.0));
        assert_eq!(m.captures[0].extra["isdb:channel"], json!("nd2"));
        assert_eq!(
            m.captures[0].extra["isdb:channel_rf_hz"],
            json!(12_291_000_000u64)
        );
        assert_eq!(m.captures[0].extra["isdb:lo_hz"], json!(1_612_999_998u64));
        assert!(!m.captures[0].extra.contains_key("isdb:gap_before"));
        assert_eq!(m.captures[1].extra["isdb:gap_before"], json!(true));
        assert_eq!(
            m.captures[1].datetime.as_deref(),
            Some("2026-10-20T08:15:31.000Z")
        );
        let back = Meta::from_json(&m.to_json()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.segment_ranges(8), vec![0..4, 4..8]);
    }
}
