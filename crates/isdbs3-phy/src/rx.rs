//! The ISDB-S3 receiver over a block of symbols: synchronise, decode each
//! whole frame's TMCC and main signal, and hand every slot's data, in
//! order, to the caller with its stream identity.

use crate::Cplx;
use crate::main_signal::{
    Ldpc, decode_frame, learned_points, main_symbols, nominal_points, pilots,
};
use crate::sync::synchronise;
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
}

impl Default for RxOptions {
    fn default() -> Self {
        Self {
            max_iter: 50,
            ldpc: Ldpc::FIXED,
            learned_constellation: true,
            equalise: true,
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
}

/// Run the receiver over symbols `z` (one per symbol, any scale).
pub fn receive(z: &[Cplx], opt: &RxOptions) -> Rx {
    let Some(sy0) = synchronise(z) else {
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
        let Some(s) = synchronise(z) else { return rx };
        rx.sync_snr_eq_db = Some(s.sync_snr_db);
        s
    } else {
        sy0
    };
    let es_n0 = 10f64.powf(sy.sync_snr_db / 10.0);
    for (fi, &f) in sy.frames.iter().enumerate() {
        let Some(l) = tmcc::llrs(z, &sy.slots, f, 1.0 / (2.0 * es_n0)) else {
            break;
        };
        let t = match tmcc::decode(&l, opt.max_iter) {
            Ok(d) => d,
            Err(e) => {
                rx.frames.push(RxFrame {
                    slot_index: f,
                    tmcc: Err(e),
                    tmcc_iterations: None,
                    mer_db: None,
                });
                continue;
            }
        };
        let Some(lay) = layout(&t.info) else {
            rx.frames.push(RxFrame {
                slot_index: f,
                tmcc: Ok(t.info),
                tmcc_iterations: Some(t.ldpc_iterations),
                mer_db: None,
            });
            continue;
        };
        let (Some(ms), Some(pil)) = (main_symbols(z, &sy.slots, f), pilots(z, &sy.slots, f)) else {
            break;
        };
        let pts_for = |m, r| {
            if opt.learned_constellation
                && let Some(p) = learned_points(&pil, &lay, m)
            {
                return p;
            }
            nominal_points(m, r)
        };
        let n0 = noise_of(&ms, &lay, &pts_for);
        let decoded = decode_frame(&ms, &lay, n0, &opt.ldpc, &pts_for);
        for d in decoded {
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
