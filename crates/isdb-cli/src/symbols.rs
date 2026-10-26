//! `isdb symbols` and `isdb iq symbols`: the single-carrier front end of
//! `isdb_frontend` (IQ to `Symbols` words), on the radio's FPGA when it
//! offers it ([`isdb_radio::Offload::Symbols`]) and on the CPU otherwise,
//! with the same output bit for bit.
//!
//! Output: the `Symbols` words (`isdb_frontend::format`, 16-bit
//! little-endian) in `OUT`, and `OUT.json` with the parameters, the
//! system, the rates and where the symbols were computed.

use crate::bands::{Freq, parse_count, parse_hz};
use crate::phyargs::parse_gain;
use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::format::{Event, Packer, Reader};
use isdb_frontend::metrics::{psk_mer, ring_stats};
use isdb_frontend::params::{Design, ONE, Params};
use isdb_frontend::vectors;
use isdb_iq::survey::{KNOWN_SYSTEMS, KnownSystem, Options, survey};
use isdb_iq::{C32, Recording};
use isdb_radio::{CaptureFormat, DeviceSpec, Offload, RadioConfig};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Which system's symbols to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SystemArg {
    /// The system whose symbol rate the survey finds.
    Auto,
    /// ISDB-S (28.86 Mbaud, roll-off 0.35).
    IsdbS,
    /// ISDB-S3 (33.7561 Mbaud, roll-off 0.03).
    IsdbS3,
}

/// Where the front end runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OffloadArg {
    /// The radio's FPGA when it offers a compatible front end, else the
    /// CPU on one contiguous IQ capture.
    Auto,
    /// The radio's FPGA (an error when it has none).
    Fpga,
    /// The CPU, on one contiguous IQ capture.
    Cpu,
}

/// Front-end settings shared by both commands.
#[derive(Debug, Clone, Args)]
pub struct FrontendOpts {
    /// System (`auto`: from the survey of the signal).
    #[arg(long, value_enum, default_value_t = SystemArg::Auto)]
    pub system: SystemArg,
    /// Taps per phase of the matched filter (a multiple of 4 for the
    /// FPGA; the device's own count when it runs there).
    #[arg(long, default_value_t = 32)]
    pub taps: usize,
    /// Bits per symbol component; default 6 (both systems).
    #[arg(long)]
    pub bits: Option<u8>,
    /// Output instant relative to where the timing loop locks, in symbol
    /// periods, or `auto`: measured on the IQ before the run from known
    /// symbols (ISDB-S3 sync words, ISDB-S bursts). The loop's detector
    /// locks 0.15–0.18 symbol off the best instant on the lab's ISDB-S3
    /// signals (5–6 dB of SNR) and 0.05–0.10 on ISDB-S.
    #[arg(long, value_name = "auto|T", default_value = "auto", value_parser = parse_trim)]
    pub trim: TrimArg,
}

/// `--trim`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrimArg {
    /// Measured on the signal.
    Auto,
    /// Symbol periods.
    Fixed(f64),
}

pub(crate) fn parse_trim(s: &str) -> Result<TrimArg> {
    if s.eq_ignore_ascii_case("auto") {
        return Ok(TrimArg::Auto);
    }
    let t: f64 = s
        .parse()
        .with_context(|| format!("--trim {s:?}: auto or a number"))?;
    if !(-0.5..=0.5).contains(&t) {
        bail!("--trim {t}: within ±0.5 symbol");
    }
    Ok(TrimArg::Fixed(t))
}

/// `isdb symbols` arguments.
#[derive(Debug, Args)]
pub struct SymbolsArgs {
    /// The radio: BACKEND[:key=value,...] (e.g. `pluto:uri=ip:192.0.2.30`).
    #[arg(long, value_name = "SPEC")]
    pub radio: DeviceSpec,
    /// Centre frequency: Hz or a channel name (`BS1`, `ND2` at the
    /// satellite IF, `T27`).
    #[arg(long, value_name = "HZ|CHANNEL")]
    pub freq: Freq,
    /// Sample rate (at least (1 + roll-off) × the symbol rate: 40M covers
    /// ISDB-S and ISDB-S3).
    #[arg(long, value_name = "SPS", value_parser = parse_hz, default_value = "40M")]
    pub rate: f64,
    /// Analog bandwidth; the default 56M keeps the satellite signals'
    /// band edges (a narrower filter costs MER).
    #[arg(long, value_name = "HZ", value_parser = parse_hz, default_value = "56M")]
    pub bw: f64,
    /// Radio gain NAME=VALUE (repeatable).
    #[arg(long, value_name = "NAME=VALUE", value_parser = parse_gain)]
    pub gain: Vec<(String, String)>,
    #[command(flatten)]
    pub fe: FrontendOpts,
    /// Seconds of signal (the CPU path takes at most one contiguous
    /// capture).
    #[arg(long, value_name = "S", default_value_t = 1.0)]
    pub seconds: f64,
    /// Where the front end runs.
    #[arg(long, value_enum, default_value_t = OffloadArg::Auto)]
    pub offload: OffloadArg,
    /// Only report the stages the radio can run itself, then exit.
    #[arg(long)]
    pub probe: bool,
    /// Output words file (`OUT.json` gets the parameters).
    #[arg(short, long, value_name = "PATH", required_unless_present = "probe")]
    pub out: Option<PathBuf>,
    /// Replace existing output files.
    #[arg(long)]
    pub overwrite: bool,
}

/// What the front end produced, for the report.
struct Report {
    symbols: u64,
    blocks: u64,
    period_mean: f64,
    z: Vec<[f64; 2]>,
}

/// The system for `arg` (or from a survey of `x`) and its carrier offset.
/// ISDB-S takes the survey's strongest carrier line (its z⁸ / z⁴ line at
/// baseband, corroborated by z² on every recording). ISDB-S3 has no such
/// line: its carrier comes from the ±R/2 lines of the π/2-BPSK symbols,
/// which alias at 40 Msps, and the strongest line was an alias on two of
/// seven live recordings (plan §0.16), so the receiver's choice is used —
/// every candidate through the front end, best sync-symbol Es/N0
/// ([`isdbs3_phy::frontend::pick_carrier`]).
pub(crate) fn pick_system(
    arg: SystemArg,
    x: &[C32],
    fs: f64,
) -> Result<(&'static KnownSystem, f64)> {
    let s = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let sys = match arg {
        SystemArg::IsdbS => &KNOWN_SYSTEMS[0],
        SystemArg::IsdbS3 => &KNOWN_SYSTEMS[1],
        SystemArg::Auto => {
            let m = s
                .system
                .context("the survey finds no known symbol rate; give --system")?;
            KNOWN_SYSTEMS
                .iter()
                .find(|k| k.name == m.system.name)
                .context("unknown system")?
        }
    };
    let cfo = if sys.name == isdbs3_phy::frontend::system().name {
        let xi = isdbs3_phy::frontend::to_input(x);
        isdbs3_phy::frontend::pick_carrier(&xi, fs, &isdbs3_phy::frontend::carrier_candidates(&s))?
    } else {
        s.best_carrier().map_or(0.0, |c| c.offset_hz)
    };
    Ok((sys, cfo))
}

/// The design for `sys` at `fs` with carrier offset `cfo`, before the
/// trim.
fn base_design(sys: &KnownSystem, fs: f64, cfo: f64, o: &FrontendOpts) -> Design {
    let mut d = Design::for_system(sys, fs);
    d.carrier_offset = cfo;
    d.taps = o.taps;
    if let Some(b) = o.bits {
        d.bits = b;
    }
    d
}

/// The parameters for `sys` at `fs` with carrier offset `cfo`, and the
/// trim they use; with `--trim auto` the trim is measured on `x`
/// (unit-scale IQ at `fs`, at most the first 0.15 s used), and reported.
fn design(
    sys: &KnownSystem,
    fs: f64,
    cfo: f64,
    o: &FrontendOpts,
    x: &[C32],
) -> Result<(Params, f64)> {
    let mut d = base_design(sys, fs, cfo, o);
    d.trim = match o.trim {
        TrimArg::Fixed(t) => t,
        TrimArg::Auto => {
            let n = ((0.15 * fs) as usize).min(x.len());
            let xi = to_input(&x[..n]);
            let (t, snr) = if sys.name == KNOWN_SYSTEMS[1].name {
                let cands: Vec<f64> = (-6..=1).map(|i| f64::from(i) * 0.05).collect();
                isdb_frontend::s3sync::best_trim(&xi, &d, &cands)?
            } else {
                isdbs_phy::frontend::measure_trim(&xi, &d)?
            };
            eprintln!(
                "[symbols] trim {t:+.3} symbol measured on {:.3} s ({} {snr:.2} dB there)",
                n as f64 / fs,
                if sys.name == KNOWN_SYSTEMS[1].name {
                    "sync-symbol Es/N0"
                } else {
                    "burst MER"
                }
            );
            t
        }
    };
    Ok((Params::design(&d)?, d.trim))
}

/// Samples in the front end's input domain (12-bit components in i16).
fn to_input(x: &[C32]) -> Vec<[i16; 2]> {
    x.iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect()
}

/// Run the CPU front end over `x`, writing the words to `out`.
fn cpu_front_end(p: &Params, x: &[C32], out: &mut dyn Write) -> Result<Report> {
    let mut fe = Frontend::new(p.clone())?;
    let mut pk = Packer::new(p);
    let mut words = Vec::new();
    let mut z = Vec::new();
    let (mut symbols, mut psum, mut pn) = (0u64, 0.0, 0u64);
    let settle = 40_000u64;
    let mut syms = Vec::new();
    for chunk in x.chunks(1 << 16) {
        words.clear();
        syms.clear();
        fe.process(&to_input(chunk), &mut syms);
        for sym in &syms {
            pk.push(sym.q, sym.gain, sym.period, &mut words);
            if symbols >= settle {
                psum += f64::from(sym.period) / ONE as f64;
                pn += 1;
                if z.len() < 1 << 21 {
                    z.push([f64::from(sym.zk[0]), f64::from(sym.zk[1])]);
                }
            }
            symbols += 1;
        }
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        out.write_all(&bytes).context("write the words")?;
    }
    Ok(Report {
        symbols,
        blocks: symbols >> p.block_log2,
        period_mean: if pn > 0 { psum / pn as f64 } else { 0.0 },
        z,
    })
}

fn print_report(r: &Report, sys: &KnownSystem, fs: f64) {
    let rate = if r.period_mean > 0.0 {
        fs / r.period_mean
    } else {
        0.0
    };
    println!(
        "{} symbols in {} whole blocks; symbol rate {rate:.1} Bd ({:+.2} ppm against {} {:.0} Bd)",
        r.symbols,
        r.blocks,
        (rate / sys.symbol_rate_hz - 1.0) * 1e6,
        sys.name,
        sys.symbol_rate_hz
    );
    if r.z.is_empty() {
        return;
    }
    if sys.roll_off >= 0.1 {
        println!(
            "ring MER {:.2} dB, 8PSK decision-directed MER {:.2} dB (before the output quantiser)",
            ring_stats(&r.z, 1).mer_db,
            psk_mer(&r.z, 8, 2e-3, 20_000).mer_db
        );
        return;
    }
    // ISDB-S3: 16APSK and friends have no single ring; the known sync
    // symbols give the SNR instead.
    let z: Vec<isdbs_phy::Cplx> =
        r.z.iter()
            .map(|v| isdbs_phy::Cplx::new(v[0], v[1]))
            .collect();
    match isdb_frontend::s3sync::measure(&z) {
        Some(m) => println!(
            "sync-symbol Es/N0 {:.2} dB over {} slots (before the output quantiser)",
            m.snr_db, m.slots
        ),
        None => println!("too few symbols for the ISDB-S3 slot sync"),
    }
}

fn sidecar(out: &Path, meta: &Value, overwrite: bool) -> Result<()> {
    let mut p = out.as_os_str().to_owned();
    p.push(".json");
    let p = PathBuf::from(p);
    let mut f = crate::capture::create(&p, overwrite)?;
    f.write_all(serde_json::to_string_pretty(meta)?.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// The sidecar metadata; `version` is the `Symbols` format the words
/// were written in (the CPU front end's, or the device's).
#[allow(clippy::too_many_arguments)]
fn meta(
    source: &str,
    version: u16,
    sys: &KnownSystem,
    fs: f64,
    cfo: f64,
    centre: Option<u64>,
    p: &Params,
    trim: f64,
) -> Value {
    json!({
        "format": "isdb-rs Symbols",
        "format_version": version,
        "source": source,
        "system": sys.name,
        "symbol_rate_hz": sys.symbol_rate_hz,
        "roll_off": sys.roll_off,
        "sample_rate_hz": fs,
        "carrier_offset_hz": cfo,
        "center_hz": centre,
        "trim_symbols": trim,
        "params": vectors::params_json(p),
    })
}

/// `isdb iq symbols` arguments.
#[derive(Debug, Args)]
pub struct IqSymbolsArgs {
    /// A recording (data file, `.sigmf-meta` or SigMF base name), or with
    /// `--read` a words file.
    pub file: PathBuf,
    #[command(flatten)]
    pub fe: FrontendOpts,
    /// Samples processed from the first segment (default: all).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub samples: Option<usize>,
    /// Output words file (`OUT.json` gets the parameters).
    #[arg(short, long, value_name = "PATH", required_unless_present = "read")]
    pub out: Option<PathBuf>,
    /// Read a words file instead (from `isdb symbols` or this command):
    /// blocks, losses and the symbols' ring MER.
    #[arg(long)]
    pub read: bool,
    /// Replace existing output files.
    #[arg(long)]
    pub overwrite: bool,
}

/// Run `isdb iq symbols`.
pub fn run_iq(a: &IqSymbolsArgs) -> Result<()> {
    if a.read {
        return read_words(&a.file);
    }
    let out = a.out.as_ref().context("--out")?;
    let mut rec = Recording::open(&a.file, None, None)?;
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start)?.min(a.samples.unwrap_or(usize::MAX));
    let x = rec.read(seg.start, n)?;
    let (sys, cfo) = pick_system(a.fe.system, &x, fs)?;
    let (p, trim) = design(sys, fs, cfo, &a.fe, &x)?;
    eprintln!(
        "[symbols] {}: {} samples at {fs:.0} Hz, {} with carrier {cfo:+.0} Hz; {} taps, {} bits, CPU",
        a.file.display(),
        x.len(),
        sys.name,
        p.taps(),
        p.bits
    );
    let mut f = std::io::BufWriter::new(crate::capture::create(out, a.overwrite)?);
    let r = cpu_front_end(&p, &x, &mut f)?;
    f.flush()?;
    sidecar(
        out,
        &meta(
            "cpu",
            isdb_frontend::params::FORMAT_VERSION,
            sys,
            fs,
            cfo,
            rec.frequency.map(|v| v as u64),
            &p,
            trim,
        ),
        a.overwrite,
    )?;
    print_report(&r, sys, fs);
    Ok(())
}

/// Parse a words file and report it.
fn read_words(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut r = Reader::new();
    let mut ev = Vec::new();
    r.feed_bytes(&bytes, &mut ev);
    r.finish(&mut ev);
    let (mut blocks, mut symbols, mut lost_blocks, mut losses, mut skipped) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut q = Vec::new();
    let mut first = None;
    let mut tail = 0u64;
    for e in &ev {
        match e {
            Event::Block { header, symbols: s } => {
                first.get_or_insert(*header);
                blocks += 1;
                symbols += s.len() as u64;
                if q.len() < 1 << 21 {
                    q.extend(s.iter().map(|v| [f64::from(v[0]), f64::from(v[1])]));
                }
            }
            Event::Loss { blocks: b, words } => {
                losses += 1;
                lost_blocks += b;
                skipped += words;
            }
            Event::Truncated { words } => tail = *words,
        }
    }
    let h = first.context("no whole block in the file")?;
    println!(
        "{}: {blocks} blocks of {} symbols ({symbols} symbols, {} bits per component), {losses} loss event(s): {lost_blocks} block(s) missing, {skipped} words skipped; {tail} words of an incomplete last block",
        path.display(),
        h.symbols(),
        h.bits
    );
    println!(
        "first block: sequence {}, gain {:.3}, period {:.6} samples; ring MER of the output {:.2} dB",
        h.seq,
        f64::from(h.gain) / 1024.0,
        f64::from(h.period) / ONE as f64,
        ring_stats(&q, 1).mer_db
    );
    Ok(())
}

/// Decode capture bytes to unit-scale samples.
pub(crate) fn decode(format: CaptureFormat, bytes: &[u8]) -> Vec<C32> {
    let f = match format {
        CaptureFormat::Cs8 => isdb_iq::SampleFormat::Cs8,
        CaptureFormat::Cs16 { full_scale } => isdb_iq::SampleFormat::Cs16 { full_scale },
    };
    let mut out = Vec::with_capacity(bytes.len() / f.bytes_per_sample());
    f.decode(bytes, &mut out);
    out
}

/// Run `isdb symbols`.
pub fn run(a: &SymbolsArgs) -> Result<()> {
    let backend = isdb_radio::backend(&a.radio.backend)?;
    let cfg = RadioConfig {
        center_hz: a.freq.hz,
        nominal_hz: a.freq.hz,
        rate_num: a.rate.round() as u32,
        rate_den: 1,
        bandwidth_hz: a.bw.round() as u32,
        args: a.radio.args.clone(),
        gains: a.gain.clone(),
    };
    let offers = backend.offloads(&cfg)?;
    let fpga = offers.iter().find_map(|o| match *o {
        Offload::Symbols { version, taps } => Some((version, taps)),
        _ => None,
    });
    match fpga {
        Some((v, t)) => {
            eprintln!("[symbols] the radio offers the symbol front end (format {v}, {t} taps)")
        }
        None => {
            eprintln!("[symbols] the radio offers no symbol front end (this bitstream has none)")
        }
    }
    if a.probe {
        return Ok(());
    }
    let out = a.out.as_ref().context("--out")?;
    let use_fpga = match (a.offload, fpga) {
        (OffloadArg::Cpu, _) => false,
        (_, Some((v, t)))
            if (isdb_frontend::params::MIN_ARITH_VERSION
                ..=isdb_frontend::params::FORMAT_VERSION)
                .contains(&v) =>
        {
            if t != a.fe.taps {
                eprintln!("[symbols] using the device's {t} taps");
            }
            true
        }
        (OffloadArg::Fpga, _) => {
            bail!("--offload fpga: the radio has no compatible symbol front end")
        }
        _ => false,
    };
    let stop = crate::stop::flag();
    // A short IQ capture for the system, carrier offset and rate.
    let (limit, _) = backend
        .capture_limit(&cfg)?
        .context("this radio cannot capture IQ")?;
    let probe_n = limit.min(1 << 22);
    let mut probe = Vec::new();
    let cap = backend.capture(&cfg, probe_n, &mut probe, &stop)?;
    let fs = cap.sample_rate_hz;
    let x = decode(cap.format, &probe);
    let (sys, cfo) = pick_system(a.fe.system, &x, fs)?;
    let mut fe = a.fe.clone();
    if use_fpga {
        fe.taps = fpga.map_or(fe.taps, |(_, t)| t);
    }
    let (p, trim) = design(sys, fs, cfo, &fe, &x)?;
    eprintln!(
        "[symbols] {} at {fs:.0} Hz, carrier {cfo:+.0} Hz; {} taps, {} bits per component, on the {}",
        sys.name,
        p.taps(),
        p.bits,
        if use_fpga { "FPGA" } else { "CPU" }
    );
    let mut f = std::io::BufWriter::new(crate::capture::create(out, a.overwrite)?);
    if use_fpga {
        let words = (a.seconds * sys.symbol_rate_hz * p.words_per_symbol()).ceil() as u64;
        let c = backend.capture_symbols(&cfg, &p, words, &mut f, &stop)?;
        f.flush()?;
        sidecar(
            out,
            &meta(
                "fpga",
                fpga.map_or(isdb_frontend::params::FORMAT_VERSION, |(v, _)| v),
                sys,
                c.sample_rate_hz,
                cfo,
                Some(c.lo_hz),
                &p,
                trim,
            ),
            a.overwrite,
        )?;
        println!(
            "{} words ({:.1} MB/s of symbols), {} DMA overflow report(s); {}; {}",
            c.words,
            c.words as f64 * 2.0 / a.seconds / 1e6,
            c.overflows,
            c.method,
            c.gain
        );
        return Ok(());
    }
    // CPU: one contiguous capture (a stream would lose samples at this
    // rate), then the same front end.
    let n = ((a.seconds * fs) as u64).min(limit);
    if (n as f64) < a.seconds * fs {
        eprintln!(
            "[symbols] the CPU path takes one contiguous capture: {:.3} s here (continuous symbols need the FPGA front end)",
            n as f64 / fs
        );
    }
    let mut bytes = Vec::new();
    let cap = backend.capture(&cfg, n, &mut bytes, &stop)?;
    let x = decode(cap.format, &bytes);
    let r = cpu_front_end(&p, &x, &mut f)?;
    f.flush()?;
    sidecar(
        out,
        &meta(
            "cpu",
            isdb_frontend::params::FORMAT_VERSION,
            sys,
            cap.sample_rate_hz,
            cfo,
            Some(cap.lo_hz),
            &p,
            trim,
        ),
        a.overwrite,
    )?;
    print_report(&r, sys, cap.sample_rate_hz);
    Ok(())
}
