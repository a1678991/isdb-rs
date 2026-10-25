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

/// Opt-in test of the main signal: `ISDB_RS_ISDBS_TS_IQ` lists SigMF
/// recordings of ISDB-S transponders whose main signal is strong enough to
/// decode (paths separated by `:`). Every decoded superframe must give
/// RS(204,188)-correctable packets only, every TS packet must start with
/// the sync byte, and the continuity counters of each TS_ID must run
/// without a gap. Prints `SKIP` when the variable is unset. See
/// docs/test-data.md.
#[test]
fn isdbs_ts_recordings() {
    use isdbs_phy::demod::DemodOptions;
    use isdbs_phy::ts::decode_all;
    use std::collections::BTreeMap;
    let Some(list) = std::env::var_os("ISDB_RS_ISDBS_TS_IQ") else {
        eprintln!(
            "SKIP: ISDB_RS_ISDBS_TS_IQ not set (SigMF recordings of ISDB-S transponders, ':'-separated)"
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
        let d = decode_all(&r, &DemodOptions::default());
        assert!(!d.is_empty(), "{}", path.display());
        let (mut n, mut bad, mut corr) = (0usize, 0usize, 0usize);
        // (TS_ID, PID) → last continuity_counter of a payload packet
        let mut cc: BTreeMap<(u16, u16), u8> = BTreeMap::new();
        let mut cc_err = 0usize;
        for sf in &d {
            for p in &sf.packets {
                n += 1;
                match p.corrected {
                    None => bad += 1,
                    Some(c) => corr += c,
                }
                assert_eq!(p.bytes[0], 0x47);
                let pid = (u16::from(p.bytes[1] & 0x1F) << 8) | u16::from(p.bytes[2]);
                let has_payload = p.bytes[3] & 0x10 != 0;
                if pid == 0x1FFF || !has_payload {
                    continue;
                }
                let c = p.bytes[3] & 0x0F;
                if let Some(&prev) = cc.get(&(p.ts_id, pid))
                    && c != (prev + 1) & 0x0F
                    && c != prev
                {
                    cc_err += 1;
                }
                cc.insert((p.ts_id, pid), c);
            }
        }
        println!(
            "{}: {} superframes, {n} packets, {corr} bytes corrected, {bad} uncorrectable, {cc_err} CC errors",
            path.display(),
            d.len()
        );
        assert_eq!(bad, 0, "{}", path.display());
        assert_eq!(cc_err, 0, "{}", path.display());
    }
}
