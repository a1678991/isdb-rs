//! ISDB-S3 slot synchronisation and SNR from the known π/2-shift BPSK
//! synchronisation symbols, on the `Symbols` file the front end writes
//! (`isdb iq symbols --system isdb-s3 --out FILE`): whether a recording is
//! good enough to decode, before any TMCC or LDPC decoder exists.
//!
//! ARIB STD-B44 v2.1 Fig. 3.3-1: a modulation slot is 24 synchronisation
//! symbols (frame sync FSync 52F866h or its inverse !FSync AD0799h at the
//! head of odd slots, slot sync SSync 36715Ah at the head of even slots),
//! 32 pilot symbols, then 66 × (136 main + 4 TMCC) = 9 296 symbols; 120
//! slots make a frame. §3.8: π/2-shift BPSK puts 0 / 1 at +45° / −135° on
//! odd symbols and turns them 90° counter-clockwise on even ones, the first
//! symbol of the frame being odd.
//!
//! The 24 sync symbols of every slot are found by correlation at the slot
//! period. !FSync is FSync inverted, so the two are told apart only
//! against a phase reference: the carrier phase is taken from the SSync
//! slots (which have no inverted twin), and each odd slot's word from its
//! sign against its SSync neighbours. The error on the known symbols,
//! after removing the phase, gives the SNR (Es/N0 of the sync symbols,
//! which have the main signal's power).
//!
//! Observed on three live transponders (2026-10-25): one odd slot in 60
//! (one slot in 120, i.e. once per frame) has the opposite sign to the
//! other odd slots, so that slot marks the frame. Which of the two words
//! it is depends on this tool's reading of the bit order and of the
//! constellation's reference phase, not yet settled against the spec.
//!
//! cargo run --release -p isdb-frontend --example s3sync -- SYMBOLS_FILE

use isdb_frontend::format::{Event, Reader};
use num_complex::Complex64 as C;
use std::f64::consts::FRAC_PI_4;

const SLOT: usize = 24 + 32 + 66 * (136 + 4);
const FSYNC: u32 = 0x52_F866;
const NFSYNC: u32 = 0xAD_0799;
const SSYNC: u32 = 0x36_715A;

/// The 24 sync symbols of `word` (MSB first) as unit complex numbers; `odd`
/// tells whether the first one is an odd symbol of the frame. Slot length
/// 9 296 is even, so every slot starts on an odd symbol.
fn sync_symbols(word: u32, odd: bool) -> Vec<C> {
    (0..24)
        .map(|k| {
            let bit = (word >> (23 - k)) & 1;
            let base = if bit == 0 {
                FRAC_PI_4
            } else {
                -3.0 * FRAC_PI_4
            };
            let odd_symbol = (k % 2 == 0) == odd;
            C::from_polar(1.0, base + if odd_symbol { 0.0 } else { 2.0 * FRAC_PI_4 })
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: s3sync SYMBOLS_FILE");
    let bytes = std::fs::read(&path).expect("read symbols file");
    let mut reader = Reader::new();
    let mut events = Vec::new();
    reader.feed_bytes(&bytes, &mut events);
    reader.finish(&mut events);
    let (mut x, mut losses, mut bits) = (Vec::<C>::new(), 0u64, 0u8);
    for e in events {
        match e {
            Event::Block { header, symbols } => {
                bits = header.bits;
                x.extend(
                    symbols
                        .iter()
                        .map(|s| C::new(f64::from(s[0]), f64::from(s[1]))),
                );
            }
            Event::Loss { blocks, .. } => losses += blocks,
            Event::Truncated { .. } => {}
        }
    }
    assert!(x.len() > 3 * SLOT, "too few symbols: {}", x.len());
    let rms = (x.iter().map(C::norm_sqr).sum::<f64>() / x.len() as f64).sqrt();
    x.iter_mut().for_each(|s| *s /= rms);
    println!(
        "{path}: {} symbols ({bits}-bit), {losses} lost blocks",
        x.len()
    );

    let refs = [
        ("FSync", sync_symbols(FSYNC, true)),
        ("!FSync", sync_symbols(NFSYNC, true)),
        ("SSync", sync_symbols(SSYNC, true)),
    ];
    // Slot phase: the offset within one slot period where the strongest
    // sync correlation, summed over every slot of the recording, peaks.
    let slots = (x.len() - 24) / SLOT;
    let corr = |off: usize, r: &[C]| -> f64 {
        r.iter()
            .zip(&x[off..off + 24])
            .map(|(a, b)| b * a.conj())
            .sum::<C>()
            .norm()
            / 24.0
    };
    let (mut best, mut best_off) = (0.0, 0);
    for off in 0..SLOT {
        let s: f64 = (0..slots)
            .map(|k| {
                refs.iter()
                    .map(|(_, r)| corr(off + k * SLOT, r))
                    .fold(0.0, f64::max)
            })
            .sum();
        if s > best {
            (best, best_off) = (s, off);
        }
    }
    let mean_peak = best / slots as f64;
    // Off-peak level: the same statistic at offsets away from the sync.
    let off_level: f64 = (1..=64)
        .map(|j| {
            let off = (best_off + j * 131) % SLOT;
            (0..slots)
                .map(|k| {
                    refs.iter()
                        .map(|(_, r)| corr(off + k * SLOT, r))
                        .fold(0.0, f64::max)
                })
                .sum::<f64>()
                / slots as f64
        })
        .sum::<f64>()
        / 64.0;
    println!(
        "slot sync at symbol {best_off} of {SLOT}: mean |corr| {mean_peak:.3} on the sync, {off_level:.3} elsewhere ({slots} slots)"
    );

    // !FSync = FSync inverted, so the two are told apart only against a
    // phase reference: the SSync slots (no inverted twin). Even slots
    // carry SSync, so first find which parity that is, then take the
    // carrier phase of each odd slot from its SSync neighbours.
    let s_at = |k: usize| best_off + k * SLOT;
    let ss = &refs[2].1;
    let fs = &refs[0].1;
    let rot = |at: usize, r: &[C]| -> C {
        r.iter()
            .zip(&x[at..at + 24])
            .map(|(a, b)| b * a.conj())
            .sum()
    };
    let parity = (0..2)
        .max_by(|&a, &b| {
            let m = |p: usize| {
                (p..slots)
                    .step_by(2)
                    .map(|k| corr(s_at(k), ss))
                    .sum::<f64>()
            };
            m(a).total_cmp(&m(b))
        })
        .unwrap();
    let (mut sig, mut err) = (0.0, 0.0);
    let mut phases = Vec::new();
    let (mut n_f, mut n_nf) = (0usize, 0usize);
    let mut signs = String::new();
    let mut add = |at: usize, r: &[C], ph: C, amp: f64| {
        for (a, b) in r.iter().zip(&x[at..at + 24]) {
            let ideal = a * ph * amp;
            sig += ideal.norm_sqr();
            err += (b - ideal).norm_sqr();
        }
    };
    for k in (parity..slots).step_by(2) {
        let r = rot(s_at(k), ss);
        phases.push((k, r / r.norm()));
        add(s_at(k), ss, r / r.norm(), r.norm() / 24.0);
    }
    for k in (1 - parity..slots).step_by(2) {
        // phase reference: the mean of the neighbouring SSync slots
        let nb: C = phases
            .iter()
            .filter(|(j, _)| j.abs_diff(k) == 1)
            .map(|(_, p)| *p)
            .sum();
        if nb.norm() == 0.0 {
            continue;
        }
        let r = rot(s_at(k), fs);
        // positive projection on the reference: FSync, negative: !FSync
        let inverted = (r * (nb / nb.norm()).conj()).re < 0.0;
        if inverted {
            n_nf += 1;
        } else {
            n_f += 1;
        }
        if signs.len() < 130 {
            signs.push(if inverted { 'f' } else { 'F' });
        }
        add(s_at(k), fs, r / r.norm(), r.norm() / 24.0);
    }
    println!(
        "SSync on slot parity {parity}; odd slots: FSync {n_f} · !FSync {n_nf} (sign against SSync, first odd slots: {signs})"
    );
    let phases: Vec<f64> = phases.iter().map(|(_, p)| p.arg()).collect();
    println!(
        "SNR on the sync symbols: {:.2} dB (Es/N0, {} symbols)",
        10.0 * (sig / err).log10(),
        24 * slots
    );
    // phases are of SSync slots only, two slot periods apart
    let slot_pair = 2.0 * SLOT as f64;
    let steps: Vec<f64> = phases
        .windows(2)
        .map(|w| {
            let d = w[1] - w[0];
            (d + std::f64::consts::PI).rem_euclid(2.0 * std::f64::consts::PI) - std::f64::consts::PI
        })
        .collect();
    let mean = steps.iter().sum::<f64>() / steps.len() as f64;
    let sd = (steps.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / steps.len() as f64).sqrt();
    println!(
        "carrier phase step between SSync slots: mean {:+.4} rad, rms {:.4} rad (a residual offset of {:+.1} Hz at 33.7561 Mbaud)",
        mean,
        sd,
        mean / (2.0 * std::f64::consts::PI) * 33.7561e6 / slot_pair
    );
}
