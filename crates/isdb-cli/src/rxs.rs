//! `isdb rx --system isdbs`: ISDB-S reception, from a recording or a
//! radio, to one TS per TS_ID.
//!
//! The chain is `isdbs-phy`'s: the single-carrier front end, frame and
//! superframe sync with the TMCC, then the main-signal decoder per
//! superframe. It works on contiguous blocks of samples: a whole
//! recording segment, or one radio capture. Over Gigabit Ethernet a radio
//! cannot stream ISDB-S's ≥ 39 Msps without losses (VERIFICATION.md
//! "8-bit capture and streaming"), so live reception takes one contiguous
//! capture after another: each block decodes independently and the TS has
//! a gap between blocks, which `isdb rx` reports. Continuous reception
//! needs the FPGA symbol front end.

use crate::bands::{Channel, parse_hz};
use anyhow::{Context, Result, bail};
use isdb_iq::{C32, Recording};
use isdb_radio::{DeviceSpec, RadioConfig};
use isdbs_core::frame::FRAME_SYMBOLS;
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::ts::{DecodedSuperframe, decode_all};
use isdbs_phy::{RxOptions, receive};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// The default sample rate for ISDB-S from a radio: ≥ (1 + 0.35) × 28.86
/// Mbaud = 38.96 Msps (BO.1408-1 §9 roll-off), the rate the front end is
/// validated at (VERIFICATION.md "Symbol front end").
pub const DEFAULT_RATE: f64 = 40e6;
/// The default analog bandwidth: the AD9361's widest, which the 40 Msps
/// front end needs (a 32 MHz filter cost 2 dB of MER).
pub const DEFAULT_BW: f64 = 56e6;

/// What to write: per-TS output files.
pub struct Outputs {
    template: Option<String>,
    only: Vec<u16>,
    strip_nulls: bool,
    files: BTreeMap<u16, Box<dyn Write>>,
    /// Packets written per TS_ID.
    pub written: BTreeMap<u16, u64>,
}

impl Outputs {
    /// `template`: a path, with `{ts_id}` replaced by the TS_ID (4 hex
    /// digits) for one file per TS; without the placeholder, exactly one
    /// TS must be selected with `only`. `-` writes to stdout (one TS).
    pub fn new(template: Option<&str>, only: &[u16], strip_nulls: bool) -> Result<Self> {
        if let Some(t) = template
            && !t.contains("{ts_id}")
            && only.len() != 1
        {
            bail!(
                "--out {t:?} names one file: select one TS with --ts-id, or put {{ts_id}} in the path for one file per TS"
            );
        }
        Ok(Self {
            template: template.map(str::to_owned),
            only: only.to_vec(),
            strip_nulls,
            files: BTreeMap::new(),
            written: BTreeMap::new(),
        })
    }

    fn wanted(&self, ts_id: u16) -> bool {
        self.only.is_empty() || self.only.contains(&ts_id)
    }

    fn write(&mut self, ts_id: u16, p: &[u8; 188]) -> Result<()> {
        // Slots whose relative TS ID points at an unused TS_ID entry
        // (FFFFh) belong to no TS (the lab transponders fill them with
        // null packets): counted, never written.
        if ts_id == isdbs_core::tmcc::UNUSED_TS_ID || !self.wanted(ts_id) {
            return Ok(());
        }
        if self.strip_nulls && crate::source::is_null(p) {
            return Ok(());
        }
        let Some(t) = &self.template else {
            return Ok(());
        };
        let w = match self.files.entry(ts_id) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let path = t.replace("{ts_id}", &format!("{ts_id:04x}"));
                let w: Box<dyn Write> = if path == "-" {
                    Box::new(std::io::stdout().lock())
                } else {
                    Box::new(std::io::BufWriter::new(
                        std::fs::File::create(&path).with_context(|| format!("create {path}"))?,
                    ))
                };
                e.insert(w)
            }
        };
        w.write_all(p)?;
        *self.written.entry(ts_id).or_default() += 1;
        Ok(())
    }

    /// Flush every file.
    pub fn flush(&mut self) -> Result<()> {
        for w in self.files.values_mut() {
            w.flush()?;
        }
        Ok(())
    }
}

/// Counters of one TS.
#[derive(Debug, Clone, Default)]
pub struct TsStats {
    /// Packets decoded.
    pub packets: u64,
    /// RS words without errors.
    pub clean: u64,
    /// RS words with corrected bytes, and the bytes.
    pub corrected_packets: u64,
    /// Corrected bytes.
    pub corrected_bytes: u64,
    /// Uncorrectable words (sent on with transport_error_indicator = 1).
    pub uncorrectable: u64,
    /// Slots per frame the TMCC gives this TS.
    pub slots: usize,
}

/// Counters of a whole reception.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Contiguous blocks decoded (recording segments or radio captures).
    pub blocks: u64,
    /// Samples processed.
    pub samples: u64,
    /// Frames found, and those whose W words decoded intact.
    pub frames: u64,
    /// Frames with intact sync words.
    pub headers_ok: u64,
    /// Frames missing inside a block.
    pub slips: u64,
    /// Whole superframes, and those whose own TMCC decoded.
    pub superframes: u64,
    /// Superframes whose TMCC RS(64,48) succeeded.
    pub tmcc_ok: u64,
    /// Superframes decoded to packets.
    pub decoded: u64,
    /// Per TS_ID.
    pub ts: BTreeMap<u16, TsStats>,
    mer_sum: f64,
    mer_n: u64,
    /// The last TMCC.
    pub tmcc: Option<isdbs_core::TmccInfo>,
    /// Mean front-end carrier offset and symbol rate of the last block.
    pub carrier_offset_hz: f64,
    /// Symbol rate the front end settled at in the last block, Bd.
    pub symbol_rate_hz: f64,
}

impl Stats {
    fn add(&mut self, d: &DecodedSuperframe) {
        self.decoded += 1;
        self.mer_sum += d.mer_db;
        self.mer_n += 1;
        let ids = d.info.slot_ts_ids();
        for p in &d.packets {
            let t = self.ts.entry(p.ts_id).or_default();
            t.slots = ids.iter().filter(|i| **i == Some(p.ts_id)).count();
            t.packets += 1;
            match p.corrected {
                Some(0) => t.clean += 1,
                Some(n) => {
                    t.corrected_packets += 1;
                    t.corrected_bytes += n as u64;
                }
                None => t.uncorrectable += 1,
            }
        }
        self.tmcc = Some(d.info.clone());
    }

    /// Mean main-signal MER, dB.
    pub fn mer_db(&self) -> Option<f64> {
        (self.mer_n > 0).then(|| self.mer_sum / self.mer_n as f64)
    }

    /// The JSON summary.
    pub fn json(&self) -> Value {
        json!({
            "system": "ISDB-S",
            "blocks": self.blocks,
            "samples": self.samples,
            "frames": self.frames,
            "headers_ok": self.headers_ok,
            "slips": self.slips,
            "superframes": self.superframes,
            "tmcc_ok": self.tmcc_ok,
            "superframes_decoded": self.decoded,
            "main_mer_db": self.mer_db(),
            "carrier_offset_hz": self.carrier_offset_hz,
            "symbol_rate_hz": self.symbol_rate_hz,
            "ts": self.ts.iter().map(|(id, t)| json!({
                "ts_id": id,
                "slots_per_frame": t.slots,
                "packets": t.packets,
                "rs_clean": t.clean,
                "rs_corrected_packets": t.corrected_packets,
                "rs_corrected_bytes": t.corrected_bytes,
                "rs_uncorrectable": t.uncorrectable,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Decode one contiguous block of unit-scale samples at `fs` and write its
/// packets. Returns the number of superframes decoded.
pub fn decode_block(
    x: &[C32],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<usize> {
    stats.blocks += 1;
    stats.samples += x.len() as u64;
    let s = isdbs_phy::frontend::symbols(x, fs, carrier_offset_hz)
        .context("ISDB-S front end parameters for this sample rate")?;
    stats.carrier_offset_hz = s.carrier_offset_hz;
    stats.symbol_rate_hz = s.symbol_rate_hz;
    if s.z.len() < 2 * FRAME_SYMBOLS {
        return Ok(0);
    }
    let r = receive(&s.z, &RxOptions::default());
    stats.frames += r.frames.len() as u64;
    stats.headers_ok += r.frames.iter().filter(|f| f.header_ok).count() as u64;
    stats.slips += r.slips as u64;
    stats.superframes += r.superframes.len() as u64;
    stats.tmcc_ok += r.superframes.iter().filter(|s| s.info.is_some()).count() as u64;
    let decoded = decode_all(&r, &DemodOptions::default());
    for d in &decoded {
        stats.add(d);
        for p in &d.packets {
            out.write(p.ts_id, &p.bytes)?;
        }
    }
    Ok(decoded.len())
}

/// The summary lines printed at the end.
pub fn report(stats: &Stats, secs: f64) {
    eprintln!(
        "[rx] done: ISDB-S, {:.3} s of signal in {} block(s); {} frames ({} with intact sync words), {} slips; {} superframes, TMCC ok {}, decoded {}; main MER {}",
        secs,
        stats.blocks,
        stats.frames,
        stats.headers_ok,
        stats.slips,
        stats.superframes,
        stats.tmcc_ok,
        stats.decoded,
        stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
    );
    if let Some(i) = &stats.tmcc {
        let combos: Vec<String> = i
            .combinations
            .iter()
            .filter_map(|c| c.combination().map(|k| format!("{k} ×{}", c.slots)))
            .collect();
        eprintln!("[rx]   TMCC: {}", combos.join(" + "));
    }
    for (id, t) in &stats.ts {
        let name = if *id == isdbs_core::tmcc::UNUSED_TS_ID {
            "unassigned slots (TS_ID FFFFh, not written)".to_string()
        } else {
            format!("TS_ID {id:#06x}")
        };
        eprintln!(
            "[rx]   {name} ({} slots/frame): {} packets; RS clean {} corrected {} ({} bytes) uncorrectable {}",
            t.slots, t.packets, t.clean, t.corrected_packets, t.corrected_bytes, t.uncorrectable
        );
    }
    if stats.blocks > 1 {
        eprintln!(
            "[rx]   the {} blocks were captured one after another with gaps between them: each TS is discontinuous at every block boundary",
            stats.blocks
        );
    }
}

/// Offline: every contiguous segment of a recording.
pub fn run_file(
    path: &str,
    max_samples: Option<u64>,
    carrier_offset_hz: Option<f64>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    let mut rec = Recording::open(std::path::Path::new(path), None, None).with_context(|| {
        format!("{path}: an ISDB-S recording needs its SigMF metadata (rate and format)")
    })?;
    let fs = rec.sample_rate;
    let mut left = max_samples.unwrap_or(u64::MAX);
    for seg in rec.segments() {
        if left == 0 {
            break;
        }
        let n = (seg.end - seg.start).min(left);
        left -= n;
        let x = rec.read(seg.start, usize::try_from(n)?)?;
        decode_block(&x, fs, carrier_offset_hz, out, stats)?;
    }
    Ok(stats.samples as f64 / fs)
}

/// Live: contiguous captures of `block_s` seconds from `spec`, decoded one
/// after another until `seconds` of signal (0: until Ctrl-C).
#[allow(clippy::too_many_arguments)]
pub fn run_radio(
    spec: &DeviceSpec,
    freq_hz: u64,
    channel: Option<Channel>,
    rate: f64,
    bw: f64,
    gains: &[(String, String)],
    block_s: f64,
    seconds: f64,
    carrier_offset_hz: Option<f64>,
    status: bool,
    stop: &AtomicBool,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    let backend = isdb_radio::backend(&spec.backend)?;
    let cfg = RadioConfig {
        center_hz: freq_hz,
        nominal_hz: freq_hz,
        rate_num: rate.round() as u32,
        rate_den: 1,
        bandwidth_hz: bw.round() as u32,
        args: spec.args.clone(),
        gains: gains.to_vec(),
    };
    eprintln!(
        "{} capture settings{}:\n{}",
        backend.name(),
        channel.map(|c| c.label()).unwrap_or_default(),
        backend.describe_capture(&cfg)?
    );
    let Some((limit, why)) = backend.capture_limit(&cfg)? else {
        bail!(
            "radio backend {:?} cannot capture, which ISDB-S reception needs",
            backend.name()
        );
    };
    let want = (block_s * rate).round() as u64;
    let n = want.min(limit);
    if n < want {
        eprintln!(
            "[rx] block shortened to {} samples ({:.3} s): {why}",
            n,
            n as f64 / rate
        );
    }
    let mut secs = 0.0;
    while !stop.load(Ordering::Relaxed) && (seconds <= 0.0 || secs < seconds) {
        let mut bytes = Vec::with_capacity(n as usize * 2);
        let cap = backend.capture(&cfg, n, &mut bytes, stop)?;
        if !cap.gaps.is_empty() {
            eprintln!(
                "[rx] the device lost samples {} time(s) inside this block; decoding its contiguous parts",
                cap.gaps.len()
            );
        }
        let x = crate::symbols::decode(cap.format, &bytes);
        let fs = cap.sample_rate_hz;
        // Decode each contiguous part of the capture separately.
        let mut start = 0usize;
        let mut cuts: Vec<usize> = cap.gaps.iter().map(|g| g.at as usize).collect();
        cuts.push(x.len());
        for c in cuts {
            if c > start {
                decode_block(&x[start..c], fs, carrier_offset_hz, out, stats)?;
            }
            start = c;
        }
        secs += x.len() as f64 / fs;
        if status {
            let last = stats.ts.values().map(|t| t.uncorrectable).sum::<u64>();
            eprintln!(
                "[rx] {secs:.2} s: block {}; {} superframes decoded; packets {} (uncorrectable {last}); MER {}",
                stats.blocks,
                stats.decoded,
                stats.ts.values().map(|t| t.packets).sum::<u64>(),
                stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
            );
        }
    }
    Ok(secs)
}

/// Parse a TS_ID (`0x4010`, `4010h`, decimal).
pub fn parse_ts_id(s: &str) -> Result<u16> {
    let t = s.trim();
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u16::from_str_radix(h, 16)
    } else if let Some(h) = t.strip_suffix('h').or_else(|| t.strip_suffix('H')) {
        u16::from_str_radix(h, 16)
    } else {
        t.parse()
    }
    .with_context(|| format!("TS_ID {t:?}"))?;
    Ok(v)
}

/// Parse a sample rate or bandwidth (`40M`, `40000000`).
pub fn parse_rate(s: &str) -> Result<f64> {
    parse_hz(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_id_forms() {
        assert_eq!(parse_ts_id("0x4010").unwrap(), 0x4010);
        assert_eq!(parse_ts_id("4010h").unwrap(), 0x4010);
        assert_eq!(parse_ts_id("16400").unwrap(), 16400);
        assert!(parse_ts_id("0x1_0000").is_err());
    }

    #[test]
    fn output_template_rules() {
        assert!(Outputs::new(Some("bs1.ts"), &[], false).is_err());
        assert!(Outputs::new(Some("bs1.ts"), &[0x4010, 0x4011], false).is_err());
        assert!(Outputs::new(Some("bs1.ts"), &[0x4010], false).is_ok());
        assert!(Outputs::new(Some("bs1-{ts_id}.ts"), &[], false).is_ok());
        assert!(Outputs::new(None, &[], false).is_ok());
    }

    #[test]
    fn outputs_split_by_ts_id() {
        let dir = std::env::temp_dir().join(format!("isdb-rxs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("x-{ts_id}.ts").display().to_string();
        let mut o = Outputs::new(Some(&t), &[0x4010, 0x4011], true).unwrap();
        let mut p = [0u8; 188];
        p[0] = 0x47;
        p[1] = 0x01;
        o.write(0x4010, &p).unwrap();
        o.write(0x4011, &p).unwrap();
        o.write(0x4012, &p).unwrap(); // not selected
        let mut null = p;
        null[1] = 0x1F;
        null[2] = 0xFF;
        o.write(0x4010, &null).unwrap(); // stripped
        o.flush().unwrap();
        drop(o);
        assert_eq!(std::fs::read(dir.join("x-4010.ts")).unwrap().len(), 188);
        assert_eq!(std::fs::read(dir.join("x-4011.ts")).unwrap().len(), 188);
        assert!(!dir.join("x-4012.ts").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
