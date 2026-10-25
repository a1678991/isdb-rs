//! A superframe's main signal to TS packets: the inner decoder of
//! [`crate::demod`] on its 8 frames, then de-interleaving, energy
//! dispersal removal and sync-byte restoration ([`isdbs_core::mainsig`]),
//! RS(204,188), and the relative TS ID → TS_ID of the TMCC.
//!
//! Packets whose RS word is uncorrectable are kept with
//! transport_error_indicator = 1 (H.222.0 §2.4.3.3), as `isdbt-phy` does.

use crate::demod::{DemodOptions, decode_frame};
use crate::rx::{Reception, Superframe};
use isdb_fec::RS_204_188;
use isdbs_core::frame::FRAMES;
use isdbs_core::mainsig::codewords;
use isdbs_core::tmcc::TmccInfo;

/// One decoded TS packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// TS_ID of its slot (from the TMCC table).
    pub ts_id: u16,
    /// Frame in the superframe (0..8) and slot (0..48) it was sent in.
    pub frame: usize,
    /// Slot number, from 0.
    pub slot: usize,
    /// The 188 bytes (sync byte 0x47 first).
    pub bytes: [u8; 188],
    /// RS outcome: Some(corrected bytes) or None when uncorrectable
    /// (transport_error_indicator set).
    pub corrected: Option<usize>,
}

/// One decoded superframe.
#[derive(Debug, Clone)]
pub struct DecodedSuperframe {
    /// Its packets, in transmission order (frame by frame, slot by slot).
    pub packets: Vec<Packet>,
    /// The TMCC it was decoded under.
    pub info: TmccInfo,
    /// Mean main-signal MER of its frames (decided points), dB.
    pub mer_db: f64,
}

/// Decode superframe `sf` of `r` under its own TMCC, or under `fallback`
/// when its RS(64,48) failed (the TMCC changes at most every superframe,
/// with advance notice in "order of change", BO.1408-1 §6.1.1). `None`
/// when no TMCC is known.
pub fn decode_superframe(
    r: &Reception,
    sf: &Superframe,
    fallback: Option<&TmccInfo>,
    o: &DemodOptions,
) -> Option<DecodedSuperframe> {
    let info = sf.info.as_ref().or(fallback)?.clone();
    let fb: Vec<_> = (0..FRAMES)
        .map(|j| decode_frame(&r.frames[sf.first_frame + j].main, &info, j == 0, o))
        .collect();
    let refs: [&[u8]; FRAMES] = std::array::from_fn(|j| fb[j].bytes.as_slice());
    let ts = info.slot_ts_ids();
    let packets = codewords(&info, &refs)
        .into_iter()
        .map(|(frame, slot, mut cw)| {
            let corrected = RS_204_188.decode(&mut cw).ok();
            let mut bytes: [u8; 188] = cw[..188].try_into().expect("188");
            if corrected.is_none() {
                bytes[1] |= 0x80;
            }
            Packet {
                ts_id: ts[slot].unwrap_or(isdbs_core::tmcc::UNUSED_TS_ID),
                frame,
                slot,
                bytes,
                corrected,
            }
        })
        .collect();
    let mer_db = fb.iter().map(|f| f.mer_db).sum::<f64>() / FRAMES as f64;
    Some(DecodedSuperframe {
        packets,
        info,
        mer_db,
    })
}

/// Decode every superframe of `r`, in order, carrying the last good TMCC
/// over superframes whose own TMCC failed.
pub fn decode_all(r: &Reception, o: &DemodOptions) -> Vec<DecodedSuperframe> {
    let mut last: Option<TmccInfo> = None;
    let mut out = Vec::new();
    for sf in &r.superframes {
        if let Some(d) = decode_superframe(r, sf, last.as_ref(), o) {
            last = Some(d.info.clone());
            out.push(d);
        }
    }
    out
}
