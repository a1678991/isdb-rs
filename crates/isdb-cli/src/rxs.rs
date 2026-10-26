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
use crate::satpipe::{self, Block, Timing};
use anyhow::{Context, Result, bail};
use isdb_iq::{C32, Recording};
use isdb_radio::{DeviceSpec, RadioConfig};
use isdbs_core::frame::FRAME_SYMBOLS;
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::frontend::{Prior, Symbols, Trim, symbols_with_prior};
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
    /// Seconds spent in the front end (all blocks), in the receiver, and
    /// in the whole run (the stages overlap, so the wall time is less
    /// than their sum).
    pub front_s: f64,
    /// See `front_s`.
    pub back_s: f64,
    /// See `front_s`.
    pub wall_s: f64,
    /// Blocks that reused the previous block's acquisition.
    pub reused: u64,
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
    /// Streamed reception (FPGA or a words file): the word stream's
    /// counters; `None` for block-wise reception.
    pub words: Option<crate::symstream::WordStats>,
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
            "stream": self.words.map(|w| json!({
                "blocks": w.blocks,
                "symbols": w.symbols,
                "losses": w.losses,
                "blocks_lost": w.blocks_lost,
                "words_skipped": w.words_skipped,
                "resyncs": w.resyncs,
                "max_queued_symbols": w.max_queued_symbols,
                "stopped": w.stopped,
            })),
            "time": json!({
                "wall_s": self.wall_s,
                "front_end_s": self.front_s,
                "receiver_s": self.back_s,
                "reused_acquisitions": self.reused,
            }),
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

/// The front end of one block: symbols, and the acquisition the next
/// block of the same signal may reuse (`prior`).
pub fn front(
    x: &[C32],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    prior: Option<&Prior>,
) -> Result<(Symbols, Prior)> {
    symbols_with_prior(x, fs, carrier_offset_hz, Trim::Auto, prior)
        .context("ISDB-S front end parameters for this sample rate")
}

/// The rest of one block of `samples` samples: receive its symbols `s`
/// and write its packets. Returns the number of superframes decoded.
pub fn back(s: &Symbols, samples: u64, out: &mut Outputs, stats: &mut Stats) -> Result<usize> {
    stats.blocks += 1;
    stats.samples += samples;
    stats.carrier_offset_hz = s.carrier_offset_hz;
    stats.symbol_rate_hz = s.symbol_rate_hz;
    if s.z.len() < 2 * FRAME_SYMBOLS {
        return Ok(0);
    }
    Ok(decode(&s.z, false, out, stats)?.1)
}

/// Receive the symbols `z` and write their packets. With `keep_last` (a
/// stream that continues), the last whole superframe found and everything
/// after it are left for the next call (each superframe decodes on its
/// own, so a stream cut at a superframe start loses nothing): only the
/// superframes before it are counted and written. Returns where the kept
/// part starts (`z.len()` without `keep_last`; 0 when no whole superframe
/// was found yet) and how many superframes were decoded.
pub fn decode(
    z: &[isdbs_phy::Cplx],
    keep_last: bool,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<(usize, usize)> {
    let mut r = receive(z, &RxOptions::default());
    let start =
        |r: &isdbs_phy::Reception, sf: &isdbs_phy::Superframe| r.frames[sf.first_frame].mark.start;
    if keep_last && r.superframes.is_empty() {
        return Ok((0, 0));
    }
    let keep = if keep_last {
        r.superframes.len() - 1
    } else {
        r.superframes.len()
    };
    let next = r.superframes.get(keep).map_or(z.len(), |sf| start(&r, sf));
    // frames counted up to the first superframe not taken (all of them
    // without a cut)
    let frame_end = r
        .superframes
        .get(keep)
        .map_or(r.frames.len(), |sf| sf.first_frame);
    let frames = &r.frames[..frame_end];
    stats.frames += frames.len() as u64;
    stats.headers_ok += frames.iter().filter(|f| f.header_ok).count() as u64;
    if !keep_last {
        stats.slips += r.slips as u64;
    }
    r.superframes.truncate(keep);
    stats.superframes += r.superframes.len() as u64;
    stats.tmcc_ok += r.superframes.iter().filter(|s| s.info.is_some()).count() as u64;
    let decoded = decode_all(&r, &DemodOptions::default());
    for d in &decoded {
        stats.add(d);
        for p in &d.packets {
            out.write(p.ts_id, &p.bytes)?;
        }
    }
    Ok((next, decoded.len()))
}

/// The summary lines printed at the end.
pub fn report(stats: &Stats, secs: f64) {
    if let Some(w) = stats.words {
        eprintln!(
            "[rx] time: {:.2} s for {secs:.3} s of signal ({:.2}× real time, startup included); receiver {:.2} s; at most {:.2} s of symbols queued behind the receiver",
            stats.wall_s,
            secs / stats.wall_s.max(1e-9),
            stats.back_s,
            w.max_queued_symbols as f64 / isdbs_phy::frontend::system().symbol_rate_hz
        );
        eprintln!(
            "[rx] stream: {} blocks of symbols; {} loss(es) ({} blocks missing, {} words skipped), {} resync(s){}: each is a discontinuity in the TS",
            w.blocks,
            w.losses,
            w.blocks_lost,
            w.words_skipped,
            w.resyncs,
            if w.stopped {
                "; stopped with symbols still queued"
            } else {
                ""
            }
        );
    } else if stats.wall_s > 0.0 {
        eprintln!(
            "[rx] time: {:.2} s for {secs:.3} s of signal ({:.2}× real time); front end {:.2} s, receiver {:.2} s (overlapping); {} of {} block(s) reused the previous acquisition",
            stats.wall_s,
            secs / stats.wall_s,
            stats.front_s,
            stats.back_s,
            stats.reused,
            stats.blocks
        );
    }
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

/// Decode the blocks `produce` sends through [`satpipe::run`] and count
/// the stage times; `after` runs after each block is written (status
/// lines).
fn pipeline(
    produce: impl FnOnce(&std::sync::mpsc::SyncSender<Block>) -> Result<()> + Send,
    carrier_offset_hz: Option<f64>,
    out: &mut Outputs,
    stats: &mut Stats,
    mut after: impl FnMut(&Stats),
) -> Result<()> {
    let stats = std::cell::RefCell::new(stats);
    satpipe::run(
        produce,
        |b: &Block, prior: Option<&Prior>| front(&b.x, b.fs, carrier_offset_hz, prior),
        |s: Symbols, n: u64| back(&s, n, out, &mut stats.borrow_mut()).map(|_| ()),
        |t: Timing| {
            let mut st = stats.borrow_mut();
            st.front_s = t.front_s;
            st.back_s = t.back_s;
            st.wall_s = t.wall_s;
            st.reused = t.reused;
            after(&st);
        },
    )
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
    pipeline(
        move |tx| {
            let mut left = max_samples.unwrap_or(u64::MAX);
            for seg in rec.segments() {
                if left == 0 {
                    break;
                }
                let n = (seg.end - seg.start).min(left);
                left -= n;
                let x = rec.read(seg.start, usize::try_from(n)?)?;
                if tx.send(Block { x, fs }).is_err() {
                    break;
                }
            }
            Ok(())
        },
        carrier_offset_hz,
        out,
        stats,
        |_| {},
    )?;
    Ok(stats.samples as f64 / fs)
}

/// Streamed reception: decode the `Symbols` words of `bytes` (a radio's
/// FPGA front end, or a words file) as one continuous stream, cut on
/// superframe starts ([`crate::symstream`]); a loss in the words ends a
/// contiguous stretch (a discontinuity in the TS, reported). Returns the
/// seconds of signal (symbols / `symbol_rate_hz`).
pub fn run_stream(
    bytes: crate::symstream::Bytes,
    symbol_rate_hz: f64,
    status: bool,
    stop: &AtomicBool,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    let t0 = std::time::Instant::now();
    let stats = std::cell::RefCell::new(stats);
    let mut next_status = 1.0;
    let w = crate::symstream::run(
        bytes,
        crate::symstream::CHUNK_SUPERFRAMES * isdbs_core::frame::FRAMES * FRAME_SYMBOLS,
        stop,
        |z, cut| {
            let t = std::time::Instant::now();
            let mut st = stats.borrow_mut();
            let (used, _) = decode(z, cut == crate::symstream::Cut::More, out, &mut st)?;
            st.back_s += t.elapsed().as_secs_f64();
            Ok(used)
        },
        |_| Ok(()),
        |w| {
            let mut st = stats.borrow_mut();
            st.words = Some(*w);
            st.wall_s = t0.elapsed().as_secs_f64();
            let secs = w.symbols as f64 / symbol_rate_hz;
            if status && secs >= next_status {
                next_status = secs.floor() + 1.0;
                eprintln!(
                    "[rx] {secs:.2} s of signal after {:.2} s: {} superframes, TMCC ok {}, decoded {}; losses {} ({} blocks); MER {}",
                    st.wall_s,
                    st.superframes,
                    st.tmcc_ok,
                    st.decoded,
                    w.losses,
                    w.blocks_lost,
                    st.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
                );
            }
        },
    )?;
    let mut st = stats.borrow_mut();
    st.words = Some(w);
    st.wall_s = t0.elapsed().as_secs_f64();
    Ok(w.symbols as f64 / symbol_rate_hz)
}

/// Lowest burst MER the acquisition accepts, dB (the live BS/CS
/// transponders give 10–15 dB on a 40 Msps probe; a wrong carrier or no
/// signal leaves the bursts unaligned).
pub const MIN_ACQUIRE_MER_DB: f64 = 4.0;

/// Live from a radio's FPGA symbol front end: acquire on a short IQ
/// capture (carrier, trim), configure the front end and decode its words
/// continuously until `seconds` of signal (or Ctrl-C).
#[allow(clippy::too_many_arguments)]
pub fn run_radio_fpga(
    backend: &'static dyn isdb_radio::Backend,
    cfg: &RadioConfig,
    taps: usize,
    seconds: f64,
    carrier_offset_hz: Option<f64>,
    status: bool,
    stop: &std::sync::Arc<AtomicBool>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    let rs = isdbs_phy::frontend::system().symbol_rate_hz;
    let mut acq = None;
    let cap_stop = std::sync::Arc::new(AtomicBool::new(false));
    let stream = crate::symstream::start_radio(backend, cfg, rs, &cap_stop, |x, fs| {
        let p = isdbs_phy::frontend::acquire(x, fs, carrier_offset_hz, Trim::Auto)?;
        if p.pilot_mer_db < MIN_ACQUIRE_MER_DB {
            eprintln!(
                "[rx] carrier {:+.0} Hz gives a burst MER of {:.2} dB, below {MIN_ACQUIRE_MER_DB} dB: no lock",
                p.carrier_offset_hz, p.pilot_mer_db
            );
            return Ok(None);
        }
        let mut d = isdbs_phy::frontend::design(fs, p.carrier_offset_hz);
        d.trim = p.trim;
        d.taps = taps;
        let params = isdb_frontend::Params::design(&d)?;
        acq = Some(p);
        Ok(Some((
            params,
            format!(
                "carrier {:+.0} Hz, trim {:+.3} symbol, burst MER {:.2} dB there; FPGA front end, {taps} taps",
                p.carrier_offset_hz, p.trim, p.pilot_mer_db
            ),
        )))
    })?;
    if let Some(p) = acq {
        stats.carrier_offset_hz = p.carrier_offset_hz;
    }
    stats.symbol_rate_hz = rs;
    crate::symstream::finish_radio(stream, seconds, stop, &cap_stop, |rx| {
        run_stream(rx, rs, status, stop, out, stats)
    })
}

/// Live: contiguous captures of `block_s` seconds from `spec`, decoded
/// while the next is captured, until `seconds` of signal (0: until
/// Ctrl-C).
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
    let t0 = std::time::Instant::now();
    let mut secs = 0.0;
    pipeline(
        |tx| {
            let mut captured = 0.0;
            while !stop.load(Ordering::Relaxed) && (seconds <= 0.0 || captured < seconds) {
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
                captured += x.len() as f64 / fs;
                // each contiguous part of the capture separately
                let mut start = 0usize;
                let mut cuts: Vec<usize> = cap.gaps.iter().map(|g| g.at as usize).collect();
                cuts.push(x.len());
                for c in cuts {
                    if c > start
                        && tx
                            .send(Block {
                                x: x[start..c].to_vec(),
                                fs,
                            })
                            .is_err()
                    {
                        return Ok(());
                    }
                    start = c;
                }
            }
            Ok(())
        },
        carrier_offset_hz,
        out,
        stats,
        |st| {
            secs = st.samples as f64 / rate;
            if status {
                let last = st.ts.values().map(|t| t.uncorrectable).sum::<u64>();
                eprintln!(
                    "[rx] {secs:.2} s of signal after {:.2} s: block {}; {} superframes decoded; packets {} (uncorrectable {last}); MER {}",
                    t0.elapsed().as_secs_f64(),
                    st.blocks,
                    st.decoded,
                    st.ts.values().map(|t| t.packets).sum::<u64>(),
                    st.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
                );
            }
        },
    )?;
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
