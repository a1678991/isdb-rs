//! Transmit frame builder: coded bits + TMCC + AC → OFDM frame
//! (ARIB STD-B31 §3.9–3.14).

use crate::ac::AcFrame;
use crate::combine::combine;
use crate::frame::{Dbpsk, FrameLayout, SegmentKind, pilot_value};
use crate::freq_interleave::{FrequencyInterleaver, RotationNumbering};
use crate::latency::bits_per_frame;
use crate::mapping::{CarrierMapper, FillRng, constellation_point, dqpsk_phase, normalization};
use crate::ofdm::OfdmTx;
use crate::time_interleave::TimeInterleaver;
use crate::tmcc::TmccInfo;
use crate::{C32, Error};
use isdbt_core::{Layer, Params, SYMBOLS_PER_FRAME};

/// Modulator options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModulatorOptions {
    /// Carrier-rotation numbering of the frequency interleaver.
    pub rotation: RotationNumbering,
    /// Frame alignment of the bit-interleaving delay (ARIB STD-B31 Part 2
    /// Figs. 3-8/3-9, default `true`): each layer's bit-interleaved
    /// carrier-symbol stream is advanced by the 2 OFDM symbols of total
    /// bit-interleaving delay through a one-frame buffer, so that after the
    /// receiver's bit deinterleaver data-frame symbol 1 coincides with OFDM
    /// frame symbol 1.  `false` reproduces the unaligned stream (2 symbols
    /// late) and is meant only for experiments.
    pub bit_interleave_frame_alignment: bool,
}

impl Default for ModulatorOptions {
    fn default() -> Self {
        Self {
            rotation: RotationNumbering::default(),
            bit_interleave_frame_alignment: true,
        }
    }
}

/// ISDB-T modulator for fixed [`Params`].
///
/// Each [`Modulator::modulate_frame`] call consumes one frame of coded bits
/// per layer and produces one OFDM frame (204 symbols).  The emitted frames
/// are counted from 0 per modulator; emitted frame `e` carries TMCC sync
/// word w0 for even `e`, w1 for odd `e` (Table 3-21).
///
/// With the default bit-interleave frame alignment (Figs. 3-8/3-9:
/// "Formation of OFDM symbol by 1-frame buffer … Two-symbol delay is caused
/// by bit interleaving.  Therefore, frame synchronization is delayed by two
/// symbols") the modulator holds one frame: emitted frame `e` is on-air
/// frame `e − 1` in data-frame numbering, i.e. the call that receives input
/// frame `k + 1` emits the OFDM frame that carries input frame `k`; the
/// first emitted frame (on-air frame −1) carries the delay-line fill and
/// the first two bit-interleaved symbols of input frame 0 in its last two
/// symbols.  See [`crate::latency`].
#[derive(Debug)]
pub struct Modulator {
    params: Params,
    layout: FrameLayout,
    mappers: [Option<CarrierMapper>; 3],
    /// Per layer: bit-interleaved carrier symbols waiting for their OFDM
    /// symbol (the one-frame buffer of Fig. 3-8).
    pending: [std::collections::VecDeque<C32>; 3],
    aligned: bool,
    ti: TimeInterleaver<C32>,
    fi: FrequencyInterleaver,
    ofdm: OfdmTx,
    frame: u64,
}

impl Modulator {
    /// Modulator for `params` (default frequency-interleave convention).
    pub fn new(params: Params) -> Self {
        Self::with_numbering(params, RotationNumbering::default())
    }

    /// Modulator with an explicit carrier-rotation numbering.
    pub fn with_numbering(params: Params, numbering: RotationNumbering) -> Self {
        Self::with_options(
            params,
            ModulatorOptions {
                rotation: numbering,
                ..ModulatorOptions::default()
            },
        )
    }

    /// Modulator with explicit options.
    pub fn with_options(params: Params, options: ModulatorOptions) -> Self {
        let numbering = options.rotation;
        let layout = FrameLayout::from_params(&params);
        let mut seed = 0u64;
        let mappers = params.layers.map(|l| {
            seed += 1;
            l.map(|l| CarrierMapper::with_seed(params.mode, &l, seed))
        });
        // Start-up fill.  Every carrier position first emits the initial
        // contents of its time-interleaver line (A + I·m_i symbols) and then,
        // with the bit-interleave frame alignment, the 204 − 2 lead symbols
        // of the one-frame buffer (Fig. 3-8: OFDM frame formation starts at
        // bit-interleaver output S-1,A,3).  The fill is a deterministic
        // pseudo-random sequence of constellation points of the layer's
        // modulation, so start-up frames look like ordinary frames to a
        // receiver; for DQPSK it is a π/4-shift random walk ending at the
        // mapper's (pseudo-random) start-up reference of that position, so
        // the first mapped symbol of
        // each carrier has its differential reference on air.
        let lead = if options.bit_interleave_frame_alignment {
            SYMBOLS_PER_FRAME - isdbt_core::ofdm_params::BIT_INTERLEAVE_TOTAL_SYMBOLS
        } else {
            0
        };
        let nc = params.mode.data_carriers_per_segment();
        let ti_delays = crate::time_interleave::tx_delays(&params);
        let mut fill_seqs: Vec<Vec<C32>> = Vec::with_capacity(ti_delays.len());
        let mut rng = FillRng::new(0x5eed);
        let mut next = || rng.next_u32();
        for (q, &d) in ti_delays.iter().enumerate() {
            let layer = params.layer_of_segment(q / nc);
            let m = params.layers[layer.index()].expect("active").modulation;
            let len = d + lead;
            let seq: Vec<C32> = if m.is_differential() {
                let mut v = vec![C32::default(); len];
                // End at this position's mapper start-up reference.
                let (first, _) = crate::combine::layer_segments(&params, layer).expect("active");
                let r = mappers[layer.index()]
                    .as_ref()
                    .expect("mapper")
                    .dqpsk_reference(q - first * nc)
                    / normalization(m);
                let (mut i, mut qq) = (r.re.round(), r.im.round());
                for slot in v.iter_mut().rev() {
                    *slot = C32::new(i, qq) * normalization(m);
                    // Step back by a random π/4-shift phase.
                    let th = -dqpsk_phase((next() & 1) as u8, (next() & 1) as u8);
                    let (c, sn) = (th.cos(), th.sin());
                    let (ni, nq) = (c * i - sn * qq, sn * i + c * qq);
                    let snap = |x: f32| {
                        if x.abs() < 0.5 {
                            0.0
                        } else if (x.abs() - 1.0).abs() < 0.2 {
                            x.signum()
                        } else {
                            x.signum() * std::f32::consts::SQRT_2
                        }
                    };
                    i = snap(ni);
                    qq = snap(nq);
                }
                v
            } else {
                let labels = 1u32 << m.bits_per_symbol();
                (0..len)
                    .map(|_| constellation_point(m, (next() % labels) as u8))
                    .collect()
            };
            fill_seqs.push(seq);
        }
        let pending = std::array::from_fn(|l| {
            let layer = Layer::ALL[l];
            match crate::combine::layer_segments(&params, layer) {
                Some((first, count)) => {
                    let per = count * nc;
                    let mut qd = std::collections::VecDeque::with_capacity(lead * per);
                    for j in 0..lead {
                        for p in 0..per {
                            let q = first * nc + p;
                            qd.push_back(fill_seqs[q][ti_delays[q] + j]);
                        }
                    }
                    qd
                }
                None => std::collections::VecDeque::new(),
            }
        });
        let ti = TimeInterleaver::with_initial(&params, |q, d| fill_seqs[q][..d].to_vec());
        Self {
            pending,
            aligned: options.bit_interleave_frame_alignment,
            ti,
            fi: FrequencyInterleaver::with_numbering(&params, numbering),
            ofdm: OfdmTx::new(params.mode, params.guard),
            layout,
            mappers,
            params,
            frame: 0,
        }
    }

    /// Parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Carrier layout.
    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// Index of the next frame to be emitted (counted from 0).
    pub fn frame_index(&self) -> u64 {
        self.frame
    }

    /// On-air (data-frame) index of the next frame to be emitted:
    /// `frame_index() − 1` with the bit-interleave frame alignment (one-frame
    /// buffer), `frame_index()` without it.
    pub fn onair_frame_index(&self) -> i64 {
        self.frame as i64 - i64::from(self.aligned)
    }

    /// Frames held by the modulator (1 with the bit-interleave frame
    /// alignment, else 0).
    pub fn buffer_frames(&self) -> usize {
        usize::from(self.aligned)
    }

    /// Coded bits per frame expected for `layer` (0 if unused).
    pub fn bits_per_frame(&self, layer: Layer) -> usize {
        bits_per_frame(&self.params, layer)
    }

    /// Set the output amplitude gain (see [`crate::ofdm`] for the scaling).
    pub fn set_gain(&mut self, g: f32) {
        self.ofdm.set_gain(g);
    }

    /// Build one frame in the carrier domain: 204 symbols × K carrier values
    /// c(n, k) (§3.14.1).
    pub fn modulate_frame_carriers(
        &mut self,
        layer_bits: [&[u8]; 3],
        tmcc: &TmccInfo,
        ac: &AcFrame,
    ) -> Result<Vec<Vec<C32>>, Error> {
        for layer in Layer::ALL {
            let expected = self.bits_per_frame(layer);
            if layer_bits[layer.index()].len() != expected {
                return Err(Error::BitCount {
                    layer,
                    expected,
                    got: layer_bits[layer.index()].len(),
                });
            }
        }
        if ac.carriers.len() != self.layout.ac.len() {
            return Err(Error::Invalid(format!(
                "AC frame has {} carriers, layout {}",
                ac.carriers.len(),
                self.layout.ac.len()
            )));
        }
        let odd = self.frame % 2 == 1;
        let tmcc_coh = tmcc.frame_bits(odd, false);
        let tmcc_diff = tmcc.frame_bits(odd, true);
        let nc = self.params.mode.data_carriers_per_segment();
        let k_total = self.layout.k_total;
        let mut layer_sym: [Vec<C32>; 3] = std::array::from_fn(|l| {
            vec![
                C32::default();
                self.mappers[l]
                    .as_ref()
                    .map_or(0, |m| m.symbols_per_ofdm_symbol())
            ]
        });
        let mut combined = vec![C32::default(); 13 * nc];
        let mut interleaved = vec![C32::default(); 13 * nc];
        let mut tmcc_enc: Vec<Dbpsk> = Vec::with_capacity(self.layout.tmcc.len());
        let mut ac_enc: Vec<Dbpsk> = Vec::with_capacity(self.layout.ac.len());
        let mut out = Vec::with_capacity(SYMBOLS_PER_FRAME);
        for n in 0..SYMBOLS_PER_FRAME {
            // §3.9: per-layer mapping of this symbol's bits.
            for (l, mapper) in self.mappers.iter_mut().enumerate() {
                if let Some(m) = mapper {
                    let per = layer_bits[l].len() / SYMBOLS_PER_FRAME;
                    m.map_symbol(&layer_bits[l][n * per..(n + 1) * per], &mut layer_sym[l]);
                    // One-frame buffer (Figs. 3-8/3-9 alignment).
                    let q = &mut self.pending[l];
                    q.extend(layer_sym[l].iter().copied());
                    for v in layer_sym[l].iter_mut() {
                        *v = q.pop_front().expect("buffered symbol");
                    }
                }
            }
            // §3.10 combine, §3.11.1 time interleave, §3.11.2 frequency interleave.
            combine(
                &self.params,
                [&layer_sym[0], &layer_sym[1], &layer_sym[2]],
                &mut combined,
            );
            self.ti.process(&mut combined);
            self.fi.interleave(&combined, &mut interleaved);
            // §3.12–3.14 frame structure.
            let mut sym = vec![C32::default(); k_total];
            let phase = n % 4;
            for (q, &k) in self.layout.data[phase].iter().enumerate() {
                sym[k as usize] = interleaved[q];
            }
            for &k in &self.layout.sp[phase] {
                sym[k as usize] = pilot_value(self.layout.w[k as usize]);
            }
            for &k in &self.layout.cp {
                sym[k as usize] = pilot_value(self.layout.w[k as usize]);
            }
            let e = self.layout.edge as usize;
            sym[e] = pilot_value(self.layout.w[e]);
            for (i, t) in self.layout.tmcc.iter().enumerate() {
                let bits = if t.kind == SegmentKind::Differential {
                    &tmcc_diff
                } else {
                    &tmcc_coh
                };
                sym[t.k as usize] = if n == 0 {
                    let (enc, v) = Dbpsk::start(self.layout.w[t.k as usize]);
                    tmcc_enc.push(enc);
                    v
                } else {
                    tmcc_enc[i].next(bits[n])
                };
            }
            for (i, a) in self.layout.ac.iter().enumerate() {
                sym[a.k as usize] = if n == 0 {
                    let (enc, v) = Dbpsk::start(self.layout.w[a.k as usize]);
                    ac_enc.push(enc);
                    v
                } else {
                    ac_enc[i].next(ac.carriers[i][n])
                };
            }
            out.push(sym);
        }
        self.frame += 1;
        Ok(out)
    }

    /// Build one frame of complex baseband samples at 512/63 MHz
    /// (204 × (Ng + N) samples).
    pub fn modulate_frame(
        &mut self,
        layer_bits: [&[u8]; 3],
        tmcc: &TmccInfo,
        ac: &AcFrame,
    ) -> Result<Vec<C32>, Error> {
        let symbols = self.modulate_frame_carriers(layer_bits, tmcc, ac)?;
        let mut out = Vec::with_capacity(self.params.samples_per_frame());
        for s in &symbols {
            self.ofdm.modulate(s, &mut out);
        }
        Ok(out)
    }
}
