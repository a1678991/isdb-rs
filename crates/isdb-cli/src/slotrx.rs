//! ISDB-S3 reception from the receive chain's `Slots` words
//! (`isdb_frontend::slots`, docs/plans/isdbs3-fpga-rx.md "Boundary"): the
//! slot blocks to the per-stream TLV assemblers and outputs of
//! [`crate::rxs3`], the tmcc blocks for the slots' stream ids, the status
//! blocks and the reader's losses to the summary ([`ChainStats`]).
//!
//! Three callers share [`Consumer`]:
//!
//! * `isdb rx --slots FILE` ([`run_file`]): a recorded `Slots` words file
//!   (`--slots-out`, or `isdbs3-phy` `examples/chaincheck`'s `SLOTS_OUT`);
//! * `isdb rx --symbols FILE --chain fixed` ([`run_model`]): the chain's
//!   CPU model (`isdbs3_phy::chain`) over a `Symbols` recording, its
//!   frames written as `Slots` words and read back, as the FPGA would send
//!   them: the bench version of bit-exactness (slow: about 5 s per
//!   second of signal on one thread for the model, plus the LDPC);
//! * `isdb rx --radio … --system isdbs3` on a radio whose FPGA has the
//!   chain ([`run_radio`]): `isdb_radio::Backend::capture_slots`.
//!
//! **Stream ids.** A frame's slots are decoded with the layout of the
//! TMCC decoded `lag` frames earlier (its tmcc block's `applies_from`),
//! so a slot's relative stream and stream id come from the latest tmcc
//! block whose `applies_from` is at or before the slot's frame; a stream
//! joined after that block uses the frame's own TMCC (the host receiver
//! uses each frame's own, the same on a constant allocation).
//!
//! **Block order.** The model sends a frame's slot blocks, then its tmcc
//! and status blocks. The FPGA's decoder returns its results in commit
//! order, and a frame's TMCC codeword is committed a few slots into the
//! next frame, so there frame N's trailing dummy slot blocks, its tmcc and
//! its status block come after frame N + 1's first slot blocks (the HDL's
//! bound: within one frame, `fpga/isdb_fpga/chain/finish.py`). Every block
//! carries its frame counter: the slot blocks wait per frame, and a frame
//! is released (to the assemblers, in frame order) when its own tmcc or
//! status block arrives: either order decodes the same. Slot blocks of a
//! frame that gets neither (the FPGA sends a frame's slots as they are
//! decoded; a frame it drops later, at a loss or a lost sync, never gets
//! its tmcc and status, and the model sends nothing of it) are discarded
//! when a later frame's tmcc or status arrives, a slot block two frames on
//! (a window of one frame), a loss or the end (`orphan_slots`).
//!
//! **Discontinuities.** The assemblers are closed (the TLV streams are
//! discontinuous there, counted) at a loss in the words, and wherever the
//! decoded frames are not consecutive: the chain's undecoded frames after
//! each (re)start (`lag` frames), or a gap in its frame counter.

use crate::rxs3::{Outputs, Stats, close, count_slot, push_slot};
use anyhow::{Context, Result};
use isdb_frontend::control::chain::{carrier_hz, mer_db};
use isdb_frontend::slots::{Block, Event, Reader, SlotBlock, Status};
use isdb_tlv::Assembler;
use isdbs3_core::TmccInfo;
use isdbs3_core::params::{FRAME_SYMBOLS, SYMBOL_RATE};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

/// Seconds of one frame.
pub const FRAME_SECONDS: f64 = FRAME_SYMBOLS as f64 / SYMBOL_RATE;

/// Who decoded the slots ([`Stats::decoded_by`]).
pub const BY_FPGA: &str = "FPGA receive chain (Slots)";
/// See [`BY_FPGA`].
pub const BY_FILE: &str = "recorded Slots words";
/// See [`BY_FPGA`].
pub const BY_MODEL: &str = "CPU model of the FPGA chain (isdbs3_phy::chain)";

/// What the `Slots` words and the chain's status blocks said over a run.
#[derive(Debug, Clone, Default)]
pub struct ChainStats {
    /// Words parsed.
    pub words: u64,
    /// Blocks by type.
    pub slot_blocks: u64,
    /// See `slot_blocks`.
    pub tmcc_blocks: u64,
    /// See `slot_blocks`.
    pub status_blocks: u64,
    /// Dummy slots (no data).
    pub dummy_slots: u64,
    /// Losses in the words (each a discontinuity), blocks missing by the
    /// sequence numbers, words skipped, words of a truncated last block.
    pub losses: u64,
    /// See `losses`.
    pub blocks_lost: u64,
    /// See `losses`.
    pub words_skipped: u64,
    /// See `losses`.
    pub truncated_words: u64,
    /// Frames whose status came without slots (the chain had no layout:
    /// the first `lag` frames after each (re)start).
    pub frames_undecoded: u64,
    /// Frames missing from the frame counter (the chain's own gaps).
    pub frames_missing: u64,
    /// Times the decoded frames were not consecutive and the assemblers
    /// were closed (besides the losses).
    pub discontinuities: u64,
    /// Slots of a frame for which no TMCC was known (not assembled).
    pub unmapped_slots: u64,
    /// Slot blocks of frames that never got their own tmcc or status block
    /// (the FPGA sends a frame's slots as they are decoded, so a frame it
    /// drops later, at a loss or a lost sync, leaves those: discarded, as
    /// the model sends nothing of a dropped frame; or blocks lost).
    pub orphan_slots: u64,
    /// TMCC blocks whose BCH failed.
    pub tmcc_failed: u64,
    /// Frames with the slot sync locked at their end.
    pub locked_frames: u64,
    /// The carrier register's extremes over the run (2⁻³² turn / symbol).
    pub carrier_range: Option<(i32, i32)>,
    /// The first and last frame counters.
    pub first_frame: Option<u32>,
    /// See `first_frame`.
    pub last_frame: Option<u32>,
    /// The last status block.
    pub last_status: Option<Box<Status>>,
    /// The CPU model's restarts (input losses and lost syncs) and frames
    /// cut short by them (not in a `Slots` stream: they show there as
    /// undecoded frames and gaps).
    pub restarts: Option<u64>,
    /// See `restarts`.
    pub frames_dropped: Option<u64>,
    /// The radio's capture: the chain's open, slow loop and counters.
    pub radio: Option<Value>,
}

impl ChainStats {
    /// The carrier register in Hz now and over the run.
    fn carrier(&self) -> Option<(f64, f64, f64)> {
        let s = self.last_status.as_ref()?;
        let (a, b) = self.carrier_range?;
        Some((
            carrier_hz(s.carrier, SYMBOL_RATE),
            carrier_hz(a, SYMBOL_RATE),
            carrier_hz(b, SYMBOL_RATE),
        ))
    }

    /// The `chain` object of `isdb rx --json`.
    pub fn json(&self) -> Value {
        let st = self.last_status.as_ref();
        json!({
            "words": self.words,
            "blocks": {"slot": self.slot_blocks, "tmcc": self.tmcc_blocks, "status": self.status_blocks},
            "dummy_slots": self.dummy_slots,
            "losses": self.losses,
            "blocks_lost": self.blocks_lost,
            "words_skipped": self.words_skipped,
            "truncated_words": self.truncated_words,
            "frames_undecoded": self.frames_undecoded,
            "frames_missing": self.frames_missing,
            "discontinuities": self.discontinuities,
            "unmapped_slots": self.unmapped_slots,
            "orphan_slots": self.orphan_slots,
            "tmcc_failed": self.tmcc_failed,
            "locked_frames": self.locked_frames,
            "first_frame": self.first_frame,
            "last_frame": self.last_frame,
            "restarts": self.restarts,
            "frames_dropped": self.frames_dropped,
            "carrier_hz": self.carrier().map(|c| c.0),
            "carrier_range_hz": self.carrier().map(|c| [c.1, c.2]),
            "status": st.map(|s| status_json(s)),
            "radio": self.radio,
        })
    }

    /// The summary lines (after `rxs3::report`'s).
    pub fn report(&self) {
        info!(
            target: "isdb::rx",
            words = self.words,
            slot_blocks = self.slot_blocks,
            tmcc_blocks = self.tmcc_blocks,
            status_blocks = self.status_blocks,
            losses = self.losses,
            blocks_lost = self.blocks_lost,
            words_skipped = self.words_skipped,
            "slots: {} words in {} slot, {} tmcc and {} status blocks; {} loss(es) ({} blocks missing, {} words skipped){}",
            self.words,
            self.slot_blocks,
            self.tmcc_blocks,
            self.status_blocks,
            self.losses,
            self.blocks_lost,
            self.words_skipped,
            if self.truncated_words > 0 {
                format!("; {} words of a truncated last block", self.truncated_words)
            } else {
                String::new()
            }
        );
        info!(
            target: "isdb::rx",
            frames_undecoded = self.frames_undecoded,
            frames_missing = self.frames_missing,
            discontinuities = self.discontinuities,
            unmapped_slots = self.unmapped_slots,
            "chain: {} frame(s) without a layout (the first after each (re)start), {} missing from the frame counter; {} discontinuit(ies) in the TLV streams besides the losses{}{}",
            self.frames_undecoded,
            self.frames_missing,
            self.discontinuities,
            if self.unmapped_slots > 0 {
                format!("; {} slots without a known TMCC, not assembled", self.unmapped_slots)
            } else {
                String::new()
            },
            match (self.restarts, self.frames_dropped) {
                (Some(r), Some(d)) => format!("; the model restarted {r} time(s), {d} frame(s) cut short"),
                _ => String::new(),
            }
        );
        if let (Some(s), Some((now, lo, hi))) = (&self.last_status, self.carrier()) {
            info!(
                target: "isdb::rx",
                carrier_hz = now,
                carrier_min_hz = lo,
                carrier_max_hz = hi,
                n0 = s.n0,
                mer_db = mer_db(s.n0),
                taps_from = s.taps_from.map(i64::from),
                "chain status at frame {}: carrier register {:+.1} Hz ({:+.1} … {:+.1} Hz over the run; the chain tracks it), n0 {} (MER {:.2} dB), taps {} (centre {:?}), sync {} at offset {} (peak/mean {:.1}, {} stray, {} missed markers); counters: {} frames ({} undecoded), {} slots, LDPC failed {}, BCH failed {}, mean {:.3} iterations",
                s.frame,
                now,
                lo,
                hi,
                s.n0,
                mer_db(s.n0),
                s.taps_from
                    .map_or_else(|| "as started (identity or the host's)".into(), |f| format!("solved from frame {f}")),
                s.taps[5],
                if s.locked { "locked" } else { "searching" },
                s.offset,
                peak_to_mean(s),
                s.stray_markers,
                s.missed_markers,
                s.frames,
                s.frames_undecoded,
                s.slots,
                s.ldpc_failed,
                s.bch_failed,
                s.iterations as f64 / f64::from(s.slots.saturating_sub(s.ldpc_failed).max(1))
            );
        }
    }
}

/// The last status block in `--json`.
fn status_json(s: &Status) -> Value {
    let sync = json!({
        "locked": s.locked,
        "offset": s.offset,
        "ssync_parity": s.ssync_parity,
        "peak": s.peak,
        "peak_pos": s.peak_pos,
        "peak_to_mean": peak_to_mean(s),
        "stray_markers": s.stray_markers,
        "missed_markers": s.missed_markers,
    });
    let counters = json!({
        "frames": s.frames,
        "frames_undecoded": s.frames_undecoded,
        "tmcc_failed": s.tmcc_failed,
        "slots": s.slots,
        "ldpc_failed": s.ldpc_failed,
        "bch_failed": s.bch_failed,
        "iterations": s.iterations,
        "mean_iterations": s.iterations as f64 / f64::from(s.slots.saturating_sub(s.ldpc_failed).max(1)),
    });
    json!({
        "frame": s.frame,
        "carrier_register": s.carrier,
        "carrier_hz": carrier_hz(s.carrier, SYMBOL_RATE),
        "agc": s.agc,
        "n0_q20": s.n0,
        "mer_db": mer_db(s.n0),
        "llr_scale": s.llr_scale,
        "learned_from": s.learned_from,
        "taps": s.taps,
        "taps_from": s.taps_from,
        "sums_frame": s.sums_frame,
        "constellation_points": s.constellation.iter().map(Vec::len).collect::<Vec<_>>(),
        "sync": sync,
        "counters": counters,
    })
}

/// The sync statistic: the period's peak over its mean accumulator value.
fn peak_to_mean(s: &Status) -> f64 {
    f64::from(s.peak) * 9_296.0 / (s.sum.max(1) as f64)
}

/// The `Slots` words of a run through the TLV assemblers (module
/// documentation).
#[derive(Debug, Default)]
pub struct Consumer {
    asm: BTreeMap<u16, Assembler>,
    /// TMCCs by the first frame their layout applies to.
    in_force: BTreeMap<u32, TmccInfo>,
    /// The last frame's own TMCC.
    own: Option<(u32, TmccInfo)>,
    /// The slot blocks of the frames being received (at most two: the
    /// window of one frame, module documentation "Block order").
    pending: BTreeMap<u32, Vec<SlotBlock>>,
    last_decoded: Option<u32>,
    started: bool,
}

impl Consumer {
    /// A consumer at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    fn chain(stats: &mut Stats) -> &mut ChainStats {
        stats.chain.get_or_insert_with(ChainStats::default)
    }

    /// One reader event.
    ///
    /// # Errors
    ///
    /// Writing an output.
    pub fn event(&mut self, e: Event, out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        match e {
            Event::Block { block, .. } => {
                self.started = true;
                self.block(block, out, stats)
            }
            Event::Loss { blocks, words } => {
                let c = Self::chain(stats);
                c.blocks_lost += blocks;
                c.words_skipped += words;
                if !self.started {
                    // joined mid-block: nothing lost from the output yet
                    return Ok(());
                }
                c.losses += 1;
                warn!(
                    target: "isdb::rx",
                    blocks_lost = blocks,
                    words_skipped = words,
                    "slots loss: {blocks} block(s) missing, {words} words skipped: a discontinuity in the TLV streams"
                );
                self.flush_all(out, stats)?;
                self.end_stretch(stats);
                Ok(())
            }
            Event::Truncated { words } => {
                Self::chain(stats).truncated_words += words;
                Ok(())
            }
        }
    }

    /// Close the assemblers: the streams are discontinuous here.
    fn end_stretch(&mut self, stats: &mut Stats) {
        close(std::mem::take(&mut self.asm), stats);
        self.last_decoded = None;
    }

    fn block(&mut self, b: Block, out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        match b {
            Block::Slot(s) => {
                let c = Self::chain(stats);
                c.slot_blocks += 1;
                c.dummy_slots += u64::from(s.dummy);
                // a frame two before this one has had its window: release it
                // (its tmcc and status blocks were lost or never sent)
                self.flush_before(s.frame.wrapping_sub(1), out, stats)?;
                self.pending.entry(s.frame).or_default().push(s);
            }
            Block::Tmcc(t) => {
                let c = Self::chain(stats);
                c.tmcc_blocks += 1;
                if t.bch.is_some() && t.bits.len() == isdb_frontend::slots::TMCC_BITS {
                    let info = TmccInfo::parse(&t.bits);
                    stats.tmcc_ok += 1;
                    stats.tmcc = Some(info.clone());
                    self.in_force.insert(t.applies_from, info.clone());
                    // a few frames of history are enough (lag ≤ 2)
                    while self.in_force.len() > 8 {
                        self.in_force.pop_first();
                    }
                    self.own = Some((t.frame, info));
                } else {
                    c.tmcc_failed += 1;
                }
                // the frame's slots came before its TMCC (and any earlier
                // frame's still waiting is released first)
                self.flush_through(t.frame, out, stats)?;
            }
            Block::Status(s) => {
                self.flush_through(s.frame, out, stats)?;
                self.status(&s, stats);
            }
        }
        Ok(())
    }

    fn status(&mut self, s: &Status, stats: &mut Stats) {
        stats.frames += 1;
        let decoded = self.last_decoded == Some(s.frame);
        let c = Self::chain(stats);
        c.status_blocks += 1;
        if let Some(l) = c.last_frame {
            let gap = s.frame.wrapping_sub(l);
            if gap > 1 && gap < u32::MAX / 2 {
                c.frames_missing += u64::from(gap - 1);
            }
        }
        c.first_frame.get_or_insert(s.frame);
        c.last_frame = Some(s.frame);
        c.locked_frames += u64::from(s.locked);
        c.carrier_range = Some(c.carrier_range.map_or((s.carrier, s.carrier), |(a, b)| {
            (a.min(s.carrier), b.max(s.carrier))
        }));
        c.last_status = Some(Box::new(s.clone()));
        if decoded {
            stats.add_mer(mer_db(s.n0));
        } else {
            c.frames_undecoded += 1;
        }
        // the chain's carrier register is the drift it tracks in the
        // symbols: the host receiver's `tracked` figures
        let hz = carrier_hz(s.carrier, SYMBOL_RATE);
        stats.tracked_hz = hz;
        stats.tracked_range_hz = if stats.frames == 1 {
            (hz, hz)
        } else {
            (
                stats.tracked_range_hz.0.min(hz),
                stats.tracked_range_hz.1.max(hz),
            )
        };
    }

    /// `frame`'s tmcc or status block: the frames waiting before it are
    /// orphans (frame counters compared within a window: a frame "before"
    /// is less than half the counter's range behind), its own slots go to
    /// the assemblers.
    fn flush_through(&mut self, frame: u32, out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        self.flush_before(frame, out, stats)?;
        if let Some(slots) = self.pending.remove(&frame) {
            self.flush(frame, slots, out, stats)?;
        }
        Ok(())
    }

    /// The waiting frames before `frame`: orphans (no tmcc or status of
    /// their own), discarded.
    fn flush_before(&mut self, frame: u32, _out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        while let Some(&f) = self.pending.keys().next()
            && frame.wrapping_sub(f).cast_signed() > 0
        {
            let slots = self.pending.remove(&f).unwrap_or_default();
            self.orphans(f, &slots, stats);
        }
        Ok(())
    }

    /// Every waiting frame (a loss, the end): orphans.
    fn flush_all(&mut self, _out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        while let Some((f, slots)) = self.pending.pop_first() {
            self.orphans(f, &slots, stats);
        }
        Ok(())
    }

    fn orphans(&mut self, frame: u32, slots: &[SlotBlock], stats: &mut Stats) {
        let n = slots.len() as u64;
        Self::chain(stats).orphan_slots += n;
        warn!(
            target: "isdb::rx",
            frame,
            blocks = n,
            "{n} slot block(s) of frame {frame} without its tmcc and status block (a frame dropped after its first slots, or blocks lost): discarded"
        );
    }

    /// A frame's slots to the assemblers.
    fn flush(
        &mut self,
        frame: u32,
        slots: Vec<SlotBlock>,
        out: &mut Outputs,
        stats: &mut Stats,
    ) -> Result<()> {
        let info = self
            .in_force
            .range(..=frame)
            .next_back()
            .map(|(_, t)| t)
            .or(self.own.as_ref().filter(|o| o.0 == frame).map(|o| &o.1))
            .cloned();
        let Some(info) = info else {
            let n = slots.iter().filter(|s| !s.dummy).count() as u64;
            Self::chain(stats).unmapped_slots += n;
            warn!(
                target: "isdb::rx",
                frame,
                slots = n,
                "slots of frame {frame} arrived without a TMCC for their layout: {n} slots not assembled"
            );
            self.end_stretch(stats);
            return Ok(());
        };
        if self
            .last_decoded
            .is_some_and(|l| frame != l.wrapping_add(1))
        {
            Self::chain(stats).discontinuities += 1;
            self.end_stretch(stats);
        }
        for s in slots.iter().filter(|s| !s.dummy) {
            let Some((slot, head, data)) = isdbs3_phy::chain::out::slot_data(s) else {
                continue;
            };
            count_slot(
                stats,
                s.converged.then_some(usize::from(s.iterations)),
                s.bch.map(usize::from),
            );
            let rel = info.slot_stream[usize::from(slot) % info.slot_stream.len()];
            let id = info.stream_id[usize::from(rel & 0x0F)];
            push_slot(id, data.as_deref(), head, &mut self.asm, out, stats)?;
        }
        self.last_decoded = Some(frame);
        Ok(())
    }

    /// The end of the stream: the last frame's slots and the assemblers.
    ///
    /// # Errors
    ///
    /// Writing an output.
    pub fn finish(&mut self, out: &mut Outputs, stats: &mut Stats) -> Result<()> {
        self.flush_all(out, stats)?;
        self.end_stretch(stats);
        Ok(())
    }
}

/// Seconds of signal the stream covered: its frame counter's span.
fn seconds(stats: &Stats) -> f64 {
    stats
        .chain
        .as_ref()
        .map_or(0.0, |c| match (c.first_frame, c.last_frame) {
            (Some(a), Some(b)) => f64::from(b.wrapping_sub(a).wrapping_add(1)) * FRAME_SECONDS,
            _ => 0.0,
        })
}

/// A status line every second of signal.
struct StatusLines {
    on: bool,
    next: f64,
    t0: std::time::Instant,
}

impl StatusLines {
    fn new(on: bool) -> Self {
        Self {
            on,
            next: 1.0,
            t0: std::time::Instant::now(),
        }
    }

    fn after(&mut self, stats: &Stats) {
        let secs = seconds(stats);
        if !self.on || secs < self.next {
            return;
        }
        self.next = secs.floor() + 1.0;
        let c = stats.chain.as_ref();
        info!(
            target: "isdb::rx",
            seconds = secs,
            wall_s = self.t0.elapsed().as_secs_f64(),
            frames = stats.frames,
            slots = stats.slots,
            ldpc_failed = stats.ldpc_failed,
            bch_failed = stats.bch_failed,
            losses = c.map_or(0, |c| c.losses),
            mer_db = stats.mer_db().unwrap_or(f64::NAN),
            "{secs:.2} s of signal after {:.2} s: {} frames ({} undecoded); {} slots, LDPC failed {}, BCH failed {}; losses {}; MER {}; carrier {:+.1} Hz",
            self.t0.elapsed().as_secs_f64(),
            stats.frames,
            c.map_or(0, |c| c.frames_undecoded),
            stats.slots,
            stats.ldpc_failed,
            stats.bch_failed,
            c.map_or(0, |c| c.losses),
            stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB")),
            stats.tracked_hz
        );
    }
}

/// Words from `bytes` (a file's or a radio's) through a [`Consumer`] until
/// the source ends or `stop`; `seconds` (> 0) bounds the signal (frames)
/// and sets `enough` when reached. Returns the seconds of signal.
#[allow(clippy::too_many_arguments)]
fn consume(
    bytes: crate::symstream::Bytes,
    seconds_max: f64,
    status: bool,
    stop: &AtomicBool,
    enough: Option<&AtomicBool>,
    mut tee: Option<&mut dyn Write>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    let t0 = std::time::Instant::now();
    let mut reader = Reader::new();
    let mut ev = Vec::new();
    let mut c = Consumer::new();
    let mut lines = StatusLines::new(status);
    Consumer::chain(stats);
    for chunk in bytes {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(t) = tee.as_mut() {
            t.write_all(&chunk).context("write --slots-out")?;
        }
        Consumer::chain(stats).words += (chunk.len() / 2) as u64;
        reader.feed_bytes(&chunk, &mut ev);
        for e in ev.drain(..) {
            c.event(e, out, stats)?;
        }
        stats.wall_s = t0.elapsed().as_secs_f64();
        lines.after(stats);
        if seconds_max > 0.0 && seconds(stats) >= seconds_max {
            if let Some(e) = enough {
                e.store(true, Ordering::SeqCst);
            }
            break;
        }
    }
    reader.finish(&mut ev);
    for e in ev.drain(..) {
        c.event(e, out, stats)?;
    }
    c.finish(out, stats)?;
    stats.wall_s = t0.elapsed().as_secs_f64();
    Ok(seconds(stats))
}

/// `isdb rx --slots FILE`: a recorded `Slots` words file.
///
/// # Errors
///
/// Reading the file or writing an output.
pub fn run_file(
    path: &str,
    seconds_max: f64,
    status: bool,
    stop: &AtomicBool,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    stats.decoded_by = BY_FILE;
    let bytes = crate::symstream::file_bytes(std::path::Path::new(path))?;
    consume(bytes, seconds_max, status, stop, None, None, out, stats)
}

/// The `OUT.json` sidecar of a `Slots` words file.
pub fn sidecar(path: &std::path::Path, source: &str, extra: Value, overwrite: bool) -> Result<()> {
    let mut p = path.as_os_str().to_owned();
    p.push(".json");
    let mut f = crate::capture::create(std::path::Path::new(&p), overwrite)?;
    let mut meta = json!({
        "format": "isdb-rs Slots",
        "format_version": isdb_frontend::slots::SLOTS_VERSION,
        "source": source,
        "system": isdbs3_phy::frontend::system().name,
        "symbol_rate_hz": SYMBOL_RATE,
    });
    if let (Some(m), Value::Object(e)) = (meta.as_object_mut(), extra) {
        m.extend(e);
    }
    f.write_all(serde_json::to_string_pretty(&meta)?.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

/// `isdb rx --symbols FILE --chain fixed`: the chain's CPU model over a
/// `Symbols` words file, each frame's `Slots` words read back by the
/// [`Consumer`] (and written to `slots_out`).
///
/// # Errors
///
/// Reading the file or writing an output.
pub fn run_model(
    path: &str,
    seconds_max: f64,
    status: bool,
    stop: &AtomicBool,
    mut slots_out: Option<&mut dyn Write>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    use isdbs3_phy::chain::input::{Input, for_each};
    use isdbs3_phy::chain::{Chain, ChainOptions, Output};
    stats.decoded_by = BY_MODEL;
    let t0 = std::time::Instant::now();
    let f = std::fs::File::open(path).with_context(|| format!("open {path}"))?;
    let opt = ChainOptions::default();
    let lag = opt.layout_lag;
    let mut chain = Chain::new(opt);
    let mut writer = isdb_frontend::slots::Writer::new();
    let mut reader = Reader::new();
    let mut c = Consumer::new();
    let mut lines = StatusLines::new(status);
    let (mut outs, mut words, mut ev) = (Vec::new(), Vec::new(), Vec::new());
    let mut w = crate::symstream::WordStats::default();
    let (mut restarts, mut dropped, mut block_len) = (0u64, 0u64, 0u64);
    let mut err: Option<anyhow::Error> = None;
    Consumer::chain(stats);
    let mut back_s = 0.0;
    for_each(f, |item| {
        let t = std::time::Instant::now();
        match item {
            Input::Loss(n) => {
                w.losses += 1;
                w.blocks_lost += n / block_len.max(1);
                chain.loss(n, &mut outs);
            }
            Input::Symbols(s, gain) => {
                w.blocks += 1;
                w.symbols += s.len() as u64;
                block_len = s.len() as u64;
                chain.set_agc(gain);
                for x in s {
                    chain.push(x, &mut outs);
                }
            }
        }
        back_s += t.elapsed().as_secs_f64();
        for o in outs.drain(..) {
            match o {
                Output::Frame(fr) => {
                    isdbs3_phy::chain::out::write_frame(&fr, lag, &mut writer, &mut words);
                }
                Output::Dropped { .. } => dropped += 1,
                Output::Restart { sync_lost } => {
                    restarts += 1;
                    info!(
                        target: "isdb::rx",
                        seconds = chain.symbols() as f64 / SYMBOL_RATE,
                        sync_lost,
                        "{:.3} s: the chain restarts ({})",
                        chain.symbols() as f64 / SYMBOL_RATE,
                        if sync_lost { "sync lost" } else { "input loss" }
                    );
                }
                Output::Head(_) | Output::Symbol(_) | Output::Carrier(_) => {}
            }
        }
        if words.is_empty() {
            return !stop.load(Ordering::Relaxed);
        }
        let r = (|| -> Result<()> {
            if let Some(o) = slots_out.as_mut() {
                let b: Vec<u8> = words.iter().flat_map(|x| x.to_le_bytes()).collect();
                o.write_all(&b).context("write --slots-out")?;
            }
            Consumer::chain(stats).words += words.len() as u64;
            reader.feed(&words, &mut ev);
            words.clear();
            for e in ev.drain(..) {
                c.event(e, out, stats)?;
            }
            Ok(())
        })();
        if let Err(e) = r {
            err = Some(e);
            return false;
        }
        stats.back_s = back_s;
        stats.wall_s = t0.elapsed().as_secs_f64();
        stats.words = Some(w);
        lines.after(stats);
        let secs = chain.symbols() as f64 / SYMBOL_RATE;
        !(stop.load(Ordering::Relaxed) || seconds_max > 0.0 && secs >= seconds_max)
    })
    .with_context(|| format!("read {path}"))?;
    if let Some(e) = err {
        return Err(e);
    }
    reader.finish(&mut ev);
    for e in ev.drain(..) {
        c.event(e, out, stats)?;
    }
    c.finish(out, stats)?;
    let ch = Consumer::chain(stats);
    ch.restarts = Some(restarts);
    ch.frames_dropped = Some(dropped);
    w.stopped = stop.load(Ordering::Relaxed);
    stats.words = Some(w);
    stats.back_s = back_s;
    stats.wall_s = t0.elapsed().as_secs_f64();
    let secs = chain.symbols() as f64 / SYMBOL_RATE;
    info!(
        target: "isdb::rx",
        seconds = secs,
        model_s = back_s,
        "the chain model took {back_s:.1} s for {secs:.3} s of signal ({:.2} s per s, LDPC on all cores included)",
        back_s / secs.max(1e-9)
    );
    Ok(secs)
}

/// Live from a radio whose FPGA has the chain: acquire on a short IQ
/// capture (as for the symbol front end), configure the front end and
/// the chain (`Backend::capture_slots`, the slow loop on its timer
/// thread), and decode its `Slots` words until `seconds` of signal (or
/// Ctrl-C); with `slots_out` the words are recorded too.
///
/// # Errors
///
/// The acquisition, the capture's set-up, or writing an output.
#[allow(clippy::too_many_arguments)]
pub fn run_radio(
    backend: &'static dyn isdb_radio::Backend,
    cfg: &isdb_radio::RadioConfig,
    taps: usize,
    seconds_max: f64,
    carrier_offset_hz: Option<f64>,
    status: bool,
    stop: &std::sync::Arc<AtomicBool>,
    slots_out: Option<&mut dyn Write>,
    out: &mut Outputs,
    stats: &mut Stats,
) -> Result<f64> {
    use isdbs3_phy::frontend::Trim;
    stats.decoded_by = BY_FPGA;
    let rs = isdbs3_phy::frontend::system().symbol_rate_hz;
    let mut acq = None;
    let cap_stop = std::sync::Arc::new(AtomicBool::new(false));
    let init = isdb_frontend::control::chain::ChainInit::default();
    let slow = isdb_radio::chain::SlowLoopOptions::new(isdbs3_phy::chain::eq::solve_training, rs);
    let stream = crate::symstream::start_radio_with(
        backend,
        cfg,
        rs,
        &cap_stop,
        |x, fs| {
            let p = isdbs3_phy::frontend::acquire(x, fs, carrier_offset_hz, Trim::Auto)?;
            if p.sync_snr_db < crate::rxs3::MIN_ACQUIRE_SNR_DB {
                warn!(
                    target: "isdb::rx",
                    carrier_offset_hz = p.carrier_offset_hz,
                    sync_snr_db = p.sync_snr_db,
                    "carrier {:+.0} Hz gives a sync Es/N0 of {:.2} dB, below {} dB: no lock",
                    p.carrier_offset_hz,
                    p.sync_snr_db,
                    crate::rxs3::MIN_ACQUIRE_SNR_DB
                );
                return Ok(None);
            }
            let mut d = isdbs3_phy::frontend::design(fs, p.carrier_offset_hz);
            d.trim = p.trim;
            d.taps = taps;
            let params = isdb_frontend::Params::design(&d)?;
            acq = Some(p);
            Ok(Some((
                params,
                format!(
                    "carrier {:+.0} Hz, trim {:+.3} symbol, sync Es/N0 {:.2} dB there; FPGA front end ({taps} taps) and receive chain",
                    p.carrier_offset_hz, p.trim, p.sync_snr_db
                ),
            )))
        },
        move |b, cfg, p, w, stop| b.capture_slots(cfg, p, &init, Some(slow), u64::MAX, w, stop),
    )?;
    if let Some(p) = acq {
        stats.carrier_offset_hz = p.carrier_offset_hz;
        stats.trim = p.trim;
    }
    // the decoder ends the capture once `seconds` of frames are in
    let enough = std::sync::Arc::clone(&cap_stop);
    let (secs, capture) = crate::symstream::finish_radio_with(
        stream,
        0.0,
        stop,
        &cap_stop,
        |c: &isdb_radio::SlotsCapture| &c.stream,
        |rx| {
            consume(
                rx,
                seconds_max,
                status,
                stop,
                Some(&enough),
                slots_out,
                out,
                stats,
            )
        },
    );
    let secs = secs?;
    if let Some(c) = capture {
        let ch = Consumer::chain(stats);
        ch.radio = Some(json!({
            "open": c.open.describe(),
            "features": c.open.features,
            "loaded": c.open.loaded,
            "slow_loop": c.slow_loop.as_ref().map(|l| json!({
                "ticks": l.ticks,
                "solves": l.solves,
                "singular": l.singular,
                "solves_rejected": l.solves_rejected,
                "torn_reads": l.torn,
                "unsettled_reads": l.unsettled,
                "words_read": l.words_read,
                "rereads": l.rereads,
                "captures_rejected": l.captures_rejected,
                "captures_rejected_by": l.captures_rejected_by,
                "last_frame": l.last_frame,
                "last_taps": l.last_taps,
                "carrier_register": l.carrier,
                "error": l.error,
            })),
            "counters": c.counters.map(|k| json!({
                "frames": k.frames,
                "frames_undecoded": k.frames_undecoded,
                "tmcc_failed": k.tmcc_failed,
                "slots": k.slots,
                "ldpc_failed": k.ldpc_failed,
                "bch_failed": k.bch_failed,
                "iterations": k.iterations,
            })),
            "words": c.stream.words,
            // not polled on the Slots path (#27): the blocks' sequence numbers
            // count the losses (chain.blocks_lost)
            "overflows": Value::Null,
        }));
        if let Some(l) = &c.slow_loop {
            info!(
                target: "isdb::rx",
                solves = l.solves,
                solves_rejected = l.solves_rejected,
                ticks = l.ticks,
                captures_rejected = l.captures_rejected,
                rereads = l.rereads,
                "the equaliser's slow loop wrote taps {} time(s) in {} ticks ({} solve(s) rejected by its gate, {} capture(s) by the checks on the sums{}; {} of {} register reads read again after a foreign value, {} torn){}",
                l.solves,
                l.ticks,
                l.solves_rejected,
                l.captures_rejected,
                if l.captures_rejected_by.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": {}",
                        l.captures_rejected_by
                            .iter()
                            .map(|(k, v)| format!("{v} {k}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
                l.rereads,
                l.words_read,
                l.torn,
                l.error
                    .as_ref()
                    .map_or(String::new(), |e| format!("; it stopped on: {e}"))
            );
        }
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbs3_phy::chain::synth::{SynthOptions, generate};
    use isdbs3_phy::chain::{Chain, ChainOptions, Output};

    /// A synthetic stream through the CPU chain, as `Slots` words.
    fn slots_words(frames: usize, loss_at: Option<usize>) -> (Vec<u16>, usize) {
        let o = SynthOptions::default();
        let s = generate(&o, frames);
        let opt = ChainOptions::default();
        let lag = opt.layout_lag;
        let mut chain = Chain::new(opt);
        let mut w = isdb_frontend::slots::Writer::new();
        let (mut out, mut words) = (Vec::new(), Vec::new());
        let mut starts = Vec::new();
        for &x in &s.symbols {
            chain.push(x, &mut out);
            for o in out.drain(..) {
                if let Output::Frame(f) = o {
                    starts.push(words.len());
                    isdbs3_phy::chain::out::write_frame(&f, lag, &mut w, &mut words);
                }
            }
        }
        if let Some(k) = loss_at {
            // drop frame k's first slot block (a loss in the words)
            let a = starts[k];
            let h = isdb_frontend::slots::Header::parse(&words[a..]).unwrap();
            words.drain(a..a + h.block_words());
        }
        (words, starts.len())
    }

    fn consume_words(words: &[u16], split: usize) -> (Stats, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!(
            "isdb-slotrx-{}-{split}-{}",
            std::process::id(),
            words.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("{tlv_id}.tlv").display().to_string();
        let mut out = Outputs::new(Some(&path), &[]).unwrap();
        let mut stats = Stats::default();
        let mut c = Consumer::new();
        let mut r = Reader::new();
        let mut ev = Vec::new();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        for ch in bytes.chunks(split) {
            r.feed_bytes(ch, &mut ev);
            for e in ev.drain(..) {
                c.event(e, &mut out, &mut stats).unwrap();
            }
        }
        r.finish(&mut ev);
        for e in ev.drain(..) {
            c.event(e, &mut out, &mut stats).unwrap();
        }
        c.finish(&mut out, &mut stats).unwrap();
        out.flush().unwrap();
        drop(out);
        // the background writers have finished once the outputs are gone
        let tlv = std::fs::read(dir.join("b070.tlv")).unwrap_or_default();
        std::fs::remove_dir_all(&dir).unwrap();
        (stats, tlv)
    }

    /// Valid slots of a 16APSK frame (Table 3.2-2: 96 of 120, the rest
    /// dummies).
    const VALID: u64 = 96;

    /// The synthetic stream (one TLV stream id B070h on all 120 slots of
    /// 16APSK 93/120, random data): every decoded frame's slots counted,
    /// the first `lag` frames undecoded, the stream id from the TMCC, the
    /// status block's counters and carrier in the summary; any split of
    /// the bytes gives the same output.
    #[test]
    fn a_synthetic_chain_stream_through_the_consumer() {
        let (words, n) = slots_words(7, None);
        assert!(n >= 4, "{n} frames out of the chain");
        let n = u64::try_from(n).unwrap();
        let decoded = n - 2;
        let (st, tlv) = consume_words(&words, 65_536);
        let c = st.chain.as_ref().unwrap();
        assert_eq!(st.frames, n);
        assert_eq!(c.frames_undecoded, 2, "lag 2");
        assert_eq!(st.slots, decoded * VALID);
        assert_eq!((st.ldpc_failed, st.bch_failed), (0, 0));
        assert_eq!(st.tmcc_ok, n);
        assert_eq!((c.losses, c.discontinuities, c.unmapped_slots), (0, 0, 0));
        // a slot block per slot, dummies included
        assert_eq!(c.slot_blocks, decoded * 120);
        assert_eq!(c.dummy_slots, decoded * (120 - VALID));
        assert_eq!((c.tmcc_blocks, c.status_blocks), (n, n));
        assert!(st.streams.contains_key(&0xB070), "{:?}", st.streams.keys());
        // the TMCC assigns all 120 slots to the stream (the dummies too)
        assert_eq!(st.streams[&0xB070].2, 120, "slots per frame");
        assert!(st.mer_db().is_some());
        let s = c.last_status.as_ref().unwrap();
        assert_eq!(
            (u64::from(s.frames), u64::from(s.slots)),
            (n, decoded * VALID)
        );
        // any split of the bytes: the same output
        let (st2, tlv2) = consume_words(&words, 777);
        assert_eq!(tlv, tlv2);
        assert_eq!(st2.slots, st.slots);
        let j = st.json();
        assert_eq!(j["decoded_by"], crate::rxs3::DECODED_BY_HOST);
        assert_eq!(j["chain"]["frames_undecoded"], 2);
        assert_eq!(j["chain"]["status"]["counters"]["slots"], decoded * VALID);
    }

    /// The FPGA's block order (its decoder's commit order): from the
    /// third frame on, a frame's last slot blocks, its tmcc and its status
    /// block come after the next frame's first four slot blocks (the
    /// sequence numbers in the order sent). The same slots, the same TLV
    /// bytes, the same summary as the model's order.
    #[test]
    fn the_fpga_block_order_decodes_the_same() {
        let o = SynthOptions::default();
        let s = generate(&o, 7);
        let opt = ChainOptions::default();
        let lag = opt.layout_lag;
        let mut chain = Chain::new(opt);
        let mut frames: Vec<Vec<Block>> = Vec::new();
        let mut out = Vec::new();
        for &x in &s.symbols {
            chain.push(x, &mut out);
            for o in out.drain(..) {
                if let Output::Frame(f) = o {
                    frames.push(isdbs3_phy::chain::out::blocks(&f, lag));
                }
            }
        }
        assert!(frames.len() >= 5);
        let write = |order: &[Block]| {
            let mut w = isdb_frontend::slots::Writer::new();
            let mut words = Vec::new();
            for b in order {
                w.write(b, &mut words);
            }
            words
        };
        let in_order: Vec<Block> = frames.iter().flatten().cloned().collect();
        // frame k's tail (its last 3 slot blocks, if any, its tmcc and
        // status) after frame k + 1's first 4 slot blocks
        let mut moved: Vec<Block> = Vec::new();
        let mut tail: Vec<Block> = Vec::new();
        let mut late = 0;
        for f in &frames {
            let (slots, rest): (Vec<&Block>, Vec<&Block>) =
                f.iter().partition(|b| matches!(b, Block::Slot(_)));
            let head = slots.len().min(4);
            moved.extend(slots[..head].iter().copied().cloned());
            moved.append(&mut tail);
            let cut = slots.len().saturating_sub(3).max(head);
            moved.extend(slots[head..cut].iter().copied().cloned());
            tail.extend(slots[cut..].iter().copied().cloned());
            tail.extend(rest.into_iter().cloned());
            late += usize::from(head > 0);
        }
        moved.append(&mut tail);
        assert!(late >= 3, "{late} frames reordered");
        assert_ne!(moved, in_order);
        assert_eq!(moved.len(), in_order.len());
        let (a, tlv_a) = consume_words(&write(&in_order), 4096);
        let (b, tlv_b) = consume_words(&write(&moved), 4096);
        assert_eq!(tlv_a, tlv_b);
        assert!(a.slots > 0);
        assert_eq!(
            (a.frames, a.slots, a.tmcc_ok),
            (b.frames, b.slots, b.tmcc_ok)
        );
        assert_eq!(
            a.streams.get(&0xB070).map(|x| x.2),
            b.streams.get(&0xB070).map(|x| x.2)
        );
        let (ca, cb) = (a.chain.as_ref().unwrap(), b.chain.as_ref().unwrap());
        assert_eq!(
            (ca.discontinuities, ca.unmapped_slots, ca.frames_undecoded),
            (cb.discontinuities, cb.unmapped_slots, cb.frames_undecoded)
        );
        assert_eq!((cb.discontinuities, cb.unmapped_slots), (0, 0));
        // a frame the FPGA dropped after decoding its first slots: its slot
        // blocks, no tmcc or status; discarded, the rest as before
        let last = frames.iter().flatten().map(Block::frame).max().unwrap();
        let mut with_orphans = moved.clone();
        let Some(Block::Status(st)) = with_orphans.pop() else {
            panic!("the last block is a status")
        };
        let orphans: Vec<Block> = frames[frames.len() - 2]
            .iter()
            .filter_map(|b| match b {
                Block::Slot(x) => Some(Block::Slot(SlotBlock {
                    frame: last + 1,
                    ..x.clone()
                })),
                _ => None,
            })
            .take(10)
            .collect();
        with_orphans.extend(orphans);
        with_orphans.push(Block::Status(st));
        let (c, tlv_c) = consume_words(&write(&with_orphans), 4096);
        assert_eq!(tlv_c, tlv_a);
        assert_eq!((c.frames, c.slots), (a.frames, a.slots));
        assert_eq!(c.chain.as_ref().unwrap().orphan_slots, 10);
    }

    /// A lost slot block is a loss: counted, the assemblers closed, the
    /// rest of the frame still decoded.
    #[test]
    fn a_loss_in_the_words_is_a_discontinuity() {
        let (whole, n) = slots_words(7, None);
        let (words, _) = slots_words(7, Some(n - 1));
        assert!(words.len() < whole.len());
        let (st, _) = consume_words(&words, 4096);
        let c = st.chain.as_ref().unwrap();
        assert_eq!(c.losses, 1);
        assert_eq!(c.blocks_lost, 1);
        assert_eq!(st.slots, (u64::try_from(n).unwrap() - 2) * VALID - 1);
    }
}
