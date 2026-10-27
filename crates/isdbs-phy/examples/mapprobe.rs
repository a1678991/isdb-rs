//! Decode the main signal of a recording's superframes under every
//! constellation reading of `demod::Mapping` and count RS(204,188)
//! successes, to settle the readings the texts leave open (development
//! tool; SPEC-NOTES "qpsk-map", "tc8psk-map").
//!
//! cargo run --release -p isdbs-phy --example mapprobe -- REC [SUPERFRAMES]

use isdb_fec::RS_204_188;
use isdb_iq::Recording;
use isdbs_core::frame::FRAMES;
use isdbs_core::mainsig::codewords;
use isdbs_phy::demod::{DemodOptions, Mapping, decode_frame};
use isdbs_phy::{RxOptions, receive};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("REC"));
    let max_sf: usize = args.next().map_or(1, |s| s.parse().unwrap());
    let mut rec = Recording::open(&path, None, None).unwrap();
    let seg = rec.segments()[0].clone();
    let x = rec
        .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
        .unwrap();
    let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
    let r = receive(&s.z, &RxOptions::default());
    println!(
        "{}: {} frames, {} superframes",
        path.display(),
        r.frames.len(),
        r.superframes.len()
    );
    let variants: Vec<(String, Mapping)> = [false, true]
        .iter()
        .flat_map(|&cj| {
            [false, true].iter().flat_map(move |&q| {
                [false, true].iter().map(move |&t| {
                    (
                        format!("conj={cj:<5} qpsk_p1_on_q={q:<5} tc8psk_c1_is_y={t:<5}"),
                        Mapping {
                            conjugate: cj,
                            qpsk_p1_on_q: q,
                            tc8psk_c1_is_y: t,
                        },
                    )
                })
            })
        })
        .collect();
    for (name, m) in &variants {
        let o = DemodOptions {
            mapping: *m,
            ..DemodOptions::default()
        };
        let (mut ok, mut corr, mut bad, mut mer) = (0, 0, 0, Vec::new());
        for sf in r.superframes.iter().take(max_sf) {
            let Some(info) = &sf.info else { continue };
            let fb: Vec<_> = (0..FRAMES)
                .map(|j| decode_frame(&r.frames[sf.first_frame + j].main, info, j == 0, &o))
                .collect();
            mer.extend(fb.iter().map(|f| f.mer_db));
            let refs: [&[u8]; FRAMES] = std::array::from_fn(|j| fb[j].bytes.as_slice());
            for (_, _, mut cw) in codewords(info, &refs) {
                match RS_204_188.decode(&mut cw) {
                    Ok(0) => ok += 1,
                    Ok(_) => corr += 1,
                    Err(_) => bad += 1,
                }
            }
        }
        let mer = mer.iter().sum::<f64>() / mer.len().max(1) as f64;
        println!(
            "  {name}: RS clean {ok:5} corrected {corr:5} uncorrectable {bad:5}; decided MER {mer:.1} dB"
        );
    }
}
