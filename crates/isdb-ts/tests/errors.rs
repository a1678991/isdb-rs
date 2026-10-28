#![allow(missing_docs)]
//! Error kinds of the decoders: each malformed input is refused with the
//! variant naming its rule (range, fixed value, table_id of another table),
//! matched on the variant and its fields, not on the message.

use isdb_ts::cc::ContinuityCounter;
use isdb_ts::crc::crc32;
use isdb_ts::packet::{AdaptationField, AdaptationFields, NULL_PACKET, Packet, View};
use isdb_ts::pcr::Pcr;
use isdb_ts::pes::PesPacket;
use isdb_ts::psi::{Pat, PatEntry, Pmt};
use isdb_ts::section::{Long, LongHeader, build_long, split_long, split_short};
use isdb_ts::{Error, Pid};

fn with_crc(mut v: Vec<u8>) -> Vec<u8> {
    let c = crc32(&v);
    v.extend_from_slice(&c.to_be_bytes());
    v
}

fn pat() -> Vec<u8> {
    Pat::section(1, 0, vec![PatEntry::new(1, Pid::new(0x100).unwrap())])
        .encode()
        .unwrap()
}

/// §2.4.4.10: section_syntax_indicator "shall be set to '1'" in a long
/// section header.
#[test]
fn spec_long_section_syntax_indicator_is_a_mismatch() {
    let mut sec = pat();
    sec[1] &= 0x7F;
    let e = split_long(&sec).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Mismatch {
                what: "section_syntax_indicator",
                value: 0,
                expected: 1
            }
        ),
        "{e}"
    );
}

/// A short section has section_syntax_indicator '0' (B10 Part 2 §5.1).
#[test]
fn spec_short_section_syntax_indicator_is_a_mismatch() {
    let e = split_short(&with_crc(vec![0x73, 0xF0, 0x04]), true).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Mismatch {
                what: "section_syntax_indicator",
                value: 1,
                expected: 0
            }
        ),
        "{e}"
    );
}

/// A long section_length shorter than the header remainder and CRC_32 (9)
/// and a short one with CRC shorter than the CRC (4) are out of range.
#[test]
fn spec_section_length_below_its_minimum_is_a_range_error() {
    let e = split_long(&[0x00, 0xB0, 0x05, 0, 0, 0, 0, 0]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "section_length",
                value: 5,
                min: 9,
                ..
            }
        ),
        "{e}"
    );
    let e = split_short(&[0x73, 0x70, 0x02, 0, 0], true).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "section_length",
                value: 2,
                min: 4,
                ..
            }
        ),
        "{e}"
    );
}

/// A typed decoder refuses the table_id of another table.
#[test]
fn table_id_of_another_table_is_not_accepted() {
    let pmt = build_long(&LongHeader::new(0x02, 1, 0), &[0xE1, 0x00, 0xF0, 0x00]).unwrap();
    assert!(Long::<Pmt>::decode(&pmt).is_ok());
    let e = Long::<Pat>::decode(&pmt).unwrap_err();
    assert!(
        matches!(
            e,
            Error::NotAccepted {
                what: "table_id",
                value: 0x02
            }
        ),
        "{e}"
    );
}

/// §2.4.3.5: adaptation_field_length is at most 183 and stays inside the
/// packet; an extension length of 0 cannot hold its flags byte.
#[test]
fn spec_adaptation_field_lengths_out_of_range() {
    let mut p = NULL_PACKET;
    p[3] = 0x30; // adaptation_field_control '11'
    p[4] = 184;
    let e = View::parse(&p).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "adaptation_field_length",
                value: 184,
                max: 183,
                ..
            }
        ),
        "{e}"
    );
    let e = AdaptationField::decode(&[5, 0]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "adaptation_field_length",
                value: 5,
                max: 1,
                ..
            }
        ),
        "{e}"
    );
    // adaptation_field_extension_flag set, adaptation_field_extension_length 0
    let e = AdaptationField::decode(&[2, 0x01, 0x00]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "adaptation_field_extension_length",
                value: 0,
                min: 1,
                ..
            }
        ),
        "{e}"
    );
}

/// §2.4.3.7: packet_start_code_prefix is 0x000001.
#[test]
fn spec_pes_start_code_prefix_is_a_mismatch() {
    let e = PesPacket::decode(&[0x00, 0x00, 0x02, 0xE0, 0x00, 0x00]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Mismatch {
                what: "packet_start_code_prefix",
                value: 2,
                expected: 1
            }
        ),
        "{e}"
    );
}

/// The null-packet constant is `Packet::null` encoded, and `pcr_only` is
/// the packet `with_adaptation` builds for the same fields.
#[test]
fn null_packet_and_pcr_only_constructors_agree() {
    assert_eq!(NULL_PACKET, Packet::null().encode().unwrap());
    let pid = Pid::new(0x1FF).unwrap();
    let cc = ContinuityCounter::from_wire(5);
    let pcr = Pcr::from_ticks(123_456_789);
    let fields = AdaptationFields {
        pcr: Some(pcr),
        ..Default::default()
    };
    assert_eq!(
        Packet::pcr_only(pid, cc, pcr),
        Packet::with_adaptation(pid, false, cc, Some(fields), &[]).unwrap()
    );
}
