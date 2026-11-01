//! A chain frame as `Slots` blocks (`isdb_frontend::slots`): what the
//! FPGA's packer (stage 9) sends for it, in this order: the frame's slots
//! (all 120, dummies flagged, when the frame was decoded), its TMCC, its
//! status.

use super::frame::{FrameOut, SlotOut, Status};
use isdb_frontend::slots::{Block, SlotBlock, Status as SlotsStatus, TmccBlock, bytes_to_bits};
use isdbs3_core::params::HEADER_BITS;

/// The rate's TMCC code and the modulation's (Tables 3.11-1/2).
fn codes(s: &SlotOut) -> (u8, u8) {
    (s.modulation.code(), s.rate.code())
}

/// The `Slots` blocks of `f` (the layout lag `lag` gives the TMCC
/// block's first frame).
pub fn blocks(f: &FrameOut, lag: u32) -> Vec<Block> {
    let mut out = Vec::with_capacity(f.slots.len() + 2);
    for s in &f.slots {
        let (m, r) = codes(s);
        let bits = if s.dummy {
            Vec::new()
        } else {
            let mut b = s.header.clone();
            debug_assert_eq!(b.len(), HEADER_BITS);
            b.extend(bytes_to_bits(&s.data, s.rate.data_bits()));
            b
        };
        out.push(Block::Slot(SlotBlock {
            frame: f.frame,
            slot: s.slot,
            mode: s.mode,
            dummy: s.dummy,
            converged: s.converged,
            bch: s
                .bch_corrected
                .map(|c| u16::try_from(c).unwrap_or(u16::MAX)),
            modulation: m,
            rate: r,
            iterations: u8::try_from(s.ldpc_iterations).unwrap_or(u8::MAX),
            bits,
        }));
    }
    let t = &f.tmcc;
    out.push(Block::Tmcc(TmccBlock {
        frame: f.frame,
        bch: t
            .bch_corrected
            .map(|c| u16::try_from(c).unwrap_or(u16::MAX)),
        iterations: t
            .ldpc_iterations
            .map(|i| u16::try_from(i).unwrap_or(u16::MAX - 1)),
        applies_from: f.frame.wrapping_add(lag),
        bits: t.bits.clone(),
    }));
    out.push(Block::Status(Box::new(status(&f.status))));
    out
}

fn status(s: &Status) -> SlotsStatus {
    let c = s.counters;
    SlotsStatus {
        frame: s.frame,
        peak: s.sync.peak,
        peak_pos: s.sync.peak_pos,
        sum: s.sync.sum,
        locked: s.sync.locked,
        ssync_parity: s.sync.ssync_parity,
        offset: s.sync.offset,
        stray_markers: s.sync.stray_markers,
        missed_markers: s.sync.missed_markers,
        carrier: s.carrier,
        agc: s.agc,
        taps: s.taps,
        taps_from: s.taps_from,
        sums_frame: s.sums_frame,
        // n0 ≥ 0 and K < 2²⁴ by their widths (chain::demap)
        n0: u32::try_from(s.n0).unwrap_or(u32::MAX),
        llr_scale: u32::try_from(s.llr_scale).unwrap_or(u32::MAX),
        learned_from: s.learned_from,
        constellation: s.constellation.clone(),
        frames: c.frames,
        frames_undecoded: c.frames_undecoded,
        tmcc_failed: c.tmcc_failed,
        slots: c.slots,
        ldpc_failed: c.ldpc_failed,
        bch_failed: c.bch_failed,
        iterations: c.iterations,
    }
}

/// A decoded slot from a `Slots` slot block: (slot index, head-TLV
/// indication = the header's first 16 bits, data bytes when BCH
/// succeeded), as `rx::RxSlot` carries them.
pub fn slot_data(b: &SlotBlock) -> Option<(u8, u16, Option<Vec<u8>>)> {
    if b.dummy {
        return None;
    }
    let head = b.bits[..16]
        .iter()
        .fold(0u16, |a, &x| (a << 1) | u16::from(x));
    let data = b
        .bch
        .is_some()
        .then(|| isdb_frontend::slots::bits_to_bytes(&b.bits[HEADER_BITS..]));
    Some((b.slot, head, data))
}
