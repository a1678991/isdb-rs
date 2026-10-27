//! The `tracing` events of the TLV slot assembler
//! (docs/plans/errors-and-logging.md §2.1): a lost slot is counted in
//! `Stats` and emitted at `debug` with the bytes it cost; a clean stream
//! emits nothing.

use std::sync::{Arc, Mutex};

use isdb_tlv::{Assembler, Packet, PacketType};
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

/// ARIB STD-B44 §3.2 (1): a slot that failed error correction drops the
/// partial packet in the assembler; the drop is `lost_slots` /
/// `skipped_bytes` and one `debug` event naming the bytes.  The whole
/// packets before and after it emit nothing.
#[test]
fn lost_slot_is_a_debug_event_with_skipped_bytes() {
    let p = Packet {
        kind: PacketType::Ipv4,
        data: (0..=u8::MAX).cycle().take(300).collect(),
    };
    let bytes = p.encode().unwrap();
    let mut out = Vec::new();
    let mut stats = None;
    let events = capture(|| {
        let mut a = Assembler::new();
        // slot 1: one whole packet then the first 100 bytes of the next
        let mut slot1 = bytes.clone();
        slot1.extend_from_slice(&bytes[..100]);
        a.push_slot(Some(&slot1), 0, &mut out);
        // slot 2 lost
        a.push_slot(None, 0xFFFF, &mut out);
        // slot 3: a whole packet starting at offset 0
        a.push_slot(Some(&bytes), 0, &mut out);
        stats = Some(a.stats.clone());
    });
    assert_eq!(out.len(), 2);
    let st = stats.unwrap();
    assert_eq!((st.lost_slots, st.skipped_bytes), (1, 100));
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(e.target, "isdb_tlv::tlv");
    assert_eq!(e.field("skipped_bytes"), Some("100"));
    assert_eq!(e.field("lost_slots"), Some("1"));
    assert_eq!(
        e.message,
        "TLV slot lost: 100 bytes of the partial packet dropped, packet sync lost"
    );
}
