//! `arib rx`: IQ file or HackRF → ISDB-T demodulation → TS, with status.

use crate::phyargs::{TuneArgs, describe_params};
use crate::radio::RxRfSettings;
use crate::sink::IqFormat;
use crate::source::{Packet, is_null};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdbt_core::{Params, sample_rate_hz};
use isdbt_ofdm::rx::RxState;
use isdbt_ofdm::{C32, DemodConfig};
use isdbt_phy::{PipelinedReceiver, RxFrame};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// `arib rx` arguments.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["iq", "hackrf"])))]
pub struct RxArgs {
    /// Read baseband from this file ("-" = stdin) at 512/63 MHz.
    #[arg(long, value_name = "PATH")]
    pub iq: Option<String>,
    /// IQ file format.
    #[arg(long, value_enum, default_value_t = IqFormat::Cf32)]
    pub iq_format: IqFormat,
    /// Receive with a HackRF (needs --channel/--freq).
    #[arg(long)]
    pub hackrf: bool,
    #[command(flatten)]
    pub tune: TuneArgs,
    /// HackRF LNA gain, 0..40 dB in 8 dB steps.
    #[arg(long, default_value_t = 16)]
    pub lna: u32,
    /// HackRF baseband VGA gain, 0..62 dB in 2 dB steps.
    #[arg(long, default_value_t = 20)]
    pub vga: u32,
    /// Enable the HackRF RF amplifier.
    #[arg(long)]
    pub amp: bool,
    /// Duration bound in seconds of signal.  Default 0 = unbounded: the
    /// whole file for --iq, until Ctrl-C / SIGTERM for --hackrf.
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
    /// Write the reproduced single TS (§3.2.2, valid frames only).
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
    /// Write layer A packets (valid frames only).
    #[arg(long, value_name = "PATH")]
    pub a_out: Option<String>,
    /// Write layer B packets.
    #[arg(long, value_name = "PATH")]
    pub b_out: Option<String>,
    /// Write layer C packets.
    #[arg(long, value_name = "PATH")]
    pub c_out: Option<String>,
    /// Drop null packets (PID 0x1FFF) from all outputs.
    #[arg(long)]
    pub strip_nulls: bool,
    /// Print a JSON summary on stdout at the end.
    #[arg(long)]
    pub json: bool,
    /// Status line interval in seconds of signal (0 = none).
    #[arg(long, default_value_t = 1.0)]
    pub status_secs: f64,
}

/// Per-layer reception counters.
#[derive(Debug, Clone, Default)]
pub struct LayerStats {
    /// Packets output (valid frames).
    pub packets: u64,
    /// RS words decoded without errors (valid frames).
    pub clean: u64,
    /// RS words with corrected bytes.
    pub corrected_packets: u64,
    /// Corrected bytes.
    pub corrected_bytes: u64,
    /// Uncorrectable RS words.
    pub uncorrectable: u64,
    mer_sum: f64,
    mer_n: u64,
    mer_min: f32,
    mer_last: Option<f32>,
}

impl LayerStats {
    fn add(&mut self, fr: &RxFrame, l: usize) {
        // RS counters of warm-up frames describe filler, not the signal.
        if let (Some(rs), true) = (fr.rs[l], fr.valid) {
            self.clean += rs.clean as u64;
            self.corrected_packets += rs.corrected_packets as u64;
            self.corrected_bytes += rs.corrected_bytes as u64;
            self.uncorrectable += rs.uncorrectable as u64;
        }
        if let Some(m) = fr.metrics.mer_db[l] {
            self.mer_sum += m as f64;
            self.mer_min = if self.mer_n == 0 {
                m
            } else {
                self.mer_min.min(m)
            };
            self.mer_n += 1;
            self.mer_last = Some(m);
        }
        if fr.valid {
            self.packets += fr.layers[l].as_ref().map_or(0, |v| v.len() as u64);
        }
    }

    /// Mean MER in dB.
    pub fn mer_mean(&self) -> Option<f64> {
        (self.mer_n > 0).then(|| self.mer_sum / self.mer_n as f64)
    }

    fn json(&self) -> Value {
        json!({
            "packets": self.packets,
            "rs_clean": self.clean,
            "rs_corrected_packets": self.corrected_packets,
            "rs_corrected_bytes": self.corrected_bytes,
            "rs_uncorrectable": self.uncorrectable,
            "mer_db_mean": self.mer_mean(),
            "mer_db_min": (self.mer_n > 0).then_some(self.mer_min),
        })
    }
}

/// Reception statistics (whole run and current status interval).
#[derive(Debug, Clone, Default)]
pub struct RxStats {
    /// Frames output by the receiver.
    pub frames: u64,
    /// Of which valid.
    pub valid: u64,
    /// TMCC parity failures.
    pub tmcc_failed: u64,
    /// Per layer.
    pub layers: [LayerStats; 3],
    /// Parameters of the last frame.
    pub params: Option<Params>,
    /// Last CFO estimate (carrier spacings).
    pub cfo: Option<f64>,
    /// Last timing drift (samples/symbol).
    pub timing_drift: Option<f64>,
    interval: Option<Box<RxStats>>,
}

impl RxStats {
    /// Account one receiver frame.
    pub fn add(&mut self, fr: &RxFrame) {
        self.add_one(fr);
        self.interval
            .get_or_insert_with(Default::default)
            .add_one(fr);
    }

    fn add_one(&mut self, fr: &RxFrame) {
        self.frames += 1;
        self.valid += u64::from(fr.valid);
        self.tmcc_failed += u64::from(!fr.tmcc.parity.is_ok());
        for (l, st) in self.layers.iter_mut().enumerate() {
            st.add(fr, l);
        }
        self.params = Some(fr.params.clone());
        self.cfo = Some(fr.metrics.cfo_carriers);
        self.timing_drift = Some(fr.metrics.timing_drift);
    }

    /// Status line over the interval since the previous call.
    pub fn status_line(&mut self, t: f64, state: RxState) -> String {
        let iv = self.interval.take().map(|b| *b).unwrap_or_default();
        let lock = match state {
            RxState::Locked => "LOCKED",
            RxState::Acquiring => "acquiring",
            RxState::Searching => "searching",
        };
        let mut s = format!("[rx] {t:7.1} s {lock:9}");
        let Some(p) = &self.params else {
            return s + " (no frame yet)";
        };
        s += &format!(
            " M{} GI1/{} | frames {}/{} valid",
            p.mode.number(),
            p.guard.denominator(),
            iv.valid,
            iv.frames
        );
        s += " | MER";
        for (id, _) in p.active_layers() {
            match iv.layers[id.index()].mer_last {
                Some(m) => s += &format!(" {id} {m:.1}"),
                None => s += &format!(" {id} -"),
            }
        }
        s += " dB";
        if let Some(c) = iv.cfo {
            s += &format!(" | CFO {c:+.3} car");
        }
        s += " | RS clean/corr/uncorr";
        for (id, _) in p.active_layers() {
            let l = &iv.layers[id.index()];
            s += &format!(
                " {id} {}/{}/{}",
                l.clean, l.corrected_packets, l.uncorrectable
            );
        }
        if iv.tmcc_failed > 0 {
            s += &format!(" | TMCC parity failed {}", iv.tmcc_failed);
        }
        s
    }

    /// JSON summary.
    pub fn json(&self) -> Value {
        let layers: serde_json::Map<String, Value> = match &self.params {
            Some(p) => p
                .active_layers()
                .map(|(id, _)| (id.to_string(), self.layers[id.index()].json()))
                .collect(),
            None => Default::default(),
        };
        json!({
            "frames": self.frames,
            "valid_frames": self.valid,
            "tmcc_parity_failed": self.tmcc_failed,
            "params": self.params.as_ref().map(params_json),
            "cfo_carriers": self.cfo,
            "timing_drift_samples_per_symbol": self.timing_drift,
            "layers": layers,
        })
    }
}

/// Parameters as JSON.
pub fn params_json(p: &Params) -> Value {
    let layers: serde_json::Map<String, Value> = p
        .active_layers()
        .map(|(id, l)| {
            let (n, d) = l.code_rate.ratio();
            (
                id.to_string(),
                json!({
                    "segments": l.segments,
                    "modulation": format!("{:?}", l.modulation),
                    "code_rate": format!("{n}/{d}"),
                    "time_interleave_index": l.time_interleave.0,
                    "tsp_per_frame": l.tsp_per_frame(p.mode),
                }),
            )
        })
        .collect();
    json!({
        "mode": p.mode.number(),
        "guard_interval": format!("1/{}", p.guard.denominator()),
        "partial_reception": p.partial_reception,
        "layers": layers,
        "description": describe_params(p),
    })
}

/// A source of baseband chunks.
pub trait IqSource {
    /// Next chunk; `None` at end.
    fn read(&mut self, stop: &AtomicBool) -> Result<Option<Vec<C32>>>;
    /// Status fragment.
    fn status(&self) -> String {
        String::new()
    }
    /// Stop the device (radio) and return a summary.
    fn finish(&mut self) -> Result<String> {
        Ok(String::new())
    }
}

/// cf32/cs8 file or stdin.
pub struct FileIqSource {
    r: Box<dyn Read + Send>,
    format: IqFormat,
    buf: Vec<u8>,
    carry: Vec<u8>,
}

impl FileIqSource {
    /// Open `path` ("-" = stdin).
    pub fn open(path: &str, format: IqFormat) -> Result<Self> {
        let r: Box<dyn Read + Send> = if path == "-" {
            Box::new(std::io::stdin())
        } else {
            Box::new(std::fs::File::open(path).with_context(|| format!("open {path}"))?)
        };
        Ok(Self {
            r,
            format,
            buf: vec![0; 1 << 23],
            carry: Vec::new(),
        })
    }
}

impl IqSource for FileIqSource {
    fn read(&mut self, _stop: &AtomicBool) -> Result<Option<Vec<C32>>> {
        let n = loop {
            match self.r.read(&mut self.buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        };
        if n == 0 {
            return Ok(None);
        }
        self.carry.extend_from_slice(&self.buf[..n]);
        let unit = match self.format {
            IqFormat::Cf32 => 8,
            IqFormat::Cs8 => 2,
        };
        let used = self.carry.len() / unit * unit;
        let s = match self.format {
            IqFormat::Cf32 => isdbt_phy::iq::cf32_from_bytes(&self.carry[..used]),
            IqFormat::Cs8 => {
                let v: Vec<i8> = self.carry[..used].iter().map(|&b| b as i8).collect();
                isdbt_phy::iq::samples_from_cs8(&v)
            }
        };
        self.carry.drain(..used);
        Ok(Some(s))
    }
}

/// HackRF receive stream.
pub struct HackrfIqSource {
    dev: hackrf::Device,
    stream: Option<hackrf::RxStream>,
}

impl IqSource for HackrfIqSource {
    fn read(&mut self, stop: &AtomicBool) -> Result<Option<Vec<C32>>> {
        let Some(st) = &self.stream else {
            return Ok(None);
        };
        loop {
            match st.receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(b) => return Ok(Some(isdbt_phy::iq::samples_from_cs8(&b))),
                Err(RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => return Ok(None),
                Err(RecvTimeoutError::Timeout) => {
                    if st.counters.finished.load(Ordering::Relaxed) {
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

/// Output files.
#[derive(Default)]
pub struct Outputs {
    ts: Option<std::io::BufWriter<Box<dyn Write>>>,
    layers: [Option<std::io::BufWriter<Box<dyn Write>>>; 3],
    strip_nulls: bool,
    /// Packets written: TS, A, B, C.
    pub written: [u64; 4],
}

fn create(path: &str) -> Result<std::io::BufWriter<Box<dyn Write>>> {
    let w: Box<dyn Write> = if path == "-" {
        Box::new(std::io::stdout())
    } else {
        Box::new(std::fs::File::create(path).with_context(|| format!("create {path}"))?)
    };
    Ok(std::io::BufWriter::with_capacity(1 << 20, w))
}

impl Outputs {
    /// Open the requested outputs.
    pub fn open(ts: Option<&str>, layers: [Option<&str>; 3], strip_nulls: bool) -> Result<Self> {
        let mut o = Outputs {
            strip_nulls,
            ..Default::default()
        };
        o.ts = ts.map(create).transpose()?;
        for (l, p) in layers.iter().enumerate() {
            o.layers[l] = p.map(create).transpose()?;
        }
        Ok(o)
    }

    fn put(
        w: &mut std::io::BufWriter<Box<dyn Write>>,
        pkts: &[Packet],
        strip: bool,
        n: &mut u64,
    ) -> Result<()> {
        for p in pkts {
            if strip && is_null(p) {
                continue;
            }
            w.write_all(p)?;
            *n += 1;
        }
        Ok(())
    }

    /// Write a valid frame.
    pub fn write(&mut self, fr: &RxFrame) -> Result<()> {
        if !fr.valid {
            return Ok(());
        }
        if let Some(w) = &mut self.ts {
            Self::put(w, &fr.ts, self.strip_nulls, &mut self.written[0])?;
        }
        for l in 0..3 {
            if let (Some(w), Some(p)) = (&mut self.layers[l], &fr.layers[l]) {
                Self::put(w, p, self.strip_nulls, &mut self.written[l + 1])?;
            }
        }
        Ok(())
    }

    /// Flush everything.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(w) = &mut self.ts {
            w.flush()?;
        }
        for w in self.layers.iter_mut().flatten() {
            w.flush()?;
        }
        Ok(())
    }
}

/// Demodulate `src` until it ends, `max_samples` or stop; every frame is
/// passed to `on_frame`.  Returns the number of samples consumed.
pub fn receive(
    src: &mut dyn IqSource,
    max_samples: Option<u64>,
    status_secs: f64,
    stop: &AtomicBool,
    mut on_frame: impl FnMut(&RxFrame) -> Result<()>,
    stats: &mut RxStats,
) -> Result<u64> {
    let mut prx = PipelinedReceiver::new(DemodConfig::default(), 4);
    let fs = sample_rate_hz();
    let mut samples = 0u64;
    let mut next_status = status_secs;
    let mut handle = |r: isdbt_phy::Result<RxFrame>, stats: &mut RxStats| -> Result<()> {
        let fr = r?;
        stats.add(&fr);
        on_frame(&fr)
    };
    while !stop.load(Ordering::Relaxed) {
        let Some(mut chunk) = src.read(stop)? else {
            break;
        };
        if let Some(m) = max_samples {
            let left = m.saturating_sub(samples) as usize;
            chunk.truncate(left);
        }
        samples += chunk.len() as u64;
        prx.push(chunk);
        while let Some(r) = prx.try_recv() {
            handle(r, stats)?;
        }
        let t = samples as f64 / fs;
        if status_secs > 0.0 && t >= next_status {
            while next_status <= t {
                next_status += status_secs;
            }
            eprintln!("{}{}", stats.status_line(t, prx.state()), src.status());
        }
        if max_samples.is_some_and(|m| samples >= m) {
            break;
        }
    }
    for r in prx.finish() {
        handle(r, stats)?;
    }
    Ok(samples)
}

/// Entry point.
pub fn run(a: RxArgs) -> Result<()> {
    if a.json
        && [&a.out, &a.a_out, &a.b_out, &a.c_out]
            .iter()
            .any(|o| o.as_deref() == Some("-"))
    {
        bail!("--json prints on stdout; write the TS outputs to files");
    }
    let fs = sample_rate_hz();
    let seconds = crate::tx::resolve_seconds(a.seconds)?;
    let max_samples = (seconds > 0.0).then(|| (seconds * fs).round() as u64);
    let stop = crate::stop::flag();
    let (mut src, input_desc): (Box<dyn IqSource>, String) = if a.hackrf {
        let rf = RxRfSettings {
            tuning: a.tune.tuning()?,
            lna_db: a.lna,
            vga_db: a.vga,
            amp: a.amp,
            bb_filter_hz: a.tune.bb_filter_hz,
            serial: a.tune.serial.clone(),
        };
        rf.check()?;
        eprintln!("HackRF RX settings:\n{}", rf.describe());
        let (mut dev, info) = crate::radio::open_rx(&rf)?;
        eprintln!("  opened: {info}");
        let stream = dev.start_rx(64)?;
        (
            Box::new(HackrfIqSource {
                dev,
                stream: Some(stream),
            }),
            format!("HackRF {}", rf.tuning.describe()),
        )
    } else {
        let p = a.iq.as_deref().expect("clap group");
        (
            Box::new(FileIqSource::open(p, a.iq_format)?),
            format!("{p} ({:?})", a.iq_format),
        )
    };
    eprintln!(
        "arib rx: {input_desc}; {}",
        match max_samples {
            Some(_) => format!("for {seconds} s"),
            None if a.hackrf => "unbounded; stop with Ctrl-C".into(),
            None => "until end of input".into(),
        }
    );
    let mut out = Outputs::open(
        a.out.as_deref(),
        [a.a_out.as_deref(), a.b_out.as_deref(), a.c_out.as_deref()],
        a.strip_nulls,
    )?;
    let mut stats = RxStats::default();
    let res = receive(
        src.as_mut(),
        max_samples,
        a.status_secs,
        &stop,
        |fr| out.write(fr),
        &mut stats,
    );
    let fin = src.finish();
    out.flush()?;
    let samples = res?;
    let fin = fin?;
    let secs = samples as f64 / fs;
    eprintln!(
        "[rx] done: {:.2} s of signal, {} frames ({} valid){}",
        secs,
        stats.frames,
        stats.valid,
        stats
            .params
            .as_ref()
            .map_or(" — no ISDB-T signal found".into(), |p| format!(
                "; {}",
                describe_params(p)
            ))
    );
    if let Some(p) = &stats.params {
        for (id, _) in p.active_layers() {
            let l = &stats.layers[id.index()];
            eprintln!(
                "[rx]   layer {id}: {} packets; RS clean {} corrected {} ({} bytes) uncorrectable {}; MER mean {}",
                l.packets,
                l.clean,
                l.corrected_packets,
                l.corrected_bytes,
                l.uncorrectable,
                l.mer_mean().map_or("-".into(), |m| format!("{m:.1} dB"))
            );
        }
    }
    if !fin.is_empty() {
        eprintln!("[rx] {fin}");
    }
    if a.json {
        let mut j = stats.json();
        j["input"] = json!(input_desc);
        j["samples"] = json!(samples);
        j["seconds"] = json!(secs);
        j["written_packets"] = json!({"ts": out.written[0], "A": out.written[1], "B": out.written[2], "C": out.written[3]});
        if !fin.is_empty() {
            j["radio"] = json!(fin);
        }
        println!("{}", serde_json::to_string_pretty(&j)?);
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
        a: RxArgs,
    }

    #[test]
    fn rx_args() {
        let a = T::try_parse_from([
            "rx",
            "--iq",
            "x.cs8",
            "--iq-format",
            "cs8",
            "--out",
            "o.ts",
            "--json",
        ])
        .unwrap()
        .a;
        assert_eq!(a.iq_format, IqFormat::Cs8);
        assert_eq!((a.lna, a.vga, a.amp), (16, 20, false));
        assert!(T::try_parse_from(["rx"]).is_err());
        assert!(T::try_parse_from(["rx", "--iq", "x", "--hackrf"]).is_err());
        let h = T::try_parse_from(["rx", "--hackrf", "--channel", "27", "--lna", "24"])
            .unwrap()
            .a;
        assert_eq!(h.tune.tuning().unwrap().nominal_hz, 557_142_857);
        // Default: no bound (whole file / until Ctrl-C), same as tx.
        for s in [a.seconds, h.seconds] {
            assert_eq!(crate::tx::resolve_seconds(s).unwrap(), 0.0);
        }
        let bad = RxRfSettings {
            tuning: h.tune.tuning().unwrap(),
            lna_db: 12,
            vga_db: 20,
            amp: false,
            bb_filter_hz: 6_000_000,
            serial: None,
        };
        assert!(bad.check().is_err());
    }

    #[test]
    fn file_source_handles_partial_samples() {
        let dir = std::env::temp_dir().join(format!("arib-rx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.cs8");
        std::fs::write(&p, [64u8, 192, 0, 127, 5]).unwrap();
        let mut s = FileIqSource::open(p.to_str().unwrap(), IqFormat::Cs8).unwrap();
        let stop = AtomicBool::new(false);
        let v = s.read(&stop).unwrap().unwrap();
        assert_eq!(v, vec![C32::new(0.5, -0.5), C32::new(0.0, 127.0 / 128.0)]);
        assert!(s.read(&stop).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
