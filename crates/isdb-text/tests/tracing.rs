//! The `tracing` events of the text decoder
//! (docs/plans/errors-and-logging.md §2.1): every `Issue` a decode
//! records is also a `trace` event (per character, so never louder) with
//! `offset` and `kind`; a string that decodes cleanly emits nothing.

use std::sync::{Arc, Mutex};

use isdb_text::AribString;
use isdb_text::decode::IssueKind;
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

/// Table 7-14 Note 2: 0x01 is a blank of the C0 area, an
/// `UndefinedControl` issue at offset 0 and one `trace` event; the Kanji
/// that follows decodes without a word.
#[test]
fn undefined_control_code_is_a_trace_event() {
    let mut decoded = None;
    let events = capture(|| {
        decoded = Some(AribString(vec![0x01, 0x30, 0x21]).decode());
    });
    let d = decoded.unwrap();
    assert_eq!(d.text, "亜");
    assert_eq!(d.issues.len(), 1);
    assert_eq!(d.issues[0].kind, IssueKind::UndefinedControl(0x01));
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    assert_eq!(e.level, Level::TRACE);
    assert_eq!(e.target, "isdb_text::decode");
    assert_eq!(e.field("offset"), Some("0"));
    assert_eq!(e.field("kind"), Some("UndefinedControl(1)"));
    assert_eq!(e.message, "ARIB text: UndefinedControl(1) at byte 0");

    let events = capture(|| {
        assert_eq!(
            AribString(vec![0x30, 0x21, 0x30, 0x22]).decode().text,
            "亜唖"
        );
    });
    assert!(events.is_empty(), "{events:?}");
}
