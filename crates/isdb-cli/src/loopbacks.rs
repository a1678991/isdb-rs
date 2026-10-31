//! `isdb loopback --system isdbs`: the ISDB-S transmitter of
//! [`crate::txs`] → software channel → the ISDB-S receiver of
//! [`crate::rxs`], in one process, verifying every TS_ID byte-exact.
//!
//! ```text
//! producer (Transmitter + Shaper) ─► Channel (AWGN, CFO in Hz, offset)
//!   ─► one buffer at --backoff 10 dB ─► rxs::front (survey, trim, front end)
//!   ─► isdbs_phy::receive ─► ts::decode_all ─► VerifierS
//! ```
//!
//! The whole signal is one buffer (a loopback of N superframes is N × 443 k
//! samples at 40 Msps), received as `isdb rx --iq` receives a recording
//! segment. [`VerifierS`] anchors the first decoded superframe that
//! carries an input packet by its content, numbers the later ones by their
//! position in the symbol stream (319 488 symbols apart) and compares them
//! packet by packet per TS_ID, and each one's TMCC with the one sent.

use crate::channel::Channel;
use crate::loopback::{LayerVerdict, LoopbackArgs};
use crate::producer::{EndReason, FrameKind};
use crate::source::{Feed, Packet, is_null};
use crate::txs::{DEFAULT_RATE, Layout, SatConfig, TsInputs, superframe_seconds};
use anyhow::{Context, Result, bail};
use isdb_frontend::shape::Shaper;
use isdb_radio::C32;
use isdbs_core::tmcc::TmccInfo;
use isdbs_phy::demod::DemodOptions;
use isdbs_phy::ts::{DecodedSuperframe, decode_all};
use isdbs_phy::tx::{SUPERFRAME_SYMBOLS, Transmitter};
use isdbs_phy::{RxOptions, receive};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::info;

/// RMS below full scale at which the loopback hands the signal to the
/// front end, whose input is a fraction of full scale (12-bit): the
/// `--backoff-db` default of `isdb tx`.
pub const BACKOFF_DB: f32 = 10.0;

/// One sent superframe.
struct Sent {
    /// Packets per relative TS.
    packets: Arc<[Vec<Packet>]>,
    /// Input (non-padding) packets per relative TS.
    real: Vec<usize>,
}

/// Matches the receiver's superframes to the sent ones, per TS_ID. The
/// receiver finds superframes wherever it acquires, so the first decoded
/// superframe that carries an input packet is anchored by content: it
/// must equal a sent superframe exactly (the earliest if the content
/// repeats). Every later one is numbered from it by its start in the
/// symbol stream, [`SUPERFRAME_SYMBOLS`] apart.
pub struct VerifierS {
    info: TmccInfo,
    ts_ids: Vec<u16>,
    sent: Vec<Sent>,
    verified: Vec<bool>,
    /// Symbol start of the anchor superframe and its sent index.
    anchor: Option<(usize, usize)>,
    /// Decoded superframes seen before the anchor.
    pub unanchored: u64,
    /// Decoded superframes compared.
    pub compared: u64,
    /// Compared superframes whose sent superframe is not known.
    pub unknown: u64,
    /// Decoded superframes whose TMCC differs from the one sent.
    pub tmcc_differs: u64,
    /// Decoded packets of a TS_ID that was not sent.
    pub foreign: u64,
    /// Per relative TS.
    pub ts: Vec<LayerVerdict>,
}

impl VerifierS {
    /// A verifier of the superframes of `layout`.
    pub fn new(layout: &Layout) -> Self {
        let ts_ids = layout.ts_ids();
        Self {
            info: layout.info.clone(),
            ts: vec![LayerVerdict::default(); ts_ids.len()],
            ts_ids,
            sent: Vec::new(),
            verified: Vec::new(),
            anchor: None,
            unanchored: 0,
            compared: 0,
            unknown: 0,
            tmcc_differs: 0,
            foreign: 0,
        }
    }

    /// TS_ID of each relative TS.
    pub fn ts_ids(&self) -> &[u16] {
        &self.ts_ids
    }

    /// Record the next sent superframe (lead or data, in order): its
    /// packets and how many of them per TS came from the input.
    pub fn sent(&mut self, packets: Arc<[Vec<Packet>]>, real: Vec<usize>) {
        self.sent.push(Sent { packets, real });
        self.verified.push(false);
    }

    /// Check decoded superframe `d`, which starts at symbol `start` of the
    /// receiver's stream.
    pub fn received(&mut self, start: usize, d: &DecodedSuperframe) {
        if d.info != self.info {
            self.tmcc_differs += 1;
        }
        let got: Vec<Vec<Packet>> = self
            .ts_ids
            .iter()
            .map(|id| {
                d.packets
                    .iter()
                    .filter(|p| p.ts_id == *id)
                    .map(|p| p.bytes)
                    .collect()
            })
            .collect();
        let foreign = d
            .packets
            .iter()
            .filter(|p| !self.ts_ids.contains(&p.ts_id))
            .count() as u64;
        let (s0, i0) = match self.anchor {
            Some(a) => a,
            None => {
                let has_data = got.iter().flatten().any(|p| !is_null(p));
                let hit = if has_data {
                    self.sent.iter().position(|s| s.packets[..] == got[..])
                } else {
                    None
                };
                let Some(i) = hit else {
                    self.unanchored += 1;
                    return;
                };
                self.anchor = Some((start, i));
                (start, i)
            }
        };
        self.compared += 1;
        self.foreign += foreign;
        // the sent index: whole superframes from the anchor, rounded
        let sf = SUPERFRAME_SYMBOLS as i128;
        let delta = start as i128 - s0 as i128;
        let idx = i0 as i128 + (2 * delta + sf).div_euclid(2 * sf);
        let Some(i) = usize::try_from(idx).ok().filter(|&i| i < self.sent.len()) else {
            self.unknown += 1;
            for (v, g) in self.ts.iter_mut().zip(&got) {
                v.fail += g.len() as u64;
            }
            return;
        };
        let want = self.sent[i].packets.clone();
        for ((v, w), g) in self.ts.iter_mut().zip(want.iter()).zip(&got) {
            for (k, p) in w.iter().enumerate() {
                if g.get(k) == Some(p) {
                    v.pass += 1;
                    v.input_pass += u64::from(!is_null(p));
                } else {
                    v.fail += 1;
                }
            }
            v.fail += g.len().saturating_sub(w.len()) as u64;
        }
        self.verified[i] = true;
    }

    /// Account the input packets of sent superframes never verified.
    pub fn finish(&mut self) {
        for (s, &ok) in self.sent.iter().zip(&self.verified) {
            if !ok {
                for (v, &n) in self.ts.iter_mut().zip(&s.real) {
                    v.lost += n as u64;
                }
            }
        }
    }

    /// True if anchored, every input packet arrived byte-exact, nothing
    /// differed and every superframe's TMCC was the one sent.
    pub fn pass(&self) -> bool {
        self.anchor.is_some()
            && self.unknown == 0
            && self.tmcc_differs == 0
            && self.foreign == 0
            && self.ts.iter().all(|v| v.fail == 0 && v.lost == 0)
    }
}

/// `isdb loopback --system isdbs`.
pub fn run(a: LoopbackArgs) -> Result<()> {
    let layout = Layout::of_inputs(&a.sat, &a.input.mux_config)?;
    let rate = a.sat.rate.unwrap_or(DEFAULT_RATE);
    let sys = isdbs_phy::frontend::system();
    let shaper = Shaper::new(rate, sys.symbol_rate_hz, sys.roll_off)
        .with_context(|| format!("--rate {rate}"))?;
    let tx = Transmitter::new(layout.info.clone())?;
    let counts: Vec<usize> = (0..tx.ts_count())
        .map(|r| tx.packets_per_superframe(r))
        .collect();
    let lead = a.sat.lead_superframes.unwrap_or(1);
    let file_feed = a.input.feed == Feed::File && !layout.synthetic_only();
    let data = a.superframes.or((!file_feed).then_some(6));
    let plan = crate::txs::plan_inputs(
        &layout,
        a.input.feed,
        a.sat.ts_pacing,
        a.input.allow_overrate,
    )?;
    let input = TsInputs::open(&layout, a.input.feed, &counts, &plan)?;
    info!(
        target: "isdb::loopback",
        system = "ISDB-S",
        "ISDB-S (ITU-R BO.1408-1): {}",
        layout.describe()
    );
    info!(target: "isdb::loopback", "  input: {}", input.describe());
    info!(
        target: "isdb::loopback",
        snr_db = a.snr_db.map_or(f64::INFINITY, f64::from),
        cfo_hz = a.cfo_hz,
        sample_offset = a.sample_offset,
        sample_rate_hz = rate,
        lead_superframes = lead,
        flush = !a.no_flush,
        "  channel: SNR {}, CFO {:+} Hz, sample offset {} at {:.3} Msps; pre-roll frame, lead {lead} superframe(s), flush {}; front end at {BACKOFF_DB} dB below full scale",
        a.snr_db.map_or("∞".into(), |s| format!("{s} dB")),
        a.cfo_hz,
        a.sample_offset,
        rate / 1e6,
        if a.no_flush {
            "none"
        } else {
            "a null frame and the shaper's tail"
        }
    );
    let stop = crate::stop::flag();
    let cfg = SatConfig {
        lead,
        flush: !a.no_flush,
        max_superframes: data.map(|d| d + lead as u64),
        pace: crate::producer::default_pace(a.input.feed, false),
        record: true,
        depth: 2,
        placed: None,
    };
    let t0 = Instant::now();
    let (items, worker) = crate::txs::spawn(
        cfg,
        tx,
        crate::txs::Modulate::shaped(shaper),
        input,
        stop.clone(),
    )?;
    let mut ch = Channel::with_cfo_hz(a.snr_db, a.cfo_hz, rate, a.sample_offset, a.seed)?;
    let mut ver = VerifierS::new(&layout);
    let (mut sent, mut sent_data) = (0u64, 0u64);
    let mut x: Vec<C32> = Vec::new();
    for f in items.iter() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if f.kind != FrameKind::Flush {
            // `record: true` above: lead and data carry their packets
            let packets = f
                .packets
                .clone()
                .context("the producer did not record a superframe's packets")?;
            ver.sent(packets, f.real.clone());
            sent += 1;
            sent_data += u64::from(f.kind == FrameKind::Data);
        }
        x.extend(ch.process(f.payload.into_samples()?));
    }
    drop(items);
    // a panic of the producer is re-raised, not reported as an ordinary error
    let end = worker
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))?;
    crate::source::check_not_scrambled()?;
    let produce_s = t0.elapsed().as_secs_f64();
    let k = 10f32.powf(-BACKOFF_DB / 20.0);
    for v in &mut x {
        *v *= k;
    }
    let t1 = Instant::now();
    let (s, _) = crate::rxs::front(&x, rate, None, None)?;
    let front_s = t1.elapsed().as_secs_f64();
    let t2 = Instant::now();
    let r = receive(&s.z, &RxOptions::default());
    let decoded = decode_all(&r, &DemodOptions::default());
    // decode_all leaves out the superframes before the first whose TMCC
    // decoded (no TMCC is known for them); the rest come in order
    let starts: Vec<usize> = r
        .superframes
        .iter()
        .skip_while(|sf| sf.info.is_none())
        .map(|sf| r.frames[sf.first_frame].mark.start)
        .collect();
    if starts.len() != decoded.len() {
        bail!(
            "{} superframes decoded of {} after the first TMCC",
            decoded.len(),
            starts.len()
        );
    }
    // the counters `isdb rx` keeps for one block
    let mut stats = crate::rxs::Stats::default();
    stats.blocks = 1;
    stats.samples = x.len() as u64;
    stats.frames = r.frames.len() as u64;
    stats.headers_ok = r.frames.iter().filter(|f| f.header_ok).count() as u64;
    stats.slips = r.slips as u64;
    stats.superframes = r.superframes.len() as u64;
    stats.tmcc_ok = r.superframes.iter().filter(|s| s.info.is_some()).count() as u64;
    stats.carrier_offset_hz = s.carrier_offset_hz;
    stats.symbol_rate_hz = s.symbol_rate_hz;
    for (&start, d) in starts.iter().zip(&decoded) {
        stats.add(d);
        ver.received(start, d);
    }
    ver.finish();
    let back_s = t2.elapsed().as_secs_f64();
    stats.front_s = front_s;
    stats.back_s = back_s;
    stats.wall_s = t0.elapsed().as_secs_f64();
    let secs = x.len() as f64 / rate;
    let ok = ver.pass() && end != EndReason::Stopped;
    info!(
        target: "isdb::loopback",
        sent_superframes = sent,
        sent_data = sent_data,
        received_superframes = r.superframes.len(),
        decoded_superframes = decoded.len(),
        unanchored_superframes = ver.unanchored,
        compared_superframes = ver.compared,
        tmcc_differs = ver.tmcc_differs,
        "sent {sent} superframes ({sent_data} data); received {} superframes ({} decoded, {} before the anchor); TMCC as sent {}/{}",
        r.superframes.len(),
        decoded.len(),
        ver.unanchored,
        decoded.len() as u64 - ver.tmcc_differs,
        decoded.len()
    );
    info!(
        target: "isdb::loopback",
        carrier_offset_hz = s.carrier_offset_hz,
        trim = s.trim,
        symbol_rate_hz = s.symbol_rate_hz,
        mer_db = stats.mer_db().unwrap_or(f64::NAN),
        seconds = secs,
        produce_s,
        front_s,
        back_s,
        "  receiver: carrier {:+.0} Hz, trim {:+.3} symbol, {:.1} Bd, main MER {}; {secs:.3} s of signal: produced and channel {produce_s:.2} s, front end {front_s:.2} s, receiver {back_s:.2} s",
        s.carrier_offset_hz,
        s.trim,
        s.symbol_rate_hz,
        stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
    );
    for (r, &id) in ver.ts_ids().iter().enumerate() {
        let v = &ver.ts[r];
        let st = stats.ts.get(&id).cloned().unwrap_or_default();
        let ts_ok = v.fail == 0 && v.lost == 0 && ver.compared > 0;
        info!(
            target: "isdb::loopback",
            ts_id = id,
            pass = ts_ok,
            exact_packets = v.pass,
            input_exact_packets = v.input_pass,
            fail_packets = v.fail,
            lost_packets = v.lost,
            rs_corrected = st.corrected_packets,
            rs_uncorrectable = st.uncorrectable,
            mer_db = stats.mer_db().unwrap_or(f64::NAN),
            "  TS_ID {id:#06x}: {} — {} packets byte-exact ({} input), {} FAIL, {} input packets lost; RS corrected {} uncorrectable {}; MER {}",
            if ts_ok { "PASS" } else { "FAIL" },
            v.pass,
            v.input_pass,
            v.fail,
            v.lost,
            st.corrected_packets,
            st.uncorrectable,
            stats.mer_db().map_or("-".into(), |m| format!("{m:.1} dB"))
        );
    }
    info!(target: "isdb::loopback", pass = ok, end = ?end, "{}", if ok { "PASS" } else { "FAIL" });
    if a.json {
        let verify: serde_json::Map<String, serde_json::Value> = ver
            .ts_ids()
            .iter()
            .zip(&ver.ts)
            .map(|(id, v)| {
                (
                    format!("{id:#06x}"),
                    json!({"pass": v.pass, "input_pass": v.input_pass, "fail": v.fail, "lost": v.lost}),
                )
            })
            .collect();
        let j = json!({
            "result": if ok { "PASS" } else { "FAIL" },
            "system": "ISDB-S",
            "sent_superframes": sent,
            "sent_data_superframes": sent_data,
            "superframes_compared": ver.compared,
            "superframes_unanchored": ver.unanchored,
            "superframes_unknown": ver.unknown,
            "tmcc_differs": ver.tmcc_differs,
            "foreign_packets": ver.foreign,
            "trim_symbols": s.trim,
            "superframe_seconds": superframe_seconds(),
            "verify": verify,
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
    use crate::txs::{SatArgs, synthetic_packet};
    use clap::Parser;
    use isdbs_phy::ts;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        sat: SatArgs,
    }

    fn layout(args: &[&str]) -> Layout {
        let mut v = vec!["t"];
        v.extend_from_slice(args);
        Layout::of(&T::try_parse_from(v).unwrap().sat).unwrap()
    }

    /// Sent superframe `sf` of two TSs, 0x4010 and 0x4011: 16 and 8
    /// packets (fewer than a superframe of the layout carries; the
    /// verifier compares what was sent with what came back, whatever the
    /// count).
    fn superframe(sf: u64) -> Vec<Vec<Packet>> {
        vec![
            (0..16)
                .map(|i| synthetic_packet(0x4010, sf, i, 0))
                .collect(),
            (0..8).map(|i| synthetic_packet(0x4011, sf, i, 0)).collect(),
        ]
    }

    /// The receiver's view of a sent superframe: packets in slot order
    /// (here TS 0x4010 then 0x4011), with TS_IDs.
    fn decoded(l: &Layout, p: &[Vec<Packet>], info: Option<TmccInfo>) -> DecodedSuperframe {
        let packets = p
            .iter()
            .zip(l.ts_ids())
            .flat_map(|(v, id)| {
                v.iter().map(move |b| ts::Packet {
                    ts_id: id,
                    frame: 0,
                    slot: 0,
                    bytes: *b,
                    corrected: Some(0),
                })
            })
            .collect();
        DecodedSuperframe {
            packets,
            info: info.unwrap_or_else(|| l.info.clone()),
            mer_db: 20.0,
        }
    }

    #[test]
    fn verifier_counts_and_anchors_by_content() {
        let l = layout(&[
            "--combination",
            "qpsk3/4:48",
            "--ts",
            "0x4010:24",
            "--ts",
            "0x4011:12",
        ]);
        let mut v = VerifierS::new(&l);
        let null: Vec<Vec<Packet>> = vec![
            vec![crate::source::null_packet(); 16],
            vec![crate::source::null_packet(); 8],
        ];
        // sent: lead, then data 0..4
        v.sent(Arc::from(null.clone()), vec![0, 0]);
        for sf in 0..4 {
            v.sent(Arc::from(superframe(sf)), vec![16, 8]);
        }
        let n = SUPERFRAME_SYMBOLS;
        // the receiver starts mid-lead: the lead is missed, data 0 starts
        // at an arbitrary symbol, data 1 is corrupt, data 2 decoded under a
        // different TMCC, data 3 never received
        let s0 = n + 12_345;
        v.received(s0 - n, &decoded(&l, &null, None)); // lead: no input packet
        v.received(s0, &decoded(&l, &superframe(0), None));
        let mut bad = superframe(1);
        bad[1][3][100] ^= 1;
        v.received(s0 + n + 2, &decoded(&l, &bad, None));
        let mut other = l.info.clone();
        other.order_of_change = 1;
        v.received(s0 + 2 * n - 1, &decoded(&l, &superframe(2), Some(other)));
        v.finish();
        assert_eq!((v.unanchored, v.compared, v.unknown), (1, 3, 0));
        assert_eq!(v.tmcc_differs, 1);
        assert_eq!(
            (v.ts[0].pass, v.ts[0].input_pass, v.ts[0].fail, v.ts[0].lost),
            (48, 48, 0, 16)
        );
        assert_eq!(
            (v.ts[1].pass, v.ts[1].input_pass, v.ts[1].fail, v.ts[1].lost),
            (23, 23, 1, 8)
        );
        assert!(!v.pass());

        // a clean run passes; a superframe beyond the sent ones is unknown
        let mut v = VerifierS::new(&l);
        v.sent(Arc::from(null.clone()), vec![0, 0]);
        v.sent(Arc::from(superframe(0)), vec![16, 8]);
        v.received(0, &decoded(&l, &null, None));
        v.received(n, &decoded(&l, &superframe(0), None));
        v.finish();
        assert!(v.pass(), "unanchored {}", v.unanchored);
        assert_eq!((v.ts[0].input_pass, v.ts[1].input_pass), (16, 8));
        v.received(2 * n, &decoded(&l, &superframe(1), None));
        assert_eq!(v.unknown, 1);
        assert!(!v.pass());

        // nothing anchored (no data received): FAIL with everything lost
        let mut v = VerifierS::new(&l);
        v.sent(Arc::from(superframe(0)), vec![16, 8]);
        v.received(0, &decoded(&l, &null, None));
        v.finish();
        assert_eq!((v.ts[0].lost, v.ts[1].lost), (16, 8));
        assert!(!v.pass());

        // packets of a TS_ID that was not sent
        let mut v = VerifierS::new(&l);
        v.sent(Arc::from(superframe(0)), vec![16, 8]);
        let mut d = decoded(&l, &superframe(0), None);
        d.packets.push(ts::Packet {
            ts_id: 0x4999,
            frame: 7,
            slot: 47,
            bytes: crate::source::null_packet(),
            corrected: Some(0),
        });
        v.received(0, &d);
        v.finish();
        assert_eq!(v.foreign, 1);
        assert!(!v.pass());
    }
}
