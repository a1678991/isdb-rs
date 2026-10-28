#![allow(missing_docs)]
//! Error kinds of the SI codecs: each malformed input or out-of-range value
//! is refused with the `isdb_ts::Error` variant naming its rule, matched on
//! the variant and its fields, not on the message.

use isdb_si::AnyTable;
use isdb_si::dsmcc::{Ddb, module_blocks};
use isdb_si::tables::StuffingSection;
use isdb_si::time::{BcdDuration, Date, DateTime, MjdTime, u8_to_bcd};
use isdb_ts::crc::crc32;
use isdb_ts::{Error, Pid};

/// table_id 0xFF is stuffing, not a table (B10 Part 2 §5.1.2; "forbidden"
/// in H.222.0 Table 2-31).
#[test]
fn spec_table_id_0xff_is_reserved() {
    let e = AnyTable::decode(Pid::new(0x10).unwrap(), &[0xFF, 0xFF, 0xFF]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Reserved {
                what: "table_id",
                value: 0xFF
            }
        ),
        "{e}"
    );
}

/// The stuffing_section decoder takes table_id 0x72 only (B10 Part 2
/// §5.2.11).
#[test]
fn stuffing_section_refuses_another_table_id() {
    let e = StuffingSection::decode(&[0x70, 0x70, 0x00]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::NotAccepted {
                what: "table_id",
                value: 0x70
            }
        ),
        "{e}"
    );
}

/// STD-B24 Vol. 3 Table 6-22: protocolDiscriminator is 0x11.
#[test]
fn spec_dsmcc_protocol_discriminator_is_a_mismatch() {
    let ddb = module_blocks(1, 0, 0, 4066, &[1, 2, 3]).unwrap();
    let mut sec = ddb[0].encode().unwrap();
    // dsmcc_section header (8 bytes), then dsmccDownloadDataHeader.
    sec[8] = 0x12;
    let n = sec.len() - 4;
    let c = crc32(&sec[..n]);
    sec[n..].copy_from_slice(&c.to_be_bytes());
    let e = Ddb::decode(&sec).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Mismatch {
                what: "protocolDiscriminator",
                value: 0x12,
                expected: 0x11
            }
        ),
        "{e}"
    );
}

/// A blockSize of 0 cannot split a module (Vol. 3 §6.3.1).
#[test]
fn zero_block_size_is_a_range_error() {
    let e = module_blocks(1, 0, 0, 0, &[1]).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "blockSize",
                value: 0,
                min: 1,
                ..
            }
        ),
        "{e}"
    );
}

/// B10 Part 2 §5.2.8 / Annex C: hours 0..23, minutes and seconds 0..59,
/// BCD digit pairs 0..99.
#[test]
fn spec_time_fields_out_of_range() {
    let dt = DateTime {
        date: Date {
            year: 2026,
            month: 10,
            day: 28,
        },
        hour: 24,
        minute: 0,
        second: 0,
    };
    let e = MjdTime::from_jst(dt).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "JST_time hour",
                value: 24,
                max: 23,
                ..
            }
        ),
        "{e}"
    );
    let e = BcdDuration::from_hms(1, 60, 0).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "duration minutes",
                value: 60,
                max: 59,
                ..
            }
        ),
        "{e}"
    );
    let e = u8_to_bcd(100).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Range {
                what: "BCD digit pair",
                value: 100,
                max: 99,
                ..
            }
        ),
        "{e}"
    );
}
