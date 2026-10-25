//! `isdb rx --system isdbs3`: ISDB-S3 reception, from a recording or a
//! radio, to one TLV stream file per TLV stream id.
//!
//! The chain is `isdbs3-phy`'s: the single-carrier front end with the
//! timing trim, slot and frame sync, the pilot-trained equaliser, TMCC,
//! the main signal per frame (LDPC, BCH), then `isdb-tlv`'s assembler per
//! stream. A TLV stream file is the TLV packets back to back, each with
//! its 4-byte header (ARIB STD-B32 Part 3 §3.5). Like ISDB-S, a radio
//! delivers one contiguous capture after another, so each stream is
//! discontinuous at every block boundary (reported).

use crate::bands::Channel;
use anyhow::{Context, Result, bail};
use isdb_iq::{C32, Recording};
use isdb_radio::{DeviceSpec, RadioConfig};
use isdb_tlv::Assembler;
use isdb_tlv::stats::StreamStats;
use isdbs3_core::params::FRAME_SYMBOLS;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::rx::{RxOptions, receive};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

/// Default sample rate from a radio: ≥ (1 + 0.03) × 33.756 Mbaud = 34.8
/// Msps; 40 Msps is the rate the ISDB-S front end shares (VERIFICATION.md
/// "Symbol front end").
pub const DEFAULT_RATE: f64 = 40e6;
/// Default analog bandwidth.
pub const DEFAULT_BW: f64 = 56e6;
/// TLV stream id of the relative streams the TMCC leaves unassigned.
pub const UNUSED_ID: u16 = 0xFFFF;

/// Per-stream output files.
pub struct Outputs {
    template: Option<String>,
    only: Vec<u16>,
    files: BTreeMap<u16, Box<dyn Write>>,
    /// TLV packets written per stream id.
    pub written: BTreeMap<u16, u64>,
}

impl Outputs {
    /// `template`: a path with `{tlv_id}` (or `{ts_id}`) replaced by the
    /// stream id (4 hex digits); without a placeholder exactly one stream
    /// must be selected with `only`. `-` writes to stdout.
    pub fn new(template: Option<&str>, only: &[u16]) -> Result<Self> {
        if let Some(t) = template
            && !t.contains("{tlv_id}")
            && !t.contains("{ts_id}")
            && only.len() != 1
        {
            bail!(
                "--out {t:?} names one file: select one TLV stream with --ts-id, or put {{tlv_id}} in the path for one file per stream"
            );
        }
        Ok(Self {
            template: template.map(str::to_owned),
            only: only.to_vec(),
            files: BTreeMap::new(),
            written: BTreeMap::new(),
        })
    }

    fn write(&mut self, id: u16, bytes: &[u8]) -> Result<()> {
        let Some(t) = &self.template else {
            return Ok(());
        };
        if id == UNUSED_ID || !(self.only.is_empty() || self.only.contains(&id)) {
            return Ok(());
        }
        let w = match self.files.entry(id) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let path = t
                    .replace("{tlv_id}", &format!("{id:04x}"))
                    .replace("{ts_id}", &format!("{id:04x}"));
                let w: Box<dyn Write> = if path == "-" {
                    Box::new(std::io::stdout().lock())
                } else {
                    Box::new(std::io::BufWriter::new(
                        crate::capture::create(std::path::Path::new(&path), true)
                            .with_context(|| format!("create {path}"))?,
                    ))
                };
                e.insert(w)
            }
        };
        w.write_all(bytes)?;
        *self.written.entry(id).or_default() += 1;
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

/// Counters over a whole run.
#[derive(Debug, Default)]
pub struct Stats {
    /// Blocks decoded.
    pub blocks: u64,
    /// Samples read.
    pub samples: u64,
    /// Whole frames found.
    pub frames: u64,
    /// Frames whose TMCC decoded.
    pub tmcc_ok: u64,
    /// Valid slots decoded / LDPC failures / BCH failures.
    pub slots: u64,
    /// See `slots`.
    pub ldpc_failed: u64,
    /// See `slots`.
    pub bch_failed: u64,
    /// LDPC iterations (sum) and BCH bits corrected (sum).
    pub ldpc_iterations: u64,
    /// See `ldpc_iterations`.
    pub bch_corrected: u64,
    mer_sum: f64,
    mer_n: u64,
    /// Sync Es/N0 before and after equalisation (last block), dB.
    pub sync_snr_db: f64,
    /// See `sync_snr_db`.
    pub sync_snr_eq_db: Option<f64>,
    /// Carrier offset and trim of the last block.
    pub carrier_offset_hz: f64,
    /// See `carrier_offset_hz`.
    pub trim: f64,
    /// The last TMCC.
    pub tmcc: Option<isdbs3_core::TmccInfo>,
    /// Per stream id: assembler counters and content counters.
    pub streams: BTreeMap<u16, (isdb_tlv::tlv::Stats, StreamStats, usize)>,
}

impl Stats {
    /// Mean main-signal MER, dB.
    pub fn mer_db(&self) -> Option<f64> {
        (self.mer_n > 0).then(|| self.mer_sum / self.mer_n as f64)
    }

    /// JSON summary.
    pub fn json(&self) -> Value {
        json!({
            "system": "ISDB-S3",
            "blocks": self.blocks,
            "frames": self.frames,
            "tmcc_ok": self.tmcc_ok,
            "slots": self.slots,
            "ldpc_failed": self.ldpc_failed,
            "bch_failed": self.bch_failed,
            "ldpc_mean_iterations": self.ldpc_iterations as f64 / (self.slots - self.ldpc_failed).max(1) as f64,
            "bch_corrected_bits": self.bch_corrected,
            "main_mer_db": self.mer_db(),
            "sync_esn0_db": self.sync_snr_db,
            "sync_esn0_equalised_db": self.sync_snr_eq_db,
            "carrier_offset_hz": self.carrier_offset_hz,
            "trim": self.trim,
            "tmcc": self.tmcc.as_ref().map(|t| json!({
                "modes": t.modes.iter().filter(|m| m.is_used()).map(|m| json!({
                    "modulation": m.modulation().map(|x| x.name()),
                    "code_rate": m.rate().map(|r| format!("{}/120", r.numerator())),
                    "slots": m.slots,
                    "obo_db": f64::from(m.obo) / 10.0,
                })).collect::<Vec<_>>(),
                "streams": (0..16).filter(|&k| t.stream_type[k] != 0xFF).map(|k| json!({
                    "relative": k,
                    "type": t.stream_type[k],
                    "id": t.stream_id[k],
                    "slots": t.slot_stream.iter().filter(|&&s| usize::from(s) == k).count(),
                })).collect::<Vec<_>>(),
                "ews": t.ews,
            })),
            "streams": self.streams.iter().map(|(id, (a, s, slots))| json!({
                "id": id,
                "slots_per_frame": slots,
                "tlv_packets": a.packets,
                "lost_slots": a.lost_slots,
                "pointer_mismatches": a.pointer_mismatches,
                "skipped_bytes": a.skipped_bytes,
                "by_type": s.by_type.iter().map(|(k, (n, b))| (k.clone(), json!({"packets": n, "bytes": b}))).collect::<serde_json::Map<_, _>>(),
                "ip_errors": s.ip_errors,
                "mmtp_packet_ids": s.mmtp.iter().map(|(k, (n, _))| (format!("{k:#06x}"), json!(n))).collect::<serde_json::Map<_, _>>(),
                "mmt_si_tables": s.mmt_tables.iter().map(|(k, n)| (format!("{k:#04x} {}", isdb_tlv::mmtp::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
                "tlv_si_tables": s.tlv_si.iter().map(|(k, n)| (format!("{k:#04x} {}", isdb_tlv::si::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
                "mpu_payloads": s.mpu,
                "scrambled_mpu_payloads": s.mpu_with_extension,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Decode one contiguous block of unit-scale samples at `fs`.
pub fn decode_block(
    x: &[C32],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<()> {
    stats.blocks += 1;
    stats.samples += x.len() as u64;
    let s = symbols(x, fs, carrier_offset_hz, Trim::Auto)
        .context("ISDB-S3 front end parameters for this sample rate")?;
    stats.carrier_offset_hz = s.carrier_offset_hz;
    stats.trim = s.trim;
    if s.z.len() < 2 * FRAME_SYMBOLS {
        return Ok(());
    }
    let r = receive(&s.z, &RxOptions::default());
    stats.sync_snr_db = r.sync_snr_db;
    stats.sync_snr_eq_db = r.sync_snr_eq_db;
    stats.frames += r.frames.len() as u64;
    for f in &r.frames {
        if let Ok(t) = &f.tmcc {
            stats.tmcc_ok += 1;
            stats.tmcc = Some(t.clone());
        }
        if let Some(m) = f.mer_db {
            stats.mer_sum += m;
            stats.mer_n += 1;
        }
    }
    // a fresh assembler per block: the blocks are not contiguous
    let mut asm: BTreeMap<u16, Assembler> = BTreeMap::new();
    for sl in &r.slots {
        stats.slots += 1;
        match sl.ldpc_iterations {
            Some(i) => stats.ldpc_iterations += i as u64,
            None => stats.ldpc_failed += 1,
        }
        match sl.bch_corrected {
            Some(c) => stats.bch_corrected += c as u64,
            None => stats.bch_failed += u64::from(sl.ldpc_iterations.is_some()),
        }
        let mut pk = Vec::new();
        asm.entry(sl.stream_id)
            .or_default()
            .push_slot(sl.data.as_deref(), sl.head, &mut pk);
        let e = stats.streams.entry(sl.stream_id).or_default();
        if let Some(t) = &stats.tmcc {
            e.2 = t
                .slot_stream
                .iter()
                .filter(|&&k| t.stream_id[usize::from(k & 0x0F)] == sl.stream_id)
                .count();
        }
        for p in &pk {
            e.1.add(p);
            out.write(sl.stream_id, &p.bytes())?;
        }
    }
    for (id, a) in asm {
        let e = &mut stats.streams.entry(id).or_default().0;
        e.packets += a.stats.packets;
        e.lost_slots += a.stats.lost_slots;
        e.pointer_mismatches += a.stats.pointer_mismatches;
        e.skipped_bytes += a.stats.skipped_bytes;
    }
    Ok(())
}

/// The summary lines printed at the end.
pub fn report(stats: &Stats, secs: f64) {
    eprintln!(
        "[rx] done: ISDB-S3, {:.3} s of signal in {} block(s); {} frames, TMCC ok {}; {} slots, LDPC failed {}, BCH failed {} ({} bits corrected), mean {:.2} LDPC iterations; sync Es/N0 {:.1} dB{}; main MER {}",
        secs,
        stats.blocks,
        stats.frames,
        stats.tmcc_ok,
        stats.slots,
        stats.ldpc_failed,
        stats.bch_failed,
        stats.bch_corrected,
        stats.ldpc_iterations as f64 / (stats.slots - stats.ldpc_failed).max(1) as f64,
        stats.sync_snr_db,
        stats
            .sync_snr_eq_db
            .map_or(String::new(), |e| format!(" ({e:.1} dB equalised)")),
        stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
    );
    if let Some(t) = &stats.tmcc {
        let modes: Vec<String> = t
            .modes
            .iter()
            .filter(|m| m.is_used())
            .filter_map(|m| {
                Some(format!(
                    "{} {} ×{}",
                    m.modulation()?.name(),
                    m.rate()?,
                    m.slots
                ))
            })
            .collect();
        eprintln!("[rx]   TMCC: {}", modes.join(" + "));
    }
    for (id, (a, s, slots)) in &stats.streams {
        let name = if *id == UNUSED_ID {
            "unassigned slots (stream id FFFFh, not written)".to_string()
        } else {
            format!("TLV stream {id:#06x}")
        };
        let types: Vec<String> = s
            .by_type
            .iter()
            .map(|(k, (n, _))| format!("{k} {n}"))
            .collect();
        let mut tables: Vec<String> = s
            .mmt_tables
            .keys()
            .map(|k| isdb_tlv::mmtp::table_name(*k).to_string())
            .chain(
                s.tlv_si
                    .keys()
                    .map(|k| isdb_tlv::si::table_name(*k).to_string()),
            )
            .collect();
        tables.dedup();
        eprintln!(
            "[rx]   {name} ({slots} slots/frame): {} TLV packets ({}); lost slots {}, head-indication mismatches {}; signalling: {}",
            a.packets,
            types.join(", "),
            a.lost_slots,
            a.pointer_mismatches,
            if tables.is_empty() {
                "-".into()
            } else {
                tables.join(", ")
            }
        );
    }
    if stats.blocks > 1 {
        eprintln!(
            "[rx]   the {} blocks were captured one after another with gaps between them: each stream is discontinuous at every block boundary",
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
        format!("{path}: an ISDB-S3 recording needs its SigMF metadata (rate and format)")
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

/// Live: contiguous captures of `block_s` seconds from `spec`.
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
            "radio backend {:?} cannot capture, which ISDB-S3 reception needs",
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
            eprintln!(
                "[rx] {secs:.2} s: block {}; {} frames; {} slots, LDPC failed {}, BCH failed {}; MER {}",
                stats.blocks,
                stats.frames,
                stats.slots,
                stats.ldpc_failed,
                stats.bch_failed,
                stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
            );
        }
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_template_rules() {
        assert!(Outputs::new(Some("x.tlv"), &[]).is_err());
        assert!(Outputs::new(Some("x.tlv"), &[0xB0B0]).is_ok());
        assert!(Outputs::new(Some("bs11-{tlv_id}.tlv"), &[]).is_ok());
        assert!(Outputs::new(Some("bs11-{ts_id}.tlv"), &[]).is_ok());
    }
}
