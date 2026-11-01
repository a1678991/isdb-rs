//! Shared by the LDPC examples (`ldpcfixed`, `ldpcsched`, `ldpcvectors`):
//! the codewords of a recording, demapped as `isdb rx` does.

// each example uses part of this module (the N0 of `Word::Main` only
// `ldpcsched`): what one example leaves unused is not dead
#![allow(dead_code)]

use isdb_iq::Recording;
use isdbs3_core::CodeRate;
use isdbs3_core::slots::layout;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::main_signal::{codeword_llrs, learned_points, main_symbols, pilots};
use isdbs3_phy::sync::synchronise;
use isdbs3_phy::tmcc::{decode, llrs};

/// One codeword of a recording ([`for_each_word`]).
pub enum Word {
    /// A frame's TMCC: its 31 680 LLRs (9 614 data, 22 066 parity; the
    /// shortened word is `isdbs3_phy::tmcc::ldpc_word` of them), passed
    /// before the frame's main codewords.
    Tmcc(Vec<f32>),
    /// A main-signal codeword: its rate, its LLRs in codeword order and
    /// the frame's N0.
    Main(CodeRate, Vec<f32>, f64),
}

/// Call `f` for the codewords of the first `max_frames` whole frames of
/// each segment of the recording at `path` whose TMCC decodes and gives
/// a layout: the TMCC, then every valid slot's codeword. As `isdb rx`:
/// equalised (taps from the first frame whose TMCC decodes), TMCC LLRs
/// from the sync-symbol Es/N0, main symbols demapped against the
/// pilot-learned constellation with the measured N0 (float LLRs,
/// ln P(0)/P(1)).
///
/// `extra_noise` > 0 adds complex Gaussian noise of that variance (per
/// complex symbol, relative to the unit sync amplitude; deterministic
/// seed) to the main symbols before demapping, the pilots and the TMCC
/// left as received, and N0 is measured on the noisy symbols as the
/// receiver would: a recording made worse, to look at the decoder near
/// threshold.
pub fn for_each_word(path: &str, max_frames: usize, extra_noise: f64, mut f: impl FnMut(Word)) {
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut gauss = move || {
        let mut u = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64
        };
        let (u1, u2) = (u() + 1e-300, u());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    };
    let sd = (extra_noise / 2.0).sqrt();
    let mut rec = Recording::open(std::path::Path::new(path), None, None).expect("open");
    let fs = rec.sample_rate;
    for seg in rec.segments().clone() {
        let x = rec
            .read(
                seg.start,
                usize::try_from(seg.end - seg.start).expect("segment length"),
            )
            .expect("read");
        let s = symbols(&x, fs, None, Trim::Auto).expect("front end");
        let Some(sy0) = synchronise(&s.z) else {
            continue;
        };
        let es0 = 10f64.powf(sy0.sync_snr_db / 10.0);
        let w = sy0.frames.iter().find_map(|&f| {
            let l = llrs(&s.z, &sy0.slots, f, 1.0 / (2.0 * es0))?;
            let lay = layout(&decode(&l, 50).ok()?.info)?;
            isdbs3_phy::eq::train(&s.z, &sy0.slots, f, &lay)
        });
        let z = w.map_or(s.z.clone(), |w| isdbs3_phy::eq::apply(&s.z, &w));
        let Some(sy) = synchronise(&z) else { continue };
        let es = 10f64.powf(sy.sync_snr_db / 10.0);
        for &fr in sy.frames.iter().take(max_frames) {
            let Some(l) = llrs(&z, &sy.slots, fr, 1.0 / (2.0 * es)) else {
                continue;
            };
            let Ok(t) = decode(&l, 50) else { continue };
            let Some(lay) = layout(&t.info) else { continue };
            let (Some(mut ms), Some(pil)) =
                (main_symbols(&z, &sy.slots, fr), pilots(&z, &sy.slots, fr))
            else {
                continue;
            };
            let Some(pts) = learned_points(&pil, &lay, lay[0].modulation) else {
                continue;
            };
            f(Word::Tmcc(l));
            if extra_noise > 0.0 {
                for y in &mut ms {
                    *y += isdbs3_phy::Cplx::new(sd * gauss(), sd * gauss());
                }
            }
            let n0 = isdbs3_phy::rx::noise_of(&ms, &lay, &|_, _| pts.clone());
            for sl in lay.iter().filter(|s| !s.dummy) {
                f(Word::Main(sl.rate, codeword_llrs(&ms, sl, &pts, n0), n0));
            }
        }
    }
}
