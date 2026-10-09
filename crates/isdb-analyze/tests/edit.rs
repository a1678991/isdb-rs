//! Edit transforms on synthetic streams: every output must be a valid TS
//! (CRC/CC clean per the validator) with packet positions kept.

mod common;
use common::*;
use isdb_analyze::edit::{self, EditOp, EditOptions, NullSlots, Selector};
use isdb_analyze::mutate::Expr;
use isdb_analyze::validate::Severity::Error;

fn clean(p: &[Pkt]) {
    let r = validate(p);
    let bad: Vec<_> = r
        .findings
        .iter()
        .filter(|f| {
            f.severity == Error || f.check.starts_with("section.") || f.check == "transport.cc"
        })
        .collect();
    assert!(bad.is_empty(), "{}", r.to_text(Error));
}

#[test]
fn identity_rewrite_is_byte_exact() {
    for packing in [
        isdb_ts::packetize::Packing::OnePerPacket,
        isdb_ts::packetize::Packing::Packed,
    ] {
        let mut s = default_spec(3);
        s.packing = packing;
        let p = build(&s);
        for pid in [0x0000, 0x0010, 0x0011, 0x0012, 0x0014, PMT_PID] {
            let mut q = p.clone();
            let r = edit::rewrite_pid(&mut q, pid, |_| Ok(None), vec![], NullSlots::WhenNeeded)
                .unwrap();
            assert_eq!(r.dropped, 0);
            assert!(q == p, "pid {pid:#06x} {packing:?}: {r:?}");
        }
    }
}

#[test]
fn null_pid_keeps_positions() {
    let p = build(&default_spec(3));
    let mut q = p.clone();
    let rep = edit::apply(
        &mut q,
        &[EditOp::NullPids(vec![AUDIO_PID])],
        &EditOptions::default(),
    )
    .unwrap();
    assert!(rep[0].nulled_packets > 0);
    assert_eq!(q.len(), p.len());
    let cap = scan(&q);
    assert!(!cap.stats.pids.contains_key(&AUDIO_PID));
    let r = isdb_analyze::inspect::inspect(&cap, &Default::default());
    assert!((r.duration_s.unwrap() - 3.0).abs() < 0.01);
    assert_eq!(r.packets, p.len() as u64);
}

#[test]
fn nit_mutation_diff_shows_exactly_that_field() {
    let p = build(&default_spec(3));
    let mut q = p.clone();
    edit::apply(
        &mut q,
        &[EditOp::Mutate(
            Expr::parse("nit.ts[0].desc[0xFA].guard_interval=1").unwrap(),
        )],
        &EditOptions::default(),
    )
    .unwrap();
    clean(&q);
    let d = isdb_analyze::diff::diff(&scan(&p), &scan(&q), &Default::default());
    let changed: Vec<_> = d.sections.iter().filter(|s| !s.identical).collect();
    assert_eq!(changed.len(), 1, "{}", d.to_text(true));
    assert_eq!(changed[0].key_a.unwrap().table_id, 0x40);
    assert_eq!(changed[0].changes.len(), 1, "{:?}", changed[0].changes);
    let c = &changed[0].changes[0];
    assert!(
        c.path
            .ends_with("descriptors[TerrestrialDeliverySystem].guard_interval"),
        "{}",
        c.path
    );
    assert_eq!((c.a.as_deref(), c.b.as_deref()), (Some("2"), Some("1")));
}

#[test]
fn replace_with_larger_section_uses_null_slots() {
    let p = build(&default_spec(3));
    // A NIT with many frequencies (larger than one packet).
    let mut big = isdb_si::tables::Nit::decode(&nit()).unwrap();
    if let isdb_si::Descriptor::TerrestrialDeliverySystem(t) =
        &mut big.body.0.transport_streams[0].descriptors[1]
    {
        t.frequencies = (0..110).map(|i| 3300 + i).collect();
    }
    let big = big.encode().unwrap();
    assert!(big.len() > 184, "{}", big.len());
    let mut q = p.clone();
    let rep = edit::apply(
        &mut q,
        &[EditOp::Replace {
            sel: Selector::parse("0x10:0x40").unwrap(),
            sections: vec![big.clone()],
        }],
        &EditOptions::default(),
    )
    .unwrap();
    let rw = &rep[0].rewrites[0];
    assert_eq!(rw.dropped, 0, "{rw:?}");
    assert!(rw.null_slots_used > 0);
    assert_eq!(q.len(), p.len());
    clean(&q);
    let cap = scan(&q);
    let slot = cap
        .slots
        .iter()
        .find(|(k, _)| k.table_id == 0x40)
        .unwrap()
        .1;
    assert_eq!(slot.latest, big);
    assert_eq!(slot.occurrences.len(), 3);
    // with Never there is no room
    let mut r = p.clone();
    let rep = edit::apply(
        &mut r,
        &[EditOp::Replace {
            sel: Selector::parse("0x10:0x40").unwrap(),
            sections: vec![big],
        }],
        &EditOptions {
            null_slots: NullSlots::Never,
            nominal_bps: None,
        },
    )
    .unwrap();
    assert!(rep[0].rewrites[0].dropped > 0 || rep[0].rewrites[0].max_delay_packets > 0);
}

#[test]
fn remove_and_insert_sections() {
    let p = build(&default_spec(4));
    let mut q = p.clone();
    edit::apply(
        &mut q,
        &[EditOp::Remove(
            Selector::parse("0x12:0x4e:0x0408:1").unwrap(),
        )],
        &EditOptions::default(),
    )
    .unwrap();
    let cap = scan(&q);
    assert!(
        cap.slots
            .keys()
            .any(|k| k.table_id == 0x4E && k.sn == Some(0))
    );
    assert!(
        !cap.slots
            .keys()
            .any(|k| k.table_id == 0x4E && k.sn == Some(1))
    );
    let r = validate(&q);
    assert!(
        r.findings.iter().any(|f| f.check == "eit.pf"),
        "{}",
        r.to_text(Error)
    );
    // insert a BIT every 500 ms into null slots
    let mut q = p.clone();
    let rep = edit::apply(
        &mut q,
        &[EditOp::Insert {
            pid: 0x0024,
            sections: vec![bit()],
            interval_s: 0.5,
            offset_s: 0.1,
        }],
        &EditOptions::default(),
    )
    .unwrap();
    assert_eq!(rep[0].rewrites[0].dropped, 0);
    clean(&q);
    let cap = scan(&q);
    let rep = isdb_analyze::inspect::inspect(&cap, &Default::default());
    let b = rep
        .sections
        .iter()
        .find(|s| s.key.table_id == 0xC4)
        .unwrap();
    assert_eq!(b.count, 8);
    assert!(
        (b.interval.unwrap().mean - 0.5).abs() < 0.01,
        "{:?}",
        b.interval
    );
}

#[test]
fn mutate_pat_add_program() {
    let p = build(&default_spec(2));
    let mut q = p.clone();
    edit::apply(
        &mut q,
        &[EditOp::Mutate(
            Expr::parse("pat.add=0x0928:0x1fc8").unwrap(),
        )],
        &EditOptions::default(),
    )
    .unwrap();
    let cap = scan(&q);
    let pat = cap.pat().unwrap();
    assert!(pat.body.programs().any(|x| x == (0x0928, 0x1FC8)));
    let r = isdb_analyze::validate::validate(&cap, &Default::default());
    assert!(
        r.findings
            .iter()
            .any(|f| f.check == "psi.pmt" && f.message.contains("0x0928")
                || f.location.contains("0x0928")),
        "{}",
        r.to_text(Error)
    );
}

#[test]
fn extract_roundtrips_through_hex_loader() {
    let cap = scan(&build(&default_spec(2)));
    let text = edit::extract(&cap, Some(0x0010), None, false);
    let v = edit::sections_from_hex(&text, None).unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0], (0x0010, nit()));
}
