//! Opt-in tests on satellite IF recordings (SigMF): `ISDB_RS_ISDBS_IQ`
//! names an ISDB-S transponder, `ISDB_RS_ISDBS3_IQ` an ISDB-S3 one (as for
//! `isdb-iq`). Each test prints a `SKIP` line and passes when its variable
//! is unset. See docs/test-data.md.

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
