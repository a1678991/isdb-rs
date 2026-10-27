//! `isdb loopback`: TX → software channel → RX in one process, verifying
//! byte-exact delivery of every layer and of the §3.2.2 reproduced TS.

use crate::channel::Channel;
use crate::input::InputArgs;
use crate::phyargs::{PhyArgs, describe_params};
use crate::producer::{self, EndReason, FrameKind, ProducerConfig};
use crate::rx::RxStats;
use crate::source::{Packet, is_null};
use anyhow::{Context, Result, bail};
use clap::Args;
use isdbt_core::Params;
use isdbt_fec::remux::{MultiplexPattern, null_tsp};
use isdbt_ofdm::DemodConfig;
use isdbt_phy::{PipelinedReceiver, RxFrame, Transmitter};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// `isdb loopback` arguments.
#[derive(Debug, Args)]
pub struct LoopbackArgs {
    #[command(flatten)]
    pub phy: PhyArgs,
    /// Inputs as for `isdb tx`; synthetic packets if none is given.
    #[command(flatten)]
    pub input: InputArgs,
    /// Data frames to send (default: until EOF for --feed file inputs,
    /// 6 otherwise).
    #[arg(long)]
    pub frames: Option<u64>,
    /// AWGN at this SNR (dB, relative to the signal power).
    #[arg(long, allow_negative_numbers = true)]
    pub snr_db: Option<f32>,
    /// Carrier frequency offset in carrier spacings (may be fractional).
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    pub cfo_carriers: f64,
    /// Receiver timing offset: N > 0 samples of silence before the signal,
    /// N < 0 drops the first |N| samples (receiver starts mid-frame).
    #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
    pub sample_offset: i64,
    /// Noise generator seed.
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// Null frames before the data (default 2).
    #[arg(long)]
    pub lead_frames: Option<usize>,
    /// Omit the flush frames (the last frames are then reported lost).
    #[arg(long)]
    pub no_flush: bool,
    /// Print a JSON summary on stdout.
    #[arg(long)]
    pub json: bool,
    /// --relay: terrestrial channel named in the NIT (default 44, i.e. T44).
    #[arg(long, value_name = "13..62", requires = "relay")]
    pub channel: Option<u32>,
}

/// Per-layer verification counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayerVerdict {
    /// Packets identical to what was sent.
    pub pass: u64,
    /// Packets that differ.
    pub fail: u64,
    /// Input (non-null) packets among the passed ones.
    pub input_pass: u64,
    /// Input packets in data frames that never came out of the receiver.
    pub lost: u64,
}

struct Sent {
    index: u64,
    packets: Arc<[Vec<Packet>; 3]>,
}

/// Matches receiver output to transmitted frames.  The receiver numbers
/// frames from its lock point, so the first valid frame carrying input data
/// is anchored by content: it must equal a transmitted frame exactly (the
/// earliest such frame if the content repeats).  All later frames are then
/// compared at that offset.
pub struct Verifier {
    pattern: MultiplexPattern,
    active: [bool; 3],
    ring: VecDeque<Sent>,
    real: Vec<[usize; 3]>,
    verified: Vec<bool>,
    offset: Option<i64>,
    /// Valid frames seen before the anchor.
    pub unanchored: u64,
    /// Valid frames compared.
    pub frames_compared: u64,
    /// Compared frames whose transmitted frame was no longer known.
    pub frames_unknown: u64,
    /// Reproduced TS frames identical / different.
    pub ts_pass: u64,
    /// See `ts_pass`.
    pub ts_fail: u64,
    /// Per layer.
    pub layers: [LayerVerdict; 3],
}

const RING: usize = 256;

impl Verifier {
    /// Verifier for `params`.
    pub fn new(params: &Params) -> Self {
        Self {
            pattern: MultiplexPattern::new(params),
            active: std::array::from_fn(|l| params.layers()[l].is_some()),
            ring: VecDeque::new(),
            real: Vec::new(),
            verified: Vec::new(),
            offset: None,
            unanchored: 0,
            frames_compared: 0,
            frames_unknown: 0,
            ts_pass: 0,
            ts_fail: 0,
            layers: Default::default(),
        }
    }

    /// Record a transmitted frame.
    pub fn sent(&mut self, index: u64, packets: Arc<[Vec<Packet>; 3]>, real: [usize; 3]) {
        assert_eq!(index, self.real.len() as u64, "frames recorded in order");
        self.real.push(real);
        self.verified.push(false);
        self.ring.push_back(Sent { index, packets });
        if self.ring.len() > RING {
            self.ring.pop_front();
        }
    }

    fn lookup(&self, index: i64) -> Option<&Sent> {
        let first = self.ring.front()?.index as i64;
        let k = usize::try_from(index - first).ok()?;
        self.ring.get(k)
    }

    fn matches(&self, fr: &RxFrame, s: &Sent) -> bool {
        (0..3).all(|l| !self.active[l] || fr.layers[l].as_deref() == Some(&s.packets[l][..]))
    }

    /// Check one receiver frame.
    pub fn received(&mut self, fr: &RxFrame) {
        if !fr.valid {
            return;
        }
        if self.offset.is_none() {
            let has_data = fr.layers.iter().flatten().flatten().any(|p| !is_null(p));
            let hit = if has_data {
                self.ring
                    .iter()
                    .find(|s| self.matches(fr, s))
                    .map(|s| s.index)
            } else {
                None
            };
            match hit {
                Some(i) => self.offset = Some(i as i64 - fr.tx_frame),
                None => {
                    self.unanchored += 1;
                    return;
                }
            }
        }
        let idx = fr.tx_frame + self.offset.expect("anchored");
        self.frames_compared += 1;
        let Some(s) = self.lookup(idx) else {
            self.frames_unknown += 1;
            for l in 0..3 {
                self.layers[l].fail += fr.layers[l].as_ref().map_or(0, |v| v.len() as u64);
            }
            return;
        };
        let packets = s.packets.clone();
        for l in (0..3).filter(|&l| self.active[l]) {
            let got = fr.layers[l].as_deref().unwrap_or(&[]);
            let want = &packets[l];
            let v = &mut self.layers[l];
            for (k, w) in want.iter().enumerate() {
                if got.get(k) == Some(w) {
                    v.pass += 1;
                    v.input_pass += u64::from(!is_null(w));
                } else {
                    v.fail += 1;
                }
            }
            v.fail += got.len().saturating_sub(want.len()) as u64;
        }
        let refs: [&[Packet]; 3] = std::array::from_fn(|l| &packets[l][..]);
        match self.pattern.assemble(refs, null_tsp) {
            Ok(ts) if ts == fr.ts => self.ts_pass += 1,
            _ => self.ts_fail += 1,
        }
        if let Some(v) = usize::try_from(idx)
            .ok()
            .and_then(|i| self.verified.get_mut(i))
        {
            *v = true;
        }
    }

    /// Account input packets of frames never verified.
    pub fn finish(&mut self) {
        for (i, r) in self.real.iter().enumerate() {
            if !self.verified[i] {
                for (v, n) in self.layers.iter_mut().zip(r) {
                    v.lost += *n as u64;
                }
            }
        }
    }

    /// True if every input packet arrived byte-exact and nothing differed.
    pub fn pass(&self) -> bool {
        self.offset.is_some()
            && self.frames_unknown == 0
            && self.ts_fail == 0
            && self.layers.iter().all(|v| v.fail == 0 && v.lost == 0)
    }
}

/// Entry point.
pub fn run(a: LoopbackArgs) -> Result<()> {
    let params = a.phy.params()?;
    let (params, prepared) =
        match crate::relay::prepare(&a.input, params, || Ok(a.channel.unwrap_or(44)))? {
            crate::relay::Prepared::Exit => return Ok(()),
            crate::relay::Prepared::Ready(p, i) => (p, i),
        };
    let mut input_args = a.input.clone();
    if input_args.is_empty() {
        input_args.synthetic = true;
    }
    let file_feed = input_args.feed == crate::source::Feed::File && !input_args.is_generated();
    let tx = Transmitter::new(params.clone());
    let lead = a.lead_frames.unwrap_or(tx.lead_frames());
    let flush = if a.no_flush { 0 } else { tx.flush_frames() };
    let data_frames = a.frames.or((!file_feed).then_some(6));
    let input = match prepared {
        Some(i) => i,
        None => input_args.open(&params)?,
    };
    eprintln!("isdb loopback: {}", describe_params(&params));
    eprintln!("  input: {}", input.describe());
    eprintln!(
        "  channel: SNR {}, CFO {:+} carriers, sample offset {}; lead {lead}, flush {flush} frames",
        a.snr_db.map_or("∞".into(), |s| format!("{s} dB")),
        a.cfo_carriers,
        a.sample_offset
    );
    let stop = crate::stop::flag();
    let cfg = ProducerConfig {
        params: params.clone(),
        lead_frames: lead,
        flush: !a.no_flush,
        max_frames: data_frames.map(|d| d + (lead + flush) as u64),
        pace: producer::default_pace(input_args.feed, false),
        record: true,
        depth: 2,
    };
    let (frames, worker) = producer::spawn(cfg, input, stop.clone(), None);
    let mut ch = Channel::new(
        a.snr_db,
        a.cfo_carriers,
        params.mode().fft_size(),
        a.sample_offset,
        a.seed,
    )?;
    let mut prx =
        PipelinedReceiver::new(DemodConfig::default(), 2).context("start the receiver threads")?;
    let mut ver = Verifier::new(&params);
    let mut stats = RxStats::default();
    let (mut sent_frames, mut sent_data) = (0u64, 0u64);
    let on_rx =
        |r: isdbt_phy::Result<RxFrame>, ver: &mut Verifier, stats: &mut RxStats| -> Result<()> {
            let fr = r?;
            stats.add(&fr);
            ver.received(&fr);
            Ok(())
        };
    for f in frames.iter() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        ver.sent(f.index, f.packets.clone().expect("recording"), f.real);
        sent_frames += 1;
        sent_data += u64::from(f.kind == FrameKind::Data);
        prx.push(ch.process(f.samples));
        while let Some(r) = prx.try_recv() {
            on_rx(r, &mut ver, &mut stats)?;
        }
    }
    drop(frames);
    let end = worker
        .join()
        .map_err(|_| anyhow::anyhow!("producer panicked"))??;
    crate::source::check_not_scrambled()?;
    for r in prx.finish() {
        on_rx(r, &mut ver, &mut stats)?;
    }
    ver.finish();
    let ok = ver.pass() && end != EndReason::Stopped;
    eprintln!(
        "[loopback] sent {sent_frames} frames ({sent_data} data); received {} frames ({} valid, {} before anchor); TS frames exact {}/{}",
        stats.frames,
        stats.valid,
        ver.unanchored,
        ver.ts_pass,
        ver.ts_pass + ver.ts_fail
    );
    for (id, _) in params.active_layers() {
        let (v, st) = (&ver.layers[id.index()], &stats.layers[id.index()]);
        eprintln!(
            "[loopback]   layer {id}: {} — {} packets byte-exact ({} input), {} FAIL, {} input packets lost; RS corrected {} uncorrectable {}; MER {}",
            if v.fail == 0 && v.lost == 0 && ver.frames_compared > 0 {
                "PASS"
            } else {
                "FAIL"
            },
            v.pass,
            v.input_pass,
            v.fail,
            v.lost,
            st.corrected_packets,
            st.uncorrectable,
            st.mer_mean().map_or("-".into(), |m| format!("{m:.1} dB"))
        );
    }
    eprintln!("[loopback] {}", if ok { "PASS" } else { "FAIL" });
    if a.json {
        let layers: serde_json::Map<String, serde_json::Value> = params
            .active_layers()
            .map(|(id, _)| {
                let v = &ver.layers[id.index()];
                (
                    id.to_string(),
                    json!({"pass": v.pass, "input_pass": v.input_pass, "fail": v.fail, "lost": v.lost}),
                )
            })
            .collect();
        let j = json!({
            "result": if ok { "PASS" } else { "FAIL" },
            "sent_frames": sent_frames,
            "sent_data_frames": sent_data,
            "frames_compared": ver.frames_compared,
            "ts_frames_exact": ver.ts_pass,
            "ts_frames_differ": ver.ts_fail,
            "verify": layers,
            "rx": stats.json(),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
    }
    if !ok {
        bail!("loopback FAILED");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::SyntheticInput;

    #[test]
    fn verifier_counts_and_anchors_by_content() {
        let params = PhyArgs {
            mode: 1,
            gi: 8,
            partial: false,
            layer_a: "13,qpsk,1/2,0".into(),
            layer_b: "none".into(),
            layer_c: "none".into(),
        }
        .params()
        .unwrap();
        let n = params.layers()[0].unwrap().tsp_per_frame(params.mode());
        let pattern = MultiplexPattern::new(&params);
        let frame = |i: u64| -> [Vec<Packet>; 3] {
            [
                (0..n as u64)
                    .map(|k| SyntheticInput::packet(0, i * 1000 + k))
                    .collect(),
                vec![],
                vec![],
            ]
        };
        let mut v = Verifier::new(&params);
        for i in 0..5 {
            v.sent(i, Arc::new(frame(i)), [n, 0, 0]);
        }
        // Receiver output for TX frames 1..=3, numbered from its own lock.
        let mk = |i: u64, rx_idx: i64, corrupt: bool| {
            let mut l = frame(i);
            if corrupt {
                l[0][3][100] ^= 1;
            }
            let refs: [&[Packet]; 3] = std::array::from_fn(|k| &l[k][..]);
            RxFrame {
                tx_frame: rx_idx,
                valid: true,
                ts: pattern.assemble(refs, null_tsp).unwrap(),
                layers: [Some(l[0].clone()), None, None],
                rs: [None; 3],
                tmcc: isdbt_ofdm::tmcc::decode_frame(&[0; 204]),
                params: params.clone(),
                metrics: isdbt_ofdm::rx::Metrics {
                    cfo_carriers: 0.0,
                    cfo_integer: 0,
                    gi_metric: 0.0,
                    mer_db: [None; 3],
                    n0: 0.0,
                    cpe_drift: 0.0,
                    timing_drift: 0.0,
                    timing_correction: 0.0,
                },
                start_sample: 0,
            }
        };
        v.received(&mk(1, 7, false));
        v.received(&mk(2, 8, true));
        v.received(&mk(3, 9, false));
        v.finish();
        assert_eq!(v.frames_compared, 3);
        assert_eq!(v.layers[0].fail, 1);
        assert_eq!(v.layers[0].pass, 3 * n as u64 - 1);
        assert_eq!(
            v.layers[0].lost,
            2 * n as u64,
            "TX frames 0 and 4 never received"
        );
        assert_eq!((v.ts_pass, v.ts_fail), (2, 1));
        assert!(!v.pass());
    }
}
