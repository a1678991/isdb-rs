//! Opt-in tests on satellite IF recordings (SigMF): `ISDB_RS_ISDBS_IQ`
//! names an ISDB-S transponder, `ISDB_RS_ISDBS3_IQ` an ISDB-S3 one (as for
//! `isdb-iq`). Each test prints a `SKIP` line and passes when its variable
//! is unset. See docs/test-data.md.
//!
//! `ISDB_RS_ISDBS3_IQ` also runs the timing-trim acceptance
//! ([`isdb_s3_trim_recovers_the_sync_snr`]); give it a recording with the
//! full 56 MHz analog bandwidth (2026-10-25 set or later) for the
//! thresholds below.

use isdb_frontend::fixed::Frontend;
use isdb_frontend::float::FloatFrontend;
use isdb_frontend::format::{Event, Packer, Reader};
use isdb_frontend::metrics::{psk_mer, ring_stats};
use isdb_frontend::params::{Design, ONE, Params};
use isdb_iq::Recording;
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use std::path::PathBuf;

fn check(var: &str, system: usize) {
    let Some(path) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("SKIP: {var} not set (path of a SigMF recording)");
        return;
    };
    let sys = &KNOWN_SYSTEMS[system];
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start).unwrap().min(3 << 20);
    let x = rec.read(seg.start, n).unwrap();
    let sv = survey(&x, fs, &Options::default());
    let cfo = sv.best_carrier().map(|c| c.offset_hz).unwrap();
    let mut d = Design::for_system(sys, fs);
    d.carrier_offset = cfo;
    let p = Params::design(&d).unwrap();
    let input: Vec<[i16; 2]> = x
        .iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect();
    let mut fe = Frontend::new(p.clone()).unwrap();
    let mut syms = Vec::new();
    fe.process(&input, &mut syms);
    let mut ff = FloatFrontend::new(p.clone(), d.sps(), d.roll_off, d.kaiser_beta);
    let fsyms: Vec<_> = input
        .iter()
        .filter_map(|s| ff.push([f64::from(s[0]), f64::from(s[1])]))
        .collect();
    let settle = 40_000;
    let z: Vec<[f64; 2]> = syms[settle..]
        .iter()
        .map(|s| [f64::from(s.z[0]), f64::from(s.z[1])])
        .collect();
    let zf: Vec<[f64; 2]> = fsyms[settle..].iter().map(|s| s.z).collect();
    // Timing lock: the symbol rate within 5 ppm of the survey's.
    let pm = syms[settle..]
        .iter()
        .map(|s| f64::from(s.period))
        .sum::<f64>()
        / (syms.len() - settle) as f64
        / ONE as f64;
    let survey_rate = sv
        .system
        .map(|m| m.symbol_rate_hz * (1.0 + m.ppm * 1e-6))
        .unwrap();
    let ppm = (fs / pm / survey_rate - 1.0) * 1e6;
    let (rf, rx) = (ring_stats(&zf, 1).mer_db, ring_stats(&z, 1).mer_db);
    eprintln!(
        "{}: {} symbols, rate {ppm:+.2} ppm from the survey, ring MER fixed {rx:.2} dB float {rf:.2} dB",
        path.display(),
        syms.len()
    );
    assert!(ppm.abs() < 5.0, "timing not locked: {ppm:+.2} ppm");
    // The fixed-point arithmetic costs at most 0.1 dB.
    assert!(rf - rx < 0.1, "fixed {rx:.2} dB, float {rf:.2} dB");
    if sys.roll_off >= 0.1 {
        let m = psk_mer(&z, 8, 2e-3, 20_000);
        eprintln!("  8PSK decision-directed MER {:.2} dB", m.mer_db);
        assert!(m.mer_db > 10.0, "{m:?}");
    }
    // The words read back to the same output symbols, no loss.
    let mut pk = Packer::new(&p);
    let mut words = Vec::new();
    for s in &syms {
        pk.push(s.q, s.gain, s.period, &mut words);
    }
    let mut ev = Vec::new();
    let mut r = Reader::new();
    r.feed(&words, &mut ev);
    r.finish(&mut ev);
    let back: Vec<[i8; 2]> = ev
        .iter()
        .flat_map(|e| match e {
            Event::Block { symbols, .. } => symbols.clone(),
            Event::Loss { .. } => panic!("loss in a clean stream"),
            Event::Truncated { .. } => Vec::new(),
        })
        .collect();
    assert_eq!(back.len(), syms.len() >> p.block_log2 << p.block_log2);
    assert!(back.iter().zip(&syms).all(|(a, b)| *a == b.q));
}

#[test]
fn isdb_s_recording() {
    check("ISDB_RS_ISDBS_IQ", 0);
}

#[test]
fn isdb_s3_recording() {
    check("ISDB_RS_ISDBS3_IQ", 1);
}

/// The trim measured on 0.1 s raises the sync-symbol Es/N0 over the rest
/// of the recording by more than 3 dB and above 13 dB (measured: 9.3 →
/// 14.6 dB on BS7, 11 and 17 at 61.44 Msps, 56 MHz), and the 6-bit output
/// loses less than 0.15 dB of it.
#[test]
fn isdb_s3_trim_recovers_the_sync_snr() {
    use isdb_frontend::s3sync::{self, SLOT};
    use num_complex::Complex64 as C;
    let var = "ISDB_RS_ISDBS3_IQ";
    let Some(path) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("SKIP: {var} not set (path of a SigMF recording)");
        return;
    };
    let mut rec = Recording::open(&path, None, None).unwrap();
    let fs = rec.sample_rate;
    let seg = rec.segments()[0].clone();
    let n = usize::try_from(seg.end - seg.start).unwrap().min(12 << 20);
    let x = rec.read(seg.start, n).unwrap();
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let mut d = Design::for_system(&KNOWN_SYSTEMS[1], fs);
    d.carrier_offset = sv.best_carrier().map(|c| c.offset_hz).unwrap();
    let input: Vec<[i16; 2]> = x
        .iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect();
    let excerpt = ((0.1 * fs) as usize).min(input.len());
    let cands: Vec<f64> = (-6..=1).map(|i| f64::from(i) * 0.05).collect();
    let (trim, _) = s3sync::best_trim(&input[..excerpt], &d, &cands).unwrap();
    let run = |trim: f64| {
        let p = Params::design(&Design { trim, ..d.clone() }).unwrap();
        let mut fe = Frontend::new(p.clone()).unwrap();
        let mut out = Vec::new();
        fe.process(&input, &mut out);
        let zk: Vec<C> = out
            .iter()
            .skip(3 * SLOT)
            .map(|s| C::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        let q: Vec<C> = out
            .iter()
            .skip(3 * SLOT)
            .map(|s| C::new(f64::from(s.q[0]), f64::from(s.q[1])))
            .collect();
        (
            s3sync::measure(&zk).unwrap().snr_db,
            s3sync::measure(&q).unwrap().snr_db,
        )
    };
    let (plain, _) = run(0.0);
    let (trimmed, quantised) = run(trim);
    eprintln!(
        "{}: trim {trim:+.3} T; sync Es/N0 {plain:.2} dB untrimmed, {trimmed:.2} dB trimmed, {quantised:.2} dB at {} bits",
        path.display(),
        d.bits
    );
    assert!(trimmed - plain > 3.0, "{plain:.2} → {trimmed:.2} dB");
    assert!(trimmed > 13.0, "{trimmed:.2} dB");
    assert!(
        trimmed - quantised < 0.15,
        "{trimmed:.2} → {quantised:.2} dB"
    );
}
