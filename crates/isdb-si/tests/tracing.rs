//! The `tracing` event of the section dispatcher
//! (docs/plans/errors-and-logging.md §2.1): a table_id on a PID ARIB
//! STD-B10 Part 2 Table 5-1 does not assign to it is still decoded (as a
//! raw section) and reported at `debug` with `pid` and `table_id`.

use std::sync::{Arc, Mutex};

use isdb_si::any::AnyTable;
use isdb_ts::Pid;
use isdb_ts::section::{LongHeader, build_long};
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

/// Table 5-1: an SDT (table_id 0x42) belongs on PID 0x0011.  On PID
/// 0x0010 it is `AnyTable::Unknown` and one `debug` event; a PAT on its
/// own PID (not an SI table) is `Unknown` without an event.
#[test]
fn table_on_wrong_pid_is_a_debug_event() {
    let sec = build_long(&LongHeader::new(0x42, 0x7FE0, 0), &[0x12, 0x34, 0xFF]).unwrap();
    let mut result = None;
    let events = capture(|| {
        result = Some(AnyTable::decode(Pid::new(0x0010).unwrap(), &sec).unwrap());
    });
    assert!(matches!(result, Some(AnyTable::Unknown(_))));
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::DEBUG);
    assert_eq!(e.target, "isdb_si::any");
    assert_eq!(e.field("pid"), Some("0x0010"));
    assert_eq!(e.field("table_id"), Some("66"));
    assert_eq!(
        e.message,
        "PID 0x0010: table_id 0x42 is not assigned to this PID (B10 Part 2 Table 5-1); kept as a raw section"
    );

    let pat = build_long(&LongHeader::new(0x00, 1, 0), &[0, 1, 0xE1, 0x00]).unwrap();
    let events = capture(|| {
        assert!(matches!(
            AnyTable::decode(Pid::PAT, &pat).unwrap(),
            AnyTable::Unknown(_)
        ));
    });
    assert!(
        events.is_empty(),
        "a PAT on PID 0 is not a deviation: {events:?}"
    );
}
