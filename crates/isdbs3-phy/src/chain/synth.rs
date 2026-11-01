//! A synthetic ISDB-S3 symbol stream for the chain's tests and vectors:
//! whole frames built from the standard's parts (B44 Fig. 3.3-1: sync
//! words, §3.9 pilots with the §3.6 (3) PRBS signs, the §3.11 TMCC
//! through BCH, dispersal and the shortened LDPC code, and every valid
//! slot's codeword: random header and data, BCH, slot dispersal, LDPC,
//! §3.7 interleaving, §3.8 points), then a carrier offset (and a drift),
//! an amplitude, Gaussian noise and the front end's n-bit quantiser.
//! Not a transmitter: everything here is the receiver's own reading of
//! the standard, so it checks the chain's consistency, not the reading.

use crate::Cplx;
use crate::chain::frame::{Part, part};
use isdb_frontend::s3sync::sync_symbols;
use isdbs3_core::constellation::points;
use isdbs3_core::dispersal::{SlotDispersal, pilot_signs};
use isdbs3_core::interleave::permutation;
use isdbs3_core::params::{
    BCH_PARITY_BITS, CodeRate, FSYNC, HEADER_BITS, MAIN_PER_MOD_SLOT, MOD_SLOT, Modulation, NFSYNC,
    PILOT_SYMBOLS, SLOTS, SSYNC, STUFF_BITS, SYMBOL_RATE, SYNC_SYMBOLS, TMCC_PER_MOD_SLOT,
};
use isdbs3_core::slots::{Slot, layout};
use isdbs3_core::tmcc::{Mode, ModulationCode, PacketFormat, RateCode, StreamType, TmccInfo};

/// What to generate.
#[derive(Debug, Clone)]
pub struct SynthOptions {
    /// The transmission modes: (modulation, rate numerator, slots), slots
    /// summing to 120 in multiples of 5, in the §3.11 order.
    pub modes: Vec<(Modulation, u8, u8)>,
    /// Symbols before the first frame (filler QPSK).
    pub lead: usize,
    /// Carrier offset at the start, Hz, and its drift, Hz/s.
    pub carrier_hz: f64,
    /// See `carrier_hz`.
    pub drift_hz_s: f64,
    /// Es/N0 of the added noise, dB (∞: none).
    pub snr_db: f64,
    /// RMS amplitude of the symbols in output LSBs (before quantising).
    pub amplitude: f64,
    /// Bits per output component (the front end's 6).
    pub bits: u8,
    /// Seed of the data, the noise and the filler.
    pub seed: u64,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self {
            modes: vec![(Modulation::Apsk16, 93, 120)],
            lead: 1_000,
            carrier_hz: 0.0,
            drift_hz_s: 0.0,
            snr_db: f64::INFINITY,
            amplitude: 9.0,
            bits: 6,
            seed: 1,
        }
    }
}

/// A small deterministic generator (SplitMix64).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeded.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// A bit.
    pub fn bit(&mut self) -> u8 {
        u8::from(self.next_u64() >> 63 == 1)
    }
    /// Uniform in (0, 1).
    pub fn unit(&mut self) -> f64 {
        // 53 random bits as the mantissa of a value in [0, 1)
        #[allow(clippy::cast_precision_loss)]
        let u = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        u.max(1e-300)
    }
    /// A standard normal value (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let (u1, u2) = (self.unit(), self.unit());
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// The TMCC of `modes`: one TLV stream on every slot, the rest as
/// `tmcc::tests::sample` (order of change 7, no pointers, extension all
/// ones).
pub fn tmcc_info(modes: &[(Modulation, u8, u8)]) -> TmccInfo {
    let mut m = [Mode::UNUSED; 8];
    for (i, &(mo, r, s)) in modes.iter().enumerate() {
        m[i] = Mode {
            modulation_code: ModulationCode::Assigned(mo),
            rate_code: RateCode::Assigned(CodeRate::new(r).expect("a B44 rate")),
            slots: s,
            obo: 0,
        };
    }
    let mut stream_type = [StreamType::None; 16];
    stream_type[0] = StreamType::Tlv;
    let mut stream_id = [0xFFFF; 16];
    stream_id[0] = 0xB070;
    TmccInfo {
        change: 7,
        modes: m,
        stream_type,
        packet_format: [PacketFormat::default(); 16],
        pointers: [(0, 0xFFFF); SLOTS],
        slot_stream: [0; SLOTS],
        stream_id,
        ews: false,
        uplink: 0,
        reserved: 0b1111,
        extension_id: 0,
        extension_all_ones: true,
    }
}

/// A valid slot as generated: (slot index, 176 header bits, data bytes
/// MSB first).
pub type SlotTruth = (usize, Vec<u8>, Vec<u8>);

/// One generated frame's content, for checking the chain's output.
#[derive(Debug, Clone)]
pub struct FrameTruth {
    /// Index of its first symbol in the stream.
    pub at: usize,
    /// Per valid slot: (slot index, 176 header bits, data bytes MSB
    /// first).
    pub slots: Vec<SlotTruth>,
}

/// The stream: the quantised symbols, and what each frame carried.
#[derive(Debug, Clone)]
pub struct Synth {
    /// One `[I, Q]` per symbol, `bits` wide.
    pub symbols: Vec<[i8; 2]>,
    /// The frames.
    pub frames: Vec<FrameTruth>,
    /// The layout of every frame.
    pub layout: Vec<Slot>,
}

/// One frame's main symbols (unit power, before any impairment) and its
/// truth.
fn main_symbols(lay: &[Slot], rng: &mut Rng) -> (Vec<Cplx>, Vec<SlotTruth>) {
    let mut ms = vec![Cplx::new(0.0, 0.0); SLOTS * MAIN_PER_MOD_SLOT];
    let mut truth = Vec::new();
    let mut disp = SlotDispersal::new();
    for s in lay.iter().filter(|s| !s.dummy) {
        let d = s.rate.data_bits();
        let msg: Vec<u8> = (0..HEADER_BITS + d).map(|_| rng.bit()).collect();
        let mut info = isdb_fec::bch::encode(&msg);
        info.extend(std::iter::repeat_n(1u8, STUFF_BITS));
        debug_assert_eq!(info.len(), s.rate.k_ldpc());
        let spread = HEADER_BITS + d + BCH_PARITY_BITS + STUFF_BITS;
        disp.apply(&mut info[..spread]);
        let cw = isdbs3_core::ldpc::code(s.rate).encode(&info);
        let perm = permutation(s.modulation, s.rate);
        let pts = points(s.modulation, s.rate);
        let bits = s.modulation.bits();
        for sym in 0..s.symbols {
            let mut lab = 0usize;
            for b in 0..bits {
                lab = (lab << 1) | usize::from(cw[perm[sym * bits + b] as usize]);
            }
            let (i, q) = pts[lab];
            ms[s.symbol_start + sym] = Cplx::new(i, q);
        }
        let data = msg[HEADER_BITS..]
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        truth.push((s.index, msg[..HEADER_BITS].to_vec(), data));
    }
    (ms, truth)
}

/// Generate `frames` whole frames (and the lead and one extra slot after
/// them, so the last frame's slots all have a following head).
///
/// # Panics
///
/// If the modes do not give a consistent allocation.
pub fn generate(o: &SynthOptions, frames: usize) -> Synth {
    let info = tmcc_info(&o.modes);
    let lay = layout(&info).expect("a consistent allocation");
    let tmcc_bits = isdbs3_core::tmcc::encode(&info);
    let signs = pilot_signs();
    let (fs, nfs, ss) = (
        sync_symbols(FSYNC),
        sync_symbols(NFSYNC),
        sync_symbols(SSYNC),
    );
    let mut rng = Rng::new(o.seed);
    let qpsk = |rng: &mut Rng| {
        let h = std::f64::consts::FRAC_1_SQRT_2;
        Cplx::new(
            if rng.bit() == 0 { h } else { -h },
            if rng.bit() == 0 { h } else { -h },
        )
    };
    let mut x: Vec<Cplx> = (0..o.lead).map(|_| qpsk(&mut rng)).collect();
    let mut truth = Vec::new();
    for _ in 0..frames {
        let at = x.len();
        let (ms, slots) = main_symbols(&lay, &mut rng);
        for k in 0..SLOTS {
            let word = match k {
                0 => &fs,
                _ if k % 2 == 0 => &nfs,
                _ => &ss,
            };
            x.extend_from_slice(word);
            let s = &lay[k];
            let pts = points(s.modulation, s.rate);
            let n = pts.len();
            for t in 0..PILOT_SYMBOLS {
                let (i, q) = pts[t % n];
                x.push(Cplx::new(i, q) * f64::from(signs[k * PILOT_SYMBOLS + t]));
            }
            for j in SYNC_SYMBOLS + PILOT_SYMBOLS..MOD_SLOT {
                match part(j) {
                    Part::Main(mi) => x.push(ms[k * MAIN_PER_MOD_SLOT + mi]),
                    Part::Tmcc(ti) => {
                        let b = tmcc_bits[k * TMCC_PER_MOD_SLOT + ti];
                        // π/2-BPSK (§3.8): slot position j is frame symbol
                        // number k·9 296 + j + 1 (1-based, odd for even j)
                        let (i, q) = isdbs3_core::constellation::bpsk(b, j + 1);
                        x.push(Cplx::new(i, q));
                    }
                    Part::Sync(_) | Part::Pilot(_) => {}
                }
            }
        }
        truth.push(FrameTruth { at, slots });
    }
    // one more slot head (FSync of the next frame) and its slot
    x.extend_from_slice(&fs);
    while x.len() % 2 == 1 || x.len() < o.lead + frames * SLOTS * MOD_SLOT + MOD_SLOT {
        x.push(qpsk(&mut rng));
    }
    // impairments
    let sigma = if o.snr_db.is_finite() {
        (10f64.powf(-o.snr_db / 10.0) / 2.0).sqrt()
    } else {
        0.0
    };
    let lim = f64::from(1i32 << (o.bits - 1));
    let mut phase = 0.0f64;
    let symbols = x
        .iter()
        .enumerate()
        .map(|(n, v)| {
            let t = n as f64 / SYMBOL_RATE;
            let f = o.carrier_hz + o.drift_hz_s * t;
            phase += std::f64::consts::TAU * f / SYMBOL_RATE;
            let w = Cplx::new(rng.normal(), rng.normal()) * sigma;
            let y = (v + w) * Cplx::from_polar(o.amplitude, phase);
            let q = |c: f64| -> i8 {
                // rounded and clamped to the n-bit range, so the cast is exact
                #[allow(clippy::cast_possible_truncation)]
                let r = c.round().clamp(-lim, lim - 1.0) as i8;
                r
            };
            [q(y.re), q(y.im)]
        })
        .collect();
    Synth {
        symbols,
        frames: truth,
        layout: lay,
    }
}
