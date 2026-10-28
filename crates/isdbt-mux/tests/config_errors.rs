//! Configuration errors are typed: each rule a configuration breaks comes
//! back as its [`MuxError`] variant with the TOML key (`service[0].stream[1].pid`)
//! as a field. One bad input per variant, matched on the variant, not on
//! the message.

mod common;

use common::bench_params;
use isdb_ts::Pid;
use isdbt_core::Layer;
use isdbt_mux::{Config, Multiplexer, MuxError};
use std::path::Path;

/// One service on layer B with a declared stream; `service` and `stream`
/// lines are added to those tables, `tail` after the stream.
fn text(service: &str, stream: &str, tail: &str) -> String {
    format!(
        r#"
[network]
network_id = 0x79EE
name = "ベンチ"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "ベンチHD"
provider = "ベンチ"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
{service}

  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
{stream}
{tail}
"#
    )
}

/// The error of loading `t` and building the multiplexer from it.
fn error(t: &str) -> MuxError {
    let e = match Config::from_toml(t) {
        Err(e) => e,
        Ok(cfg) => match Multiplexer::new(&cfg, &bench_params(), Path::new(".")) {
            Err(e) => e,
            Ok(_) => panic!("accepted:\n{t}"),
        },
    };
    // every configuration variant renders its key
    if let Some(k) = e.key() {
        assert!(e.to_string().contains(k), "{e}");
    }
    e
}

#[test]
fn the_base_configuration_builds() {
    let cfg = Config::from_toml(&text("", "", "")).unwrap();
    Multiplexer::new(&cfg, &bench_params(), Path::new(".")).unwrap();
}

#[test]
fn toml_syntax() {
    let e = error(&text("", "  colour = 1", ""));
    assert!(matches!(e, MuxError::Toml { .. }), "{e}");
}

#[test]
fn hex() {
    let e = error(&text(
        "",
        "  descriptors = [ { tag = 0x80, hex = \"0g\" } ]",
        "",
    ));
    assert!(
        matches!(&e, MuxError::Hex { key, bad: Some(d), .. }
            if key == "service[0].stream[0].descriptors[0].hex" && d == "0g"),
        "{e}"
    );
    let e = error(&text(
        "",
        "",
        "[nit]\nadd_descriptors = [ { loop = \"network\", tag = 0x80, hex = \"abc\" } ]",
    ));
    assert!(
        matches!(&e, MuxError::Hex { key, bad: None, .. } if key == "nit.add_descriptors[0].hex"),
        "{e}"
    );
}

#[test]
fn format_and_time_range() {
    let e = error(&text("", "", "").replace("2026-01-01T12:00:00", "2026-01-01 25:00:00"));
    assert!(
        matches!(&e, MuxError::Format { key, .. } if key == "clock.start"),
        "{e}"
    );
    let e = error(&text(
        "",
        "",
        "  [[service.event]]\n  event_id = 1\n  duration = \"1:00\"\n  name = \"a\"\n  text = \"b\"",
    ));
    assert!(
        matches!(&e, MuxError::Format { key, .. } if key == "service[0].event[0].duration"),
        "{e}"
    );
    let e = error(&text("", "", "").replace("2026-01-01T12:00:00", "2040-01-01T12:00:00"));
    assert!(
        matches!(&e, MuxError::TimeRange { key, .. } if key == "clock.start"),
        "{e}"
    );
}

#[test]
fn range() {
    let e = error(&text("", "", "").replace("channels = [27]", "channels = [27, 70]"));
    assert!(
        matches!(&e, MuxError::Range { key, value: 70, min: 13, max: 62, .. } if key == "network.channels[1]"),
        "{e}"
    );
    let e =
        error(&text("", "", "").replace("remote_control_key_id = 9", "remote_control_key_id = 13"));
    // a serde-level check: the toml message names the key and the range
    assert!(matches!(e, MuxError::Toml { .. }), "{e}");
    assert!(e.to_string().contains("1..=12"), "{e}");
}

#[test]
fn reserved() {
    let e = error(&text(
        "  [service.hybridcast]\n  organization_id = 0\n  application_id = 1\n  url = \"https://example.com/a/index.html\"\n  carriage = \"es\"\n  pid = 0x0170\n  data_component_id = 0x0020",
        "",
        "",
    ));
    assert!(
        matches!(&e, MuxError::Reserved { key, value: 0, .. } if key == "service[0].hybridcast.organization_id"),
        "{e}"
    );
}

#[test]
fn pid_range_and_pid_in_use() {
    let e = error(&text("", "", "").replace("pid = 0x0111", "pid = 0x0005"));
    assert!(
        matches!(&e, MuxError::PidRange { key, pid } if key == "service[0].stream[0].pid" && pid.get() == 5),
        "{e}"
    );
    // a second stream on the PID of the first
    let e = error(&text(
        "",
        "",
        "  [[service.stream]]\n  kind = \"declared\"\n  pid = 0x0111\n  stream_type = 0x0F",
    ));
    assert!(
        matches!(&e, MuxError::PidInUse { key, pid, by, other_layer: false }
            if key == "service[0].stream[1].pid" && *pid == Pid::new(0x0111).unwrap() && by == "service[0].stream[0].pid"),
        "{e}"
    );
    // ... on another layer
    let e = error(&text(
        "",
        "",
        "  [[service.stream]]\n  kind = \"declared\"\n  pid = 0x0111\n  stream_type = 0x0F\n  layer = \"A\"",
    ));
    assert!(
        matches!(
            &e,
            MuxError::PidInUse {
                other_layer: true,
                ..
            }
        ),
        "{e}"
    );
}

#[test]
fn inactive_layer() {
    let e = error(&text("", "", "").replace("layer = \"B\"", "layer = \"C\""));
    assert!(
        matches!(&e, MuxError::InactiveLayer { key, layer: Layer::C } if key == "service[0].layer"),
        "{e}"
    );
}

#[test]
fn missing() {
    let e = error(&text("", "", "").replace("  stream_type = 0x02\n", ""));
    assert!(
        matches!(&e, MuxError::Missing { key, .. } if key == "service[0].stream[0].stream_type"),
        "{e}"
    );
    let e = error(&text("", "", "").replace("start = \"2026-01-01T12:00:00\"", ""));
    assert!(
        matches!(&e, MuxError::Missing { key, .. } if key == "clock.start"),
        "{e}"
    );
    let e = error(&text(
        "",
        "",
        "[[raw_table]]\npid = 0x0030\nlayer = \"A\"\ninterval_ms = 1000\nsections = []",
    ));
    assert!(
        matches!(&e, MuxError::Missing { key, .. } if key == "raw_table[0].sections"),
        "{e}"
    );
}

#[test]
fn not_applicable_conflict_duplicate() {
    let e = error(&text("", "  at_eof = \"end\"", ""));
    assert!(
        matches!(&e, MuxError::NotApplicable { key, .. } if key == "service[0].stream[0].at_eof"),
        "{e}"
    );
    let e = error(&text(
        "",
        "",
        "  [[service.event]]\n  event_id = 1\n  start = \"2026-01-01T12:00:00\"\n  start_offset_s = 0\n  duration = \"01:00:00\"\n  name = \"a\"\n  text = \"b\"",
    ));
    assert!(
        matches!(&e, MuxError::Conflict { key, other } if key == "service[0].event[0].start" && other == "start_offset_s"),
        "{e}"
    );
    let e = error(&text(
        "",
        "  kind = \"ts-program\"\n  source = \"x.ts\"\n  pid_map = [[0x0140, 0x0111], [0x0140, 0x0112]]",
        "",
    )
    .replace("  kind = \"declared\"\n  pid = 0x0111\n  stream_type = 0x02\n", ""));
    assert!(
        matches!(&e, MuxError::Duplicate { key, .. } if key == "service[0].stream[0].pid_map[1]"),
        "{e}"
    );
}

#[test]
fn free_form_rules_name_the_key() {
    let e = error(&text(
        "",
        "",
        "[nit]\nadd_descriptors = [ { loop = \"nowhere\", tag = 0x80, hex = \"00\" } ]",
    ));
    assert!(
        matches!(&e, MuxError::Config { key, .. } if key == "nit.add_descriptors[0].loop"),
        "{e}"
    );
    let e = error(&text("", "", "").replace("channels = [27]", "channels = []"));
    assert!(
        matches!(&e, MuxError::Config { key, .. } if key == "network.channels"),
        "{e}"
    );
}

#[test]
fn playback_control_after_the_first_frame() {
    let cfg = Config::from_toml(&text("", "", "")).unwrap();
    let mut m = Multiplexer::new(&cfg, &bench_params(), Path::new(".")).unwrap();
    m.next_frame_full();
    let e = m.enable_playback_control().unwrap_err();
    assert!(matches!(e, MuxError::CallOrder { .. }), "{e}");
}
