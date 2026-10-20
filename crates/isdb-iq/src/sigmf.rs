//! SigMF metadata (`NAME.sigmf-meta`) of an IQ recording.
//!
//! [SigMF](https://sigmf.org) 1.2 is a JSON file next to the raw samples
//! with three parts: `global` (sample format, rate, recorder),
//! `captures` (segments of contiguous samples, each with its own centre
//! frequency and start time) and `annotations` (unused here, written
//! empty). This module reads and writes the `core:` fields it needs and
//! keeps every other field, so a recording made elsewhere (maia-sdr, GNU
//! Radio) reads back and re-writes without loss of its global and capture
//! fields.
//!
//! # The `isdb` extension
//!
//! Fields this project adds, declared in `core:extensions` as `isdb`
//! version [`EXTENSION_VERSION`], optional (a reader may ignore them):
//!
//! | Field | Where | Meaning |
//! |---|---|---|
//! | `isdb:full_scale` | global | component value of 1.0 for `ci16_le` (2048: 12-bit AD936x samples sign-extended by libiio; absent = 32768) |
//! | `isdb:lo_hz` | capture | LO frequency read back from the radio (`core:frequency` is the nominal centre) |
//! | `isdb:rf_bandwidth_hz` | capture | analog bandwidth read back from the radio |
//! | `isdb:gain` | capture | gain mode and value as the radio reported it, e.g. `AGC slow_attack, 33 dB at the end` |
//! | `isdb:channel` | capture | the channel name the centre frequency was chosen by (`bs1`, `nd2`, `ch27`), if any |
//! | `isdb:channel_rf_hz` | capture | that channel's RF centre on air (for a satellite channel tuned at its IF) |
//! | `isdb:capture_method` | capture | how the samples were taken (`libiio single buffer`, `libiio stream …`, `maia-sdr recorder IQ8bit: …`) |
//! | `isdb:gap_before` | capture | `true`: samples between this segment and the previous one were lost (the segments were captured one after another, or a streamed capture lost samples) |
//! | `isdb:samples_discarded` | capture | with `isdb:gap_before`: samples received around the loss but not written, because the radio reported the loss without its position (the loss itself is at least one more sample) |
//! | `isdb:samples_discarded_after` | capture | samples received after this segment but not written: the capture ended in the samples discarded around a loss |
//!
//! Each capture segment covers the samples from its `core:sample_start`
//! to the next segment's (or to the end of the data file); within a
//! segment the samples are contiguous in time.

use crate::{Error, Result, SampleFormat};
use serde_json::{Map, Value, json};

/// The SigMF version written in `core:version`.
pub const SIGMF_VERSION: &str = "1.2.0";
/// Name of this project's SigMF extension namespace.
pub const EXTENSION: &str = "isdb";
/// Version of the `isdb` extension written in `core:extensions`.
pub const EXTENSION_VERSION: &str = "0.2.0";

/// One capture segment: contiguous samples from `sample_start`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Segment {
    /// `core:sample_start`: index of the segment's first sample.
    pub sample_start: u64,
    /// `core:frequency`: centre frequency in Hz.
    pub frequency: Option<f64>,
    /// `core:datetime`: ISO 8601 UTC time of the first sample.
    pub datetime: Option<String>,
    /// Every other field of the segment (`isdb:*` and foreign ones).
    pub extra: Map<String, Value>,
}

/// The metadata of one recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    /// `core:datatype` (with `isdb:full_scale`).
    pub format: SampleFormat,
    /// `core:sample_rate` in Hz.
    pub sample_rate: f64,
    /// `core:description`.
    pub description: Option<String>,
    /// `core:recorder`.
    pub recorder: Option<String>,
    /// `core:hw`.
    pub hw: Option<String>,
    /// Every other global field (except `core:version` and
    /// `core:extensions`, which are written by [`Meta::to_json`]).
    pub global_extra: Map<String, Value>,
    /// The capture segments, in increasing `sample_start` order.
    pub captures: Vec<Segment>,
}

fn opt_str(m: &mut Map<String, Value>, k: &str) -> Option<String> {
    m.remove(k).and_then(|v| v.as_str().map(str::to_string))
}

impl Meta {
    /// Metadata for `format` at `sample_rate` Hz with no segment.
    pub fn new(format: SampleFormat, sample_rate: f64) -> Self {
        Self {
            format,
            sample_rate,
            description: None,
            recorder: None,
            hw: None,
            global_extra: Map::new(),
            captures: Vec::new(),
        }
    }

    /// The JSON text (pretty-printed, newline-terminated).
    pub fn to_json(&self) -> String {
        let mut g = Map::new();
        g.insert("core:datatype".into(), json!(self.format.sigmf_datatype()));
        g.insert("core:sample_rate".into(), json!(self.sample_rate));
        g.insert("core:version".into(), json!(SIGMF_VERSION));
        for (k, v) in [
            ("core:description", &self.description),
            ("core:recorder", &self.recorder),
            ("core:hw", &self.hw),
        ] {
            if let Some(v) = v {
                g.insert(k.into(), json!(v));
            }
        }
        if let SampleFormat::Cs16 { full_scale } = self.format
            && full_scale != 32768.0
        {
            g.insert("isdb:full_scale".into(), json!(full_scale));
        }
        for (k, v) in &self.global_extra {
            g.insert(k.clone(), v.clone());
        }
        let prefix = format!("{EXTENSION}:");
        let uses_ext = g.keys().any(|k| k.starts_with(&prefix))
            || self
                .captures
                .iter()
                .any(|c| c.extra.keys().any(|k| k.starts_with(&prefix)));
        if uses_ext {
            g.insert(
                "core:extensions".into(),
                json!([{"name": EXTENSION, "version": EXTENSION_VERSION, "optional": true}]),
            );
        }
        let captures: Vec<Value> = self
            .captures
            .iter()
            .map(|c| {
                let mut m = Map::new();
                m.insert("core:sample_start".into(), json!(c.sample_start));
                if let Some(f) = c.frequency {
                    m.insert("core:frequency".into(), json!(f));
                }
                if let Some(d) = &c.datetime {
                    m.insert("core:datetime".into(), json!(d));
                }
                for (k, v) in &c.extra {
                    m.insert(k.clone(), v.clone());
                }
                Value::Object(m)
            })
            .collect();
        let doc = json!({"global": g, "captures": captures, "annotations": []});
        let mut s = serde_json::to_string_pretty(&doc).expect("JSON values serialise");
        s.push('\n');
        s
    }

    /// Parse SigMF metadata. Needs `core:datatype` and `core:sample_rate`;
    /// unknown fields are kept.
    pub fn from_json(text: &str) -> Result<Self> {
        let bad = |m: &str| Error::Format(format!("SigMF metadata: {m}"));
        let doc: Value = serde_json::from_str(text).map_err(|e| bad(&e.to_string()))?;
        let mut g = doc
            .get("global")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| bad("no global object"))?;
        let datatype = opt_str(&mut g, "core:datatype").ok_or_else(|| bad("no core:datatype"))?;
        let full_scale = g
            .remove("isdb:full_scale")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32);
        let format = SampleFormat::from_sigmf(&datatype, full_scale)?;
        let sample_rate = g
            .remove("core:sample_rate")
            .and_then(|v| v.as_f64())
            .filter(|r| r.is_finite() && *r > 0.0)
            .ok_or_else(|| bad("no positive core:sample_rate"))?;
        g.remove("core:version");
        g.remove("core:extensions");
        let description = opt_str(&mut g, "core:description");
        let recorder = opt_str(&mut g, "core:recorder");
        let hw = opt_str(&mut g, "core:hw");
        let mut captures = Vec::new();
        for c in doc
            .get("captures")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
        {
            let mut m = c
                .as_object()
                .cloned()
                .ok_or_else(|| bad("capture is not an object"))?;
            let sample_start = m
                .remove("core:sample_start")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let frequency = m.remove("core:frequency").and_then(|v| v.as_f64());
            let datetime = opt_str(&mut m, "core:datetime");
            captures.push(Segment {
                sample_start,
                frequency,
                datetime,
                extra: m,
            });
        }
        if captures
            .windows(2)
            .any(|w| w[1].sample_start <= w[0].sample_start)
        {
            return Err(bad(
                "capture segments not in increasing core:sample_start order",
            ));
        }
        Ok(Self {
            format,
            sample_rate,
            description,
            recorder,
            hw,
            global_extra: g,
            captures,
        })
    }

    /// The sample ranges of the capture segments in a recording of
    /// `total` samples (one range `0..total` without segments).
    pub fn segment_ranges(&self, total: u64) -> Vec<std::ops::Range<u64>> {
        if self.captures.is_empty() {
            return std::iter::once(0..total).collect();
        }
        let starts: Vec<u64> = self
            .captures
            .iter()
            .map(|c| c.sample_start.min(total))
            .collect();
        starts
            .iter()
            .enumerate()
            .map(|(i, &s)| s..starts.get(i + 1).copied().unwrap_or(total))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_extension_fields() {
        let mut m = Meta::new(SampleFormat::AD936X_CS16, 57_719_999.0);
        m.recorder = Some("isdb capture".into());
        m.hw = Some("PlutoSky".into());
        let mut extra = Map::new();
        extra.insert("isdb:lo_hz".into(), json!(1_049_479_998u64));
        extra.insert("isdb:channel".into(), json!("bs1"));
        m.captures.push(Segment {
            sample_start: 0,
            frequency: Some(1_049_480_000.0),
            datetime: Some("2026-10-20T08:15:30.125Z".into()),
            extra: extra.clone(),
        });
        extra.insert("isdb:gap_before".into(), json!(true));
        m.captures.push(Segment {
            sample_start: 1000,
            frequency: Some(1_049_480_000.0),
            datetime: None,
            extra,
        });
        let text = m.to_json();
        assert!(text.contains("\"core:datatype\": \"ci16_le\""), "{text}");
        assert!(text.contains("\"isdb:full_scale\": 2048.0"), "{text}");
        assert!(text.contains("\"name\": \"isdb\""), "{text}");
        let back = Meta::from_json(&text).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.segment_ranges(1500), vec![0..1000, 1000..1500]);
    }

    #[test]
    fn reads_a_maia_sdr_recording() {
        // As written by maia-sdr 0.6 (Tezuka v0.3.24): 12-bit samples
        // left-aligned in 16 bits, so no full-scale field (32768).
        let text = r#"{"annotations":[],"captures":[{"core:datetime":"1970-01-01T00:46:42.251Z",
            "core:frequency":1049479998.0,"core:sample_start":0}],
            "global":{"core:author":"","core:datatype":"ci16_le","core:description":"",
            "core:recorder":"Maia SDR v0.6.0","core:sample_rate":57719999.0,"core:version":"1.2.3"}}"#;
        let m = Meta::from_json(text).unwrap();
        assert_eq!(
            m.format,
            SampleFormat::Cs16 {
                full_scale: 32768.0
            }
        );
        assert_eq!(m.sample_rate, 57_719_999.0);
        assert_eq!(m.recorder.as_deref(), Some("Maia SDR v0.6.0"));
        assert_eq!(m.global_extra.get("core:author"), Some(&json!("")));
        assert_eq!(m.captures[0].frequency, Some(1_049_479_998.0));
        assert_eq!(
            m.segment_ranges(10),
            std::iter::once(0..10).collect::<Vec<_>>()
        );
        // No isdb field: no extension declared.
        assert!(
            !Meta::from_json(text)
                .unwrap()
                .to_json()
                .contains("core:extensions")
        );
    }

    #[test]
    fn malformed_metadata() {
        for bad in [
            "{}",
            r#"{"global":{"core:sample_rate":1.0}}"#,
            r#"{"global":{"core:datatype":"ci16_le"}}"#,
            r#"{"global":{"core:datatype":"ci16_le","core:sample_rate":-1}}"#,
            r#"{"global":{"core:datatype":"cu8","core:sample_rate":1}}"#,
            r#"{"global":{"core:datatype":"ci8","core:sample_rate":1},
                "captures":[{"core:sample_start":5},{"core:sample_start":5}]}"#,
        ] {
            assert!(Meta::from_json(bad).is_err(), "{bad}");
        }
    }
}
