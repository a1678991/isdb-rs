//! The `tracing` events of the MFU reassembler
//! (docs/plans/errors-and-logging.md §2.1): a gap in the MMTP packet
//! sequence numbers drops the MFU in progress; both the gap and the drop
//! are counted in `AssetStats` and emitted at `debug` with the
//! `packet_id`.  A clean sequence emits nothing.

use std::sync::{Arc, Mutex};

use isdb_mmt::mpu::{FragmentType, FragmentationIndicator, Reassembler};
use isdb_tlv::mmtp::{FecType, Header, PayloadType};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

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

fn capture(f: impl FnOnce()) -> Vec<Captured> {
    let cap = Capture::default();
    let sub = tracing_subscriber::registry().with(cap.clone());
    tracing::subscriber::with_default(sub, f);
    cap.0.lock().unwrap().clone()
}

/// An MPU payload (B60 Table 6-1) carrying one timed MFU or fragment.
fn payload(frag: FragmentationIndicator, counter: u8, mpu: u32, data: &[u8]) -> Vec<u8> {
    let mut p = vec![
        0,
        0,
        (FragmentType::Mfu.code() << 4) | 0x08 | (frag.code() << 1),
        counter,
    ];
    p.extend(mpu.to_be_bytes()); // MPU sequence number
    p.extend(0u32.to_be_bytes()); // movie fragment sequence number
    p.extend(1u32.to_be_bytes()); // sample number
    p.extend(0u32.to_be_bytes()); // offset
    p.extend([0, 0]); // priority, dependency counter
    p.extend(data);
    let len = u16::try_from(p.len() - 2).unwrap();
    p[..2].copy_from_slice(&len.to_be_bytes());
    p
}

fn header(id: u16, seq: u32) -> Header {
    Header {
        fec_type: FecType::NotProtected,
        rap: false,
        payload_type: PayloadType::Mpu,
        packet_id: id,
        sequence: seq,
        scramble_info: false,
    }
}

/// §6.3.2: packet 5 is lost between the head and the last fragment of an
/// MFU; the MFU is dropped.  Two `debug` events, both with `packet_id`:
/// the sequence gap (`expected`, `got`, `lost`) and the drop (`reason`).
#[test]
fn lost_fragment_drops_the_mfu_with_debug_events() {
    use FragmentationIndicator::{Head, Last};
    let mut out = Vec::new();
    let mut stats = None;
    let events = capture(|| {
        let mut r = Reassembler::new();
        r.push(&header(9, 4), &payload(Head, 1, 6, b"xx"), &mut out);
        r.push(&header(9, 6), &payload(Last, 0, 6, b"yy"), &mut out);
        stats = Some(r.stats().next().unwrap().1.clone());
    });
    assert!(out.is_empty());
    let st = stats.unwrap();
    assert_eq!((st.packets_lost, st.mfus_dropped), (1, 1));
    assert_eq!(events.len(), 2, "{events:?}");
    let gap = &events[0];
    assert_eq!(gap.level, Level::DEBUG);
    assert_eq!(gap.target, "isdb_mmt::mpu");
    assert_eq!(gap.field("packet_id"), Some("9"));
    assert_eq!(gap.field("expected"), Some("5"));
    assert_eq!(gap.field("got"), Some("6"));
    assert_eq!(gap.field("lost"), Some("1"));
    assert_eq!(
        gap.message,
        "packet id 0x0009: MMTP packet sequence gap, expected 5, got 6 (1 packets lost)"
    );
    let drop = &events[1];
    assert_eq!(drop.level, Level::DEBUG);
    assert_eq!(drop.field("packet_id"), Some("9"));
    assert_eq!(drop.field("reason"), Some("fragment lost"));
    assert_eq!(
        drop.message,
        "packet id 0x0009: MFU dropped, a fragment was lost"
    );
}

/// A divided MFU whose fragments all arrive emits nothing.
#[test]
fn a_clean_sequence_emits_nothing() {
    use FragmentationIndicator::{Head, Last, Middle};
    let mut out = Vec::new();
    let events = capture(|| {
        let mut r = Reassembler::new();
        r.push(&header(9, 1), &payload(Head, 2, 5, b"ab"), &mut out);
        r.push(&header(9, 2), &payload(Middle, 1, 5, b"cd"), &mut out);
        r.push(&header(9, 3), &payload(Last, 0, 5, b"ef"), &mut out);
    });
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].data, b"abcdef");
    assert!(events.is_empty(), "{events:?}");
}
