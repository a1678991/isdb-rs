//! What the recorder needs of the MMT-SI: per asset of every MPT, its
//! kind, component tag, service, language, and the timing its timestamp
//! descriptors accumulate.
//!
//! Every copy of an MPT is read, not just a new version: the MPU
//! timestamp and extended-timestamp descriptors move on with every copy
//! while the version stays (a whole BS recording on one version). Each
//! copy names only the current and next few MPUs, so the times and rows
//! accumulate ([`AssetTiming`]); [`crate::timing::au_times`] resolves
//! them.

use isdb_mmt::desc::{Descriptor, MpuExtended, tag};
use isdb_mmt::es::SubtitleInfo;
use isdb_mmt::message::TableBytes;
use isdb_mmt::table::Mpt;
use std::collections::BTreeMap;

/// One asset as every MPT copy describes it.
#[derive(Debug, Clone, Default)]
pub struct AssetTiming {
    /// The asset type (`hev1`, `mp4a`, `stpp`, `aapp`, …).
    pub kind: [u8; 4],
    /// The MH-stream identifier descriptor's component tag.
    pub component_tag: Option<u16>,
    /// The service of the MPT naming it (its package id's low 16 bits).
    pub service: Option<u16>,
    /// Its language (ISO 639-2) per the video / audio component
    /// descriptor (`8010h`, `8014h`, B60 §7.4.3.14/16).
    pub language: Option<String>,
    /// The audio's `component_type` (B60 Table 7-31: `01h` mono, `03h`
    /// stereo, `09h` 3/2+LFE …).
    pub audio_component_type: Option<u8>,
    /// The NTP presentation time of the MPU's first access unit in
    /// presentation order (`MpuTimestamp`, §7.4.3.5), by MPU sequence
    /// number (NTP 64-bit: seconds since 1900 in the high 32 bits).
    pub mpu_times: BTreeMap<u32, u64>,
    /// The extended-timestamp descriptor's timescale (§7.4.3.35).
    pub timescale: Option<u32>,
    /// Its per-MPU rows (the decode-order AU offsets), by MPU sequence
    /// number; the default PTS offset already filled in.
    pub rows: BTreeMap<u32, MpuExtended>,
    /// A caption asset's additional arib subtitle information as the MPTs
    /// changed it, oldest first: the clock of its documents (B60 Table 9-3). See
    /// [`AssetTiming::subtitle_at`].
    pub subtitles: Vec<SubtitleInfo>,
}

impl AssetTiming {
    /// The MPUs kept per map (a few minutes of video MPUs; the oldest go
    /// first).
    const KEEP: usize = 512;
    /// The subtitle information kept: one may wait for its start MPU while the
    /// one before it still applies.
    const KEEP_SUBTITLES: usize = 16;

    /// The subtitle information valid for a document of `mpu`: the latest listed
    /// that names no start, or whose `start_mpu_sequence_number` ("the first MPU
    /// sequence number ... for which setting by this descriptor is valid", Table 9-3)
    /// is not after `mpu` on the sequence number's ring.
    #[must_use]
    pub fn subtitle_at(&self, mpu: u32) -> Option<SubtitleInfo> {
        self.subtitles
            .iter()
            .rev()
            .find(|i| i.start_mpu.is_none_or(|s| mpu.wrapping_sub(s) < 1 << 31))
            .copied()
    }

    /// Take one descriptor of the asset's loop; whether it changed
    /// anything.
    pub(crate) fn add(&mut self, d: &Descriptor) -> bool {
        let mut changed = false;
        match d {
            Descriptor::MpuTimestamp(rows) => {
                for row in rows {
                    changed |= self
                        .mpu_times
                        .insert(row.mpu_sequence_number, row.presentation_time)
                        != Some(row.presentation_time);
                }
            }
            Descriptor::ExtendedTimestamp(e) => {
                if e.timescale.is_some() && self.timescale != e.timescale {
                    self.timescale = e.timescale;
                    changed = true;
                }
                for m in &e.mpus {
                    changed |= self.rows.get(&m.mpu_sequence_number) != Some(m);
                    self.rows.insert(m.mpu_sequence_number, m.clone());
                }
            }
            Descriptor::Other(r) if r.tag == tag::DATA_CODING => {
                if let Some(info) = SubtitleInfo::parse(&r.body)
                    && self.subtitles.last() != Some(&info)
                {
                    self.subtitles.push(info);
                    if self.subtitles.len() > Self::KEEP_SUBTITLES {
                        self.subtitles.remove(0);
                    }
                    changed = true;
                }
            }
            _ => {}
        }
        while self.mpu_times.len() > Self::KEEP {
            self.mpu_times.pop_first();
        }
        while self.rows.len() > Self::KEEP {
            self.rows.pop_first();
        }
        changed
    }
}

/// The assets of every MPT seen, by asset packet id.
#[derive(Debug, Default)]
pub struct Signal {
    /// The assets, accumulated.
    pub assets: BTreeMap<u16, AssetTiming>,
    changed: bool,
}

impl Signal {
    /// An empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One table of a control message: an MPT (table id 20h) of a PA
    /// message updates its assets.
    pub fn feed(&mut self, t: &TableBytes) {
        if t.message_id != 0x0000 || t.bytes.first() != Some(&0x20) {
            return;
        }
        let Some(mpt) = Mpt::parse(&t.bytes) else {
            return;
        };
        let service = mpt.service_id();
        for asset in &mpt.assets {
            let Some(pid) = asset.packet_id() else {
                continue;
            };
            let a = self.assets.entry(pid).or_default();
            let tag = asset.component_tag();
            let (language, audio_type) = component_language(&asset.descriptors);
            if (
                a.kind,
                a.component_tag,
                a.service,
                &a.language,
                a.audio_component_type,
            ) != (asset.asset_type, tag, service, &language, audio_type)
            {
                a.kind = asset.asset_type;
                a.component_tag = tag;
                a.service = service;
                a.language = language;
                a.audio_component_type = audio_type;
                self.changed = true;
            }
            for d in &asset.descriptors {
                self.changed |= a.add(d);
            }
        }
    }

    /// Which service and asset type `packet_id` holds.
    #[must_use]
    pub fn asset_of(&self, packet_id: u16) -> Option<(u16, [u8; 4])> {
        self.assets
            .get(&packet_id)
            .and_then(|a| Some((a.service?, a.kind)))
    }

    /// Whether the assets took anything new since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// The language (and an audio's `component_type`) of an asset's video
/// component descriptor (`8010h`: video fields, component tag, a flags
/// byte, then the ISO 639 code — `63 e8 00 00 3f "jpn"` on BS17) or
/// MH-audio component descriptor (`8014h`: stream content / component
/// type, component tag, stream type, simulcast group, flags, then the
/// ISO 639 code — `f3 09 00 10 11 00 5f "jpn"`). Without the optional
/// text, the code ends the descriptor.
fn component_language(descriptors: &[Descriptor]) -> (Option<String>, Option<u8>) {
    let lang = |b: &[u8]| {
        let l = b.get(..3)?;
        l.iter()
            .all(u8::is_ascii_lowercase)
            .then(|| String::from_utf8_lossy(l).into_owned())
    };
    for d in descriptors {
        if let Descriptor::Other(r) = d {
            match r.tag {
                0x8010 => return (r.body.get(5..).and_then(lang), None),
                0x8014 => return (r.body.get(7..).and_then(lang), r.body.get(1).copied()),
                _ => {}
            }
        }
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_mmt::desc::Raw;

    #[test]
    fn component_languages_of_bs17() {
        let v = Descriptor::Other(Raw {
            tag: 0x8010,
            body: vec![0x63, 0xe8, 0, 0, 0x3f, b'j', b'p', b'n'],
        });
        assert_eq!(component_language(&[v]), (Some("jpn".into()), None));
        let a = Descriptor::Other(Raw {
            tag: 0x8014,
            body: vec![0xf3, 0x09, 0, 0x10, 0x11, 0, 0x5f, b'j', b'p', b'n'],
        });
        assert_eq!(component_language(&[a]), (Some("jpn".into()), Some(0x09)));
        assert_eq!(component_language(&[]), (None, None));
    }

    /// The reference moves with each event: a new one is a change, and what
    /// is not caption information leaves it.
    #[test]
    fn subtitle_info_follows_the_mpt() {
        let info = |reference: u8| {
            let mut body = vec![0x00, 0x20, 0x30, 0x07];
            body.extend(b"jpn");
            body.extend([0x01, 0x2a, 0x10, reference, 0, 0, 0, 0, 0, 0, 0, 0x3f]);
            Descriptor::Other(Raw {
                tag: tag::DATA_CODING,
                body,
            })
        };
        let mut a = AssetTiming::default();
        assert!(a.add(&info(1)));
        assert!(!a.add(&info(1)), "another copy of the same");
        assert!(a.add(&info(2)));
        let held = a.subtitle_at(0).and_then(|i| i.reference_start_time);
        assert_eq!(held, Some(2u64 << 56));
        let multimedia = Descriptor::Other(Raw {
            tag: tag::DATA_CODING,
            body: vec![0x00, 0x21, 0, 0, 0, 0, 0, 0, 0, 0],
        });
        assert!(!a.add(&multimedia));
        assert_eq!(a.subtitle_at(0).and_then(|i| i.reference_start_time), held);
    }

    /// Table 9-3: a descriptor with a start MPU waits for it; the one before
    /// applies until then, across the sequence number's wrap.
    #[test]
    fn spec_table_9_3_subtitle_info_waits_for_its_start_mpu() {
        let info = |reference: u64, start: Option<u32>| SubtitleInfo {
            tag: 0x30,
            version: 0,
            language: *b"jpn",
            kind: 0,
            format: 0,
            operation_mode: 1,
            time_control: SubtitleInfo::TMD_REFERENCE,
            display_mode: 0b1010,
            resolution: 1,
            compression: 0,
            start_mpu: start,
            reference_start_time: Some(reference),
            leap_indicator: Some(0),
        };
        let reference_at = |a: &AssetTiming, mpu| a.subtitle_at(mpu)?.reference_start_time;
        let mut a = AssetTiming::default();
        assert_eq!(a.subtitle_at(0), None);
        a.subtitles = vec![info(1, None), info(2, Some(u32::MAX))];
        assert_eq!(reference_at(&a, u32::MAX - 1), Some(1));
        assert_eq!(reference_at(&a, u32::MAX), Some(2));
        assert_eq!(reference_at(&a, 3), Some(2), "past the wrap");
        // Nothing valid yet for an MPU before the only start.
        a.subtitles = vec![info(2, Some(100))];
        assert_eq!(reference_at(&a, 99), None);
        // One with no start applies at once, whatever waits behind it.
        a.subtitles = vec![info(2, Some(100)), info(3, None)];
        assert_eq!(reference_at(&a, 99), Some(3));
    }

    #[test]
    fn subtitle_info_history_is_bounded() {
        let mut a = AssetTiming::default();
        for n in 0..=u8::try_from(AssetTiming::KEEP_SUBTITLES).unwrap() {
            let mut body = vec![0x00, 0x20, 0x30, 0x07];
            body.extend(b"jpn");
            body.extend([0x01, 0x2a, 0x10, n, 0, 0, 0, 0, 0, 0, 0, 0x3f]);
            a.add(&Descriptor::Other(Raw {
                tag: tag::DATA_CODING,
                body,
            }));
        }
        assert_eq!(a.subtitles.len(), AssetTiming::KEEP_SUBTITLES);
    }
}
