//! Opt-in test on ISDB-S recordings: `ISDB_RS_ISDBS_TMCC_IQ` lists SigMF
//! recordings of ISDB-S transponders (paths separated by `:`). Every
//! recording must give consecutive frames with no slip, at least one whole
//! superframe, RS(64,48) success on every superframe, and TMCC
//! information that validates (BO.1408-1 §3.2, annexed table 48) and stays
//! the same across the recording. Prints `SKIP` when the variable is
//! unset. See docs/test-data.md.

use isdb_iq::Recording;
use isdbs_phy::{RxOptions, receive};

#[test]
fn isdbs_tmcc_recordings() {
    let Some(list) = std::env::var_os("ISDB_RS_ISDBS_TMCC_IQ") else {
        eprintln!(
            "SKIP: ISDB_RS_ISDBS_TMCC_IQ not set (SigMF recordings of ISDB-S transponders, ':'-separated)"
        );
        return;
    };
    for path in std::env::split_paths(&list) {
        let mut rec = Recording::open(&path, None, None).unwrap();
        let seg = rec.segments()[0].clone();
        let x = rec
            .read(seg.start, usize::try_from(seg.end - seg.start).unwrap())
            .unwrap();
        let s = isdbs_phy::frontend::symbols(&x, rec.sample_rate, None).unwrap();
        let r = receive(&s.z, &RxOptions::default());
        let ok = r.superframes.iter().filter(|sf| sf.info.is_some()).count();
        println!(
            "{}: {} frames, {} slips, {} superframes, RS ok {ok}",
            path.display(),
            r.frames.len(),
            r.slips,
            r.superframes.len()
        );
        assert_eq!(r.slips, 0, "{}", path.display());
        assert!(!r.superframes.is_empty(), "{}", path.display());
        assert_eq!(ok, r.superframes.len(), "{}", path.display());
        let first = r.superframes[0].info.as_ref().unwrap();
        first.validate().unwrap();
        for sf in &r.superframes {
            assert_eq!(sf.info.as_ref(), Some(first), "{}", path.display());
        }
        for w in r.frames.windows(2) {
            assert_eq!(
                w[1].mark.start - w[0].mark.start,
                isdbs_core::frame::FRAME_SYMBOLS,
                "{}",
                path.display()
            );
        }
    }
}
