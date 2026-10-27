//! The `tracing` events of the continuity checker and the section
//! assembler (docs/plans/errors-and-logging.md §2.1): a capturing `Layer`
//! records every event a behaviour the crate already tests produces, and
//! the test asserts its level and fields.  The events are in addition to
//! the values the API returns; nothing here replaces those tests.

use std::sync::{Arc, Mutex};

use isdb_ts::cc::{CcChecker, CcEventKind, ContinuityCounter};
use isdb_ts::demux::{Assembled, DemuxEvent, DemuxEventKind, DiscardReason, SectionAssembler};
use isdb_ts::packet::{PACKET_SIZE, Packet};
use isdb_ts::section::{LongHeader, build_long, split_long};
use isdb_ts::{Error, Pid};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// One captured event: level, target, message and the other fields as
/// `(name, value formatted with Debug or Display)`.
#[derive(Debug, Clone)]
struct Captured {
    level: Level,
    target: String,
    message: String,
    fields: Vec<(String, String)>,
}

impl Captured {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct Visitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .push((field.name().to_string(), value.to_string()));
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Captured>>>);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = Visitor::default();
        event.record(&mut v);
        self.0.lock().unwrap().push(Captured {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message: v.message,
            fields: v.fields,
        });
    }
}

/// Run `f` with a capturing subscriber installed for the current thread
/// and return what it captured.
fn capture(f: impl FnOnce()) -> Vec<Captured> {
    let cap = Capture::default();
    let sub = tracing_subscriber::registry().with(cap.clone());
    tracing::subscriber::with_default(sub, f);
    cap.0.lock().unwrap().clone()
}

fn cc(v: u8) -> ContinuityCounter {
    ContinuityCounter::new(v).unwrap()
}

fn payload_pkt(pid: Pid, c: u8) -> [u8; PACKET_SIZE] {
    Packet::with_payload(pid, false, cc(c), &[c; 184])
        .unwrap()
        .encode()
        .unwrap()
}

/// §2.4.3.3: a gap in the continuity_counter is a `Break` event, and the
/// checker emits it at `debug` with `pid`, `expected` and `got`.  The
/// continuous packets around it emit nothing.
#[test]
fn cc_break_is_a_debug_event_with_pid_and_expected() {
    let pid = Pid::new(0x100).unwrap();
    let mut returned = Vec::new();
    let events = capture(|| {
        let mut ch = CcChecker::new();
        // counters 5, 6, then 9: 7 was expected
        for (i, c) in [5u8, 6, 9, 10].into_iter().enumerate() {
            returned.extend(ch.push(i as u64, &payload_pkt(pid, c)));
        }
    });
    assert_eq!(returned.len(), 1);
    assert_eq!(
        returned[0].kind,
        CcEventKind::Break {
            expected: cc(7),
            got: cc(9)
        }
    );
    assert_eq!(events.len(), 1, "one event for one deviation: {events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(e.target, "isdb_ts::cc");
    assert_eq!(e.field("pid"), Some("0x0100"));
    assert_eq!(e.field("expected"), Some("7"));
    assert_eq!(e.field("got"), Some("9"));
    assert_eq!(e.field("packet_index"), Some("2"));
    assert_eq!(
        e.message,
        "PID 0x0100: continuity_counter break at packet 2: expected 7, got 9"
    );
}

/// §2.4.3.3: the partial section a continuity break discards is a
/// `Discarded` event, emitted at `debug` with the `table_id` of the
/// section that was in progress.
#[test]
fn discarded_partial_section_is_a_debug_event_with_table_id() {
    let pid = Pid::new(0x11).unwrap();
    let body: Vec<u8> = (0..=u8::MAX).cycle().take(400).collect();
    let sec = build_long(&LongHeader::new(0x42, 1, 0), &body).unwrap();
    let pkt = |pusi: bool, c: u8, payload: &[u8]| {
        let mut p = payload.to_vec();
        p.resize(184, 0xFF);
        Packet::with_payload(pid, pusi, cc(c), &p)
            .unwrap()
            .encode()
            .unwrap()
    };
    let mut out = Vec::new();
    let events = capture(|| {
        let mut a = SectionAssembler::new(pid);
        let mut p0 = vec![0u8];
        p0.extend_from_slice(&sec[..183]);
        out.extend(a.push(0, &pkt(true, 0, &p0)));
        // packet cc=1 lost
        out.extend(a.push(2, &pkt(false, 2, &sec[367..])));
    });
    assert!(matches!(
        &out[..],
        [Assembled::Event(DemuxEvent {
            kind: DemuxEventKind::Discarded {
                reason: DiscardReason::ContinuityBreak,
                bytes: 183
            },
            ..
        })]
    ));
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(e.target, "isdb_ts::demux");
    assert_eq!(e.field("pid"), Some("0x0011"));
    assert_eq!(e.field("table_id"), Some("66"));
    assert_eq!(e.field("packet_index"), Some("2"));
    assert_eq!(
        e.message,
        "PID 0x0011: table_id 0x42 partial section dropped (183 bytes): continuity break (packet 2)"
    );
}

/// Annex A: a CRC_32 mismatch is `Error::Crc` and a `debug` event with
/// the `table_id` and the residue.
#[test]
fn crc_failure_is_a_debug_event() {
    let mut sec = build_long(&LongHeader::new(0x42, 0x1234, 3), &[1, 2, 3]).unwrap();
    let last = sec.len() - 1;
    sec[last] ^= 0x01;
    let mut result = None;
    let events = capture(|| {
        result = Some(split_long(&sec).map(|(h, _)| h));
    });
    assert!(matches!(result, Some(Err(Error::Crc { .. }))));
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(e.target, "isdb_ts::section");
    assert_eq!(e.field("table_id"), Some("66"));
    assert_eq!(e.field("table_id_extension"), Some("4660"));
    assert!(
        e.message
            .starts_with("table_id 0x42 (extension 0x1234, section 0): CRC_32 mismatch")
    );
}

/// A stream with no deviation emits nothing: the events are for what
/// went wrong, not for every packet.
#[test]
fn a_continuous_stream_emits_nothing() {
    let pid = Pid::new(0x100).unwrap();
    let events = capture(|| {
        let mut ch = CcChecker::new();
        for i in 0..40u8 {
            assert_eq!(ch.push(u64::from(i), &payload_pkt(pid, i % 16)), None);
        }
    });
    assert!(events.is_empty(), "{events:?}");
}
