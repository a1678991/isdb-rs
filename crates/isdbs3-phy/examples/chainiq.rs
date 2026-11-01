//! The fixed-point chain on IQ recordings (development tool, plan
//! isdbs3-fpga-rx piece 3): each SigMF recording through the acquisition
//! and the bit-exact front end (`frontend::acquire`, then
//! `FastFrontend` with the chosen carrier and trim, its 6-bit output `q`:
//! what the FPGA sends), then the chain. Compared with the float side on
//! the same symbols (`q` as complex values): `s3sync::measure` (slot
//! offset, SSync parity), `sync::synchronise` (frame heads), and
//! `rx::receive` (slot bytes, after the chain's layout lag).
//!
//! cargo run --release -p isdbs3-phy --example chainiq -- REC.sigmf-meta [...]

use isdb_frontend::fast::FastFrontend;
use isdb_frontend::params::Params;
use isdbs3_core::params::{MOD_SLOT, SYMBOL_RATE};
use isdbs3_phy::Cplx;
use isdbs3_phy::chain::{Chain, ChainOptions, Output};
use isdbs3_phy::frontend::{Trim, acquire, design, to_input};
use isdbs3_phy::rx::{RxOptions, receive};
use std::collections::BTreeMap;

fn main() {
    for path in std::env::args().skip(1) {
        let mut rec = isdb_iq::Recording::open(std::path::Path::new(&path), None, None)
            .expect("open recording");
        let seg = rec.segments()[0].clone();
        let n = usize::try_from(seg.end - seg.start).unwrap();
        let x = rec.read(seg.start, n).expect("read");
        let fs = rec.sample_rate;
        let p = acquire(&x, fs, None, Trim::Auto).expect("acquire");
        let mut d = design(fs, p.carrier_offset_hz);
        d.trim = p.trim;
        let mut fe = FastFrontend::new(Params::design(&d).unwrap()).unwrap();
        let mut q: Vec<[i8; 2]> = Vec::new();
        fe.process_each(&to_input(&x), |s| q.push(s.q));
        let z: Vec<Cplx> = q
            .iter()
            .map(|s| Cplx::new(f64::from(s[0]), f64::from(s[1])))
            .collect();
        // the float side
        let m = isdb_frontend::s3sync::measure(&z).expect("s3sync");
        let sy = isdbs3_phy::sync::synchronise(&z).expect("sync");
        let r = receive(&z, &RxOptions::default());
        // the chain
        let mut chain = Chain::new(ChainOptions {
            trace: true,
            ..ChainOptions::default()
        });
        let mut out = Vec::new();
        for &s in &q {
            chain.push(s, &mut out);
        }
        let mut heads = Vec::new();
        let mut frames = Vec::new();
        for o in out {
            match o {
                Output::Head(h) => heads.push(h),
                Output::Frame(f) => frames.push(f),
                _ => {}
            }
        }
        let st = chain.sync_stats();
        let ss_float: Vec<usize> = sy
            .slots
            .iter()
            .enumerate()
            .filter(|(k, _)| k % 2 == sy.ssync_parity)
            .map(|(_, s)| s.at)
            .collect();
        let ss_chain: Vec<usize> = heads
            .iter()
            .filter(|h| h.word == isdbs3_phy::chain::sync::Word::Ssync)
            .map(|h| usize::try_from(h.at).unwrap())
            .collect();
        let ss_common = ss_chain.iter().filter(|a| ss_float.contains(a)).count();
        let fh_float: Vec<usize> = sy.frames.iter().map(|&f| sy.slots[f].at).collect();
        let fh_chain: Vec<usize> = frames
            .iter()
            .map(|f| usize::try_from(f.at).unwrap())
            .collect();
        let fh_common = fh_chain.iter().filter(|a| fh_float.contains(a)).count();
        // slot bytes: the float receiver's frames by start, against the chain's
        let mut rf: BTreeMap<usize, BTreeMap<usize, Option<Vec<u8>>>> = BTreeMap::new();
        for s in &r.slots {
            rf.entry(r.frame_starts[s.frame])
                .or_default()
                .insert(s.slot, s.data.clone());
        }
        let (mut cmp, mut eq) = (0, 0);
        for f in &frames {
            let Some(rs) = rf.get(&usize::try_from(f.at).unwrap()) else {
                continue;
            };
            for s in f.slots.iter().filter(|s| !s.dummy) {
                if let Some(Some(d)) = rs.get(&usize::from(s.slot)) {
                    cmp += 1;
                    eq += usize::from(s.bch_corrected.is_some() && &s.data == d);
                }
            }
        }
        let decoded = frames.iter().filter(|f| !f.slots.is_empty()).count();
        println!("{path}");
        println!(
            "  {:.3} s at {:.2} Msps: carrier {:+.0} Hz, trim {:+.3}, {} symbols ({:.3} s)",
            n as f64 / fs,
            fs / 1e6,
            p.carrier_offset_hz,
            p.trim,
            q.len(),
            q.len() as f64 / SYMBOL_RATE
        );
        println!(
            "  slot offset: s3sync {} (parity {}), chain {} (parity {}); same offset: {}",
            m.offset,
            m.ssync_parity,
            st.offset,
            st.ssync_parity,
            usize::from(st.offset) == m.offset % MOD_SLOT
        );
        println!(
            "  SSync heads: float {}, chain {}, common {ss_common}; frame heads: float {}, chain {}, common {fh_common}",
            ss_float.len(),
            ss_chain.len(),
            fh_float.len(),
            fh_chain.len()
        );
        println!(
            "  frames: float {} (TMCC ok {}), chain {} ({} decoded after the lag); slots compared {cmp}, bytes equal {eq}; chain counters {:?}",
            r.frames.len(),
            r.frames.iter().filter(|f| f.tmcc.is_ok()).count(),
            frames.len(),
            decoded,
            frames.last().map(|f| f.status.counters)
        );
    }
}
