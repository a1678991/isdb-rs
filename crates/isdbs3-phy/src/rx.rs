//! The ISDB-S3 receiver over a block of symbols: synchronise, decode each
//! whole frame's TMCC and main signal, and hand every slot's data, in
//! order, to the caller with its stream identity.

use crate::Cplx;
use crate::main_signal::{
    FrameInput, Ldpc, decode_frames, learned_points, main_symbols, nominal_points, pilots,
};
use crate::sync::{synchronise, synchronise_at};
use crate::tmcc;
use isdbs3_core::slots::{Slot, layout};
use isdbs3_core::tmcc::TmccInfo;

/// Receiver options.
#[derive(Debug, Clone)]
pub struct RxOptions {
    /// Maximum LDPC iterations of the TMCC.
    pub max_iter: usize,
    /// The main signal's LDPC decoder.
    pub ldpc: Ldpc,
    /// Demap against the pilot-learned constellation (else the nominal
    /// one of B44 §3.8).
    pub learned_constellation: bool,
    /// Equalise the block with taps trained on the first frame's sync and
    /// pilot symbols ([`crate::eq`]).
    pub equalise: bool,
    /// Where the first slot head is expected (a stream cut on a frame
    /// boundary starts with one at 0): the synchronisation measures there
    /// instead of searching every offset of a slot, and searches only if
    /// no slot sync is found there.
    pub slot_offset: Option<usize>,
}

impl Default for RxOptions {
    fn default() -> Self {
        Self {
            max_iter: 50,
            ldpc: Ldpc::FIXED,
            learned_constellation: true,
            equalise: true,
            slot_offset: None,
        }
    }
}

/// One decoded slot, in transmission order.
#[derive(Debug, Clone)]
pub struct RxSlot {
    /// Frame number within this block (0 = first whole frame).
    pub frame: usize,
    /// Slot index 0 … 119.
    pub slot: usize,
    /// Relative stream number (TMCC).
    pub relative_stream: u8,
    /// Transmission stream id (TLV stream id or TS_ID) of that stream.
    pub stream_id: u16,
    /// Stream type (TMCC: 1 TS, 2 TLV).
    pub stream_type: u8,
    /// Head-TLV indication from the slot header (first 16 bits).
    pub head: u16,
    /// The slot's data bytes, `None` if LDPC or BCH failed.
    pub data: Option<Vec<u8>>,
    /// LDPC iterations (None: not converged).
    pub ldpc_iterations: Option<usize>,
    /// BCH bits corrected (None: uncorrectable).
    pub bch_corrected: Option<usize>,
}

/// One frame's summary.
#[derive(Debug, Clone)]
pub struct RxFrame {
    /// Index of its slot #1 among the block's slots.
    pub slot_index: usize,
    /// Its TMCC, or the error.
    pub tmcc: Result<TmccInfo, tmcc::Error>,
    /// TMCC LDPC iterations.
    pub tmcc_iterations: Option<usize>,
    /// MER of the main symbols against the constellation used, dB.
    pub mer_db: Option<f64>,
}

/// What [`receive`] found.
#[derive(Debug, Clone, Default)]
pub struct Rx {
    /// Es/N0 on the sync symbols before equalisation, dB.
    pub sync_snr_db: f64,
    /// Es/N0 on the sync symbols after equalisation, dB (when equalised).
    pub sync_snr_eq_db: Option<f64>,
    /// The equaliser taps used.
    pub taps: Vec<Cplx>,
    /// Whole frames found.
    pub frames: Vec<RxFrame>,
    /// Every valid slot of every frame whose TMCC decoded, in order.
    pub slots: Vec<RxSlot>,
    /// Index in `z` of the first symbol (slot #1's sync) of every whole
    /// frame the synchronisation found, decoded or not: where a caller
    /// streaming symbols can cut its buffer on a frame boundary.
    pub frame_starts: Vec<usize>,
}

/// Lowest sync-symbol Es/N0 at which [`RxOptions::slot_offset`] is
/// trusted, dB (else the offset is searched).
pub const HINT_MIN_SNR_DB: f64 = 3.0;

/// Run the receiver over symbols `z` (one per symbol, any scale).
pub fn receive(z: &[Cplx], opt: &RxOptions) -> Rx {
    // the hint holds when the sync words correlate there (≈ 13 dB on a live
    // stream; a wrong offset leaves them uncorrelated, ≤ 0 dB)
    let hinted = opt
        .slot_offset
        .and_then(|o| synchronise_at(z, o))
        .filter(|s| s.sync_snr_db > HINT_MIN_SNR_DB);
    let Some(sy0) = hinted.or_else(|| synchronise(z)) else {
        return Rx::default();
    };
    let mut rx = Rx {
        sync_snr_db: sy0.sync_snr_db,
        ..Rx::default()
    };
    // equalise with taps trained on the first frame whose TMCC decodes
    let mut zeq = None;
    if opt.equalise {
        let es = 10f64.powf(sy0.sync_snr_db / 10.0);
        for &f in &sy0.frames {
            let Some(l) = tmcc::llrs(z, &sy0.slots, f, 1.0 / (2.0 * es)) else {
                break;
            };
            let Some(lay) = tmcc::decode(&l, opt.max_iter)
                .ok()
                .and_then(|t| layout(&t.info))
            else {
                continue;
            };
            if let Some(w) = crate::eq::train(z, &sy0.slots, f, &lay) {
                zeq = Some(crate::eq::apply(z, &w));
                rx.taps = w;
            }
            break;
        }
    }
    let z: &[Cplx] = zeq.as_deref().unwrap_or(z);
    let sy = if zeq.is_some() {
        // the equaliser's output is aligned with its input (centred taps):
        // the slot heads are where they were, only their phases and the
        // SNR change
        let Some(s) = synchronise_at(z, sy0.offset) else {
            return rx;
        };
        rx.sync_snr_eq_db = Some(s.sync_snr_db);
        s
    } else {
        sy0
    };
    rx.frame_starts = sy.frames.iter().map(|&f| sy.slots[f].at).collect();
    let es_n0 = 10f64.powf(sy.sync_snr_db / 10.0);
    // Each frame's TMCC, main symbols and pilots, frames in parallel; then
    // the frames in order up to the first incomplete one, as a sequential
    // pass would stop there.
    let preps: Vec<Prep> = parallel_map(&sy.frames, |&f| prepare(z, &sy.slots, f, es_n0, opt));
    let mut ready: Vec<Ready> = Vec::new();
    let mut order: Vec<Result<usize, RxFrame>> = Vec::new(); // index into `ready`, or a frame without slots
    for (fi, (prep, &f)) in preps.into_iter().zip(&sy.frames).enumerate() {
        match prep {
            Prep::Stop => break,
            Prep::TmccError(e) => order.push(Err(RxFrame {
                slot_index: f,
                tmcc: Err(e),
                tmcc_iterations: None,
                mer_db: None,
            })),
            Prep::NoLayout(t) => order.push(Err(RxFrame {
                slot_index: f,
                tmcc: Ok(t.info),
                tmcc_iterations: Some(t.ldpc_iterations),
                mer_db: None,
            })),
            Prep::Ready { t, lay, ms, pil } => {
                order.push(Ok(ready.len()));
                ready.push((fi, f, t, lay, ms, pil));
            }
        }
    }
    let pts: Vec<_> = ready
        .iter()
        .map(|(_, _, _, lay, _, pil)| {
            move |m, r| {
                if opt.learned_constellation
                    && let Some(p) = learned_points(pil, lay, m)
                {
                    return p;
                }
                nominal_points(m, r)
            }
        })
        .collect();
    let n0s: Vec<f64> = ready
        .iter()
        .zip(&pts)
        .map(|((_, _, _, lay, ms, _), p)| noise_of(ms, lay, p))
        .collect();
    let inputs: Vec<FrameInput<'_>> = ready
        .iter()
        .zip(&pts)
        .zip(&n0s)
        .map(|(((_, _, _, lay, ms, _), p), &n0)| FrameInput {
            ms,
            layout: lay,
            n0,
            pts_for: p,
        })
        .collect();
    let mut decoded = decode_frames(&inputs, &opt.ldpc).into_iter();
    let mut n0s = n0s.into_iter();
    let mut ready = ready.into_iter();
    for o in order {
        let Ok(_) = o else {
            rx.frames.push(o.expect_err("frame without slots"));
            continue;
        };
        let (fi, f, t, lay, _, _) = ready.next().expect("one per ready frame");
        let n0 = n0s.next().expect("one per ready frame");
        for d in decoded.next().expect("one per ready frame") {
            let s: &Slot = &lay[d.slot];
            let rel = t.info.slot_stream[d.slot];
            let head = u16::from_be_bytes([bits_byte(&d.header, 0), bits_byte(&d.header, 1)]);
            let ok = d.bch_corrected.is_some();
            rx.slots.push(RxSlot {
                frame: fi,
                slot: s.index,
                relative_stream: rel,
                stream_id: t.info.stream_id[usize::from(rel & 0x0F)],
                stream_type: t.info.stream_type[usize::from(rel & 0x0F)],
                head,
                data: ok.then_some(d.data),
                ldpc_iterations: d.ldpc_iterations,
                bch_corrected: d.bch_corrected,
            });
        }
        rx.frames.push(RxFrame {
            slot_index: f,
            tmcc: Ok(t.info),
            tmcc_iterations: Some(t.ldpc_iterations),
            mer_db: Some(-10.0 * n0.log10()),
        });
    }
    rx
}

/// A frame ready to decode: its index among the block's frames, its slot
/// #1 index, TMCC, layout, main symbols and pilots.
type Ready = (usize, usize, tmcc::Decoded, Vec<Slot>, Vec<Cplx>, Vec<Cplx>);

/// One frame before its main signal is decoded.
enum Prep {
    /// The frame is incomplete: this and every later frame are dropped.
    Stop,
    /// Its TMCC failed.
    TmccError(tmcc::Error),
    /// Its TMCC decoded but gives no slot layout.
    NoLayout(tmcc::Decoded),
    /// Ready to decode.
    Ready {
        t: tmcc::Decoded,
        lay: Vec<Slot>,
        ms: Vec<Cplx>,
        pil: Vec<Cplx>,
    },
}

fn prepare(
    z: &[Cplx],
    slots: &[crate::sync::SlotHead],
    f: usize,
    es_n0: f64,
    opt: &RxOptions,
) -> Prep {
    let Some(l) = tmcc::llrs(z, slots, f, 1.0 / (2.0 * es_n0)) else {
        return Prep::Stop;
    };
    let t = match tmcc::decode(&l, opt.max_iter) {
        Ok(d) => d,
        Err(e) => return Prep::TmccError(e),
    };
    let Some(lay) = layout(&t.info) else {
        return Prep::NoLayout(t);
    };
    match (main_symbols(z, slots, f), pilots(z, slots, f)) {
        (Some(ms), Some(pil)) => Prep::Ready { t, lay, ms, pil },
        _ => Prep::Stop,
    }
}

/// `f` over `items` on all cores, results in order.
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = items.len().div_ceil(workers).max(1);
    let f = &f;
    std::thread::scope(|sc| {
        let hs: Vec<_> = items
            .chunks(chunk)
            .map(|c| sc.spawn(move || c.iter().map(f).collect::<Vec<R>>()))
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().expect("frame thread"))
            .collect()
    })
}

fn bits_byte(bits: &[u8], i: usize) -> u8 {
    bits[8 * i..8 * i + 8]
        .iter()
        .fold(0u8, |a, &b| (a << 1) | (b & 1))
}

/// Mean squared distance of the main symbols to their nearest point of
/// their slot's constellation (a sample of each slot): the N0 the
/// receiver demaps with.
pub fn noise_of(
    ms: &[Cplx],
    lay: &[Slot],
    pts_for: &dyn Fn(isdbs3_core::Modulation, isdbs3_core::CodeRate) -> Vec<Cplx>,
) -> f64 {
    let (mut e, mut n) = (0.0, 0usize);
    for s in lay.iter().filter(|s| !s.dummy) {
        let pts = pts_for(s.modulation, s.rate);
        for y in ms[s.symbol_start..s.symbol_start + s.symbols]
            .iter()
            .step_by(16)
        {
            e += pts
                .iter()
                .map(|p| (y - p).norm_sqr())
                .fold(f64::MAX, f64::min);
            n += 1;
        }
    }
    e / n.max(1) as f64
}
