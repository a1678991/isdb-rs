//! Frame producer: a worker thread that assembles each OFDM frame's packets
//! from the input, modulates it with [`isdbt_phy::Transmitter`] and queues
//! the baseband on a bounded channel ahead of the sink.
//!
//! ```text
//! reader thread(s) ──packets──► producer thread ──frames (bounded)──► sink loop
//! ```
//!
//! On-air sequence: `lead_frames` null frames, the data frames, then (unless
//! stopped) [`isdbt_phy::Transmitter::flush_frames`] null frames: the first
//! is [`isdbt_phy::Transmitter::finish`] (emits the frame the modulator still
//! buffers), the rest drain the interleavers so a receiver outputs every
//! input packet.

use crate::input::FrameInput;
use crate::keys::Command;
use crate::source::{Feed, Packet};
use anyhow::Result;
use isdbt_core::{Layer, Params};
use isdbt_ofdm::C32;
use isdbt_phy::Transmitter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Role of a frame in the transmission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Null lead-in.
    Lead,
    /// Input data (possibly null-padded).
    Data,
    /// Null flush after the end of input.
    Flush,
}

/// One modulated frame.
pub struct ProducedFrame {
    /// Transmitter call index = data-frame number (0 = first lead frame).
    pub index: u64,
    /// Role.
    pub kind: FrameKind,
    /// `samples_per_frame` samples at 512/63 MHz.
    pub samples: Vec<C32>,
    /// Input packets per layer in this frame.
    pub real: [usize; 3],
    /// Null packets per layer (padding).
    pub nulls: [usize; 3],
    /// The packets themselves (when recording, for verification).
    pub packets: Option<Arc<[Vec<Packet>; 3]>>,
    /// Input counters ([`FrameInput::status`]) as of this frame.
    pub input_status: String,
    /// Time spent assembling this frame's packets from the input.
    pub input_time: Duration,
    /// Time spent modulating this frame.
    pub modulate_time: Duration,
}

/// Why the producer stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Every input ended.
    InputEnded,
    /// The frame/duration limit was reached.
    Limit,
    /// Stop requested (signal).
    Stopped,
    /// The consumer went away.
    SinkClosed,
}

/// Producer settings.
#[derive(Debug, Clone)]
pub struct ProducerConfig {
    /// Transmission parameters.
    pub params: Params,
    /// Null frames before the data.
    pub lead_frames: usize,
    /// Append the flush frames after the data.
    pub flush: bool,
    /// Bound on all on-air frames (lead + data + flush); `None` = unbounded.
    pub max_frames: Option<u64>,
    /// Pace frame production to real time (live input into a file sink).
    pub pace: bool,
    /// Attach each frame's packets (for loopback verification).
    pub record: bool,
    /// Frames queued ahead of the sink.
    pub depth: usize,
}

/// Number of frames covering `seconds` of air time (rounded up).
pub fn frames_for_seconds(params: &Params, seconds: f64) -> u64 {
    // f64 -> u64 saturates: a frame count, at least 1
    #[allow(clippy::cast_possible_truncation)]
    let n = (seconds / params.frame_seconds()).ceil().max(1.0) as u64;
    n
}

/// Start the producer thread. `controls` (playback keys) are applied to
/// the input before each data frame.
pub fn spawn(
    cfg: ProducerConfig,
    mut input: Box<dyn FrameInput>,
    stop: Arc<AtomicBool>,
    controls: Option<Receiver<Command>>,
) -> (Receiver<ProducedFrame>, JoinHandle<Result<EndReason>>) {
    let (out, rx) = sync_channel::<ProducedFrame>(cfg.depth.max(1));
    let h = std::thread::Builder::new()
        .name("tx-producer".into())
        .spawn(move || -> Result<EndReason> {
            let mut tx = Transmitter::new(cfg.params.clone());
            tx.preroll()?;
            let counts: [usize; 3] = std::array::from_fn(|l| tx.tsps_per_frame(Layer::ALL[l]));
            let flush_n = if cfg.flush { tx.flush_frames() } else { 0 };
            let data_limit = cfg
                .max_frames
                .map(|t| t.saturating_sub((cfg.lead_frames + flush_n) as u64));
            let frame_s = cfg.params.frame_seconds();
            let t0 = Instant::now();
            let mut index = 0u64;
            let pace = |index: u64| {
                if cfg.pace {
                    let due = t0 + Duration::from_secs_f64(index as f64 * frame_s);
                    let now = Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                }
            };
            let nulls = || -> [Vec<Packet>; 3] {
                std::array::from_fn(|l| vec![isdbt_phy::null_packet(); counts[l]])
            };
            // Send one frame; false if the consumer is gone.
            #[allow(clippy::too_many_arguments)]
            let send = |index: u64,
                        kind,
                        samples,
                        real,
                        packets: [Vec<Packet>; 3],
                        input_status,
                        input_time,
                        modulate_time| {
                let f = ProducedFrame {
                    index,
                    kind,
                    samples,
                    real,
                    nulls: std::array::from_fn(|l| counts[l] - real[l]),
                    packets: cfg.record.then(|| Arc::new(packets)),
                    input_status,
                    input_time,
                    modulate_time,
                };
                out.send(f).is_ok()
            };
            for _ in 0..cfg.lead_frames {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                let s = tx.transmit_null_frame()?;
                let mt = tm.elapsed();
                if !send(
                    index,
                    FrameKind::Lead,
                    s,
                    [0; 3],
                    nulls(),
                    String::new(),
                    Duration::ZERO,
                    mt,
                ) {
                    return Ok(if stop.load(Ordering::Relaxed) {
                        EndReason::Stopped
                    } else {
                        EndReason::SinkClosed
                    });
                }
                index += 1;
            }
            let mut data = 0u64;
            let reason = loop {
                if data_limit.is_some_and(|l| data >= l) {
                    break EndReason::Limit;
                }
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                for cmd in controls.iter().flat_map(Receiver::try_iter) {
                    eprintln!("[tx] {}", input.control(cmd));
                }
                let ti = Instant::now();
                let Some(fp) = input.take_frame(counts, &stop) else {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(EndReason::Stopped);
                    }
                    break EndReason::InputEnded;
                };
                let it = ti.elapsed();
                let tm = Instant::now();
                let s = tx.transmit_frame([&fp.layers[0], &fp.layers[1], &fp.layers[2]])?;
                let mt = tm.elapsed();
                if !send(
                    index,
                    FrameKind::Data,
                    s,
                    fp.real,
                    fp.layers,
                    input.status(),
                    it,
                    mt,
                ) {
                    return Ok(if stop.load(Ordering::Relaxed) {
                        EndReason::Stopped
                    } else {
                        EndReason::SinkClosed
                    });
                }
                index += 1;
                data += 1;
            };
            for i in 0..flush_n {
                if stop.load(Ordering::Relaxed) {
                    return Ok(EndReason::Stopped);
                }
                pace(index);
                let tm = Instant::now();
                let s = if i == 0 {
                    tx.finish()?
                } else {
                    tx.transmit_null_frame()?
                };
                let mt = tm.elapsed();
                if !send(
                    index,
                    FrameKind::Flush,
                    s,
                    [0; 3],
                    nulls(),
                    String::new(),
                    Duration::ZERO,
                    mt,
                ) {
                    return Ok(if stop.load(Ordering::Relaxed) {
                        EndReason::Stopped
                    } else {
                        EndReason::SinkClosed
                    });
                }
                index += 1;
            }
            Ok(reason)
        })
        .expect("spawn producer");
    (rx, h)
}

/// Pace by default only for live input written to a non-real-time sink.
pub fn default_pace(feed: Feed, sink_realtime: bool) -> bool {
    feed == Feed::Live && !sink_realtime
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::SyntheticInput;

    fn params() -> Params {
        // Mode 1 keeps the test fast.
        crate::phyargs::PhyArgs {
            mode: 1,
            gi: 8,
            partial: true,
            layer_a: "1,qpsk,2/3,2".into(),
            layer_b: "12,64qam,3/4,2".into(),
            layer_c: "none".into(),
        }
        .params()
        .unwrap()
    }

    #[test]
    fn lead_data_flush_sequence_and_limit() {
        let p = params();
        let flush = Transmitter::new(p.clone()).flush_frames();
        let cfg = ProducerConfig {
            params: p.clone(),
            lead_frames: 2,
            flush: true,
            max_frames: Some(2 + 3 + flush as u64),
            pace: false,
            record: true,
            depth: 2,
        };
        let (rx, h) = spawn(
            cfg,
            Box::new(SyntheticInput::new()),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        let frames: Vec<_> = rx.iter().collect();
        assert_eq!(h.join().unwrap().unwrap(), EndReason::Limit);
        let kinds: Vec<_> = frames.iter().map(|f| f.kind).collect();
        let mut want = vec![FrameKind::Lead; 2];
        want.extend([FrameKind::Data; 3]);
        want.extend(vec![FrameKind::Flush; flush]);
        assert_eq!(kinds, want);
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(f.index, i as u64);
            assert_eq!(f.samples.len(), p.samples_per_frame());
            let pk = f.packets.as_ref().unwrap();
            assert_eq!(pk[1].len(), f.real[1] + f.nulls[1]);
        }
        assert_eq!(
            frames[2].real,
            [
                p.layers()[0].unwrap().tsp_per_frame(p.mode()),
                p.layers()[1].unwrap().tsp_per_frame(p.mode()),
                0
            ]
        );
        assert_eq!(frames[0].real, [0; 3]);
    }

    #[test]
    fn stop_ends_without_flush() {
        let stop = Arc::new(AtomicBool::new(false));
        let cfg = ProducerConfig {
            params: params(),
            lead_frames: 0,
            flush: true,
            max_frames: None,
            pace: false,
            record: false,
            depth: 1,
        };
        let (rx, h) = spawn(cfg, Box::new(SyntheticInput::new()), stop.clone(), None);
        let first = rx.recv().unwrap();
        assert_eq!(first.kind, FrameKind::Data);
        stop.store(true, Ordering::SeqCst);
        let rest: Vec<_> = rx.iter().collect();
        assert!(rest.iter().all(|f| f.kind == FrameKind::Data));
        assert_eq!(h.join().unwrap().unwrap(), EndReason::Stopped);
    }
}
