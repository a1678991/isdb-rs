//! The MFUs of the recorded assets, reassembled from their MMTP packets
//! (the clear path of `isdb_mmt::mpu::Reassembler`'s state machine, with
//! the 16 MiB MFU ceiling and the untimed `item_id` path), and handed to
//! the recorder ([`Session`]) with the losses the packet sequence
//! numbers show.
//!
//! A loss does not only drop the MFU under reassembly: the recorder hears
//! of it ([`Session::lost`]) with what it can still use — a video MFU cut
//! at the gap (its NAL units up to there, the cut one's length prefix
//! shortened to the bytes that came: a slice decodes up to the loss), and
//! for audio the number of frames lost while every packet so far carried
//! one complete, unaggregated MFU (one frame each, as on air).

use crate::ts::Session;
use isdb_mmt::mpu::{self, FragmentType, FragmentationIndicator, MAX_MFU};
use isdb_tlv::mmtp::Header;
use std::collections::BTreeMap;
use tracing::debug;

/// One asset's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// MMTP packets.
    pub packets: u64,
    /// MMTP packets lost (sequence gaps).
    pub packets_lost: u64,
    /// Whole MFUs delivered.
    pub mfus: u64,
    /// MFUs dropped (a fragment lost, oversize, or an unparsed payload).
    pub mfus_dropped: u64,
}

/// A partial MFU under reassembly: its MPU sequence, the item id of its
/// head fragment, the bytes so far.
#[derive(Debug)]
struct Partial {
    mpu: u32,
    item: Option<u32>,
    data: Vec<u8>,
}

/// Per-asset reassembly state.
#[derive(Debug)]
struct State {
    last_seq: Option<u32>,
    partial: Option<Partial>,
    /// Every packet so far carried one complete, unaggregated MFU.
    single_units: bool,
    stats: Stats,
}

impl Default for State {
    fn default() -> Self {
        Self {
            last_seq: None,
            partial: None,
            single_units: true,
            stats: Stats::default(),
        }
    }
}

/// The length-prefixed NAL units of an MFU cut short: the whole ones, and
/// the cut one with its 4-byte length set to the bytes that came (at
/// least its 2-byte NAL header); `None` when nothing is left.
pub(crate) fn cut_nal_units(mfu: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(mfu.len());
    let mut p = 0;
    while let Some(len) = mfu.get(p..p + 4) {
        let n = u32::from_be_bytes(len.try_into().ok()?) as usize;
        let body = &mfu[p + 4..];
        if body.len() >= n {
            out.extend_from_slice(&mfu[p..p + 4 + n]);
            p += 4 + n;
            continue;
        }
        if body.len() > 2 {
            out.extend_from_slice(&u32::try_from(body.len()).ok()?.to_be_bytes());
            out.extend_from_slice(body);
        }
        break;
    }
    (!out.is_empty()).then_some(out)
}

/// The reassembler over the stream's MPU packets.
#[derive(Debug, Default)]
pub struct Reassembler {
    states: BTreeMap<u16, State>,
}

impl Reassembler {
    /// A reassembler with nothing seen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The counts per asset packet id.
    #[must_use]
    pub fn stats(&self) -> BTreeMap<u16, Stats> {
        self.states.iter().map(|(&p, s)| (p, s.stats)).collect()
    }

    /// Feed one MMTP MPU packet of an asset of `kind` (`hev1`, `mp4a`,
    /// `stpp`, …) into `ts`. A scrambled payload is skipped (its losses
    /// still counted).
    ///
    /// # Errors
    ///
    /// The recorder's write.
    pub fn push(
        &mut self,
        ts: &mut Session,
        kind: &[u8; 4],
        h: &Header,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let pid = h.packet_id;
        let video = matches!(kind, b"hev1" | b"hvc1");
        let state = self.states.entry(pid).or_default();
        state.stats.packets += 1;
        if let Some(l) = state.last_seq
            && h.sequence != l.wrapping_add(1)
        {
            let lost = h.sequence.wrapping_sub(l).wrapping_sub(1);
            state.stats.packets_lost += u64::from(lost);
            debug!(
                packet_id = pid,
                expected = l.wrapping_add(1),
                sequence = h.sequence,
                lost,
                "MMTP packet sequence gap"
            );
            let cut = state.partial.take();
            if cut.is_some() {
                state.stats.mfus_dropped += 1;
            }
            // Audio: a packet is one complete MFU (one frame), when the
            // asset's packets were so far.
            let units = (kind == b"mp4a" && state.single_units)
                .then(|| usize::try_from(lost).ok())
                .flatten();
            if video
                && let Some(c) = cut
                && let Some(data) = cut_nal_units(&c.data)
            {
                ts.video_mfu(pid, c.mpu, &data)?;
            }
            ts.lost(pid, units)?;
        }
        let state = self.states.entry(pid).or_default();
        state.last_seq = Some(h.sequence);
        if h.scramble_info {
            return Ok(());
        }
        let Some((ph, units)) = mpu::parse(payload) else {
            state.stats.mfus_dropped += 1;
            debug!(
                packet_id = pid,
                sequence = h.sequence,
                "MPU payload does not parse"
            );
            return Ok(());
        };
        if ph.fragment_type != FragmentType::Mfu {
            return Ok(());
        }
        state.single_units &= ph.fragmentation == FragmentationIndicator::Complete
            && !ph.aggregated
            && units.len() == 1;
        for u in units {
            let unit = Partial {
                mpu: ph.mpu_sequence_number,
                item: u.item_id,
                data: u.data.to_vec(),
            };
            if let Some(whole) = self.append(pid, ph.fragmentation, unit) {
                deliver(ts, kind, pid, whole)?;
            }
        }
        Ok(())
    }

    /// One data unit into the state machine; a whole MFU when it
    /// completes one.
    fn append(&mut self, pid: u16, frag: FragmentationIndicator, unit: Partial) -> Option<Partial> {
        let state = self.states.entry(pid).or_default();
        match frag {
            FragmentationIndicator::Complete => {
                if state.partial.take().is_some() {
                    state.stats.mfus_dropped += 1;
                }
                state.stats.mfus += 1;
                Some(unit)
            }
            FragmentationIndicator::Head => {
                if state.partial.take().is_some() {
                    state.stats.mfus_dropped += 1;
                }
                state.partial = Some(unit);
                None
            }
            FragmentationIndicator::Middle | FragmentationIndicator::Last => {
                // None: the tail of an MFU begun before the stream was
                // joined.
                let m = state.partial.as_mut()?;
                if m.data.len() + unit.data.len() > MAX_MFU {
                    state.partial = None;
                    state.stats.mfus_dropped += 1;
                    debug!(packet_id = pid, "MFU dropped: over the 16 MiB ceiling");
                    return None;
                }
                m.data.extend_from_slice(&unit.data);
                if frag == FragmentationIndicator::Last {
                    state.stats.mfus += 1;
                    return state.partial.take();
                }
                None
            }
        }
    }
}

/// One whole MFU to the recorder, by its asset's kind.
fn deliver(ts: &mut Session, kind: &[u8; 4], pid: u16, m: Partial) -> std::io::Result<()> {
    match kind {
        b"hev1" | b"hvc1" => ts.video_mfu(pid, m.mpu, &m.data),
        b"mp4a" => ts.audio_mfu(pid, m.mpu, &m.data),
        b"stpp" => {
            ts.caption_mfu(pid, &m.data);
            Ok(())
        }
        _ => ts.data_mfu(pid, m.mpu, m.item, &m.data),
    }
}
