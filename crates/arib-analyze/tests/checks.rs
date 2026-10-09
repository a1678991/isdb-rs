//! One test per validation check: a synthetic stream (arib-ts/arib-si
//! encoders) violating exactly one rule, asserting the finding and clause.

mod common;
use arib_analyze::validate::Severity::{Error, Info, Warning};
use common::*;

#[test]
fn baseline_stream_has_no_errors() {
    let p = build(&default_spec(12));
    let r = validate(&p);
    println!("{}", r.to_text(Info));
    let errs: Vec<_> = r.findings.iter().filter(|f| f.severity >= Warning).collect();
    assert!(errs.is_empty(), "{}", r.to_text(Warning));
    assert!(has(&r, Info, "pcr.interval", "§2.7.2"));
    let cap = scan(&p);
    let rep = arib_analyze::inspect::inspect(&cap, &Default::default());
    assert_eq!(rep.timebase_pcr_pid, Some(PCR_PID));
    assert!((rep.duration_s.unwrap() - 12.0).abs() < 0.01, "{:?}", rep.duration_s);
    assert!((rep.rate_bps.unwrap() - PPS as f64 * 1504.0).abs() < 1.0);
    let nit = rep.sections.iter().find(|s| s.key.table_id == 0x40).unwrap();
    assert!((nit.interval.unwrap().mean - 1.0).abs() < 0.01);
    assert_eq!(rep.si.networks[0].name.as_deref(), Some("テスト"));
    assert_eq!(rep.si.networks[0].transport_streams[0].delivery.as_ref().unwrap().guard_interval_name, "1/8");
}

#[test]
fn pcr_interval_over_100ms() {
    let mut s = default_spec(5);
    s.pcr_period_ms = 150;
    let r = validate(&build(&s));
    assert!(has(&r, Error, "pcr.interval", "H.222.0 §2.7.2"), "{}", r.to_text(Info));
}

#[test]
fn continuity_break() {
    let mut p = build(&default_spec(3));
    let i = p.iter().enumerate().filter(|(_, x)| x[1] & 0x1F == 0 && x[2] == 0).nth(5).unwrap().0;
    p.remove(i);
    let r = validate(&p);
    assert!(has(&r, Error, "transport.cc", "§2.4.3.3"), "{}", r.to_text(Info));
}

#[test]
fn pmt_missing_for_pat_program() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != PMT_PID);
    let r = validate(&build(&s));
    assert!(has(&r, Error, "psi.pmt", "H.222.0 §2.4.4"), "{}", r.to_text(Info));
    assert!(has(&r, Error, "tables.mandatory", "Table 5-2"), "{}", r.to_text(Info));
}

#[test]
fn table_on_wrong_pid() {
    let mut s = default_spec(3);
    s.items.push(item(0x0010, 1000, 300, sdt()));
    let r = validate(&build(&s));
    assert!(has(&r, Error, "section.placement", "Table 5-1"), "{}", r.to_text(Info));
}

#[test]
fn version_incoherence() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0010);
    s.items.push(Item { pid: 0x0010, period_ms: 500, phase_ms: 11, make: Box::new(|n| vec![nit_with(1, (n % 2) as u8)]) });
    let r = validate(&build(&s));
    assert!(has(&r, Error, "section.version", "version_number"), "{}", r.to_text(Info));
}

#[test]
fn crc_error() {
    let mut s = default_spec(3);
    let mut bad = sdt();
    let n = bad.len();
    bad[n - 1] ^= 0xFF;
    s.items.push(item(0x0011, 1000, 500, bad));
    let r = validate(&build(&s));
    assert!(has(&r, Error, "section.crc", "Annex A"), "{}", r.to_text(Info));
}

#[test]
fn mandatory_sdt_missing() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0011);
    let r = validate(&build(&s));
    assert!(r.findings.iter().any(|f| f.check == "tables.mandatory" && f.message.contains("SDT")), "{}", r.to_text(Info));
}

#[test]
fn frequency_criterion_exceeded_is_warning() {
    let mut s = default_spec(8);
    for it in &mut s.items {
        if it.pid == 0x0011 {
            it.period_ms = 3000; // SDT criterion: once or more / 2 s
        }
    }
    let r = validate(&build(&s));
    assert!(has(&r, Warning, "tables.frequency", "Table 5-2"), "{}", r.to_text(Info));
    assert!(!r.findings.iter().any(|f| f.check == "tables.frequency" && f.severity == Error));
    let row = r.frequencies.iter().find(|x| x.key.table_id == 0x42).unwrap();
    assert!(row.status.starts_with("exceeded") && (row.interval.unwrap().max - 3.0).abs() < 0.01);
}

#[test]
fn reserved_bits_are_warnings() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0);
    let mut p = arib_ts::psi::Pat::section(TSID, 1, vec![arib_ts::psi::PatEntry::new(0, 0x10), arib_ts::psi::PatEntry::new(SID, PMT_PID)]);
    p.body.entries[1].reserved = 0;
    s.items.push(item(0, 80, 0, p.encode().unwrap()));
    let r = validate(&build(&s));
    assert!(has(&r, Warning, "psi.syntax", "§2.1.64"), "{}", r.to_text(Info));
}

#[test]
fn psi_section_too_long() {
    let mut s = default_spec(2);
    s.items.retain(|i| i.pid != PMT_PID);
    let big: Vec<arib_ts::descriptor::Descriptor> = (0..5).map(|_| arib_ts::descriptor::Descriptor::new(0x80, vec![0u8; 250])).collect();
    let pmt = arib_ts::psi::Pmt::section(SID, 1, arib_ts::psi::Pmt::new(PCR_PID, big, vec![arib_ts::psi::PmtStream::new(0x0F, AUDIO_PID, vec![])])).encode().unwrap();
    assert!(pmt.len() - 3 > 1021);
    s.items.push(item(PMT_PID, 80, 5, pmt));
    let r = validate(&build(&s));
    assert!(r.findings.iter().any(|f| f.check == "psi.syntax" && f.message.contains("1021") && f.severity == Error), "{}", r.to_text(Info));
}

#[test]
fn si_mandatory_descriptor() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0012);
    let mut ev = event(1, 12);
    ev.descriptors.clear();
    s.items.push(Item { pid: 0x0012, period_ms: 1000, phase_ms: 17, make: Box::new(move |_| vec![eit_pf(0, vec![ev.clone()]), eit_pf(1, vec![])]) });
    let r = validate(&build(&s));
    assert!(has(&r, Warning, "si.conformance", "Table 6-1"), "{}", r.to_text(Info));
}

#[test]
fn sdt_tsid_and_onid_mismatch() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0011);
    s.items.push(item(0x0011, 1000, 13, sdt_with(0x1234, 0x0001, &[SID])));
    let r = validate(&build(&s));
    assert!(has(&r, Error, "cross.tsid", "§5.2.6"), "{}", r.to_text(Info));
    assert!(has(&r, Error, "cross.onid", "original_network_id"), "{}", r.to_text(Info));
}

#[test]
fn pat_program_absent_from_sdt() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0011);
    s.items.push(item(0x0011, 1000, 13, sdt_with(TSID, ONID, &[0x0999])));
    let r = validate(&build(&s));
    assert!(has(&r, Warning, "cross.services", "Appendix §1.3"), "{}", r.to_text(Info));
}

#[test]
fn eit_pf_two_events_in_present_section() {
    let mut s = default_spec(3);
    s.items.retain(|i| i.pid != 0x0012);
    s.items.push(Item { pid: 0x0012, period_ms: 1000, phase_ms: 17, make: Box::new(|_| vec![eit_pf(0, vec![event(1, 12), event(2, 13)]), eit_pf(1, vec![])]) });
    let r = validate(&build(&s));
    assert!(has(&r, Error, "eit.pf", "§5.2.7"), "{}", r.to_text(Info));
}

#[test]
fn pts_interval_over_700ms() {
    let mut s = default_spec(5);
    s.audio_period_ms = 1000;
    let r = validate(&build(&s));
    assert!(has(&r, Error, "pes.pts", "§2.7.4"), "{}", r.to_text(Info));
}

#[test]
fn es_on_allocated_si_pid() {
    let mut s = default_spec(2);
    s.items.retain(|i| i.pid != PMT_PID);
    let pmt = arib_ts::psi::Pmt::section(SID, 1, arib_ts::psi::Pmt::new(PCR_PID, vec![], vec![arib_ts::psi::PmtStream::new(0x0F, 0x0024, vec![])])).encode().unwrap();
    s.items.push(item(PMT_PID, 80, 5, pmt));
    let r = validate(&build(&s));
    assert!(has(&r, Error, "pid.allocation", "Table 5-1"), "{}", r.to_text(Info));
}

#[test]
fn expectations() {
    let cap = scan(&build(&default_spec(2)));
    let ok = arib_analyze::validate::Expectations {
        transport_stream_id: Some(TSID),
        original_network_id: Some(ONID),
        network_id: Some(ONID),
        service_ids: Some(vec![SID]),
        frequency_mhz: Some(479.142857),
        guard_interval: Some(2),
        transmission_mode: Some(2),
        area_code: Some(0x8D5),
        remote_control_key_id: Some(10),
        ts_name: Some("テスト".into()),
        network_name: Some("テスト".into()),
    };
    let r = arib_analyze::validate::validate(&cap, &arib_analyze::validate::ValidateOptions { expect: ok.clone(), nominal_bps: None });
    assert!(!r.findings.iter().any(|f| f.check == "expect"), "{}", r.to_text(Info));
    let bad = arib_analyze::validate::Expectations { transport_stream_id: Some(0x1111), frequency_mhz: Some(521.142857), guard_interval: Some(3), ..ok };
    let r = arib_analyze::validate::validate(&cap, &arib_analyze::validate::ValidateOptions { expect: bad, nominal_bps: None });
    let e: Vec<_> = r.findings.iter().filter(|f| f.check == "expect" && f.severity == Error).collect();
    assert!(e.iter().any(|f| f.message.contains("transport_stream_id")), "{e:?}");
    assert!(e.iter().any(|f| f.message.contains("521.142857")), "{e:?}");
    assert!(e.iter().any(|f| f.message.contains("guard_interval")), "{e:?}");
    assert!(r.has_errors());
}

/// digital_copy_control_descriptor with copy_control_type '00' in
/// user_defined (B10 Annex F layout): warning (a Sony BDZ-series recorder refuses it,
/// bench 2026-10-08); '01' (0b0100, what live broadcasters send) is clean.
#[test]
fn copy_control_type_00_warns() {
    let pmt_with = |ud: u8| {
        use arib_ts::psi::{Pmt, PmtStream};
        let mut c1 = Vec::new();
        arib_si::Descriptor::DigitalCopyControl(arib_si::descriptor::DigitalCopyControl {
            digital_recording_control_data: 0b10,
            user_defined: ud,
            maximum_bitrate: None,
            component_control: None,
        })
        .encode(&mut c1)
        .unwrap();
        let c1 = arib_ts::descriptor::Descriptor::new(c1[0], c1[2..].to_vec());
        Pmt::section(SID, 1, Pmt::new(PCR_PID, vec![c1], vec![PmtStream::new(0x0F, AUDIO_PID, vec![arib_ts::descriptor::Descriptor::new(0x52, [0x10])])]))
            .encode()
            .unwrap()
    };
    for (ud, warn) in [(0b0000, true), (0b0100, false)] {
        let mut s = default_spec(3);
        s.items.retain(|i| i.pid != PMT_PID);
        s.items.push(item(PMT_PID, 80, 5, pmt_with(ud)));
        let r = validate(&build(&s));
        assert_eq!(has(&r, Warning, "si.copy_control", "Annex F"), warn, "{}", r.to_text(Info));
    }
}

/// ARIB STD-B32 Part 2 §5.2 ADTS header rules on a stream_type 0x0F PID:
/// each violated header field is an error citing its clause; the clean
/// baseline has none.  (Header-level only: the crc_check value and the
/// raw_data_block element rules are not checked.)
#[test]
fn aac_adts_rules() {
    let run = |payload: Vec<u8>| {
        let mut s = default_spec(3);
        s.audio_payload = payload;
        validate(&build(&s))
    };
    let r = run(adts_frame(|_| {}));
    assert!(!r.findings.iter().any(|f| f.check.starts_with("aac.") && f.severity > Info), "{}", r.to_text(Warning));
    assert!(has(&r, Info, "aac.summary", "§5.2"));
    type Edit = Box<dyn Fn(&mut arib_mux::adts::AdtsHeader)>;
    let cases: [(&str, &str, Edit); 6] = [
        ("aac.protection_absent", "§5.2.2 (1)", Box::new(|h| h.protection_absent = true)),
        ("aac.adts_buffer_fullness", "§5.2.2 (2)", Box::new(|h| h.adts_buffer_fullness = 0x7FF)),
        ("aac.profile", "§5.2.2 (1)", Box::new(|h| h.profile = 0)),
        ("aac.sampling_frequency_index", "§5.2.2 (1)", Box::new(|h| h.sampling_frequency_index = 0)),
        ("aac.id", "13818-7", Box::new(|h| h.id = 0)),
        ("aac.number_of_raw_data_blocks_in_frame", "§5.2.2 (2)", Box::new(|h| {
            h.number_of_raw_data_blocks_in_frame = 1;
            h.protection_absent = true;
        })),
    ];
    for (check, clause, edit) in cases {
        let r = run(adts_frame(|h| edit(h)));
        assert!(has(&r, Error, check, clause), "{check}: {}", r.to_text(Warning));
    }
    // 7.1 channels
    let r = run(adts_frame(|h| h.channel_configuration = 7));
    assert!(has(&r, Error, "aac.channel_configuration", "§5.2.1"), "{}", r.to_text(Warning));
}

/// quality_indicator of the EIT audio component descriptor against the
/// measured stream (B32 Part 2 Attachment Ch. 3 Table 1): the synthetic
/// 200-byte frames are 75 kbit/s stereo, so Mode 1 is contradicted
/// (warning) and Mode 3 is consistent (info).
#[test]
fn aac_quality_indicator_consistency() {
    use arib_si::descriptor::{AudioComponent, Descriptor as D};
    let with_quality = |q: u8| {
        let ac = D::AudioComponent(AudioComponent {
            reserved_future_use_1: 0xF,
            stream_content: 0x2,
            component_type: 0x03,
            component_tag: 0x10,
            stream_type: 0x0F,
            simulcast_group_tag: 0xFF,
            main_component_flag: true,
            quality_indicator: q,
            sampling_rate: 0b111,
            reserved_future_use_2: true,
            language: *b"jpn",
            language_2: None,
            text: arib_text::AribString::encode("").unwrap(),
        });
        let mut s = default_spec(3);
        s.items.retain(|i| i.pid != 0x0012);
        s.items.push(Item {
            pid: 0x0012,
            period_ms: 1000,
            phase_ms: 17,
            make: Box::new(move |_| {
                let mut e = event(1, 12);
                e.descriptors.push(ac.clone());
                vec![eit_pf(0, vec![e]), eit_pf(1, vec![event(2, 13)])]
            }),
        });
        validate(&build(&s))
    };
    let r = with_quality(0b01);
    assert!(has(&r, Warning, "aac.quality_indicator", "Table 1"), "{}", r.to_text(Info));
    let r = with_quality(0b11);
    assert!(!has(&r, Warning, "aac.quality_indicator", "Table 1"), "{}", r.to_text(Info));
    assert!(has(&r, Info, "aac.quality_indicator", "Table 1"));
}
