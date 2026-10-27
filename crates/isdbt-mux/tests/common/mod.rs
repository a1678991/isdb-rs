#![allow(dead_code, missing_docs)]
use std::path::PathBuf;

use isdbt_core::{
    CodeRate, GuardInterval, LayerConfig, Mode, Modulation, Params, Segments, TimeInterleave,
};
use isdbt_mux::Config;

/// `x` (a frame or packet count computed in f64) truncated toward zero.
pub fn count(x: f64) -> usize {
    // f64 -> usize saturates; the tests count at most a few thousand frames.
    #[allow(clippy::cast_possible_truncation)]
    let n = x as usize;
    n
}

/// Bench parameters: Mode 3, GI 1/8, partial reception, A = 1 seg QPSK 2/3,
/// B = 12 seg 64QAM 3/4 (the `isdb tx` defaults).
pub fn bench_params() -> Params {
    Params::new(
        Mode::M3,
        GuardInterval::G1_8,
        true,
        [
            Some(LayerConfig {
                segments: Segments::new(1).unwrap(),
                modulation: Modulation::Qpsk,
                code_rate: CodeRate::R2_3,
                time_interleave: TimeInterleave::K2,
            }),
            Some(LayerConfig {
                segments: Segments::new(12).unwrap(),
                modulation: Modulation::Qam64,
                code_rate: CodeRate::R3_4,
                time_interleave: TimeInterleave::K2,
            }),
            None,
        ],
    )
    .unwrap()
}

/// Synthetic MPEG-2 video: closed GOPs of 13 pictures (I P B B …),
/// 1440×1080i, frame_rate_code 4, `gops` GOPs.
pub fn synth_video(gops: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..gops {
        // sequence_header 1440x1080, aspect 3 (16:9), frc 4
        v.extend_from_slice(&[0, 0, 1, 0xB3]);
        let (w, h): (u32, u32) = (1440, 1080);
        v.extend_from_slice(&[
            u8::try_from(w >> 4).unwrap(),
            u8::try_from((w & 0xF) << 4 | h >> 8).unwrap(),
            (h & 0xFF) as u8,
            0x34,
        ]);
        let br = 12_500u32; // ×400 = 5 Mbit/s
        let vbv = 112u32;
        let x: u32 = br << 14 | 1 << 13 | vbv << 3;
        v.extend_from_slice(&x.to_be_bytes());
        // sequence_extension: Main@High-1440 (0x46), interlaced, 4:2:0, marker
        v.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x62, 0x00, 0x01, 0x00, 0x00]);
        // GOP header (marker bit set), closed_gop (H.262 §6.2.2.6)
        v.extend_from_slice(&[0, 0, 1, 0xB8, 0x00, 0x08, 0x00, 0x40]);
        // coding order: I0, then (P, B, B) × 4 with display 3k / 3k-2 / 3k-1
        let mut pics: Vec<(u16, u8, usize)> = vec![(0, 1, 40_000)];
        for k in 1..=4u16 {
            pics.push((3 * k, 2, 15_000));
            pics.push((3 * k - 2, 3, 6_000));
            pics.push((3 * k - 1, 3, 6_000));
        }
        for (tr, ty, size) in pics {
            v.extend_from_slice(&[0, 0, 1, 0x00]);
            let w: u32 = u32::from(tr) << 22 | u32::from(ty) << 19 | 0x1234 << 3;
            v.extend_from_slice(&w.to_be_bytes());
            v.extend_from_slice(&[0, 0, 1, 0xB5, 0x8F, 0xFF, 0xF3, 0x80, 0x80]);
            v.extend_from_slice(&[0, 0, 1, 0x01]);
            v.extend(std::iter::repeat_n(0x55u8, size));
        }
    }
    v
}

/// Synthetic ADTS (LC, 48 kHz, stereo) frames of 386 bytes (≈ 144.8
/// kbit/s): silent but syntactically valid AAC (one CPE with max_sfb 0,
/// fill elements) with crc_check and a constant-rate
/// adts_buffer_fullness, so the default `adts_compliance = "conform"`
/// passes them unchanged.
pub fn synth_audio(frames: usize) -> Vec<u8> {
    let f = isdbt_mux::audio::SilentFrame::stereo_48k(386).build();
    f.repeat(frames)
}

pub fn tmpdir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A test configuration with synthetic media (written into `dir`) plus
/// `extra` TOML appended.
pub fn test_config(dir: &std::path::Path, extra: &str) -> Config {
    std::fs::write(dir.join("v.m2v"), synth_video(3)).unwrap();
    std::fs::write(dir.join("a.adts"), synth_audio(150)).unwrap();
    let text = format!(
        r#"
[network]
network_id = 0x79EE
name = "ベンチ"
area_code = 0x5A5
remote_control_key_id = 12
channels = [27]
transmission_types = [ {{ info = 0x0F, layer = "B" }}, {{ info = 0xAF, layer = "A" }} ]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-10-08T11:59:50"

[[service]]
service_id = 0x0400
name = "ベンチHD"
provider = "ベンチ"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = {{ recording_control = "copy-free" }}

  [[service.stream]]
  kind = "mpeg2-video"
  pid = 0x0111
  component_tag = 0x00
  source = "v.m2v"

  [[service.stream]]
  kind = "aac-adts"
  pid = 0x0112
  component_tag = 0x10
  source = "a.adts"

  [[service.event]]
  event_id = 1
  start = "2026-10-08T11:00:00"
  duration = "01:00:00"
  name = "前の番組"
  content = [0x00]

  [[service.event]]
  event_id = 2
  duration = "00:30:00"
  name = "次の番組"

[[service]]
service_id = 0x0588
service_type = 0xC0
name = "ワンセグ"
layer = "A"
pmt_pid = 0x1FC8
pcr_pid = 0x01FE

  [[service.stream]]
  kind = "declared"
  pid = 0x0181
  stream_type = 0x1B
  component_tag = 0x81

  [[service.event]]
  event_id = 1
  start = "2026-10-08T11:00:00"
  duration = "01:00:00"
  name = "前の番組"

[bit]
broadcasters = [ {{ broadcaster_id = 1, name = "ベンチ", terrestrial = {{ terrestrial_broadcaster_id = 0x0001, affiliation_ids = [1] }} }} ]

{extra}
"#
    );
    Config::from_toml(&text).unwrap()
}

// ---- tracing event capture (docs/plans/errors-and-logging.md §2.1)

/// One captured `tracing` event.
#[derive(Debug, Clone)]
pub struct Captured {
    pub level: tracing::Level,
    pub target: String,
    /// The `message` field.
    pub message: String,
    /// Every field in `Debug` form (`%x` values print as their `Display`,
    /// strings quoted, numbers bare).
    pub fields: Vec<(String, String)>,
    thread: std::thread::ThreadId,
}

impl Captured {
    /// The `Debug` form of field `name`, if the event has it.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Clone)]
struct CaptureLayer(std::sync::Arc<std::sync::Mutex<Vec<Captured>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct V(Vec<(String, String)>);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0
                    .push((field.name().to_string(), format!("{value:?}")));
            }
        }
        let mut v = V(Vec::new());
        event.record(&mut v);
        let message =
            v.0.iter()
                .find(|(k, _)| k == "message")
                .map(|(_, m)| m.clone())
                .unwrap_or_default();
        self.0.lock().unwrap().push(Captured {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message,
            fields: v.0,
            thread: std::thread::current().id(),
        });
    }
}

/// Run `f` and return its result with the events it emitted on this
/// thread.
///
/// The capturing subscriber is installed as the process-wide default (once
/// per test binary) rather than as a thread-scoped one: `tracing-core`
/// caches a callsite's interest the first time any thread hits it, and
/// with exactly one registered, thread-scoped dispatcher it computes that
/// interest from the hitting thread's own default (`Rebuilder::JustOne`),
/// so a parallel test without a subscriber would cache `never` for every
/// callsite of the code under test. A global default is what every thread
/// sees; the events of other tests are filtered out by thread id.
pub fn capture_events<R>(f: impl FnOnce() -> R) -> (R, Vec<Captured>) {
    use tracing_subscriber::layer::SubscriberExt;
    static STORE: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<Captured>>>> =
        std::sync::OnceLock::new();
    let store = STORE.get_or_init(|| {
        let store = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(CaptureLayer(store.clone())),
        )
        .expect("the test binary installs one global subscriber");
        store
    });
    let start = store.lock().unwrap().len();
    let r = f();
    let me = std::thread::current().id();
    let events = store.lock().unwrap()[start..]
        .iter()
        .filter(|e| e.thread == me)
        .cloned()
        .collect();
    (r, events)
}
