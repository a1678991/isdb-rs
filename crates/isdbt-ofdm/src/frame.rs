//! OFDM frame structure (ARIB STD-B31 §3.12–3.14): segment
//! configuration for differential (Fig. 3-29, Table 3-14) and coherent
//! (Fig. 3-30, Table 3-15) modulation, pilot PRBS (§3.13.1, Fig. 3-31,
//! Tables 3-16/3-17), the rightmost continual carrier (Table 3-19), and
//! the arrangement of segments on the spectrum (§3.14, Fig. 3-32).
//!
//! Carrier numbering: `c` = carrier number within an OFDM segment
//! (0..C−1, C = 108/216/432, ascending frequency); `k` = carrier number
//! across the band (§3.14.1: 0 = carrier 0 of segment 11, the lowest
//! frequency), `k = position(seg)·C + c`, plus the extra continual
//! carrier `k = 13·C` at the upper band edge (K = 13·C + 1 carriers).

use crate::C32;
use crate::tables;
use isdbt_core::ofdm_params::{segment_at_position, segment_position};
use isdbt_core::{Mode, Params, SEGMENTS};

/// Pilot amplitude 4/3 (Tables 3-17, 3-19, §3.13.3/3.13.4).
pub const PILOT_AMPLITUDE: f32 = 4.0 / 3.0;

/// Modulation type of an OFDM segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// QPSK / 16QAM / 64QAM with scattered pilots (§3.12.2).
    Coherent,
    /// DQPSK with a continual pilot (§3.12.1).
    Differential,
}

/// Kind of an AC carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcKind {
    /// AC1 (all segments).
    Ac1,
    /// AC2 (differential segments only).
    Ac2,
}

/// Role of a band carrier in a given symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CarrierRole {
    /// Data carrier.
    Data,
    /// Scattered pilot.
    Sp,
    /// Continual pilot (carrier 0 of a differential segment).
    Cp,
    /// TMCC.
    Tmcc,
    /// AC1 / AC2.
    Ac(AcKind),
    /// Rightmost continual carrier of the band (Table 3-19).
    EdgeCp,
}

/// One step of the pilot PRBS of Fig. 3-31, g(x) = X^11 + X^9 + 1.
///
/// `state[0..11]` are registers D1..D11 (Table 3-16 prints them left to
/// right).  The output W is D11; the feedback D9 ⊕ D11 enters D1 while
/// every register shifts one place towards D11.
pub fn prbs_step(state: &mut [u8; 11]) -> u8 {
    let w = state[10];
    let fb = state[8] ^ state[10];
    state.copy_within(0..10, 1);
    state[0] = fb;
    w
}

/// Parse a Table 3-16 string "D1…D11".
///
/// # Panics
///
/// If `s` is not 11 binary digits.
pub fn prbs_init(s: &str) -> [u8; 11] {
    let b = s.as_bytes();
    assert_eq!(b.len(), 11);
    std::array::from_fn(|i| b[i] - b'0')
}

/// Table 3-16 initial value of segment `seg` in `mode`.
pub fn prbs_segment_init(mode: Mode, seg: usize) -> [u8; 11] {
    let t = match mode {
        Mode::M1 => &tables::PRBS_INIT_M1,
        Mode::M2 => &tables::PRBS_INIT_M2,
        Mode::M3 => &tables::PRBS_INIT_M3,
    };
    prbs_init(t[seg])
}

/// W_i for carriers 0..C of segment `seg` (§3.13.1, Table 3-16 init).
pub fn segment_w(mode: Mode, seg: usize) -> Vec<u8> {
    let mut st = prbs_segment_init(mode, seg);
    (0..mode.carriers_per_segment())
        .map(|_| prbs_step(&mut st))
        .collect()
}

/// Table 3-19: W value of the rightmost continual carrier (1 ↔ (−4/3, 0)).
pub fn edge_w(mode: Mode) -> u8 {
    match mode {
        Mode::M1 => 1,
        Mode::M2 | Mode::M3 => 0,
    }
}

/// BPSK pilot value for W (Table 3-17): 1 → −4/3, 0 → +4/3.
pub fn pilot_value(w: u8) -> C32 {
    C32::new(
        if w == 0 {
            PILOT_AMPLITUDE
        } else {
            -PILOT_AMPLITUDE
        },
        0.0,
    )
}

/// TMCC carrier positions `c` of segment `seg` (Tables 3-14/3-15).
pub fn tmcc_positions(mode: Mode, seg: usize, kind: SegmentKind) -> &'static [u16] {
    use tables::*;
    match (kind, mode) {
        (SegmentKind::Coherent, Mode::M1) => &COH_TMCC_M1[seg],
        (SegmentKind::Coherent, Mode::M2) => &COH_TMCC_M2[seg],
        (SegmentKind::Coherent, Mode::M3) => &COH_TMCC_M3[seg],
        (SegmentKind::Differential, Mode::M1) => &DIFF_TMCC_M1[seg],
        (SegmentKind::Differential, Mode::M2) => &DIFF_TMCC_M2[seg],
        (SegmentKind::Differential, Mode::M3) => &DIFF_TMCC_M3[seg],
    }
}

/// AC1 carrier positions (Tables 3-14/3-15; identical for both kinds,
/// the coherent table is used for coherent segments and the differential
/// table for differential ones — their equality is tested).
pub fn ac1_positions(mode: Mode, seg: usize, kind: SegmentKind) -> &'static [u16] {
    use tables::*;
    match (kind, mode) {
        (SegmentKind::Coherent, Mode::M1) => &COH_AC1_M1[seg],
        (SegmentKind::Coherent, Mode::M2) => &COH_AC1_M2[seg],
        (SegmentKind::Coherent, Mode::M3) => &COH_AC1_M3[seg],
        (SegmentKind::Differential, Mode::M1) => &DIFF_AC1_M1[seg],
        (SegmentKind::Differential, Mode::M2) => &DIFF_AC1_M2[seg],
        (SegmentKind::Differential, Mode::M3) => &DIFF_AC1_M3[seg],
    }
}

/// AC2 carrier positions (Table 3-14; empty for coherent segments).
pub fn ac2_positions(mode: Mode, seg: usize, kind: SegmentKind) -> &'static [u16] {
    use tables::*;
    match (kind, mode) {
        (SegmentKind::Coherent, _) => &[],
        (SegmentKind::Differential, Mode::M1) => &DIFF_AC2_M1[seg],
        (SegmentKind::Differential, Mode::M2) => &DIFF_AC2_M2[seg],
        (SegmentKind::Differential, Mode::M3) => &DIFF_AC2_M3[seg],
    }
}

/// True if carrier `c` of a coherent segment is a scattered pilot in a
/// symbol with index `n` in the frame: `c ≡ 3·(n mod 4) (mod 12)`
/// (Fig. 3-30: symbol 0 → carriers 0, 12, …; symbol 1 → 3, 15, …).
pub fn is_sp(c: usize, n: usize) -> bool {
    c % 12 == 3 * (n % 4)
}

/// Roles of the carriers `0..C` of one segment in a symbol with SP phase
/// `phase = n mod 4` (phase is ignored for differential segments).
pub fn segment_roles(mode: Mode, seg: usize, kind: SegmentKind, phase: usize) -> Vec<CarrierRole> {
    let c_total = mode.carriers_per_segment();
    let mut roles = vec![CarrierRole::Data; c_total];
    match kind {
        SegmentKind::Coherent => {
            for (c, r) in roles.iter_mut().enumerate() {
                if is_sp(c, phase) {
                    *r = CarrierRole::Sp;
                }
            }
        }
        SegmentKind::Differential => roles[0] = CarrierRole::Cp,
    }
    let mut set = |pos: &[u16], role: CarrierRole| {
        for &c in pos {
            assert_eq!(roles[c as usize], CarrierRole::Data, "overlap at {seg}/{c}");
            roles[c as usize] = role;
        }
    };
    set(tmcc_positions(mode, seg, kind), CarrierRole::Tmcc);
    set(ac1_positions(mode, seg, kind), CarrierRole::Ac(AcKind::Ac1));
    set(ac2_positions(mode, seg, kind), CarrierRole::Ac(AcKind::Ac2));
    roles
}

/// A TMCC carrier of the band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmccCarrier {
    /// Band carrier number k.
    pub k: u32,
    /// Segment number.
    pub seg: u8,
    /// Segment kind.
    pub kind: SegmentKind,
}

/// An AC carrier of the band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcCarrier {
    /// Band carrier number k.
    pub k: u32,
    /// Segment number.
    pub seg: u8,
    /// AC1 or AC2.
    pub kind: AcKind,
}

/// Complete carrier map of an OFDM frame for one segment-kind assignment.
#[derive(Debug, Clone)]
pub struct FrameLayout {
    /// Mode.
    pub mode: Mode,
    /// Kind of each segment, indexed by segment number.
    pub kinds: [SegmentKind; SEGMENTS],
    /// Total carriers K.
    pub k_total: usize,
    /// For SP phase `n mod 4`: band carrier of each data position, in
    /// data-segment order (`seg·nc + i` → k).
    pub data: [Vec<u32>; 4],
    /// For SP phase `n mod 4`: scattered-pilot carriers (ascending k).
    pub sp: [Vec<u32>; 4],
    /// Continual pilots of differential segments (carrier 0), ascending k.
    pub cp: Vec<u32>,
    /// Rightmost continual carrier k = K − 1.
    pub edge: u32,
    /// TMCC carriers, ascending k.
    pub tmcc: Vec<TmccCarrier>,
    /// AC carriers (AC1 and AC2), ascending k (Attachment §6.2.3.2 order).
    pub ac: Vec<AcCarrier>,
    /// W_k of every band carrier (§3.13.1 per-segment PRBS; edge: Table 3-19).
    pub w: Vec<u8>,
}

/// A band carrier number as the layout tables store it.  Carrier numbers
/// are below [`Mode::total_carriers`] (5617 in Mode 3), so the narrowing is
/// exact; the tables are `u32` to halve their size.
pub(crate) fn carrier_index(k: usize) -> u32 {
    debug_assert!(k < 1 << 16, "band carrier number {k}");
    #[allow(clippy::cast_possible_truncation)]
    let k = k as u32;
    k
}

impl FrameLayout {
    /// Layout for explicit segment kinds (indexed by segment number).
    pub fn new(mode: Mode, kinds: [SegmentKind; SEGMENTS]) -> Self {
        let c_total = mode.carriers_per_segment();
        let nc = mode.data_carriers_per_segment();
        let k_total = mode.total_carriers();
        let mut data: [Vec<u32>; 4] = std::array::from_fn(|_| vec![0; SEGMENTS * nc]);
        let mut sp: [Vec<u32>; 4] = std::array::from_fn(|_| Vec::new());
        let mut cp = Vec::new();
        let mut tmcc = Vec::new();
        let mut ac = Vec::new();
        let mut w = Vec::with_capacity(k_total);
        for pos in 0..SEGMENTS {
            let seg = segment_at_position(pos);
            // Segment numbers are below SEGMENTS (13).
            #[allow(clippy::cast_possible_truncation)]
            let seg_no = seg as u8;
            let kind = kinds[seg];
            let base = pos * c_total;
            w.extend(segment_w(mode, seg));
            for phase in 0..4 {
                let roles = segment_roles(mode, seg, kind, phase);
                let mut i = 0;
                for (c, r) in roles.iter().enumerate() {
                    let k = carrier_index(base + c);
                    match r {
                        CarrierRole::Data => {
                            data[phase][seg * nc + i] = k;
                            i += 1;
                        }
                        CarrierRole::Sp => sp[phase].push(k),
                        CarrierRole::Cp if phase == 0 => cp.push(k),
                        CarrierRole::Tmcc if phase == 0 => tmcc.push(TmccCarrier {
                            k,
                            seg: seg_no,
                            kind,
                        }),
                        CarrierRole::Ac(a) if phase == 0 => ac.push(AcCarrier {
                            k,
                            seg: seg_no,
                            kind: *a,
                        }),
                        _ => {}
                    }
                }
                assert_eq!(i, nc, "data carriers of segment {seg} phase {phase}");
            }
        }
        w.push(edge_w(mode));
        assert_eq!(w.len(), k_total);
        Self {
            mode,
            kinds,
            k_total,
            data,
            sp,
            cp,
            edge: carrier_index(k_total - 1),
            tmcc,
            ac,
            w,
        }
    }

    /// Layout for `params` (segment kinds from the layer modulations).
    pub fn from_params(params: &Params) -> Self {
        Self::new(params.mode(), kinds_from_params(params))
    }

    /// Band carrier number of carrier `c` of segment `seg`.
    pub fn carrier(&self, seg: usize, c: usize) -> u32 {
        carrier_index(segment_position(seg) * self.mode.carriers_per_segment() + c)
    }

    /// Roles of all K carriers for SP phase `phase`.
    pub fn roles(&self, phase: usize) -> Vec<CarrierRole> {
        let mut r = vec![CarrierRole::Data; self.k_total];
        for &k in &self.sp[phase] {
            r[k as usize] = CarrierRole::Sp;
        }
        for &k in &self.cp {
            r[k as usize] = CarrierRole::Cp;
        }
        for t in &self.tmcc {
            r[t.k as usize] = CarrierRole::Tmcc;
        }
        for a in &self.ac {
            r[a.k as usize] = CarrierRole::Ac(a.kind);
        }
        r[self.edge as usize] = CarrierRole::EdgeCp;
        r
    }
}

/// Segment kinds (by segment number) implied by `params`.
pub fn kinds_from_params(params: &Params) -> [SegmentKind; SEGMENTS] {
    std::array::from_fn(|s| {
        if params.segment_is_differential(s) {
            SegmentKind::Differential
        } else {
            SegmentKind::Coherent
        }
    })
}

/// DBPSK differential encoder of §3.13.3/§3.13.4 for one carrier:
/// `B'0 = W`, `B'k = B'(k−1) ⊕ Bk`; signal point +4/3 for 0, −4/3 for 1.
#[derive(Debug, Clone, Copy)]
pub struct Dbpsk {
    prev: u8,
}

impl Dbpsk {
    /// Start of frame: B'0 = `w`.
    pub fn start(w: u8) -> (Self, C32) {
        (Self { prev: w }, pilot_value(w))
    }

    /// Encode information bit `b` (symbols 1..203).
    pub fn next(&mut self, b: u8) -> C32 {
        self.prev ^= b & 1;
        pilot_value(self.prev)
    }
}
