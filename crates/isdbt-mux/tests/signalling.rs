#![allow(missing_docs)]
//! Typed signalling options: the shipped examples re-decode to the named
//! variants, synthetic PMT descriptors (live-service values) decode to the
//! same variants, raw descriptors duplicating typed ones are rejected, and
//! old numeric forms fail with errors naming the variants.

mod common;

use common::*;
use isdb_si::AnyTable;
use isdb_si::descriptor::{
    AccessControl, ConditionalAccess, ContentAvailability, DataComponent, Descriptor,
    DescriptorBody, DigitalCopyControl, StreamIdentifier, VideoDecodeControl,
};
use isdb_si::values::*;
use isdb_ts::Pid;
use isdb_ts::psi::{Pmt, PmtStream};
use isdb_ts::section::Long;
use isdbt_mux::analysis::{Report, analyze};
use isdbt_mux::mux::clocks_to_ticks;
use isdbt_mux::{Config, Multiplexer};

/// A shipped example with its media replaced by the synthetic 1440×1080i
/// 16:9 video / 48 kHz stereo ADTS, muxed for `frames` frames.
fn run_example(text: &str, name: &str, frames: u64) -> Report {
    let dir = tmpdir(name);
    std::fs::write(dir.join("v.m2v"), synth_video(3)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(150)).unwrap();
    let text = text
        .replace("media/video.m2v", "v.m2v")
        .replace("media/audio.m2a", "a.adts")
        .replace(
            "source = \"system\"",
            "source = \"fixed\"\nstart = \"2026-10-08T12:00:00\"",
        );
    let cfg = Config::from_toml(&text).unwrap();
    let params = bench_params();
    let mut mux = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let n = mux.pattern().len();
    let mut full = Vec::new();
    let mut times = Vec::new();
    for k in 0..frames {
        let (f, _) = mux.next_frame_full();
        times.extend((0..n).map(|j| mux.slot_time(k, j)));
        full.extend(f);
    }
    analyze(&full, &times, clocks_to_ticks(408))
}

fn si_body(d: &isdb_ts::descriptor::Descriptor) -> Descriptor {
    Descriptor::decode_body(d.tag, &d.data).unwrap()
}

fn vdc(d: &isdb_ts::descriptor::Descriptor) -> VideoDecodeControl {
    match si_body(d) {
        Descriptor::VideoDecodeControl(v) => v,
        x => panic!("{x:?}"),
    }
}

fn dcc(d: &isdb_ts::descriptor::Descriptor) -> DigitalCopyControl {
    match si_body(d) {
        Descriptor::DigitalCopyControl(v) => v,
        x => panic!("{x:?}"),
    }
}

struct Seen {
    pmt_c8: Vec<VideoDecodeControl>,
    pmt_c1: Vec<DigitalCopyControl>,
    sdt_c1: Vec<DigitalCopyControl>,
    eit_c1: Vec<DigitalCopyControl>,
    eit_video: Vec<VideoComponentType>,
    eit_audio: Vec<AudioComponentType>,
    eit_quality: Vec<u8>,
    key: u8,
}

fn collect(r: &Report) -> Seen {
    let pmt = Long::<Pmt>::decode(&r.sections[&Pid::new(0x01F0).unwrap()][0].1).unwrap();
    let pmt_c1 = pmt
        .body
        .program_info
        .iter()
        .filter(|d| d.tag == 0xC1)
        .map(dcc)
        .collect();
    let pmt_c8 = pmt.body.streams[0]
        .descriptors
        .iter()
        .filter(|d| d.tag == 0xC8)
        .map(vdc)
        .collect();
    let AnyTable::Sdt(sdt) = AnyTable::decode(
        Pid::new(0x11).unwrap(),
        &r.sections[&Pid::new(0x0011).unwrap()][0].1,
    )
    .unwrap() else {
        panic!()
    };
    let svc = sdt
        .body
        .services
        .iter()
        .find(|s| s.service_id == 0x0400)
        .unwrap();
    let sdt_c1 = svc
        .descriptors
        .iter()
        .filter_map(|d| match d {
            Descriptor::DigitalCopyControl(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    let (mut eit_c1, mut eit_video, mut eit_audio) = (Vec::new(), Vec::new(), Vec::new());
    let mut eit_quality = Vec::new();
    for (_, s) in &r.sections[&Pid::new(0x0012).unwrap()] {
        let AnyTable::Eit(eit) = AnyTable::decode(Pid::new(0x12).unwrap(), s).unwrap() else {
            continue;
        };
        if eit.header.table_id_extension != 0x0400 {
            continue;
        }
        for e in &eit.body.events {
            for d in &e.descriptors {
                match d {
                    Descriptor::DigitalCopyControl(c) => eit_c1.push(c.clone()),
                    Descriptor::Component(c) => eit_video.push(c.video_component_type().unwrap()),
                    Descriptor::AudioComponent(a) => {
                        eit_audio.push(a.audio_component_type());
                        eit_quality.push(a.quality_indicator);
                    }
                    _ => {}
                }
            }
        }
    }
    let AnyTable::Nit(nit) = AnyTable::decode(
        Pid::new(0x10).unwrap(),
        &r.sections[&Pid::new(0x0010).unwrap()][0].1,
    )
    .unwrap() else {
        panic!()
    };
    let key = nit.body.0.transport_streams[0]
        .descriptors
        .iter()
        .find_map(|d| match d {
            Descriptor::TsInformation(t) => Some(t.remote_control_key_id),
            _ => None,
        })
        .unwrap();
    Seen {
        pmt_c8,
        pmt_c1,
        sdt_c1,
        eit_c1,
        eit_video,
        eit_audio,
        eit_quality,
        key,
    }
}

fn assert_live_copy_control(c: &DigitalCopyControl) {
    assert_eq!(
        c.recording_control(),
        DigitalRecordingControl::CopyOneGeneration
    );
    assert_eq!(c.user_defined, 0b0100);
    let u = c.user_defined_annex_f();
    assert_eq!(u.copy_control_type, CopyControlType::SerialOutputEncoded);
    assert_eq!(u.low_bits, AnnexFLowBits::Aps(ApsControlData::CopyFree));
}

/// bench.toml (live-broadcaster values) re-decodes to the named variants.
#[test]
fn bench_toml_redecodes_to_live_values() {
    let r = run_example(isdbt_mux::config::EXAMPLE_TOML, "sig_bench", 12);
    let s = collect(&r);
    // 0xC8 = 0x47: 1080/I, sequence_end_code, transfer "undefined"
    assert_eq!(s.pmt_c8.len(), 1);
    let v = s.pmt_c8[0];
    assert!(!v.still_picture_flag && v.sequence_end_code_flag);
    assert_eq!(v.encode_format(), VideoEncodeFormat::I1080);
    assert_eq!(v.transfer(), TransferCharacteristics::Undefined);
    let mut b = Vec::new();
    isdb_si::descriptor::DescriptorBody::encode(&v, &mut b).unwrap();
    assert_eq!(b, [0x47]);
    // 0xC1 in PMT + SDT, not EIT
    assert_eq!(s.pmt_c1.len(), 1);
    assert_eq!(s.sdt_c1.len(), 1);
    assert!(s.eit_c1.is_empty());
    assert_live_copy_control(&s.pmt_c1[0]);
    assert_live_copy_control(&s.sdt_c1[0]);
    // EIT component 0xB3, audio 2/0
    assert!(!s.eit_video.is_empty());
    assert!(
        s.eit_video
            .iter()
            .all(|&v| v == VideoComponentType::V1080i16x9)
    );
    assert!(
        s.eit_audio
            .iter()
            .all(|a| a.mode == AudioMode::Stereo && a.code() == 0x03)
    );
    // quality_indicator derived (B32 Part 2 Attachment Ch. 3 Table 1):
    // synthetic 393-byte frames after conform ≈ 147 kbit/s stereo → Mode 2
    assert!(!s.eit_quality.is_empty());
    assert!(
        s.eit_quality.iter().all(|&q| q == 0b10),
        "{:?}",
        s.eit_quality
    );
    assert_eq!(s.key, 9);
}

const QI_LINE: &str = "  audio_quality_indicator = \"derived\"";
const AC_LINE: &str = "  adts_compliance = \"conform\"";

/// Explicit quality_indicator modes override the derived one; numbers are
/// rejected naming the modes; audio-only options on a video stream fail.
#[test]
fn audio_quality_indicator_typed() {
    let r = run_example(
        &extra_into(
            isdbt_mux::config::EXAMPLE_TOML,
            &format!("{QI_LINE}\n=>\n  audio_quality_indicator = \"mode-1\""),
        ),
        "sig_quality",
        6,
    );
    let s = collect(&r);
    assert!(!s.eit_quality.is_empty() && s.eit_quality.iter().all(|&q| q == 0b01));
    let e = cfg_err(&format!("{QI_LINE}\n=>\n  audio_quality_indicator = 2"));
    assert!(
        e.contains("\"mode-2\"") && e.contains("not a number"),
        "{e}"
    );
    let e = cfg_err(&format!("{QI_LINE}\n=>\n  audio_quality_indicator = 0"));
    assert!(e.contains("reserved"), "{e}");
    let e = cfg_err(&format!(
        "{QI_LINE}\n=>\n  audio_quality_indicator = \"mode-4\""
    ));
    assert!(e.contains("mode-3"), "{e}");
    let e = cfg_err(&format!("{AC_LINE}\n=>\n  adts_compliance = \"strict\""));
    assert!(e.contains("conform") && e.contains("pass-through"), "{e}");
    assert_eq!(
        cfg_err(&format!("{AC_LINE}\n=>\n  adts_compliance = \"warn\"")),
        ""
    );
    let e = cfg_err(&format!(
        "{VDC_LINE}\n=>\n{VDC_LINE}\n  audio_quality_indicator = \"mode-2\""
    ));
    assert!(e.contains("applies only to kind = \"aac-adts\""), "{e}");
}

/// bench-minimal.toml: derived values and copy-free.
#[test]
fn bench_minimal_toml_redecodes_to_derived_values() {
    let r = run_example(
        include_str!("../examples/bench-minimal.toml"),
        "sig_minimal",
        12,
    );
    let s = collect(&r);
    // synthetic ES: 1080 lines interlaced, no sequence_end_code → 0x04
    let v = s.pmt_c8[0];
    assert!(!v.still_picture_flag && !v.sequence_end_code_flag);
    assert_eq!(v.encode_format(), VideoEncodeFormat::I1080);
    assert_eq!(v.transfer(), TransferCharacteristics::Bt709);
    // aspect_ratio_information 3 (16:9) → 0xB3 derived
    assert!(
        s.eit_video
            .iter()
            .all(|&v| v == VideoComponentType::V1080i16x9)
    );
    assert!(s.eit_audio.iter().all(|a| a.mode == AudioMode::Stereo));
    // copy-free in PMT + EIT (default placement), user_defined default 0b0100
    assert!(s.sdt_c1.is_empty());
    for c in s.pmt_c1.iter().chain(&s.eit_c1) {
        assert_eq!(c.recording_control(), DigitalRecordingControl::CopyFree);
        assert_eq!(c.user_defined, 0b0100);
    }
    assert!(!s.pmt_c1.is_empty() && !s.eit_c1.is_empty());
}

/// A typed isdb-si descriptor as the raw H.222.0 descriptor of a PSI loop.
fn raw(d: Descriptor) -> isdb_ts::descriptor::Descriptor {
    let mut v = Vec::new();
    d.encode(&mut v).unwrap();
    isdb_ts::descriptor::Descriptor::new(v[0], v[2..].to_vec())
}

fn stream_id(tag: u8) -> isdb_ts::descriptor::Descriptor {
    raw(Descriptor::StreamIdentifier(StreamIdentifier {
        component_tag: tag,
    }))
}

fn data_component(id: u16, info: &[u8]) -> isdb_ts::descriptor::Descriptor {
    raw(Descriptor::DataComponent(DataComponent {
        data_component_id: id,
        additional_data_component_info: info.to_vec(),
    }))
}

fn copy_control(user_defined: u8, maximum_bitrate: u8) -> isdb_ts::descriptor::Descriptor {
    raw(Descriptor::DigitalCopyControl(DigitalCopyControl {
        digital_recording_control_data: 0b10, // copy-one-generation
        user_defined,
        maximum_bitrate: Some(maximum_bitrate),
        component_control: None,
    }))
}

/// content_availability_descriptor 0xDE (B10 §6.2.45): copy restriction,
/// image constraint, no retention mode, retention_state '111', encryption.
fn content_availability() -> isdb_ts::descriptor::Descriptor {
    raw(Descriptor::ContentAvailability(ContentAvailability {
        reserved_future_use: true,
        copy_restriction_mode: true,
        image_constraint_token: true,
        retention_mode: false,
        retention_state: 0b111,
        encryption_mode: true,
        reserved_future_use_bytes: vec![],
    }))
}

/// Synthetic PMTs `(pid, section)` with the program and ES descriptors a
/// live terrestrial multiplex carries: three HD services (CA + access
/// control, copy control `a4 42`, content availability; MPEG-2 video with
/// video decode control `47`, AAC, captions / superimpose with data
/// component 0x0008, data carousels with 0x000C), a data-only service and
/// the one-seg service (copy control `a8 02`, H.264, AAC, captions 0x0012,
/// carousels 0x000D).  Built with the isdb-ts / isdb-si encoders.
fn synthetic_pmts() -> Vec<(u16, Vec<u8>)> {
    let ca = |pid| {
        raw(Descriptor::ConditionalAccess(ConditionalAccess {
            ca_system_id: 0x0005,
            reserved: 0b111,
            ca_pid: pid,
            private_data: vec![],
        }))
    };
    let access = |pid| {
        raw(Descriptor::AccessControl(AccessControl {
            ca_system_id: 0x000E,
            transmission_type: 0b111,
            pid,
            private_data: vec![],
        }))
    };
    let vdc = raw(Descriptor::VideoDecodeControl(
        VideoDecodeControl::decode(&[0x47]).unwrap(),
    ));
    let bxml = [0x33, 0x3F, 0x00, 0x03, 0x00, 0x00, 0xFF, 0xBF];
    let hd = |sid: u16| {
        Pmt::section(
            sid,
            1,
            Pmt::new(
                Pid::new(0x01FF).unwrap(),
                vec![
                    ca(Pid::new(0x0060).unwrap()),
                    access(Pid::new(0x0061).unwrap()),
                    copy_control(0b0100, 66),
                    content_availability(),
                ],
                vec![
                    PmtStream::new(
                        0x02,
                        Pid::new(0x0100).unwrap(),
                        vec![stream_id(0x00), vdc.clone()],
                    ),
                    PmtStream::new(0x0F, Pid::new(0x0110).unwrap(), vec![stream_id(0x10)]),
                    PmtStream::new(
                        0x06,
                        Pid::new(0x0130).unwrap(),
                        vec![stream_id(0x30), data_component(0x0008, &[0x3D])],
                    ),
                    PmtStream::new(
                        0x06,
                        Pid::new(0x0138).unwrap(),
                        vec![
                            stream_id(0x38),
                            ca(Pid::new(0x1FFF).unwrap()),
                            access(Pid::new(0x1FFF).unwrap()),
                            data_component(0x0008, &[0x3C]),
                        ],
                    ),
                    PmtStream::new(
                        0x0D,
                        Pid::new(0x0140).unwrap(),
                        vec![stream_id(0x40), data_component(0x000C, &bxml)],
                    ),
                    PmtStream::new(
                        0x0D,
                        Pid::new(0x0150).unwrap(),
                        vec![
                            stream_id(0x50),
                            ca(Pid::new(0x1FFF).unwrap()),
                            access(Pid::new(0x1FFF).unwrap()),
                            data_component(0x000C, &[0x1F, 0xFF, 0xBF]),
                        ],
                    ),
                ],
            ),
        )
    };
    let data_only = Pmt::section(
        0x0408,
        1,
        Pmt::new(
            Pid::new(0x1FFF).unwrap(),
            vec![],
            (0..4u8)
                .map(|k| {
                    PmtStream::new(
                        0x0D,
                        Pid::new(0x0170 + u16::from(k)).unwrap(),
                        vec![stream_id(0x70 + k), data_component(0x000C, &[0x1F])],
                    )
                })
                .collect(),
        ),
    );
    let oneseg = Pmt::section(
        0x0588,
        1,
        Pmt::new(
            Pid::new(0x0181).unwrap(),
            vec![copy_control(0b1000, 2), content_availability()],
            vec![
                PmtStream::new(0x1B, Pid::new(0x0181).unwrap(), vec![stream_id(0x81)]),
                PmtStream::new(0x0F, Pid::new(0x0183).unwrap(), vec![stream_id(0x83)]),
                PmtStream::new(
                    0x06,
                    Pid::new(0x0187).unwrap(),
                    vec![stream_id(0x87), data_component(0x0012, &[0xAD])],
                ),
                PmtStream::new(
                    0x0D,
                    Pid::new(0x0189).unwrap(),
                    vec![
                        stream_id(0x80),
                        data_component(0x000D, &[0x3F, 0x2F, 0x00, 0x0C, 0x00, 0x00, 0xFF, 0xBF]),
                    ],
                ),
                PmtStream::new(0x0D, Pid::new(0x018A).unwrap(), vec![stream_id(0x8A)]),
            ],
        ),
    );
    let mut v: Vec<(u16, Vec<u8>)> = [(0x01F0, 0x0400), (0x02F0, 0x0401), (0x03F0, 0x0402)]
        .into_iter()
        .map(|(pid, sid)| (pid, hd(sid).encode().unwrap()))
        .collect();
    v.push((0x09F0, data_only.encode().unwrap()));
    v.push((0x1FC8, oneseg.encode().unwrap()));
    v
}

/// Synthetic PMTs (values of live terrestrial services): every descriptor
/// decodes to a typed isdb-si variant, the sections re-encode
/// byte-identically, the HD program loop carries `c1 02 a4 42` and the HD
/// video ES `c8 01 47`, the one-seg program loop `c1 02 a8 02`.
#[test]
fn synthetic_pmt_descriptors_decode_to_named_variants() {
    let mut hd = 0;
    let mut oneseg = 0;
    for (pid, b) in synthetic_pmts() {
        let pmt = Long::<Pmt>::decode(&b).unwrap();
        assert_eq!(pmt.encode().unwrap(), b, "PMT on {pid:#06x} not lossless");
        let all = pmt
            .body
            .program_info
            .iter()
            .chain(pmt.body.streams.iter().flat_map(|s| &s.descriptors));
        for d in all {
            assert!(
                !matches!(si_body(d), Descriptor::Unknown { .. }),
                "tag {:#04x} not typed",
                d.tag
            );
        }
        let sid = pmt.header.table_id_extension;
        let c1: Vec<_> = pmt
            .body
            .program_info
            .iter()
            .filter(|d| d.tag == 0xC1)
            .map(dcc)
            .collect();
        let de: Vec<_> = pmt
            .body
            .program_info
            .iter()
            .filter(|d| d.tag == 0xDE)
            .map(si_body)
            .collect();
        if (0x0400..=0x0402).contains(&sid) {
            assert_eq!(c1.len(), 1);
            assert_eq!(
                pmt.body
                    .program_info
                    .iter()
                    .find(|d| d.tag == 0xC1)
                    .unwrap()
                    .data,
                [0xA4, 0x42]
            );
            assert_live_copy_control(&c1[0]);
            assert_eq!(c1[0].maximum_bitrate_typed(), Some(MaximumBitrate(66))); // 16.5 Mbit/s
            let Descriptor::ContentAvailability(ca) = &de[0] else {
                panic!("{de:?}")
            };
            assert!(ca.copy_restriction_mode && ca.image_constraint_token && ca.encryption_mode);
            assert!(!ca.retention_mode);
            assert_eq!(ca.retention_state, 0b111);
            let c8 = pmt.body.streams[0]
                .descriptors
                .iter()
                .find(|d| d.tag == 0xC8)
                .unwrap();
            assert_eq!(c8.data, [0x47]);
            let v = vdc(c8);
            assert_eq!(v.encode_format(), VideoEncodeFormat::I1080);
            assert_eq!(v.transfer(), TransferCharacteristics::Undefined);
            assert!(v.sequence_end_code_flag && !v.still_picture_flag);
            hd += 1;
        }
        if sid == 0x0588 {
            // one-seg: copy-one-generation, user_defined 0b1000 =
            // copy_control_type '10' "Undefined" + '00', 0.5 Mbit/s
            assert_eq!(pmt.body.program_info[0].data, [0xA8, 0x02]);
            assert_eq!(
                c1[0].recording_control(),
                DigitalRecordingControl::CopyOneGeneration
            );
            assert_eq!(
                c1[0].user_defined_annex_f().copy_control_type,
                CopyControlType::Undefined10
            );
            assert_eq!(c1[0].maximum_bitrate_typed().unwrap().bps(), 500_000);
            oneseg += 1;
        }
    }
    assert_eq!((hd, oneseg), (3, 1));
}

fn cfg_err(extra: &str) -> String {
    let dir = tmpdir("sig_err");
    std::fs::write(dir.join("v.m2v"), synth_video(1)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(10)).unwrap();
    let base = isdbt_mux::config::EXAMPLE_TOML
        .replace("media/video.m2v", "v.m2v")
        .replace("media/audio.m2a", "a.adts");
    let text = extra_into(&base, extra);
    match Config::from_toml(&text) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

/// Apply `old => new` line replacements (separated by `\n=>\n`) to `base`.
fn extra_into(base: &str, edits: &str) -> String {
    let mut s = base.to_string();
    for e in edits.split("\n;;\n") {
        let (a, b) = e.split_once("\n=>\n").unwrap();
        assert!(s.contains(a), "edit target not found: {a}");
        s = s.replacen(a, b, 1);
    }
    s
}

const VDC_LINE: &str = "  video_decode_control = { mode = \"explicit\", still_picture = false, sequence_end_code = true, video_encode_format = \"1080i\", transfer_characteristics = \"undefined\" }";
const CC_LINE: &str = "copy_control = { recording_control = \"copy-one-generation\", copy_control_type = \"serial-output-encoded\", aps_control_data = \"copy-free\", in_pmt = true, in_sdt = true, in_eit = false }";

#[test]
fn raw_duplicate_of_typed_is_rejected_unless_off() {
    // raw 0xC8 in the stream's ES loop while the typed one is active
    let e = cfg_err(&format!(
        "{VDC_LINE}\n=>\n{VDC_LINE}\n  descriptors = [ {{ tag = 0xC8, hex = \"47\" }} ]"
    ));
    assert!(
        e.contains("tag 0xc8") && e.contains("video_decode_control_descriptor"),
        "{e}"
    );
    assert!(
        e.contains("mode = \\\"off\\\"") || e.contains("mode = \"off\""),
        "{e}"
    );
    // ... accepted with the typed one off
    let ok = cfg_err(&format!(
        "{VDC_LINE}\n=>\n  video_decode_control = {{ mode = \"off\" }}\n  descriptors = [ {{ tag = 0xC8, hex = \"47\" }} ]"
    ));
    assert_eq!(ok, "");
    // same through [service.pmt] add_descriptors
    let e = cfg_err(
        "  [service.pmt]\n=>\n  [service.pmt]\n  add_descriptors = [ { loop = \"es:0x0111\", tag = 0xC8, hex = \"47\" } ]",
    );
    assert!(e.contains("tag 0xc8"), "{e}");
    // raw 0xC1 in the PMT program loop vs copy_control.in_pmt
    let e = cfg_err(&format!(
        "{CC_LINE}\n=>\n{CC_LINE}\nprogram_descriptors = [ {{ tag = 0xC1, hex = \"a4\" }} ]"
    ));
    assert!(
        e.contains("PMT program loop") && e.contains("in_pmt = false"),
        "{e}"
    );
    // raw 0xC1 in SDT vs in_sdt; allowed in the EIT (in_eit = false)
    let e = cfg_err(&format!(
        "{CC_LINE}\n=>\n{CC_LINE}\nsdt_descriptors = [ {{ tag = 0xC1, hex = \"84\" }} ]"
    ));
    assert!(e.contains("SDT service loop"), "{e}");
    let ok = cfg_err(
        "[eit]\npresent_running_status\n=>\n[eit]\nadd_descriptors = [ { loop = \"event:0x0400\", tag = 0xC1, hex = \"84\" } ]\npresent_running_status",
    );
    assert_eq!(ok, "");
    // a raw component descriptor collides only on the same component_tag
    let e = cfg_err(
        "[eit]\npresent_running_status\n=>\n[eit]\nadd_descriptors = [ { loop = \"event\", tag = 0x50, hex = \"f1 b3 00 6a706e\" } ]\npresent_running_status",
    );
    assert!(e.contains("component_descriptor"), "{e}");
    let ok = cfg_err(
        "[eit]\npresent_running_status\n=>\n[eit]\nadd_descriptors = [ { loop = \"event\", tag = 0x50, hex = \"f1 b3 05 6a706e\" } ]\npresent_running_status",
    );
    assert_eq!(ok, "");
    // NIT: 0xCD is always generated
    let e = cfg_err(
        "[eit]\npresent_running_status\n=>\n[nit]\nadd_descriptors = [ { loop = \"ts\", tag = 0xCD, hex = \"0c00\" } ]\n\n[eit]\npresent_running_status",
    );
    assert!(e.contains("TS_information_descriptor"), "{e}");
}

#[test]
fn remote_key_range_is_validated() {
    for bad in ["0", "13", "255"] {
        let e = cfg_err(&format!(
            "remote_control_key_id = 9\n=>\nremote_control_key_id = {bad}"
        ));
        assert!(e.contains("1..=12"), "{bad}: {e}");
    }
    assert_eq!(
        cfg_err("remote_control_key_id = 9\n=>\nremote_control_key_id = 12"),
        ""
    );
}

#[test]
fn old_numeric_forms_name_the_variants() {
    // copy_control with numbers (pre-typed configs)
    let e = cfg_err(&format!(
        "{CC_LINE}\n=>\ncopy_control = {{ recording_control = 2, user_defined = 4, in_pmt = true, in_eit = true }}"
    ));
    assert!(e.contains("\"copy-one-generation\""), "{e}");
    assert!(
        e.contains("\"copy-free\"") && e.contains("\"copy-never\""),
        "{e}"
    );
    // bool video_decode_control
    let e = cfg_err(&format!("{VDC_LINE}\n=>\n  video_decode_control = false"));
    assert!(
        e.contains("mode = \"off\"") || e.contains("mode = \\\"off\\\""),
        "{e}"
    );
    // numeric component_type
    let e = cfg_err(
        "  component_type = \"1080i-16:9-without-pan-vectors\"\n=>\n  component_type = 0xB3",
    );
    assert!(e.contains("1080i-16:9-without-pan-vectors"), "{e}");
    // unknown names list the table
    let e = cfg_err(
        "  component_type = \"1080i-16:9-without-pan-vectors\"\n=>\n  component_type = \"1080i\"",
    );
    assert!(e.contains("1080i-4:3"), "{e}");
    // user_defined raw override conflicts with the typed fields
    let e = cfg_err(&format!(
        "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-one-generation\", copy_control_type = \"undefined-00\", user_defined = 0 }}"
    ));
    assert!(
        e.contains("user_defined and copy_control_type / aps_control_data both given"),
        "{e}"
    );
    // ... and works alone (bisection: user_defined 0 was refused by the recorder)
    assert_eq!(
        cfg_err(&format!(
            "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-one-generation\", user_defined = 0 }}"
        )),
        ""
    );
    assert!(cfg_err(&format!(
        "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-one-generation\", user_defined = 16 }}"
    ))
    .contains("4-bit"));
    // explicit video_decode_control needs its fields
    let e = cfg_err(&format!(
        "{VDC_LINE}\n=>\n  video_decode_control = {{ mode = \"explicit\" }}"
    ));
    assert!(
        e.contains("video_encode_format") && e.contains("\"1080i\""),
        "{e}"
    );
    // audio table on a video stream
    let e = cfg_err(
        "  component_type = \"1080i-16:9-without-pan-vectors\"\n=>\n  component_type = { audio_mode = \"2/0\" }",
    );
    assert!(
        e.contains("mpeg2-video needs a video component_type"),
        "{e}"
    );
    // component loop in the PMT
    let e = cfg_err(&format!(
        "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-free\", component_control = [ {{ component_tag = 0, recording_control = \"copy-never\" }} ] }}"
    ));
    assert!(
        e.contains("component control flag should always be '0'"),
        "{e}"
    );
    // maximum_bitrate range
    assert!(cfg_err(&format!(
        "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-free\", maximum_bitrate_mbps = 64 }}"
    ))
    .contains("63.75"));
}

#[test]
fn maximum_bitrate_and_component_loop_encode() {
    let r = run_example(
        &extra_into(
            isdbt_mux::config::EXAMPLE_TOML,
            &format!(
                "{CC_LINE}\n=>\ncopy_control = {{ recording_control = \"copy-one-generation\", maximum_bitrate_mbps = 16.5, in_pmt = false, in_sdt = false, in_eit = true, component_control = [ {{ component_tag = 0x00, recording_control = \"copy-never\", aps_control_data = \"pseudo-sync\", maximum_bitrate_mbps = 15.1 }} ] }}"
            ),
        ),
        "sig_cc",
        6,
    );
    let s = collect(&r);
    assert!(s.pmt_c1.is_empty() && s.sdt_c1.is_empty());
    let c = &s.eit_c1[0];
    assert_eq!(c.maximum_bitrate_typed(), Some(MaximumBitrate(66)));
    let cc = c.component_control.as_ref().unwrap();
    assert_eq!(
        cc[0].recording_control(),
        DigitalRecordingControl::CopyNever
    );
    assert_eq!(cc[0].user_defined, 0b0101);
    assert_eq!(cc[0].maximum_bitrate, Some(61)); // ⌈15.1 × 4⌉
}

#[test]
fn examples_round_trip_through_toml() {
    for text in [
        isdbt_mux::config::EXAMPLE_TOML,
        include_str!("../examples/bench-minimal.toml"),
    ] {
        let cfg = Config::from_toml(text).unwrap();
        let back = cfg.to_toml().unwrap();
        let again = Config::from_toml(&back).unwrap();
        assert_eq!(again.to_toml().unwrap(), back);
    }
}
