//! The ISDB-S transmitter up to the symbols: TS packets per TS → the
//! 319 488 symbols of a superframe (BO.1408-1 Fig. 1; MIC Ordinance No. 87
//! of 2011, annexed tables 45–51), the inverse of [`crate::receive`] and
//! [`crate::ts::decode_all`].
//!
//! ```text
//! TS packets (188 B) per relative TS, assigned to the sent slots in order
//!   ─RS(204,188)                                   isdb_fec::RS_204_188
//!   ─energy dispersal, depth-8 interleaver,
//!    sync bytes and dummy slots dropped             isdbs_core::mainsig::frame_bytes
//!   ─inner code and mapping, per frame              crate::demod::encode_frame_with_state
//!   ─header (W1, TMCC, W2/W3; coded BPSK),
//!    bursts, main signal → 39 936 symbols           frame_symbols
//! ```
//!
//! The unit is the superframe: the TMCC, the energy dispersal and the
//! interleaver all span one, and nothing is carried from one superframe to
//! the next but the six-bit register of the inner coder, which W1 flushes
//! (§6.3). The symbols are unit-modulus constellation points at one sample
//! per symbol; pulse shaping is the front end's business.
//!
//! The same superframe comes in two forms from the same bits:
//! [`Transmitter::superframe`] gives the `f64` points ([`Superframe`]),
//! [`Transmitter::superframe_codes`] their TX boundary codes
//! ([`SuperframeCodes`], `isdb_frontend::txformat`: 8 + k for the point
//! e^{jkπ/4} of annexed table 46), which an FPGA transmit block, or
//! `isdb_frontend::shape::fixed::Interpolator`, shapes. Neither is derived
//! from the other: one encoder path writes both.

use crate::Cplx;
use crate::demod::{Mapping, Point, encode_points};
use isdb_fec::{ConvEncoder, RS_204_188};
use isdbs_core::burst::frame_bits;
use isdbs_core::frame::{
    BURST_SYMBOLS, BURSTS, FRAME_SYMBOLS, FRAMES, HEADER_BYTES, HEADER_SYMBOLS, MAIN_SYMBOLS,
    PACKET_BYTES, burst_symbol_position, main_symbol_position,
};
use isdbs_core::mainsig::{SYNC_BYTE, frame_bytes, sent_slots};
use isdbs_core::tmcc::{CombinationWord, TM, TmccError, TmccInfo, headers};

/// An MPEG-2 TS packet (ITU-T H.222.0 §2.4.3), sync byte 0x47 first.
pub type Packet = [u8; 188];

/// Symbols of a superframe: M = 8 frames of 39 936 (BO.1408-1 Appendix 1
/// Table 3); [`isdbs_core::frame::SUPERFRAME_SYMBOLS`].
pub const SUPERFRAME_SYMBOLS: usize = isdbs_core::frame::SUPERFRAME_SYMBOLS;

/// The MPEG-2 null packet (ITU-T H.222.0 §2.4.3.2–3: PID 0x1FFF, payload
/// only, payload bytes 0xFF), as `isdbt_phy::null_packet`.
pub const NULL_PACKET: Packet = {
    let mut p = [0xFF; 188];
    p[0] = SYNC_BYTE;
    p[1] = 0x1F;
    p[2] = 0xFF;
    p[3] = 0x10;
    p
};

/// BPSK of a bit: 0 at 0°, 1 at 180° (annexed table 46).
#[cfg(test)]
fn bpsk(bit: u8) -> Cplx {
    Cplx::bpsk(bit)
}

/// A frame's 39 936 symbols in the layout of annexed table 45: the 12
/// header bytes (W1, the frame's TMCC block, W2 or W3; from
/// [`isdbs_core::tmcc::headers`]) through the r = 1/2 inner code to 192
/// BPSK symbols, X then Y of each bit (annexed table 49 note 3, SPEC-NOTES
/// "bpsk-order"), at symbols 0..192; the 768 burst bits of
/// [`isdbs_core::burst::frame_bits`] in BPSK at
/// [`burst_symbol_position`] (annexed table 51); the 38 976 `main` symbols
/// at [`main_symbol_position`].
///
/// `enc` is the inner coder as the previous frame's main signal left it
/// (one encoder runs through the frames, BO.1408-1 Fig. 14): only the first
/// 12 header symbols depend on it, since the first six bits of W1 flush
/// its register (§6.3). On return it holds the state after W2/W3, the one
/// [`crate::demod::encode_frame`] starts the main signal from; the
/// [`Transmitter`] then sets it to the main signal's final state.
///
/// # Panics
///
/// If `main` does not hold [`MAIN_SYMBOLS`] symbols.
pub fn frame_symbols(
    enc: &mut ConvEncoder,
    header: &[u8; HEADER_BYTES],
    main: &[Cplx],
) -> Vec<Cplx> {
    frame_points(enc, header, main)
}

/// [`frame_symbols`] as TX boundary codes (`isdb_frontend::txformat`):
/// the header and bursts as BPSK codes (8 for bit 0, 12 for bit 1), the
/// main signal's codes (from
/// [`crate::demod::encode_frame_codes_with_state`]) where
/// [`main_symbol_position`] puts them.
///
/// # Panics
///
/// If `main` does not hold [`MAIN_SYMBOLS`] codes.
pub fn frame_codes(enc: &mut ConvEncoder, header: &[u8; HEADER_BYTES], main: &[u8]) -> Vec<u8> {
    frame_points(enc, header, main)
}

/// [`frame_symbols`] and [`frame_codes`], over either kind of [`Point`].
fn frame_points<P: Point>(
    enc: &mut ConvEncoder,
    header: &[u8; HEADER_BYTES],
    main: &[P],
) -> Vec<P> {
    assert_eq!(main.len(), MAIN_SYMBOLS, "main-signal symbols of a frame");
    let mut out = vec![P::ZERO; FRAME_SYMBOLS];
    let mut k = 0;
    for &byte in header {
        for i in (0..8).rev() {
            let (x, y) = enc.push((byte >> i) & 1);
            out[k] = P::bpsk(x);
            out[k + 1] = P::bpsk(y);
            k += 2;
        }
    }
    debug_assert_eq!(k, HEADER_SYMBOLS);
    let burst = frame_bits();
    for b in 0..BURSTS {
        for j in 0..BURST_SYMBOLS {
            out[burst_symbol_position(b, j)] = P::bpsk(burst[b * BURST_SYMBOLS + j]);
        }
    }
    for (m, &v) in main.iter().enumerate() {
        out[main_symbol_position(m)] = v;
    }
    out
}

/// Why the transmitter refused its input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TxError {
    /// The TMCC information does not validate.
    #[error(transparent)]
    Tmcc(#[from] TmccError),
    /// The TMCC information sends no slot (every combination dummy).
    /// [`TmccInfo::validate`] and [`TxError::DummySlots`] leave no such
    /// information; kept as a guard.
    #[error("the TMCC information sends no slot")]
    NoSentSlot,
    /// A dummy combination entry (word 1111) with slots: BO.1408-1 §6.1.2
    /// sets a dummy entry's slot count to zero, and the slots it would
    /// cover carry no symbol of any kind (the main signal would hold 0 + 0j
    /// there, which no TC8PSK label represents).
    #[error(
        "TMCC combination entry {entry}: dummy (1111) with {slots} slots, 0 required (BO.1408-1 §6.1.2)"
    )]
    DummySlots {
        /// The entry, 0..4.
        entry: usize,
        /// Its slot count.
        slots: u8,
    },
    /// [`Transmitter::superframe`] got packet lists for a different number
    /// of TSs than [`Transmitter::ts_count`].
    #[error("packets for {got} TSs, the TMCC carries {want}")]
    TsCount {
        /// [`Transmitter::ts_count`].
        want: usize,
        /// Lists given.
        got: usize,
    },
    /// A TS's packets do not fill exactly its slots of a superframe.
    #[error("relative TS {relative_ts}: {got} packets, a superframe takes {want}")]
    PacketCount {
        /// Relative TS ID.
        relative_ts: usize,
        /// [`Transmitter::packets_per_superframe`].
        want: usize,
        /// Packets given.
        got: usize,
    },
    /// A packet does not start with the sync byte 0x47.
    #[error("relative TS {relative_ts}: packet {index} does not start with 0x47")]
    SyncByte {
        /// Relative TS ID.
        relative_ts: usize,
        /// Index of the packet in its list.
        index: usize,
    },
}

/// One superframe as TX boundary codes ([`Transmitter::superframe_codes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuperframeCodes {
    /// The [`SUPERFRAME_SYMBOLS`] codes in transmission order: code `n`
    /// is symbol `n` of [`Superframe::symbols`] for the same input (8 + k
    /// for the point e^{jkπ/4}, `isdb_frontend::txformat`).
    pub codes: Vec<u8>,
    /// As [`Superframe::placed`].
    pub placed: Vec<(usize, usize, u16)>,
}

/// One superframe of symbols.
#[derive(Debug, Clone, PartialEq)]
pub struct Superframe {
    /// The [`SUPERFRAME_SYMBOLS`] symbols, unit modulus, in transmission
    /// order (frame 1, the W2 frame, first).
    pub symbols: Vec<Cplx>,
    /// (frame 0..8, slot 0..48, TS_ID) of each packet in transmission
    /// order: frame by frame, sent slot by sent slot, the order and fields
    /// of the [`crate::ts::Packet`]s a receiver returns for the superframe.
    pub placed: Vec<(usize, usize, u16)>,
}

/// The ISDB-S transmitter of one TMCC information: TS packets of each
/// relative TS → superframes of symbols.
///
/// Every slot that is sent (dummy slots are not, BO.1408-1 §8) carries the
/// next packet of the TS its relative TS ID names (Fig. 9). The headers
/// and the slot tables are computed once; each superframe is then
/// independent but for the inner coder's register.
#[derive(Debug, Clone)]
pub struct Transmitter {
    info: TmccInfo,
    mapping: Mapping,
    headers: [[u8; HEADER_BYTES]; FRAMES],
    /// Slot number of each sent slot, slot order.
    sent: Vec<usize>,
    /// Relative TS ID of each sent slot.
    sent_ts: Vec<usize>,
    /// Sent slots per frame of each relative TS.
    per_frame: [usize; TM],
    ts_count: usize,
    enc: ConvEncoder,
    superframes: u64,
}

impl Transmitter {
    /// A transmitter of `info` (from [`TmccInfo::assign`] or decoded), with
    /// the default [`Mapping`] (the readings confirmed on air).
    ///
    /// # Errors
    ///
    /// [`TxError::Tmcc`] when `info` does not
    /// [`validate`](TmccInfo::validate); [`TxError::DummySlots`] for a
    /// dummy combination entry with slots (§6.1.2; [`TmccInfo::assign`]
    /// never builds one); [`TxError::NoSentSlot`] when it sends no slot.
    pub fn new(info: TmccInfo) -> Result<Self, TxError> {
        info.validate()?;
        if let Some((entry, c)) = info
            .combinations
            .iter()
            .enumerate()
            .find(|(_, c)| c.word == CombinationWord::Dummy && c.slots != 0)
        {
            return Err(TxError::DummySlots {
                entry,
                slots: c.slots,
            });
        }
        let sent = sent_slots(&info);
        if sent.is_empty() {
            return Err(TxError::NoSentSlot);
        }
        let sent_ts: Vec<usize> = sent
            .iter()
            .map(|&s| usize::from(info.relative_ts[s] & 7))
            .collect();
        let mut per_frame = [0usize; TM];
        for &r in &sent_ts {
            per_frame[r] += 1;
        }
        let ts_count = 1 + sent_ts.iter().copied().max().unwrap_or(0);
        Ok(Self {
            headers: headers(&info),
            info,
            mapping: Mapping::default(),
            sent,
            sent_ts,
            per_frame,
            ts_count,
            enc: ConvEncoder::new(),
            superframes: 0,
        })
    }

    /// Use the constellation reading `m` (the alternatives of
    /// [`Mapping`] exist to test the receiver's).
    #[must_use]
    pub fn with_mapping(mut self, m: Mapping) -> Self {
        self.mapping = m;
        self
    }

    /// The TMCC information sent.
    pub fn info(&self) -> &TmccInfo {
        &self.info
    }

    /// Relative TS IDs `0..ts_count()` are the ones up to the highest that
    /// a sent slot names (Fig. 9); [`superframe`](Self::superframe) takes
    /// one packet list for each.
    pub fn ts_count(&self) -> usize {
        self.ts_count
    }

    /// The TS_ID of relative TS `relative_ts` (Fig. 10).
    ///
    /// # Panics
    ///
    /// If `relative_ts` ≥ [`TM`].
    pub fn ts_id(&self, relative_ts: usize) -> u16 {
        self.info.ts_ids[relative_ts]
    }

    /// Sent slots of relative TS `relative_ts` per frame (0 past
    /// [`ts_count`](Self::ts_count)).
    pub fn slots_per_frame(&self, relative_ts: usize) -> usize {
        self.per_frame.get(relative_ts).copied().unwrap_or(0)
    }

    /// TS packets of relative TS `relative_ts` per superframe: its slots
    /// per frame × M = 8.
    pub fn packets_per_superframe(&self, relative_ts: usize) -> usize {
        self.slots_per_frame(relative_ts) * FRAMES
    }

    /// Superframes produced so far.
    pub fn superframes(&self) -> u64 {
        self.superframes
    }

    /// The next superframe: `ts[r]` holds exactly
    /// [`packets_per_superframe(r)`](Self::packets_per_superframe) packets
    /// of relative TS `r`, in order, for every `r` below
    /// [`ts_count`](Self::ts_count). Packets are not padded: a caller with
    /// too few fills the rest with [`NULL_PACKET`].
    ///
    /// # Errors
    ///
    /// [`TxError::TsCount`], [`TxError::PacketCount`],
    /// [`TxError::SyncByte`]; nothing is sent then.
    pub fn superframe(&mut self, ts: &[&[Packet]]) -> Result<Superframe, TxError> {
        self.check(ts)?;
        let (symbols, placed) = self.build(|r, k| &ts[r][k]);
        Ok(Superframe { symbols, placed })
    }

    /// [`superframe`](Self::superframe) as TX boundary codes, made from
    /// the bits by the same encoder (not from the points): for the same
    /// input and state, code `n` is the code of symbol `n`.
    ///
    /// # Errors
    ///
    /// As [`superframe`](Self::superframe); nothing is sent then.
    pub fn superframe_codes(&mut self, ts: &[&[Packet]]) -> Result<SuperframeCodes, TxError> {
        self.check(ts)?;
        let (codes, placed) = self.build(|r, k| &ts[r][k]);
        Ok(SuperframeCodes { codes, placed })
    }

    /// A superframe of [`NULL_PACKET`]s in every sent slot.
    pub fn null_superframe(&mut self) -> Superframe {
        let (symbols, placed) = self.build(|_, _| &NULL_PACKET);
        Superframe { symbols, placed }
    }

    /// [`null_superframe`](Self::null_superframe) as TX boundary codes.
    pub fn null_superframe_codes(&mut self) -> SuperframeCodes {
        let (codes, placed) = self.build(|_, _| &NULL_PACKET);
        SuperframeCodes { codes, placed }
    }

    /// The input rules of [`superframe`](Self::superframe).
    fn check(&self, ts: &[&[Packet]]) -> Result<(), TxError> {
        if ts.len() != self.ts_count {
            return Err(TxError::TsCount {
                want: self.ts_count,
                got: ts.len(),
            });
        }
        for (r, packets) in ts.iter().enumerate() {
            let want = self.packets_per_superframe(r);
            if packets.len() != want {
                return Err(TxError::PacketCount {
                    relative_ts: r,
                    want,
                    got: packets.len(),
                });
            }
            if let Some(index) = packets.iter().position(|p| p[0] != SYNC_BYTE) {
                return Err(TxError::SyncByte {
                    relative_ts: r,
                    index,
                });
            }
        }
        Ok(())
    }

    /// The superframe whose relative TS `r` carries `packet(r, k)` as its
    /// `k`-th packet, as points or codes, and where each packet went.
    fn build<'a, P: Point>(
        &mut self,
        packet: impl Fn(usize, usize) -> &'a Packet,
    ) -> (Vec<P>, Vec<(usize, usize, u16)>) {
        let n = self.sent.len();
        let mut next = [0usize; TM];
        let mut placed = Vec::with_capacity(FRAMES * n);
        let words: [Vec<[u8; PACKET_BYTES]>; FRAMES] = std::array::from_fn(|f| {
            (0..n)
                .map(|i| {
                    let r = self.sent_ts[i];
                    let mut w = [0u8; PACKET_BYTES];
                    w[..188].copy_from_slice(packet(r, next[r]));
                    next[r] += 1;
                    RS_204_188.encode_in_place(&mut w);
                    placed.push((f, self.sent[i], self.info.ts_ids[r]));
                    w
                })
                .collect()
        });
        let bytes = frame_bytes(&self.info, &words);
        let frames: Vec<usize> = (0..FRAMES).collect();
        let (info, mapping) = (&self.info, self.mapping);
        let mains = crate::par::map(&frames, |&f| {
            encode_points::<P>(&bytes[f], info, f == 0, mapping)
        });
        let mut symbols = Vec::with_capacity(SUPERFRAME_SYMBOLS);
        for (h, (main, end)) in self.headers.iter().zip(mains) {
            symbols.extend(frame_points(&mut self.enc, h, &main));
            self.enc = end;
        }
        self.superframes += 1;
        (symbols, placed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demod::state_after;
    use isdbs_core::combination::{Combination, CombinationWord};
    use isdbs_core::frame::{W2, W3};
    use isdbs_core::tmcc::CombinationEntry;

    fn bs() -> TmccInfo {
        TmccInfo::assign(
            &[(Combination::Tc8psk2_3, 48)],
            &[(0x4010, 16), (0x4011, 16), (0x4012, 16)],
        )
        .unwrap()
    }

    /// Annexed table 45: the coded header at symbols 0..192 (X then Y,
    /// from the given encoder state), the bursts and the main symbols
    /// where `frame::*_position` puts them, every symbol written once.
    #[test]
    fn frame_symbols_follows_table_45() {
        let h = headers(&bs());
        let main: Vec<Cplx> = (0..MAIN_SYMBOLS)
            .map(|k| Cplx::from_polar(1.0, k as f64 * 0.001))
            .collect();
        for (f, start) in [(0usize, 0u8), (3, 0x2B)] {
            let mut enc = ConvEncoder::new();
            for i in 0..6 {
                enc.push((start >> i) & 1);
            }
            let mut want = enc;
            let z = frame_symbols(&mut enc, &h[f], &main);
            assert_eq!(z.len(), FRAME_SYMBOLS);
            let mut seen = vec![0u8; FRAME_SYMBOLS];
            let mut k = 0;
            for &byte in &h[f] {
                for i in (0..8).rev() {
                    let (x, y) = want.push((byte >> i) & 1);
                    assert_eq!((z[k], z[k + 1]), (bpsk(x), bpsk(y)), "header bit {k}");
                    seen[k] += 1;
                    seen[k + 1] += 1;
                    k += 2;
                }
            }
            assert_eq!(enc, want);
            assert_eq!(enc.state(), state_after(if f == 0 { W2 } else { W3 }));
            let burst = frame_bits();
            for b in 0..BURSTS {
                for j in 0..BURST_SYMBOLS {
                    let at = burst_symbol_position(b, j);
                    assert_eq!(z[at], bpsk(burst[b * BURST_SYMBOLS + j]));
                    seen[at] += 1;
                }
            }
            for (m, &v) in main.iter().enumerate() {
                assert_eq!(z[main_symbol_position(m)], v);
                seen[main_symbol_position(m)] += 1;
            }
            assert!(seen.iter().all(|&c| c == 1));
        }
    }

    /// The slot tables of the BS and CS layouts.
    #[test]
    fn slot_tables() {
        let t = Transmitter::new(bs()).unwrap();
        assert_eq!(t.ts_count(), 3);
        assert_eq!(
            (0..4).map(|r| t.slots_per_frame(r)).collect::<Vec<_>>(),
            [16, 16, 16, 0]
        );
        assert_eq!(t.packets_per_superframe(2), 128);
        assert_eq!(t.ts_id(1), 0x4011);
        let cs = TmccInfo::assign(&[(Combination::Qpsk3_4, 48)], &[(0x4010, 36)]).unwrap();
        let t = Transmitter::new(cs).unwrap();
        assert_eq!((t.ts_count(), t.packets_per_superframe(0)), (1, 288));
    }

    /// A null superframe, received noiselessly: 8 frames with intact
    /// headers, the TMCC sent, and null packets in every sent slot where
    /// `placed` says, RS-clean.
    #[test]
    fn null_superframe_decodes_to_null_packets() {
        let info = TmccInfo::assign(
            &[(Combination::Qpsk3_4, 44), (Combination::Bpsk1_2, 4)],
            &[(0x4010, 20), (0x4011, 14)],
        )
        .unwrap();
        let mut t = Transmitter::new(info.clone()).unwrap();
        let sf = t.null_superframe();
        assert_eq!(t.superframes(), 1);
        assert_eq!(sf.symbols.len(), SUPERFRAME_SYMBOLS);
        assert!(sf.symbols.iter().all(|v| (v.norm() - 1.0).abs() < 1e-12));
        assert_eq!(sf.placed.len(), FRAMES * 34);
        let r = crate::receive(&sf.symbols, &crate::RxOptions::default());
        assert_eq!(r.superframes.len(), 1);
        assert!(r.frames.iter().all(|f| f.header_ok));
        let d = crate::ts::decode_all(&r, &crate::demod::DemodOptions::default());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].info, info);
        let got: Vec<(usize, usize, u16)> = d[0]
            .packets
            .iter()
            .map(|p| (p.frame, p.slot, p.ts_id))
            .collect();
        assert_eq!(got, sf.placed);
        assert!(
            d[0].packets
                .iter()
                .all(|p| p.bytes == NULL_PACKET && p.corrected == Some(0))
        );
    }

    /// `superframe_codes` and `null_superframe_codes` are the symbols as
    /// codes: the same length and `placed`, `txformat::point(code)` the
    /// symbol within 10⁻¹², over consecutive superframes (the inner coder's
    /// state carried) and under every mapping; the same input errors.
    #[test]
    fn codes_are_the_symbols() {
        use isdb_frontend::txformat;
        let info = TmccInfo::assign(
            &[(Combination::Tc8psk2_3, 40), (Combination::Qpsk1_2, 8)],
            &[(0x4010, 28), (0x4011, 16)],
        )
        .unwrap();
        for bits in [0u8, 1, 2, 4, 7] {
            let m = Mapping {
                conjugate: bits & 1 != 0,
                qpsk_p1_on_q: bits & 2 != 0,
                tc8psk_c1_is_y: bits & 4 == 0,
            };
            let mut ts = Transmitter::new(info.clone()).unwrap().with_mapping(m);
            let mut tc = ts.clone();
            for sf in 0..3u8 {
                let (z, c) = if sf == 1 {
                    let packets: Vec<Vec<Packet>> = (0..ts.ts_count())
                        .map(|r| {
                            (0..ts.packets_per_superframe(r))
                                .map(|i| {
                                    let mut p = NULL_PACKET;
                                    p[4..8]
                                        .copy_from_slice(&u32::try_from(i).unwrap().to_be_bytes());
                                    p[8] = u8::try_from(r).unwrap();
                                    p
                                })
                                .collect()
                        })
                        .collect();
                    let refs: Vec<&[Packet]> = packets.iter().map(Vec::as_slice).collect();
                    (
                        ts.superframe(&refs).unwrap(),
                        tc.superframe_codes(&refs).unwrap(),
                    )
                } else {
                    (ts.null_superframe(), tc.null_superframe_codes())
                };
                assert_eq!(c.codes.len(), SUPERFRAME_SYMBOLS);
                assert_eq!(c.placed, z.placed);
                for (n, (&code, &v)) in c.codes.iter().zip(&z.symbols).enumerate() {
                    assert!(
                        (txformat::point(code) - v).norm() < 1e-12,
                        "mapping {bits} superframe {sf} symbol {n}: code {code}, symbol {v}"
                    );
                }
            }
            assert_eq!(tc.superframes(), ts.superframes());
        }
        let mut t = Transmitter::new(bs()).unwrap();
        assert_eq!(
            t.superframe_codes(&[]).unwrap_err(),
            TxError::TsCount { want: 3, got: 0 }
        );
        assert_eq!(t.superframes(), 0);
    }

    /// Every error of `new` and `superframe`.
    #[test]
    fn errors() {
        let mut bad = bs();
        bad.combinations[0].slots = 40;
        assert_eq!(
            Transmitter::new(bad).unwrap_err(),
            TxError::Tmcc(TmccError::SlotSum(40))
        );
        let mut none = bs();
        none.combinations[0] = CombinationEntry {
            word: CombinationWord::Dummy,
            slots: 48,
        };
        assert_eq!(
            Transmitter::new(none).unwrap_err(),
            TxError::DummySlots {
                entry: 0,
                slots: 48
            }
        );
        // a dummy entry with slots after a real one: validates (the counts
        // sum to 48) but would leave 0 + 0j symbols
        let mut tail = bs();
        tail.combinations[0].slots = 44;
        tail.combinations[1] = CombinationEntry {
            word: CombinationWord::Dummy,
            slots: 4,
        };
        assert_eq!(tail.validate(), Ok(()));
        let e = Transmitter::new(tail).unwrap_err();
        assert_eq!(e, TxError::DummySlots { entry: 1, slots: 4 });
        assert!(e.to_string().contains("§6.1.2"), "{e}");
        let mut t = Transmitter::new(bs()).unwrap();
        let full = vec![NULL_PACKET; 128];
        assert_eq!(
            t.superframe(&[&full, &full]).unwrap_err(),
            TxError::TsCount { want: 3, got: 2 }
        );
        assert_eq!(
            t.superframe(&[&full, &full[1..], &full]).unwrap_err(),
            TxError::PacketCount {
                relative_ts: 1,
                want: 128,
                got: 127
            }
        );
        let mut broken = full.clone();
        broken[5][0] = 0x48;
        assert_eq!(
            t.superframe(&[&full, &full, &broken]).unwrap_err(),
            TxError::SyncByte {
                relative_ts: 2,
                index: 5
            }
        );
        assert_eq!(t.superframes(), 0);
        assert!(t.superframe(&[&full, &full, &full]).is_ok());
        assert_eq!(t.superframes(), 1);
    }
}
