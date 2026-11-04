//! What `isdb rx --system isdbs3 --radio … --offload auto` does on the
//! host before the radio's receive chain opens, on a synthetic probe or a
//! recording instead of a radio's: the acquisition
//! (`isdbs3_phy::frontend::acquire`) and the equaliser's start taps
//! (`isdbs3_phy::chain::probe::probe_taps`) on the first `PROBE` seconds
//! of IQ (the radio path's 0.2 s), each with its time and the process's
//! peak resident memory over it (`VmHWM`, reset before each stage
//! through `/proc/self/clear_refs`; Linux). Then how the start taps
//! compare with the chain's own solves of the later frames of the same
//! signal (the front end and the chain's model over all of it, as the
//! FPGA's run): per frame with training sums, `‖Δw‖ / ‖w‖` against the
//! frame's own solve (the slow loop's gate allows 0.20), whether the
//! gate takes that solve against the start set, and the training MER
//! (`isdb_radio::chain::fit`) on that frame's sums of the identity, the
//! start taps and the frame's own taps.
//!
//! Development tool for the Slots path's memory (6 GB max RSS on the
//! bench: the acquisition's trim search) and its start taps.
//!
//! The synthetic signal (`isdbs3_phy::chain::synth::iq`): 16APSK 93/120 on
//! every slot, the B44 roll-off, Es/N0 `--snr` dB (default 15), a carrier
//! offset of +120 kHz and a symbol-spaced echo, after `--lead` symbols of
//! filler (default 1 000; up to a frame, 1 115 520, puts the first frame
//! head that much later into the probe).
//!
//! ```text
//! cargo run --release -p isdb-cli --example s3open -- \
//!     [--iq /path/to/rec.sigmf-meta] [--seconds S] [--probe S] \
//!     [--rate HZ] [--snr DB] [--taps N] [--seed N] [--lead SYMBOLS]
//! ```

use isdb_radio::chain::{Gate, Judged, change, check_sums, fit, identity};
use isdbs3_phy::Cplx;
use isdbs3_phy::chain::eq::{solve_sums, to_training};
use isdbs3_phy::chain::probe::probe_taps;
use isdbs3_phy::chain::synth::{IqOptions, SynthOptions, iq};
use isdbs3_phy::chain::{Chain, ChainOptions, Output};
use isdbs3_phy::frontend::{Trim, acquire, design, to_input};

/// `VmHWM` and `VmRSS` of this process, MB (0 where unreadable).
fn memory_mb() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        s.lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            / 1000
    };
    (field("VmHWM:"), field("VmRSS:"))
}

/// A sample or frame count from a product of a rate and a time.
// f64 -> usize has no fallible conversion: `as` truncates toward zero and
// saturates (NaN gives 0), the wanted count for a tool's arguments.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn count(x: f64) -> usize {
    x as usize
}

/// Run `f`, reporting its time and the peak RSS over it (VmHWM reset
/// first; where the reset is refused, the peak since the start).
fn stage<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let reset = std::fs::write("/proc/self/clear_refs", "5").is_ok();
    let (_, before) = memory_mb();
    let t = std::time::Instant::now();
    let r = f();
    let secs = t.elapsed().as_secs_f64();
    let (peak, after) = memory_mb();
    println!(
        "{name}: {secs:.2} s; RSS {before} MB before, peak {peak} MB{}, {after} MB after",
        if reset { "" } else { " (since the start)" }
    );
    r
}

fn mer_db(f: f64) -> f64 {
    10.0 * (1.0 / (1.0 - f)).log10()
}

fn main() {
    let mut a = std::env::args().skip(1);
    let (mut rec, mut seconds, mut probe_s, mut fs, mut snr_db, mut taps) =
        (None, 0.6, 0.2, 40e6, 15.0, 32usize);
    let (mut seed, mut lead) = (7u64, 1_000usize);
    while let Some(k) = a.next() {
        let mut v = || a.next().expect("a value");
        match k.as_str() {
            "--iq" => rec = Some(v()),
            "--seconds" => seconds = v().parse().expect("seconds"),
            "--probe" => probe_s = v().parse().expect("seconds"),
            "--rate" => fs = v().parse().expect("Hz"),
            "--snr" => snr_db = v().parse().expect("dB"),
            "--taps" => taps = v().parse().expect("taps"),
            "--seed" => seed = v().parse().expect("seed"),
            "--lead" => lead = v().parse().expect("symbols"),
            _ => panic!("unknown argument {k}"),
        }
    }
    let x = match &rec {
        Some(p) => {
            let mut r = isdb_iq::Recording::open(std::path::Path::new(p), None, None)
                .expect("open the recording");
            fs = r.sample_rate;
            let seg = r.segments()[0].clone();
            let n =
                count(seconds * fs).min(usize::try_from(seg.end - seg.start).unwrap_or(usize::MAX));
            r.read(seg.start, n).expect("read the recording")
        }
        None => {
            let o = SynthOptions {
                carrier_hz: 120e3,
                snr_db,
                seed,
                lead,
                ..SynthOptions::default()
            };
            let io = IqOptions {
                sample_rate_hz: fs,
                channel: vec![
                    Cplx::new(1.0, 0.0),
                    Cplx::new(0.12, -0.08),
                    Cplx::new(-0.04, 0.03),
                ],
                ..IqOptions::default()
            };
            let frames = count((seconds / 0.033_047).ceil()) + 1;
            let (mut x, _) = stage("synthetic signal", || iq(&o, frames, &io));
            x.truncate(count(seconds * fs));
            x
        }
    };
    let n = count(probe_s * fs).min(x.len());
    println!(
        "{}: {} samples at {:.2} Msps ({:.3} s), the probe its first {n} ({probe_s} s); front end {taps} taps",
        rec.as_deref().unwrap_or("synthetic"),
        x.len(),
        fs / 1e6,
        x.len() as f64 / fs
    );
    let probe = x[..n].to_vec();
    let p = stage("acquisition", || {
        acquire(&probe, fs, None, Trim::Auto).expect("acquire")
    });
    println!(
        "  carrier {:+.0} Hz, trim {:+.3}, sync Es/N0 {:.2} dB ({:?} / {:?})",
        p.carrier_offset_hz, p.trim, p.sync_snr_db, p.trim, p.sync_snr_db
    );
    let mut d = design(fs, p.carrier_offset_hz);
    d.trim = p.trim;
    d.taps = taps;
    let params = isdb_frontend::Params::design(&d).expect("params");
    let start = stage("start taps from the probe", || {
        probe_taps(&probe, &params).expect("probe")
    });
    let Some(start) = start else {
        println!("  no frame of the probe gave training sums: the chain would start on identity");
        return;
    };
    let ts = to_training(&start.sums);
    let f0 = fit(&ts, &start.taps);
    println!(
        "  from the probe's frame {} ({} training symbols) after {} samples ({:.3} s) and {} symbols: sums check {}, fit {f0:.4} (training MER {:.2} dB; identity {:.2} dB); taps {:?}",
        start.sums.frame,
        start.sums.count,
        start.samples,
        start.samples as f64 / fs,
        start.symbols,
        check_sums(&ts, &isdb_radio::chain::SumsCheck::default())
            .map_or("passed".into(), |e| e.to_string()),
        mer_db(f0),
        mer_db(fit(&ts, &identity())),
        start.taps
    );
    // the chain's own solves over the whole signal, as the FPGA's run
    let mut fe = isdb_frontend::fast::FastFrontend::new(params).expect("front end");
    let mut chain = Chain::with_ldpc(
        ChainOptions {
            eq_every: 0,
            ..ChainOptions::default()
        },
        Box::new(isdbs3_phy::chain::ldpc::FixedLdpc::hdl()),
    );
    let mut out = Vec::new();
    let xi = to_input(&x);
    fe.process_each(&xi, |s| {
        chain.set_agc(s.gain);
        chain.push(s.q, &mut out);
    });
    let good = Judged {
        taps: start.taps,
        fit: None,
    };
    let mut worst: f64 = 0.0;
    for o in out {
        let Output::Frame(f) = o else { continue };
        let Some(s) = f.sums else { continue };
        let Some(own) = solve_sums(&s) else { continue };
        let ts = to_training(&s);
        let own_fit = fit(&ts, &own);
        let dw = change(&start.taps, &own);
        worst = worst.max(dw);
        let gate = Gate::default().against(
            &good,
            &Judged {
                taps: own,
                fit: Some(own_fit),
            },
        );
        println!(
            "  frame {:>3}: ‖Δw‖/‖w‖ {dw:.3} against its own solve (gate: {}); training MER identity {:.2} dB, start taps {:.2} dB, its own {:.2} dB",
            s.frame,
            gate.map_or("taken".into(), |r| format!("{r:?}")),
            mer_db(fit(&ts, &identity())),
            mer_db(fit(&ts, &start.taps)),
            mer_db(own_fit)
        );
    }
    println!("largest ‖Δw‖/‖w‖ of the start taps against a later solve: {worst:.3}");
}
