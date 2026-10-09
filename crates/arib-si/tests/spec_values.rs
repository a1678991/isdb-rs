//! Spec vectors of the typed field values (`arib_si::values`): every named
//! variant maps to exactly the code printed in its B10 table, and the
//! descriptors of the synthetic multiplex (`tests/synthetic`, values as
//! sent by live terrestrial broadcasters) decode to the expected variants.

mod synthetic;

use arib_si::AnyTable;
use arib_si::descriptor::{Descriptor, DescriptorBody, VideoDecodeControl};
use arib_si::values::*;

/// Every (variant, code, name) triple of `table` equals `T`'s mapping and
/// the triples cover `T::NAMED` exactly.
fn assert_table<T: CodedValue + std::fmt::Debug>(table: &[(T, u8, &str)]) {
    for &(v, code, name) in table {
        assert_eq!(v.code(), code, "{} {v:?}", T::TABLE);
        assert_eq!(T::from_code(code), v, "{} {code:#x}", T::TABLE);
        assert_eq!(v.name(), name, "{} {v:?}", T::TABLE);
    }
    let listed: Vec<T> = table.iter().map(|t| t.0).collect();
    assert_eq!(listed, T::NAMED, "{}: variant list differs", T::TABLE);
}

#[test]
fn spec_table_6_60_video_encode_format() {
    use VideoEncodeFormat::*;
    assert_table(&[
        (P1080, 0b0000, "1080p"),
        (I1080, 0b0001, "1080i"),
        (P720, 0b0010, "720p"),
        (P480, 0b0011, "480p"),
        (I480, 0b0100, "480i"),
        (P240, 0b0101, "240p"),
        (P120, 0b0110, "120p"),
        (P2160x60, 0b0111, "2160p60"),
        (P180, 0b1000, "180p"),
        (P2160x120, 0b1001, "2160p120"),
        (P4320x60, 0b1010, "4320p60"),
        (P4320x120, 0b1011, "4320p120"),
    ]);
    for c in 0b1100..=0b1111 {
        assert_eq!(VideoEncodeFormat::from_code(c), Extension(c));
    }
}

#[test]
fn spec_table_6_60_1_transfer_characteristics() {
    use TransferCharacteristics::*;
    assert_table(&[
        (Bt709, 0b00, "bt709"),
        (Pq, 0b01, "pq"),
        (Hlg, 0b10, "hlg"),
        (Undefined, 0b11, "undefined"),
    ]);
}

#[test]
fn spec_table_6_39_digital_recording_control() {
    use DigitalRecordingControl::*;
    assert_table(&[
        (CopyFree, 0b00, "copy-free"),
        (ProviderDefined, 0b01, "provider-defined"),
        (CopyOneGeneration, 0b10, "copy-one-generation"),
        (CopyNever, 0b11, "copy-never"),
    ]);
}

#[test]
fn spec_annex_f_tables_f2_f4() {
    use ApsControlData as A;
    use CopyControlType as C;
    assert_table(&[
        (C::Undefined00, 0b00, "undefined-00"),
        (C::SerialOutputEncoded, 0b01, "serial-output-encoded"),
        (C::Undefined10, 0b10, "undefined-10"),
        (C::SerialOutputUnencoded, 0b11, "serial-output-unencoded"),
    ]);
    assert_table(&[
        (A::CopyFree, 0b00, "copy-free"),
        (A::PseudoSync, 0b01, "pseudo-sync"),
        (A::PseudoSyncBurst2, 0b10, "pseudo-sync-2-line-burst"),
        (A::PseudoSyncBurst4, 0b11, "pseudo-sync-4-line-burst"),
    ]);
    // Table F-1: copy_control_type (2) then APS_control_data (2) iff '01'
    let u = UserDefinedAnnexF::from_code(0b0110);
    assert_eq!(u.copy_control_type, C::SerialOutputEncoded);
    assert_eq!(u.low_bits, AnnexFLowBits::Aps(A::PseudoSyncBurst2));
    let u = UserDefinedAnnexF::from_code(0b1011);
    assert_eq!(u.copy_control_type, C::Undefined10);
    assert_eq!(u.low_bits, AnnexFLowBits::Reserved(0b11));
}

#[test]
fn spec_table_6_5_component_type_mpeg2_video() {
    use VideoComponentType::*;
    assert_table(&[
        (V480i4x3, 0x01, "480i-4:3"),
        (V480i16x9Pan, 0x02, "480i-16:9-with-pan-vectors"),
        (V480i16x9, 0x03, "480i-16:9-without-pan-vectors"),
        (V480iWide, 0x04, "480i-over-16:9"),
        (V4320p16x9, 0x83, "4320p-16:9"),
        (V2160p4x3, 0x91, "2160p-4:3"),
        (V2160p16x9Pan, 0x92, "2160p-16:9-with-pan-vectors"),
        (V2160p16x9, 0x93, "2160p-16:9-without-pan-vectors"),
        (V2160pWide, 0x94, "2160p-over-16:9"),
        (V480p4x3, 0xA1, "480p-4:3"),
        (V480p16x9Pan, 0xA2, "480p-16:9-with-pan-vectors"),
        (V480p16x9, 0xA3, "480p-16:9-without-pan-vectors"),
        (V480pWide, 0xA4, "480p-over-16:9"),
        (V1080i4x3, 0xB1, "1080i-4:3"),
        (V1080i16x9Pan, 0xB2, "1080i-16:9-with-pan-vectors"),
        (V1080i16x9, 0xB3, "1080i-16:9-without-pan-vectors"),
        (V1080iWide, 0xB4, "1080i-over-16:9"),
        (V720p4x3, 0xC1, "720p-4:3"),
        (V720p16x9Pan, 0xC2, "720p-16:9-with-pan-vectors"),
        (V720p16x9, 0xC3, "720p-16:9-without-pan-vectors"),
        (V720pWide, 0xC4, "720p-over-16:9"),
        (V240p4x3, 0xD1, "240p-4:3"),
        (V240p16x9Pan, 0xD2, "240p-16:9-with-pan-vectors"),
        (V240p16x9, 0xD3, "240p-16:9-without-pan-vectors"),
        (V240pWide, 0xD4, "240p-over-16:9"),
        (V1080p4x3, 0xE1, "1080p-4:3"),
        (V1080p16x9Pan, 0xE2, "1080p-16:9-with-pan-vectors"),
        (V1080p16x9, 0xE3, "1080p-16:9-without-pan-vectors"),
        (V1080pWide, 0xE4, "1080p-over-16:9"),
        (V180p4x3, 0xF1, "180p-4:3"),
        (V180p16x9Pan, 0xF2, "180p-16:9-with-pan-vectors"),
        (V180p16x9, 0xF3, "180p-16:9-without-pan-vectors"),
        (V180pWide, 0xF4, "180p-over-16:9"),
    ]);
    // "Reserved for future use" rows
    for c in [
        0x00, 0x05, 0x82, 0x84, 0x90, 0x95, 0xA0, 0xB5, 0xC0, 0xF5, 0xFF,
    ] {
        assert_eq!(VideoComponentType::from_code(c), Reserved(c));
    }
}

#[test]
fn spec_table_6_43_audio_component_type() {
    use AudioMode::*;
    assert_table(&[
        (Mono, 0b00001, "1/0"),
        (DualMono, 0b00010, "1/0+1/0"),
        (Stereo, 0b00011, "2/0"),
        (M2_1, 0b00100, "2/1"),
        (M3_0, 0b00101, "3/0"),
        (M2_2, 0b00110, "2/2"),
        (M3_1, 0b00111, "3/1"),
        (M3_2, 0b01000, "3/2"),
        (M3_2Lfe, 0b01001, "3/2+LFE"),
        (M3_3_1, 0b01010, "3/3.1"),
        (M2_0_0_2_0_2_0_1, 0b01011, "2/0/0-2/0/2-0.1"),
        (M5_2_1, 0b01100, "5/2.1"),
        (M3_2_2_1, 0b01101, "3/2/2.1"),
        (M2_0_0_3_0_2_0_1, 0b01110, "2/0/0-3/0/2-0.1"),
        (M0_2_0_3_0_2_0_1, 0b01111, "0/2/0-3/0/2-0.1"),
        (M2_0_0_3_2_3_0_2, 0b10000, "2/0/0-3/2/3-0.2"),
        (M3_3_3_5_2_3_3_0_0_2, 0b10001, "3/3/3-5/2/3-3/0/0.2"),
    ]);
    assert_eq!(AudioMode::from_code(0), AudioMode::Reserved(0));
    assert_eq!(AudioMode::from_code(0b10010), AudioMode::Reserved(0b10010));
    use AudioForDisabilities as D;
    assert_table(&[
        (D::None, 0b00, "none"),
        (
            D::VisuallyImpairedCommentary,
            0b01,
            "visually-impaired-commentary",
        ),
        (D::HearingImpaired, 0b10, "hearing-impaired"),
    ]);
    // b7 = 1 | b6-b5 = 10 | b4-b0 = 01001
    let t = AudioComponentType {
        dialog_control: true,
        for_disabilities: D::HearingImpaired,
        mode: M3_2Lfe,
    };
    assert_eq!(t.code(), 0b1100_1001);
    assert_eq!(AudioComponentType::from_code(0b1100_1001), t);
}

#[test]
fn spec_table_6_45_sampling_rate() {
    use SamplingRate::*;
    assert_table(&[
        (Khz16, 0b001, "16kHz"),
        (Khz22_05, 0b010, "22.05kHz"),
        (Khz24, 0b011, "24kHz"),
        (Khz32, 0b101, "32kHz"),
        (Khz44_1, 0b110, "44.1kHz"),
        (Khz48, 0b111, "48kHz"),
    ]);
}

#[test]
fn spec_6_2_23_maximum_bitrate_unit() {
    // "rolling up in each 1/4Mbps"
    assert_eq!(MaximumBitrate(66).bps(), 16_500_000);
    assert_eq!(
        MaximumBitrate::from_bps_rounded_up(16_500_000),
        Some(MaximumBitrate(66))
    );
    assert_eq!(
        MaximumBitrate::from_bps_rounded_up(16_500_001),
        Some(MaximumBitrate(67))
    );
    assert_eq!(
        MaximumBitrate::from_bps_rounded_up(63_750_000),
        Some(MaximumBitrate(255))
    );
    assert_eq!(MaximumBitrate::from_bps_rounded_up(63_750_001), None);
}

/// video_decode_control_descriptor byte 0x47, the value live terrestrial
/// HD services carry in the PMT ES loop of their video.
#[test]
fn video_decode_control_0x47() {
    let d = VideoDecodeControl::decode(&[0x47]).unwrap();
    assert!(!d.still_picture_flag);
    assert!(d.sequence_end_code_flag);
    assert_eq!(d.encode_format(), VideoEncodeFormat::I1080);
    assert_eq!(d.transfer(), TransferCharacteristics::Undefined);
}

fn synthetic_sections() -> Vec<(u16, Vec<u8>)> {
    synthetic::sections()
        .into_iter()
        .map(|(pid, _, sec)| (pid, sec))
        .collect()
}

/// Synthetic SDT: every service's digital copy control descriptor is
/// copy-one-generation with user_defined 0b0100 = Annex F copy_control_type
/// '01' + APS_control_data '00'; no maximum_bitrate.
#[test]
fn synthetic_sdt_copy_control() {
    let secs = synthetic_sections();
    let (_, sdt) = secs
        .iter()
        .find(|(pid, s)| *pid == 0x11 && s[0] == 0x42)
        .unwrap();
    let AnyTable::Sdt(sdt) = AnyTable::decode(0x11, sdt).unwrap() else {
        panic!()
    };
    let mut n = 0;
    for s in &sdt.body.services {
        for d in &s.descriptors {
            if let Descriptor::DigitalCopyControl(c) = d {
                assert_eq!(
                    c.recording_control(),
                    DigitalRecordingControl::CopyOneGeneration
                );
                assert_eq!(c.user_defined, 0b0100);
                let u = c.user_defined_annex_f();
                assert_eq!(u.copy_control_type, CopyControlType::SerialOutputEncoded);
                assert_eq!(u.low_bits, AnnexFLowBits::Aps(ApsControlData::CopyFree));
                assert_eq!(c.maximum_bitrate_typed(), None);
                n += 1;
            }
        }
    }
    assert_eq!(n, sdt.body.services.len());
}

/// Synthetic EIT p/f of the HD service: component 0xB3 (1080i 16:9
/// without pan vectors), audio 2/0 at 48 kHz, and a digital copy control
/// descriptor `a4 42`: copy-one-generation, user_defined 0b0100,
/// maximum_bitrate 66 × 1/4 Mbit/s = 16.5 Mbit/s.
#[test]
fn synthetic_eit_components() {
    let secs = synthetic_sections();
    let mut video = 0;
    let mut audio = 0;
    let mut copy = 0;
    for (pid, s) in &secs {
        if s[0] != 0x4E {
            continue;
        }
        let AnyTable::Eit(eit) = AnyTable::decode(*pid, s).unwrap() else {
            panic!()
        };
        if eit.header.table_id_extension != synthetic::HD {
            continue;
        }
        for e in &eit.body.events {
            for d in &e.descriptors {
                match d {
                    Descriptor::Component(c) => {
                        assert_eq!(
                            c.video_component_type(),
                            Some(VideoComponentType::V1080i16x9)
                        );
                        video += 1;
                    }
                    Descriptor::AudioComponent(a) if a.component_tag == 0x10 => {
                        let t = a.audio_component_type();
                        assert_eq!(t.mode, AudioMode::Stereo);
                        assert_eq!(t.for_disabilities, AudioForDisabilities::None);
                        assert!(!t.dialog_control);
                        assert_eq!(a.sampling_rate_typed(), SamplingRate::Khz48);
                        audio += 1;
                    }
                    Descriptor::DigitalCopyControl(c) => {
                        assert_eq!(
                            c.recording_control(),
                            DigitalRecordingControl::CopyOneGeneration
                        );
                        assert_eq!(
                            c.user_defined_annex_f().copy_control_type,
                            CopyControlType::SerialOutputEncoded
                        );
                        assert_eq!(c.maximum_bitrate_typed(), Some(MaximumBitrate(66)));
                        assert_eq!(c.maximum_bitrate_typed().unwrap().bps(), 16_500_000);
                        copy += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(video > 0 && audio > 0 && copy > 0);
}
