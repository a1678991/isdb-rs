//! `isdb rx`: IQ file or radio → ISDB-T, ISDB-S or ISDB-S3 demodulation →
//! TS (or TLV streams), with status. ISDB-S (`--system isdbs`, or `auto` on
//! a satellite channel or recording) is in [`crate::rxs`], ISDB-S3
//! (`--system isdbs3`, or `auto` on a recording) in [`crate::rxs3`].

use crate::phyargs::{TuneArgs, describe_params, parse_gain};
use crate::sink::IqFormat;
use crate::source::{Packet, is_null};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdb_radio::{DeviceSpec, IqSource, RxOptions};
use isdbt_core::{Params, sample_rate_hz};
use isdbt_ofdm::rx::RxState;
use isdbt_ofdm::{C32, DemodConfig};
use isdbt_phy::{PipelinedReceiver, RxFrame};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

/// Seconds of signal the radio's receive queue holds before the backend
/// drops buffers (reported as overruns); each backend converts this to
/// its own buffer count.
const RX_QUEUE_SECONDS: f64 = 1.0;

/// `isdb rx` arguments.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["iq", "radio", "symbols"])))]
pub struct RxArgs {
    /// Read baseband from this file ("-" = stdin) at 512/63 MHz. A SigMF
    /// recording (data file, `.sigmf-meta` or base name) gives its format
    /// and must have been recorded at 512/63 MHz.
    #[arg(long, value_name = "PATH")]
    pub iq: Option<String>,
    /// IQ file format (default cf32, or the SigMF recording's).
    #[arg(long, value_enum)]
    pub iq_format: Option<IqFormat>,
    /// Receive with a radio: BACKEND[:key=value,...], the backend name and
    /// its device arguments (needs --channel/--freq).  An unknown backend
    /// lists the compiled-in ones.
    #[arg(long, value_name = "SPEC")]
    pub radio: Option<DeviceSpec>,
    #[command(flatten)]
    pub tune: TuneArgs,
    /// Radio gain NAME=VALUE (repeatable).  Names, ranges and defaults are
    /// the backend's: an unknown name lists them, the settings printed at
    /// start show the values used.
    #[arg(
        long,
        value_name = "NAME=VALUE",
        value_parser = parse_gain,
        requires = "radio",
        conflicts_with = "iq"
    )]
    pub gain: Vec<(String, String)>,
    /// Duration bound in seconds of signal.  Default 0 = unbounded: the
    /// whole file for --iq, until Ctrl-C / SIGTERM for --radio.
    #[arg(long, value_name = "S")]
    pub seconds: Option<f64>,
    /// Write the reproduced single TS (§3.2.2, valid frames only). ISDB-S:
    /// one TS per TS_ID with `{ts_id}` in the path; ISDB-S3: one TLV
    /// stream per stream id with `{tlv_id}`.
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
    /// Transmission system: `auto` picks ISDB-S for a BSn / NDn channel
    /// from a radio, and for a recording not at the ISDB-T rate whichever
    /// of ISDB-S / ISDB-S3 the survey finds; ISDB-T otherwise. ISDB-S3
    /// from a radio needs `--system isdbs3`.
    #[arg(long, value_enum, default_value_t = SystemArg::Auto)]
    pub system: SystemArg,
    /// ISDB-S / ISDB-S3: receive only this TS_ID or TLV stream id
    /// (repeatable; `0x4010`, `4010h` or decimal). Default: every stream
    /// of the transponder.
    #[arg(long = "ts-id", value_name = "ID", value_parser = crate::rxs::parse_ts_id)]
    pub ts_id: Vec<u16>,
    /// ISDB-S / ISDB-S3 from a radio: sample rate (default 40M; ≥ 1.35 ×
    /// 28.86 Mbaud for ISDB-S, ≥ 1.03 × 33.756 Mbaud for ISDB-S3).
    #[arg(long, value_name = "SPS", value_parser = crate::rxs::parse_rate)]
    pub rate: Option<f64>,
    /// ISDB-S / ISDB-S3 from a radio: analog bandwidth (default 56M).
    #[arg(long, value_name = "HZ", value_parser = crate::rxs::parse_rate)]
    pub bw: Option<f64>,
    /// ISDB-S / ISDB-S3 from a radio: seconds per contiguous capture
    /// (default: the longest the radio takes in one piece). Blocks are
    /// decoded one after another, with a gap in the stream between them.
    #[arg(long, value_name = "S")]
    pub block_seconds: Option<f64>,
    /// ISDB-S / ISDB-S3: carrier offset in Hz (default: from the survey of each
    /// block).
    #[arg(long, value_name = "HZ", allow_negative_numbers = true)]
    pub carrier_offset: Option<f64>,
    /// ISDB-S3: the LDPC decoder. `fixed` is the fixed-point SIMD decoder
    /// (16 codewords per lane group); `scalar` the same arithmetic one
    /// codeword at a time (its bit-exact reference, slower); `float` the
    /// floating-point reference decoder.
    #[arg(long, value_enum, default_value_t = LdpcArg::Fixed)]
    pub ldpc: LdpcArg,
    /// ISDB-S / ISDB-S3 from a radio: where the single-carrier front end
    /// runs. `auto` uses the radio's FPGA front end when it offers a
    /// compatible one and then receives continuously (the radio sends
    /// symbols, not IQ); otherwise, and with `cpu`, contiguous IQ captures
    /// one after another (with gaps between them). `fpga` fails without
    /// one.
    #[arg(long, value_enum, default_value_t = crate::symbols::OffloadArg::Auto)]
    pub offload: crate::symbols::OffloadArg,
    /// ISDB-S / ISDB-S3: decode a `Symbols` words file (`isdb symbols
    /// --out`, an FPGA or CPU front end's output) as one continuous stream;
    /// needs `--system isdbs` or `isdbs3`.
    #[arg(long, value_name = "PATH")]
    pub symbols: Option<String>,
}

/// `isdb rx --ldpc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LdpcArg {
    /// Fixed-point layered offset min-sum, SIMD.
    Fixed,
    /// The same, one codeword at a time.
    Scalar,
    /// Floating-point layered normalised min-sum.
    Float,
}

impl LdpcArg {
    /// The receiver setting.
    pub fn ldpc(self) -> isdbs3_phy::main_signal::Ldpc {
        use isdbs3_phy::main_signal::Ldpc;
        match self {
            Self::Fixed => Ldpc::FIXED,
            Self::Scalar => Ldpc::SCALAR,
            Self::Float => Ldpc::FLOAT,
        }
    }
}

/// `isdb rx --system`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SystemArg {
    /// From the channel or the recording.
    Auto,
    /// ISDB-T (ARIB STD-B31), terrestrial.
    Isdbt,
    /// ISDB-S (ITU-R BO.1408-1, ARIB STD-B20), BS and 110° CS.
    Isdbs,
    /// ISDB-S3 (ARIB STD-B44), advanced BS / wide-band CS, to TLV streams.
    Isdbs3,
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
            self.mer_sum += f64::from(m);
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

    /// The statistics of the interval since the previous call (and start
    /// the next interval).
    pub fn take_interval(&mut self) -> RxStats {
        self.interval.take().map(|b| *b).unwrap_or_default()
    }

    /// Status line over the interval `iv` ([`RxStats::take_interval`]).
    pub fn status_line(&self, t: f64, state: RxState, iv: &RxStats) -> String {
        let lock = match state {
            RxState::Locked => "LOCKED",
            RxState::Acquiring => "acquiring",
            RxState::Searching => "searching",
        };
        let mut s = format!("{t:7.1} s {lock:9}");
        let Some(p) = &self.params else {
            return s + " (no frame yet)";
        };
        s += &format!(
            " M{} GI1/{} | frames {}/{} valid",
            p.mode().number(),
            p.guard().denominator(),
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
                    "segments": l.segments.get(),
                    "modulation": format!("{:?}", l.modulation),
                    "code_rate": format!("{n}/{d}"),
                    "time_interleave_index": l.time_interleave.code(),
                    "tsp_per_frame": l.tsp_per_frame(p.mode()),
                }),
            )
        })
        .collect();
    json!({
        "mode": p.mode().number(),
        "guard_interval": format!("1/{}", p.guard().denominator()),
        "partial_reception": p.partial_reception(),
        "layers": layers,
        "description": describe_params(p),
    })
}

/// The ISDB-T sample rate, 512/63 MHz.
const ISDBT_RATE_HZ: f64 = 512e6 / 63.0;

/// The data file and format of `--iq PATH`: a SigMF recording's metadata
/// must say 512/63 MHz and cs8 or cf32 (the formats `rx` reads); without
/// metadata the path and `--iq-format` are used as given.
pub fn sigmf_input(path: &str, format: Option<IqFormat>) -> Result<(String, IqFormat)> {
    if path == "-" {
        return Ok((path.into(), format.unwrap_or(IqFormat::Cf32)));
    }
    let data = isdb_iq::data_path_for(std::path::Path::new(path));
    let meta_path = isdb_iq::meta_path_for(&data);
    if !meta_path.exists() {
        return Ok((path.into(), format.unwrap_or(IqFormat::Cf32)));
    }
    let text = std::fs::read_to_string(&meta_path)
        .with_context(|| format!("read {}", meta_path.display()))?;
    let m = isdb_iq::sigmf::Meta::from_json(&text)
        .with_context(|| format!("{}", meta_path.display()))?;
    if (m.sample_rate / ISDBT_RATE_HZ - 1.0).abs() > 10e-6 {
        bail!(
            "{}: recorded at {:.6} Msps, not the ISDB-T rate 512/63 MHz ({:.6} Msps); for ISDB-S use --system isdbs (or auto). See `isdb iq survey` (any system) and `isdb iq tmcc` (ISDB-S)",
            meta_path.display(),
            m.sample_rate / 1e6,
            ISDBT_RATE_HZ / 1e6
        );
    }
    let recorded = match m.format {
        isdb_iq::SampleFormat::Cs8 => IqFormat::Cs8,
        isdb_iq::SampleFormat::Cf32 => IqFormat::Cf32,
        f => bail!(
            "{}: sample format {f}; `isdb rx --iq` reads cf32 and cs8",
            meta_path.display()
        ),
    };
    if let Some(f) = format
        && f != recorded
    {
        bail!(
            "--iq-format {f:?} disagrees with {} ({recorded:?})",
            meta_path.display()
        );
    }
    Ok((data.display().to_string(), recorded))
}

/// After no ISDB-T frame was found in file `path`: survey its start and say
/// what single-carrier signal, if any, it holds (the rate is 512/63 MHz, so
/// only a narrow-band signal can show here; satellite recordings are
/// refused earlier by their metadata).
fn explain_no_signal(path: &str) {
    let Ok(mut rec) =
        isdb_iq::Recording::open(std::path::Path::new(path), None, Some(ISDBT_RATE_HZ)).or_else(
            |_| {
                isdb_iq::Recording::open(
                    std::path::Path::new(path),
                    Some(isdb_iq::SampleFormat::Cf32),
                    Some(ISDBT_RATE_HZ),
                )
            },
        )
    else {
        return;
    };
    let n = rec.samples.min(1 << 22) as usize;
    let Ok(x) = rec.read(0, n) else { return };
    if x.len() < 1 << 16 {
        return;
    }
    let s = isdb_iq::survey::survey(&x, rec.sample_rate, &isdb_iq::survey::Options::default());
    match s.system {
        Some(m) => warn!(
            target: "isdb::rx",
            system = m.system.name,
            symbol_rate_hz = m.system.symbol_rate_hz,
            "the survey finds {} ({:.0} Bd) in this file: not ISDB-T (see `isdb iq survey`)",
            m.system.name, m.system.symbol_rate_hz
        ),
        None => warn!(
            target: "isdb::rx",
            power_dbfs = s.power_dbfs,
            "the survey finds no known single-carrier system either (power {:.1} dBFS)",
            s.power_dbfs
        ),
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
    fn read(&mut self, _stop: &AtomicBool) -> isdb_radio::Result<Option<Vec<C32>>> {
        let n = loop {
            match self.r.read(&mut self.buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(isdb_radio::RadioError::io("read the IQ input", e)),
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
    let mut prx =
        PipelinedReceiver::new(DemodConfig::default(), 4).context("start the receiver threads")?;
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
            // a limit beyond usize::MAX keeps the whole chunk
            chunk.truncate(usize::try_from(m.saturating_sub(samples)).unwrap_or(usize::MAX));
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
            let state = prx.state();
            let iv = stats.take_interval();
            info!(
                target: "isdb::rx",
                seconds = t,
                state = ?state,
                frames = iv.frames,
                valid = iv.valid,
                tmcc_failed = iv.tmcc_failed,
                cfo = iv.cfo.unwrap_or(f64::NAN),
                "{}{}",
                stats.status_line(t, state, &iv),
                src.status()
            );
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
    match resolve_system(&a)? {
        SystemArg::Isdbs => return run_isdbs(a),
        SystemArg::Isdbs3 => return run_isdbs3(a),
        _ => {}
    }
    if !a.ts_id.is_empty()
        || a.rate.is_some()
        || a.bw.is_some()
        || a.block_seconds.is_some()
        || a.carrier_offset.is_some()
    {
        bail!(
            "--ts-id, --rate, --bw, --block-seconds and --carrier-offset are satellite options (--system isdbs or isdbs3)"
        );
    }
    if a.ldpc != LdpcArg::Fixed {
        bail!("--ldpc is an ISDB-S3 option (--system isdbs3)");
    }
    if a.json
        && [&a.out, &a.a_out, &a.b_out, &a.c_out]
            .iter()
            .any(|o| o.as_deref() == Some("-"))
    {
        bail!("--json prints on stdout; write the TS outputs to files");
    }
    let fs = sample_rate_hz();
    let seconds = crate::tx::resolve_seconds(a.seconds)?;
    let max_samples = (seconds > 0.0).then(|| crate::phyargs::sample_count(seconds, fs));
    let stop = crate::stop::flag();
    let (mut src, input_desc): (Box<dyn IqSource>, String) = if let Some(spec) = &a.radio {
        let backend = isdb_radio::backend(&spec.backend)?;
        let (cfg, tuning) = a.tune.radio_config(spec, &a.gain)?;
        let settings = backend.describe_rx(&cfg)?;
        info!(
            target: "isdb::rx",
            backend = backend.name(),
            "{} RX settings{}:\n{settings}",
            backend.name(),
            tuning.channel_label()
        );
        let opts = RxOptions {
            queue_seconds: RX_QUEUE_SECONDS,
        };
        let (src, info) = backend
            .open_rx(&cfg, &opts)
            .with_context(|| format!("open radio {spec} for RX"))?;
        info!(target: "isdb::rx", "opened: {info}");
        (
            src,
            format!(
                "radio {spec}{}, {}",
                tuning.channel_label(),
                cfg.describe_tuning()
            ),
        )
    } else {
        let p = a.iq.as_deref().expect("clap group");
        let (path, format) = sigmf_input(p, a.iq_format)?;
        (
            Box::new(FileIqSource::open(&path, format)?),
            format!("{path} ({format:?})"),
        )
    };
    info!(
        target: "isdb::rx",
        input = input_desc,
        seconds = seconds,
        "{input_desc}; {}",
        match max_samples {
            Some(_) => format!("for {seconds} s"),
            None if a.radio.is_some() => "unbounded; stop with Ctrl-C".into(),
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
    info!(
        target: "isdb::rx",
        seconds = secs,
        samples,
        frames = stats.frames,
        valid = stats.valid,
        tmcc_failed = stats.tmcc_failed,
        signal = stats.params.is_some(),
        "done: {:.2} s of signal, {} frames ({} valid){}",
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
    if stats.params.is_none()
        && let Some(p) = a.iq.as_deref().filter(|&p| p != "-")
    {
        explain_no_signal(p);
    }
    if let Some(p) = &stats.params {
        for (id, _) in p.active_layers() {
            let l = &stats.layers[id.index()];
            info!(
                target: "isdb::rx",
                layer = %id,
                packets = l.packets,
                clean = l.clean,
                corrected_packets = l.corrected_packets,
                corrected_bytes = l.corrected_bytes,
                uncorrectable = l.uncorrectable,
                mer_db = l.mer_mean().unwrap_or(f64::NAN),
                "  layer {id}: {} packets; RS clean {} corrected {} ({} bytes) uncorrectable {}; MER mean {}",
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
        info!(target: "isdb::rx", "{fin}");
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

/// The system `--system` selects (`auto` resolved).
fn resolve_system(a: &RxArgs) -> Result<SystemArg> {
    if a.system != SystemArg::Auto {
        if a.symbols.is_some() && !matches!(a.system, SystemArg::Isdbs | SystemArg::Isdbs3) {
            bail!("--symbols is an ISDB-S / ISDB-S3 input (--system isdbs or isdbs3)");
        }
        return Ok(a.system);
    }
    if let Some(p) = &a.symbols {
        // the words file's sidecar (`isdb symbols` writes `system`)
        let side = format!("{p}.json");
        let sys = std::fs::read_to_string(&side)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|j| j["system"].as_str().map(str::to_owned));
        return match sys.as_deref() {
            Some(n) if n == isdbs3_phy::frontend::system().name => Ok(SystemArg::Isdbs3),
            Some(n) if n == isdbs_phy::frontend::system().name => Ok(SystemArg::Isdbs),
            _ => bail!(
                "--symbols {p}: no sidecar {side} naming the system; give --system isdbs or isdbs3"
            ),
        };
    }
    if a.tune.channel.is_some_and(|c| c.is_satellite()) {
        return Ok(SystemArg::Isdbs);
    }
    let Some(p) = a.iq.as_deref().filter(|&p| p != "-") else {
        return Ok(SystemArg::Isdbt);
    };
    let meta = isdb_iq::meta_path_for(&isdb_iq::data_path_for(std::path::Path::new(p)));
    if !meta.exists() {
        return Ok(SystemArg::Isdbt);
    }
    let Ok(mut rec) = isdb_iq::Recording::open(std::path::Path::new(p), None, None) else {
        return Ok(SystemArg::Isdbt);
    };
    if (rec.sample_rate / ISDBT_RATE_HZ - 1.0).abs() <= 10e-6 {
        return Ok(SystemArg::Isdbt);
    }
    // Not the ISDB-T rate: ISDB-S if its survey says so.
    let n = rec.samples.min(1 << 22) as usize;
    let x = rec.read(0, n)?;
    let s = isdb_iq::survey::survey(&x, rec.sample_rate, &isdb_iq::survey::Options::default());
    match s.system {
        Some(m) if m.system.name == isdbs_phy::frontend::system().name => Ok(SystemArg::Isdbs),
        Some(m) if m.system.name == isdbs3_phy::frontend::system().name => Ok(SystemArg::Isdbs3),
        Some(m) => bail!(
            "{p}: the survey finds {} ({:.0} Bd), which `isdb rx` does not receive",
            m.system.name,
            m.system.symbol_rate_hz
        ),
        None => bail!(
            "{p}: recorded at {:.6} Msps, not the ISDB-T rate, and the survey finds neither ISDB-S nor ISDB-S3; give --system",
            rec.sample_rate / 1e6
        ),
    }
}

/// `isdb rx` for ISDB-S.
fn run_isdbs(a: RxArgs) -> Result<()> {
    if a.a_out.is_some() || a.b_out.is_some() || a.c_out.is_some() {
        bail!(
            "--a-out/--b-out/--c-out are ISDB-T layers; ISDB-S writes one TS per TS_ID (--out PATH with {{ts_id}}, or --ts-id)"
        );
    }
    if a.json && a.out.as_deref() == Some("-") {
        bail!("--json prints on stdout; write the TS to a file");
    }
    let seconds = crate::tx::resolve_seconds(a.seconds)?;
    let stop = crate::stop::flag();
    let mut out = crate::rxs::Outputs::new(a.out.as_deref(), &a.ts_id, a.strip_nulls)?;
    let mut stats = crate::rxs::Stats::default();
    let (secs, input) = if let Some(p) = &a.symbols {
        info!(target: "isdb::rx", input = p, "ISDB-S from the Symbols words of {p}");
        let rs = isdbs_phy::frontend::system().symbol_rate_hz;
        stats.symbol_rate_hz = rs;
        let s = crate::rxs::run_stream(
            crate::symstream::file_bytes(std::path::Path::new(p))?,
            crate::symstream::Source::File,
            rs,
            a.status_secs > 0.0,
            &stop,
            &mut out,
            &mut stats,
        )?;
        (s, p.clone())
    } else if let Some(spec) = &a.radio {
        let (hz, channel) = match (a.tune.channel, a.tune.freq) {
            (Some(c), None) => (c.tune_hz()?, Some(c)),
            (None, Some(f)) => (f, None),
            (None, None) => bail!("give --channel BS<n>|ND<n> or --freq HZ (the satellite IF)"),
            (Some(_), Some(_)) => bail!("--channel and --freq are exclusive"),
        };
        if a.tune.freq_corr_ppm != 0.0 {
            bail!(
                "--freq-corr-ppm is not applied to ISDB-S reception (the receiver tracks the carrier and symbol clock)"
            );
        }
        let rate = a.rate.unwrap_or(crate::rxs::DEFAULT_RATE);
        let bw = a.bw.unwrap_or(crate::rxs::DEFAULT_BW);
        info!(
            target: "isdb::rx",
            radio = %spec,
            center_hz = hz,
            sample_rate_hz = rate,
            seconds,
            "ISDB-S from radio {spec}{} at {:.3} MHz, {:.2} Msps; {}",
            channel.map(|c| c.label()).unwrap_or_default(),
            hz as f64 / 1e6,
            rate / 1e6,
            if seconds > 0.0 {
                format!("for {seconds} s of signal")
            } else {
                "until Ctrl-C".into()
            }
        );
        let backend = isdb_radio::backend(&spec.backend)?;
        let cfg = isdb_radio::RadioConfig {
            center_hz: hz,
            nominal_hz: hz,
            rate_num: crate::phyargs::whole_u32(rate, "--rate")?,
            rate_den: 1,
            bandwidth_hz: crate::phyargs::whole_u32(bw, "--bw")?,
            args: spec.args.clone(),
            gains: a.gain.clone(),
        };
        let s = if let Some(taps) = crate::symstream::fpga_taps(backend, &cfg, a.offload)? {
            if a.block_seconds.is_some() {
                bail!(
                    "--block-seconds: the FPGA front end streams continuously (--offload cpu for blocks)"
                );
            }
            info!(target: "isdb::rx", taps, "the radio's FPGA front end ({taps} taps): continuous reception");
            crate::rxs::run_radio_fpga(
                backend,
                &cfg,
                taps,
                seconds,
                a.carrier_offset,
                a.status_secs > 0.0,
                &stop,
                &mut out,
                &mut stats,
            )?
        } else {
            crate::rxs::run_radio(
                spec,
                hz,
                channel,
                rate,
                bw,
                &a.gain,
                a.block_seconds.unwrap_or(f64::INFINITY),
                seconds,
                a.carrier_offset,
                a.status_secs > 0.0,
                &stop,
                &mut out,
                &mut stats,
            )?
        };
        (s, format!("radio {spec}"))
    } else {
        let p = a.iq.as_deref().expect("clap group");
        if p == "-" {
            bail!(
                "ISDB-S reception reads a SigMF recording (rate and format from its metadata), not stdin"
            );
        }
        if a.iq_format.is_some() {
            bail!("--iq-format: an ISDB-S recording's format comes from its SigMF metadata");
        }
        let max = (seconds > 0.0).then(|| {
            let fs = isdb_iq::Recording::open(std::path::Path::new(p), None, None)
                .map(|r| r.sample_rate)
                .unwrap_or(0.0);
            crate::phyargs::sample_count(seconds, fs)
        });
        info!(target: "isdb::rx", input = p, "ISDB-S from {p}");
        let s = crate::rxs::run_file(p, max, a.carrier_offset, &mut out, &mut stats)?;
        (s, p.to_string())
    };
    out.flush()?;
    crate::rxs::report(&stats, secs);
    if a.json {
        let mut j = stats.json();
        j["input"] = json!(input);
        j["seconds"] = json!(secs);
        j["written_packets"] = json!(
            out.written
                .iter()
                .map(|(id, n)| (format!("{id:#06x}"), json!(n)))
                .collect::<serde_json::Map<_, _>>()
        );
        println!("{}", serde_json::to_string_pretty(&j)?);
    }
    Ok(())
}

/// `isdb rx` for ISDB-S3.
fn run_isdbs3(a: RxArgs) -> Result<()> {
    if a.a_out.is_some() || a.b_out.is_some() || a.c_out.is_some() {
        bail!(
            "--a-out/--b-out/--c-out are ISDB-T layers; ISDB-S3 writes one TLV stream per stream id (--out PATH with {{tlv_id}}, or --ts-id ID)"
        );
    }
    if a.strip_nulls {
        bail!("--strip-nulls is for TS output; ISDB-S3 writes TLV streams");
    }
    if a.json && a.out.as_deref() == Some("-") {
        bail!("--json prints on stdout; write the TLV stream to a file");
    }
    let seconds = crate::tx::resolve_seconds(a.seconds)?;
    let stop = crate::stop::flag();
    let mut out = crate::rxs3::Outputs::new(a.out.as_deref(), &a.ts_id)?;
    let mut stats = crate::rxs3::Stats::default();
    stats.ldpc = a.ldpc.ldpc();
    let (secs, input) = if let Some(p) = &a.symbols {
        info!(target: "isdb::rx", input = p, "ISDB-S3 from the Symbols words of {p}");
        let rs = isdbs3_phy::frontend::system().symbol_rate_hz;
        let s = crate::rxs3::run_stream(
            crate::symstream::file_bytes(std::path::Path::new(p))?,
            crate::symstream::Source::File,
            rs,
            a.status_secs > 0.0,
            &stop,
            &mut out,
            &mut stats,
        )?;
        (s, p.clone())
    } else if let Some(spec) = &a.radio {
        let (hz, channel) = match (a.tune.channel, a.tune.freq) {
            (Some(c), None) => (c.tune_hz()?, Some(c)),
            (None, Some(f)) => (f, None),
            (None, None) => bail!("give --channel BS<n>|ND<n> or --freq HZ (the satellite IF)"),
            (Some(_), Some(_)) => bail!("--channel and --freq are exclusive"),
        };
        if a.tune.freq_corr_ppm != 0.0 {
            bail!(
                "--freq-corr-ppm is not applied to ISDB-S3 reception (the receiver tracks the carrier and symbol clock)"
            );
        }
        let rate = a.rate.unwrap_or(crate::rxs3::DEFAULT_RATE);
        let bw = a.bw.unwrap_or(crate::rxs3::DEFAULT_BW);
        info!(
            target: "isdb::rx",
            radio = %spec,
            center_hz = hz,
            sample_rate_hz = rate,
            seconds,
            "ISDB-S3 from radio {spec}{} at {:.3} MHz, {:.2} Msps; {}",
            channel.map(|c| c.label()).unwrap_or_default(),
            hz as f64 / 1e6,
            rate / 1e6,
            if seconds > 0.0 {
                format!("for {seconds} s of signal")
            } else {
                "until Ctrl-C".into()
            }
        );
        let backend = isdb_radio::backend(&spec.backend)?;
        let cfg = isdb_radio::RadioConfig {
            center_hz: hz,
            nominal_hz: hz,
            rate_num: crate::phyargs::whole_u32(rate, "--rate")?,
            rate_den: 1,
            bandwidth_hz: crate::phyargs::whole_u32(bw, "--bw")?,
            args: spec.args.clone(),
            gains: a.gain.clone(),
        };
        let s = if let Some(taps) = crate::symstream::fpga_taps(backend, &cfg, a.offload)? {
            if a.block_seconds.is_some() {
                bail!(
                    "--block-seconds: the FPGA front end streams continuously (--offload cpu for blocks)"
                );
            }
            info!(target: "isdb::rx", taps, "the radio's FPGA front end ({taps} taps): continuous reception");
            crate::rxs3::run_radio_fpga(
                backend,
                &cfg,
                taps,
                seconds,
                a.carrier_offset,
                a.status_secs > 0.0,
                &stop,
                &mut out,
                &mut stats,
            )?
        } else {
            crate::rxs3::run_radio(
                spec,
                hz,
                channel,
                rate,
                bw,
                &a.gain,
                a.block_seconds.unwrap_or(f64::INFINITY),
                seconds,
                a.carrier_offset,
                a.status_secs > 0.0,
                &stop,
                &mut out,
                &mut stats,
            )?
        };
        (s, format!("radio {spec}"))
    } else {
        let p = a.iq.as_deref().expect("clap group");
        if p == "-" {
            bail!(
                "ISDB-S3 reception reads a SigMF recording (rate and format from its metadata), not stdin"
            );
        }
        if a.iq_format.is_some() {
            bail!("--iq-format: an ISDB-S3 recording's format comes from its SigMF metadata");
        }
        let max = (seconds > 0.0).then(|| {
            let fs = isdb_iq::Recording::open(std::path::Path::new(p), None, None)
                .map(|r| r.sample_rate)
                .unwrap_or(0.0);
            crate::phyargs::sample_count(seconds, fs)
        });
        info!(target: "isdb::rx", input = p, "ISDB-S3 from {p}");
        let s = crate::rxs3::run_file(p, max, a.carrier_offset, &mut out, &mut stats)?;
        (s, p.to_string())
    };
    out.flush()?;
    crate::rxs3::report(&stats, secs);
    if a.json {
        let mut j = stats.json();
        j["input"] = json!(input);
        j["seconds"] = json!(secs);
        j["written_packets"] = json!(
            out.written
                .iter()
                .map(|(id, n)| (format!("{id:#06x}"), json!(n)))
                .collect::<serde_json::Map<_, _>>()
        );
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
        assert_eq!(a.iq_format, Some(IqFormat::Cs8));
        assert_eq!((a.radio.as_ref(), a.gain.len()), (None, 0));
        assert!(T::try_parse_from(["rx"]).is_err());
        assert!(T::try_parse_from(["rx", "--iq", "x", "--radio", "sdr"]).is_err());
        // Gains reach the backend unparsed (it validates names and steps).
        let h = T::try_parse_from([
            "rx",
            "--radio",
            "sdr:serial=1",
            "--channel",
            "27",
            "--gain",
            "lna=12",
        ])
        .unwrap()
        .a;
        assert_eq!(h.radio.as_ref().unwrap().to_string(), "sdr:serial=1");
        assert_eq!(h.gain, vec![("lna".into(), "12".into())]);
        assert_eq!(h.tune.tuning().unwrap().nominal_hz, 557_142_857);
        // Default: no bound (whole file / until Ctrl-C), same as tx.
        for s in [a.seconds, h.seconds] {
            assert_eq!(crate::tx::resolve_seconds(s).unwrap(), 0.0);
        }
        // Gains need a radio and the NAME=VALUE form.
        assert!(T::try_parse_from(["rx", "--iq", "x", "--gain", "lna=16"]).is_err());
        assert!(T::try_parse_from(["rx", "--radio", "sdr", "--gain", "16"]).is_err());
    }

    #[test]
    fn system_selection() {
        let p = |args: &[&str]| T::try_parse_from(args).unwrap().a;
        // a satellite channel selects ISDB-S, a terrestrial one ISDB-T
        let a = p(&["rx", "--radio", "pluto", "--channel", "BS3"]);
        assert_eq!(resolve_system(&a).unwrap(), SystemArg::Isdbs);
        let a = p(&["rx", "--radio", "pluto", "--channel", "T27"]);
        assert_eq!(resolve_system(&a).unwrap(), SystemArg::Isdbt);
        // a file without SigMF metadata, or stdin: ISDB-T
        let a = p(&["rx", "--iq", "no-such-file.cf32"]);
        assert_eq!(resolve_system(&a).unwrap(), SystemArg::Isdbt);
        // an explicit system wins
        let a = p(&["rx", "--iq", "-", "--system", "isdbs"]);
        assert_eq!(resolve_system(&a).unwrap(), SystemArg::Isdbs);
        let a = p(&["rx", "--iq", "-", "--system", "isdbs3"]);
        assert_eq!(resolve_system(&a).unwrap(), SystemArg::Isdbs3);
        // satellite options on ISDB-T are refused
        let a = p(&[
            "rx", "--iq", "x.cf32", "--system", "isdbt", "--ts-id", "0x4010",
        ]);
        assert!(
            run(a)
                .unwrap_err()
                .to_string()
                .contains("satellite options")
        );
        // ISDB-T layer outputs on ISDB-S / ISDB-S3 are refused
        let a = p(&["rx", "--iq", "x", "--system", "isdbs", "--b-out", "b.ts"]);
        assert!(run(a).unwrap_err().to_string().contains("ISDB-T layers"));
        let a = p(&["rx", "--iq", "x", "--system", "isdbs3", "--b-out", "b.ts"]);
        assert!(run(a).unwrap_err().to_string().contains("ISDB-T layers"));
        // ISDB-S3 reads a SigMF recording, not stdin
        let a = p(&["rx", "--iq", "-", "--system", "isdbs3"]);
        assert!(run(a).unwrap_err().to_string().contains("not stdin"));
    }

    #[test]
    fn file_source_handles_partial_samples() {
        let dir = std::env::temp_dir().join(format!("isdb-rx-{}", std::process::id()));
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

    #[test]
    fn sigmf_recordings_must_be_isdbt_rate() {
        let dir = std::env::temp_dir().join(format!("isdb-rx-sigmf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = |name: &str, dt: &str, rate: f64| {
            std::fs::write(dir.join(format!("{name}.sigmf-data")), [0u8; 64]).unwrap();
            std::fs::write(
                dir.join(format!("{name}.sigmf-meta")),
                format!(r#"{{"global":{{"core:datatype":"{dt}","core:sample_rate":{rate}}},"captures":[{{"core:sample_start":0}}]}}"#),
            )
            .unwrap();
            dir.join(name).display().to_string()
        };
        // A satellite recording is refused with a pointer to the tools.
        let bs = meta("bs", "ci8", 40e6);
        let e = sigmf_input(&bs, None).unwrap_err().to_string();
        assert!(
            e.contains("40.000000 Msps") && e.contains("isdb iq tmcc"),
            "{e}"
        );
        // An ISDB-T-rate cs8 recording gives its data file and format.
        let t = meta("t", "ci8", 512e6 / 63.0);
        let (p, f) = sigmf_input(&format!("{t}.sigmf-meta"), None).unwrap();
        assert!(p.ends_with("t.sigmf-data") && f == IqFormat::Cs8, "{p}");
        assert!(sigmf_input(&t, Some(IqFormat::Cf32)).is_err());
        // cs16 is not read by rx.
        let w = meta("w", "ci16_le", 512e6 / 63.0);
        assert!(
            sigmf_input(&w, None)
                .unwrap_err()
                .to_string()
                .contains("reads cf32 and cs8")
        );
        // Without metadata: as given, cf32 by default.
        assert_eq!(
            sigmf_input("plain.cf32", None).unwrap(),
            ("plain.cf32".into(), IqFormat::Cf32)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
