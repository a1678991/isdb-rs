//! Playable files from a clear ISDB-S3 / advanced BS TLV stream: a timed
//! MPEG-TS ([`ts`]) or a Matroska file ([`mkv`]) of every track of a
//! service, with the PTS/DTS of the MPTs' timestamp descriptors
//! ([`timing`]), and its captions as SubRip / WebVTT.
//!
//! The input is a TLV stream whose payloads are in the clear — one that
//! was never scrambled, or one a cardholder's descrambler wrote (its
//! payloads keep the scramble extension with the control bits cleared,
//! which `isdb_tlv::mmtp` reads as clear). A payload still scrambled is
//! skipped: nothing is descrambled here.
//!
//! | Module | Contents |
//! |---|---|
//! | [`signal`] | the MPTs' assets: kind, component tag, service, language, timing descriptors |
//! | [`reassemble`] | MFUs from MMTP packets, the losses the sequence numbers show |
//! | [`timing`] | the MPT clock as 90 kHz PTS/DTS |
//! | [`ts`] | the recorder: pictures at their AU boundaries, loss handling, the MPEG-TS writer, caption alignment |
//! | [`mkv`] | the recorder's Matroska form (`isdb-mkv`) |
//! | [`progress`] | the progress line on stderr |
//!
//! [`Remux::run`] walks the TLV packets: control messages feed
//! [`signal::Signal`] (and the recorder's view of it), MPU packets of the
//! recorded assets the [`reassemble::Reassembler`].

pub mod mkv;
pub mod progress;
pub mod reassemble;
pub mod signal;
pub mod timing;
pub mod ts;

use isdb_mmt::decoder::NTP_PORT;
use isdb_mmt::message::Messages;
use isdb_tlv::ip::{compressed, ipv4, ipv6};
use isdb_tlv::mmtp::{self, PayloadType};
use isdb_tlv::tlv::{FileReader, PacketType};
use std::collections::BTreeMap;
use std::io::Read;

/// What a run read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// TLV packets.
    pub packets: u64,
    /// MMTP packets of the recorded assets.
    pub media_packets: u64,
    /// Of them, still scrambled (skipped).
    pub scrambled: u64,
}

/// What a finished run gives.
#[derive(Debug)]
pub struct Report {
    /// The recorder's counts.
    pub counts: ts::Counts,
    /// The caption tracks (cues on the output's timeline).
    pub captions: Vec<ts::CaptionTrack>,
    /// What the run read.
    pub stats: Stats,
    /// The reassembly's counts per asset packet id.
    pub assets: BTreeMap<u16, reassemble::Stats>,
}

/// The run of one stream into one recorder.
pub struct Remux {
    signal: signal::Signal,
    messages: Messages,
    last_ctrl_seq: BTreeMap<u16, u32>,
    reassembler: reassemble::Reassembler,
    session: ts::Session,
    stats: Stats,
}

impl Remux {
    /// A run into `session` (a TS or Matroska recorder).
    #[must_use]
    pub fn new(session: ts::Session) -> Self {
        Self {
            signal: signal::Signal::new(),
            messages: Messages::new(),
            last_ctrl_seq: BTreeMap::new(),
            reassembler: reassemble::Reassembler::new(),
            session,
            stats: Stats::default(),
        }
    }

    /// Read the TLV stream `r` to its end.
    ///
    /// # Errors
    ///
    /// The read's and the recorder's.
    pub fn run<R: Read>(&mut self, r: R) -> std::io::Result<()> {
        let mut reader = FileReader::new(r);
        let mut tables = Vec::new();
        while let Some(p) = reader.next_packet()? {
            self.stats.packets += 1;
            let d = match p.kind {
                PacketType::Ipv4 => ipv4(&p.data).ok(),
                PacketType::Ipv6 => ipv6(&p.data).ok(),
                PacketType::Compressed => compressed(&p.data).ok(),
                _ => None,
            };
            let Some(d) = d else { continue };
            if d.flow.as_ref().is_some_and(|f| f.dport == NTP_PORT) {
                continue;
            }
            let Some((h, payload)) = mmtp::parse(&d.payload) else {
                continue;
            };
            match h.payload_type {
                PayloadType::Messages => {
                    let gap = self
                        .last_ctrl_seq
                        .insert(h.packet_id, h.sequence)
                        .is_some_and(|l| h.sequence != l.wrapping_add(1));
                    self.messages.push(h.packet_id, payload, gap, &mut tables);
                    for t in tables.drain(..) {
                        self.signal.feed(&t);
                    }
                    if self.signal.take_changed() {
                        self.session.set_timing(&self.signal.assets)?;
                    }
                }
                PayloadType::Mpu => {
                    let Some((_, kind)) = self.signal.asset_of(h.packet_id) else {
                        continue;
                    };
                    if !self.session.records(h.packet_id) {
                        continue;
                    }
                    self.stats.media_packets += 1;
                    if h.scramble_info {
                        self.stats.scrambled += 1;
                    }
                    self.reassembler
                        .push(&mut self.session, &kind, &h, payload)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Finish the recording.
    ///
    /// # Errors
    ///
    /// The recorder's write.
    pub fn finish(mut self) -> std::io::Result<Report> {
        let (counts, captions) = self.session.finish()?;
        Ok(Report {
            counts,
            captions,
            stats: self.stats,
            assets: self.reassembler.stats(),
        })
    }
}
