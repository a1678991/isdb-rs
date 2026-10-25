//! Opt-in tests on ISDB-S3 recordings: `ISDB_RS_ISDBS3_TLV_IQ` names one or
//! more SigMF recordings (`:`-separated) with the full 56 MHz analog
//! bandwidth (e.g. `isdb capture --freq BS7 --rate 61.44M --bw 56M`). Each
//! test prints a `SKIP` line and passes when the variable is unset. See
//! docs/test-data.md.

use isdb_iq::Recording;
use isdb_tlv::Assembler;
use isdb_tlv::stats::StreamStats;
use isdbs3_phy::frontend::{Trim, symbols};
use isdbs3_phy::rx::{RxOptions, receive};
use std::collections::BTreeMap;

/// Every whole frame's TMCC decodes with a consistent allocation, every
/// valid slot is LDPC- and BCH-clean, the equaliser raises the sync Es/N0
/// by more than 1.5 dB, and every TLV stream reassembles with no
/// head-indication mismatch, no IP error and only MMTP in its UDP
/// payloads.
#[test]
fn isdbs3_tlv_recordings() {
    let Ok(list) = std::env::var("ISDB_RS_ISDBS3_TLV_IQ") else {
        eprintln!("SKIP isdbs3_tlv_recordings: ISDB_RS_ISDBS3_TLV_IQ not set");
        return;
    };
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let mut rec = Recording::open(std::path::Path::new(path), None, None).unwrap();
        let seg = rec.segments()[0].clone();
        let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
        let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
        assert!(s.survey_says_isdbs3, "{path}: survey");
        let r = receive(&s.z, &RxOptions::default());
        assert!(r.frames.len() >= 2, "{path}: {} frames", r.frames.len());
        for f in &r.frames {
            let t = f
                .tmcc
                .as_ref()
                .unwrap_or_else(|e| panic!("{path}: TMCC {e}"));
            assert!(t.allocation_ok(), "{path}: allocation");
        }
        let eq = r.sync_snr_eq_db.expect("equalised");
        assert!(
            eq > r.sync_snr_db + 1.5,
            "{path}: equaliser {} → {eq}",
            r.sync_snr_db
        );
        let bad = r.slots.iter().filter(|s| s.data.is_none()).count();
        assert_eq!(bad, 0, "{path}: {bad} of {} slots failed", r.slots.len());
        let mut asm: BTreeMap<u16, Assembler> = BTreeMap::new();
        let mut st: BTreeMap<u16, StreamStats> = BTreeMap::new();
        for sl in &r.slots {
            let mut out = Vec::new();
            asm.entry(sl.stream_id)
                .or_default()
                .push_slot(sl.data.as_deref(), sl.head, &mut out);
            let e = st.entry(sl.stream_id).or_default();
            out.iter().for_each(|p| e.add(p));
        }
        for (id, a) in &asm {
            let e = &st[id];
            assert_eq!(a.stats.pointer_mismatches, 0, "{path} {id:#06x}");
            assert_eq!((e.ip_errors, e.non_mmtp), (0, 0), "{path} {id:#06x}");
            eprintln!(
                "{path}: stream {id:#06x}: {} TLV packets, MMT-SI tables {:?}",
                a.stats.packets,
                e.mmt_tables.keys().collect::<Vec<_>>()
            );
        }
        eprintln!(
            "{path}: {} frames, {} slots clean, sync Es/N0 {:.2} → {eq:.2} dB",
            r.frames.len(),
            r.slots.len(),
            r.sync_snr_db
        );
    }
}

/// On the same recordings: the fixed-point LDPC decoder gives every slot
/// the same iterations, convergence and data with 16 codewords per lane
/// group (SIMD) as one at a time (the scalar reference), and decodes at
/// least as many slots as the floating-point reference.
#[test]
fn isdbs3_ldpc_simd_equals_scalar_on_recordings() {
    use isdbs3_phy::main_signal::Ldpc;
    let Ok(list) = std::env::var("ISDB_RS_ISDBS3_TLV_IQ") else {
        eprintln!(
            "SKIP isdbs3_ldpc_simd_equals_scalar_on_recordings: ISDB_RS_ISDBS3_TLV_IQ not set"
        );
        return;
    };
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let mut rec = Recording::open(std::path::Path::new(path), None, None).unwrap();
        let seg = rec.segments()[0].clone();
        let x = rec.read(seg.start, (seg.end - seg.start) as usize).unwrap();
        let s = symbols(&x, rec.sample_rate, None, Trim::Auto).unwrap();
        let run = |ldpc| {
            receive(
                &s.z,
                &RxOptions {
                    ldpc,
                    ..RxOptions::default()
                },
            )
        };
        let (simd, scalar, float) = (run(Ldpc::FIXED), run(Ldpc::SCALAR), run(Ldpc::FLOAT));
        assert_eq!(simd.slots.len(), scalar.slots.len(), "{path}");
        assert!(!simd.slots.is_empty(), "{path}: no slots");
        for (a, b) in simd.slots.iter().zip(&scalar.slots) {
            assert_eq!(
                (a.frame, a.slot, a.ldpc_iterations, a.bch_corrected, &a.data),
                (b.frame, b.slot, b.ldpc_iterations, b.bch_corrected, &b.data),
                "{path}"
            );
        }
        let ok = |r: &isdbs3_phy::rx::Rx| {
            r.slots
                .iter()
                .filter(|s| s.ldpc_iterations.is_some())
                .count()
        };
        assert!(
            ok(&simd) >= ok(&float),
            "{path}: fixed {} < float {}",
            ok(&simd),
            ok(&float)
        );
        eprintln!(
            "{path}: {} slots, SIMD = scalar; LDPC-clean fixed {} / float {}",
            simd.slots.len(),
            ok(&simd),
            ok(&float)
        );
    }
}
