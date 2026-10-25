//! IQ samples to ISDB-S symbols through the bit-exact front end of
//! `isdb-frontend` (the CPU model of the FPGA stage), with the carrier
//! offset taken from the `isdb-iq` survey and the output trim measured on
//! the bursts.

use crate::Cplx;
use crate::rx::{RxOptions, receive};
use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::params::{Design, ParamError, Params};
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};

/// ISDB-S in `isdb_iq::survey::KNOWN_SYSTEMS`.
pub fn system() -> &'static isdb_iq::survey::KnownSystem {
    &KNOWN_SYSTEMS[0]
}

/// How the front end's output trim is chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trim {
    /// Measured on the signal ([`measure_trim`]).
    Auto,
    /// This many symbol periods.
    Fixed(f64),
}

/// What [`symbols`] measured and produced.
#[derive(Debug, Clone)]
pub struct Symbols {
    /// The symbols (front-end output `zk`, i16 scale), one per symbol.
    pub z: Vec<Cplx>,
    /// Carrier offset removed by the front end's mixer, Hz.
    pub carrier_offset_hz: f64,
    /// Symbol rate the timing loop settled at (mean over the second half),
    /// baud.
    pub symbol_rate_hz: f64,
    /// Whether the survey matched ISDB-S.
    pub survey_says_isdbs: bool,
    /// Output trim used, symbol periods.
    pub trim: f64,
}

/// Run the ISDB-S front end over unit-scale samples `x` at `fs`.
/// `carrier_offset_hz` overrides the survey's estimate. The output trim is
/// measured on the signal ([`Trim::Auto`]).
pub fn symbols(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
) -> Result<Symbols, ParamError> {
    symbols_with(x, fs, carrier_offset_hz, Trim::Auto)
}

fn to_input(x: &[num_complex::Complex<f32>]) -> Vec<[i16; 2]> {
    x.iter()
        .map(|v| {
            [
                (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
            ]
        })
        .collect()
}

/// The front end's design for ISDB-S at `fs` with carrier offset `cfo`.
pub fn design(fs: f64, cfo: f64) -> Design {
    let mut d = Design::for_system(system(), fs);
    d.carrier_offset = cfo;
    d
}

/// Mean burst (pilot) MER of the frames the receiver finds in `z`, dB;
/// `None` without frames.
fn pilot_mer(z: &[Cplx]) -> Option<f64> {
    let r = receive(z, &RxOptions::default());
    (!r.frames.is_empty())
        .then(|| r.frames.iter().map(|f| f.pilot_mer_db).sum::<f64>() / r.frames.len() as f64)
}

/// The output trim (symbol periods) that maximises the burst MER of the
/// front end with design `d` on the excerpt `x` (12-bit input): a search
/// over −0.20…+0.05 in steps of 0.025, refined by the parabola through the
/// best point and its neighbours. Returns the trim and the MER there.
///
/// The front end's non-data-aided timing detector locks a fraction of a
/// symbol away from the instant of least intersymbol interference on the
/// lab's satellite IF (−0.05 … −0.10 symbol on ISDB-S); the bursts, which
/// only the host can find, measure where that instant is
/// (`isdb-frontend` README, "The timing trim").
pub fn measure_trim(x: &[[i16; 2]], d: &Design) -> Result<(f64, f64), ParamError> {
    let cands: Vec<f64> = (-8..=2).map(|i| f64::from(i) * 0.025).collect();
    let mut scores = Vec::with_capacity(cands.len());
    for &t in &cands {
        let mut dd = d.clone();
        dd.trim = t;
        let (z, _) = run(x, &dd)?;
        scores.push(pilot_mer(&z).unwrap_or(f64::NEG_INFINITY));
    }
    let i = (0..scores.len())
        .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
        .expect("candidates");
    if i == 0 || i + 1 == scores.len() || !scores[i].is_finite() {
        return Ok((cands[i], scores[i]));
    }
    let (y0, y1, y2) = (scores[i - 1], scores[i], scores[i + 1]);
    let den = y0 - 2.0 * y1 + y2;
    if den >= 0.0 {
        return Ok((cands[i], y1));
    }
    let off = 0.5 * (y0 - y2) / den;
    Ok((cands[i] + off.clamp(-1.0, 1.0) * 0.025, y1))
}

/// Run the front end with design `d` over `x`: its output and the mean
/// period over the second half (Q8.24 samples).
fn run(x: &[[i16; 2]], d: &Design) -> Result<(Vec<Cplx>, f64), ParamError> {
    let mut fe = Frontend::new(Params::design(d)?)?;
    let mut out = Vec::new();
    let mut z = Vec::with_capacity((x.len() as f64 / d.sps()) as usize + 16);
    let mut psum = 0.0;
    let mut pn = 0usize;
    let half = (x.len() as f64 / d.sps()) as usize / 2;
    for chunk in x.chunks(1 << 16) {
        out.clear();
        fe.process(chunk, &mut out);
        for s in &out {
            z.push(Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1])));
            if z.len() > half {
                psum += f64::from(s.period);
                pn += 1;
            }
        }
    }
    Ok((z, psum / pn.max(1) as f64))
}

/// Run the ISDB-S front end over unit-scale samples `x` at `fs` with the
/// carrier offset `carrier_offset_hz` (default: the survey's) and output
/// trim `trim`. [`Trim::Auto`] measures it on the first 0.15 s (or all of
/// `x` when shorter).
pub fn symbols_with(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
) -> Result<Symbols, ParamError> {
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = carrier_offset_hz.unwrap_or_else(|| sv.best_carrier().map_or(0.0, |c| c.offset_hz));
    let mut d = design(fs, cfo);
    let xi = to_input(x);
    d.trim = match trim {
        Trim::Fixed(t) => t,
        Trim::Auto => {
            let n = ((0.15 * fs) as usize).min(xi.len());
            measure_trim(&xi[..n], &d)?.0
        }
    };
    let (z, period) = run(&xi, &d)?;
    let period = period / isdb_frontend::params::ONE as f64;
    Ok(Symbols {
        z,
        carrier_offset_hz: cfo,
        symbol_rate_hz: if period > 0.0 { fs / period } else { 0.0 },
        survey_says_isdbs: sv.system.is_some_and(|m| m.system.name == system().name),
        trim: d.trim,
    })
}
