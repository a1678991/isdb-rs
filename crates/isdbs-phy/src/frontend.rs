//! IQ samples to ISDB-S symbols through the bit-exact front end of
//! `isdb-frontend` (the CPU model of the FPGA stage), with the carrier
//! offset taken from the `isdb-iq` survey and the output trim measured on
//! the bursts.

use crate::Cplx;
use crate::rx::{RxOptions, receive};
use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::params::{Design, ParamError, Params};
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};
use tracing::{debug, info};

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

/// Unit-scale samples as the front end's 12-bit input (full scale 2048).
pub fn to_input(x: &[num_complex::Complex<f32>]) -> Vec<[i16; 2]> {
    x.iter().map(|v| [q12(v.re), q12(v.im)]).collect()
}

/// One unit-scale sample as a 12-bit value (rounded, clamped to -2048…2047).
// DSP sample quantisation: the value is rounded and clamped to the 12-bit
// range before the cast, so the cast to i16 is exact.
#[allow(clippy::cast_possible_truncation)]
fn q12(v: f32) -> i16 {
    (v * 2048.0).round().clamp(-2048.0, 2047.0) as i16
}

/// The number of samples in `seconds` at `fs` (truncated; 0 for a
/// negative or NaN product, saturated past `usize::MAX`).
pub fn samples_in(seconds: f64, fs: f64) -> usize {
    count(seconds * fs)
}

/// A sample or symbol count from a product of a rate and a time.
// f64 -> usize has no fallible conversion: `as` truncates toward zero and
// saturates, which is the wanted count (NaN gives 0).
#[allow(clippy::cast_possible_truncation)]
fn count(x: f64) -> usize {
    x as usize
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
    // the candidates run independently, one thread each
    let score = |t: f64| -> Result<f64, ParamError> {
        let mut dd = d.clone();
        dd.trim = t;
        let (z, _) = run(x, &dd)?;
        Ok(pilot_mer(&z).unwrap_or(f64::NEG_INFINITY))
    };
    let scores: Vec<f64> = std::thread::scope(|sc| {
        let hs: Vec<_> = cands.iter().map(|&t| sc.spawn(move || score(t))).collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect::<Result<_, _>>()
    })?;
    // the best of the 11 candidates (the last of equal ones, as `max_by`)
    let i = (1..scores.len()).fold(0, |best, a| {
        if scores[best].total_cmp(&scores[a]).is_gt() {
            best
        } else {
            a
        }
    });
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
    let symbols = count(x.len() as f64 / d.sps());
    let mut z = Vec::with_capacity(symbols + 16);
    let mut psum = 0.0;
    let mut pn = 0usize;
    let half = symbols / 2;
    fe.process_each(x, |s| {
        z.push(Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1])));
        if z.len() > half {
            psum += f64::from(s.period);
            pn += 1;
        }
    });
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
    Ok(symbols_with_prior(x, fs, carrier_offset_hz, trim, None)?.0)
}

/// What one block's acquisition found, for the next block of the same
/// signal ([`symbols_with_prior`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prior {
    /// Carrier offset, Hz.
    pub carrier_offset_hz: f64,
    /// Output trim, symbol periods.
    pub trim: f64,
    /// Burst MER the front end reached with them on the acquisition's
    /// excerpt, dB.
    pub pilot_mer_db: f64,
    /// Whether the survey matched ISDB-S.
    pub survey_says_isdbs: bool,
}

/// How far below the prior's burst MER a block may measure before the
/// acquisition is redone, dB.
pub const PRIOR_MARGIN_DB: f64 = 3.0;

/// The acquisition of ISDB-S on the samples `x`: the carrier (the survey's
/// strongest line, unless `carrier_offset_hz` is given) and the output trim
/// (searched on the first 0.15 s, unless fixed). The front end then runs
/// with these on the CPU ([`symbols_with_prior`]) or in a radio's FPGA.
pub fn acquire(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
) -> Result<Prior, ParamError> {
    acquire_input(x, &to_input(x), fs, carrier_offset_hz, trim)
}

fn acquire_input(
    x: &[num_complex::Complex<f32>],
    xi: &[[i16; 2]],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
) -> Result<Prior, ParamError> {
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = carrier_offset_hz.unwrap_or_else(|| sv.best_carrier().map_or(0.0, |c| c.offset_hz));
    let survey_says_isdbs = sv.system.is_some_and(|m| m.system.name == system().name);
    debug!(
        samples = x.len().min(1 << 22),
        sample_rate_hz = fs,
        carrier_offset_hz = cfo,
        given = carrier_offset_hz.is_some(),
        survey_says_isdbs,
        "survey of {} samples at {fs:.0} Hz: carrier {cfo:+.0} Hz ({}), {}",
        x.len().min(1 << 22),
        if carrier_offset_hz.is_some() {
            "given"
        } else {
            "strongest line"
        },
        if survey_says_isdbs {
            "ISDB-S"
        } else {
            "system not recognised"
        }
    );
    let mut d = design(fs, cfo);
    let (t, mer) = match trim {
        Trim::Fixed(t) => {
            d.trim = t;
            let n = samples_in(0.05, fs).min(xi.len());
            let (z, _) = run(&xi[..n], &d)?;
            (t, pilot_mer(&z).unwrap_or(f64::NEG_INFINITY))
        }
        Trim::Auto => {
            let n = samples_in(0.15, fs).min(xi.len());
            measure_trim(&xi[..n], &d)?
        }
    };
    info!(
        carrier_offset_hz = cfo,
        trim = t,
        pilot_mer_db = mer,
        survey_says_isdbs,
        "acquired ISDB-S: carrier {cfo:+.0} Hz, trim {t:+.3} symbol ({}), burst MER {mer:.2} dB",
        match trim {
            Trim::Auto => "measured",
            Trim::Fixed(_) => "fixed",
        }
    );
    Ok(Prior {
        carrier_offset_hz: cfo,
        trim: t,
        pilot_mer_db: mer,
        survey_says_isdbs,
    })
}

/// [`symbols_with`], reusing the carrier and trim of an earlier block of
/// the same signal (`prior`) when they still hold: the front end runs over
/// the first 0.05 s with them, and if its burst MER is within
/// [`PRIOR_MARGIN_DB`] of the prior's, the survey and the trim search are
/// skipped. Otherwise (or without a prior, or with `carrier_offset_hz` / a
/// fixed trim given) the full acquisition runs. Returns the symbols and
/// the acquisition for the next block.
pub fn symbols_with_prior(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
    prior: Option<&Prior>,
) -> Result<(Symbols, Prior), ParamError> {
    let xi = to_input(x);
    let excerpt_mer = |d: &Design, secs: f64| -> Result<f64, ParamError> {
        let n = samples_in(secs, fs).min(xi.len());
        let (z, _) = run(&xi[..n], d)?;
        Ok(pilot_mer(&z).unwrap_or(f64::NEG_INFINITY))
    };
    let reuse = match (prior, carrier_offset_hz, trim) {
        (Some(p), None, Trim::Auto) => {
            let mut d = design(fs, p.carrier_offset_hz);
            d.trim = p.trim;
            let mer = excerpt_mer(&d, 0.05)?;
            if mer >= p.pilot_mer_db - PRIOR_MARGIN_DB {
                debug!(
                    carrier_offset_hz = p.carrier_offset_hz,
                    trim = p.trim,
                    pilot_mer_db = mer,
                    prior_mer_db = p.pilot_mer_db,
                    "previous acquisition reused: burst MER {mer:.2} dB (prior {:.2} dB)",
                    p.pilot_mer_db
                );
                Some(*p)
            } else {
                info!(
                    carrier_offset_hz = p.carrier_offset_hz,
                    trim = p.trim,
                    pilot_mer_db = mer,
                    prior_mer_db = p.pilot_mer_db,
                    margin_db = PRIOR_MARGIN_DB,
                    "acquiring again: burst MER {mer:.2} dB with the previous carrier and trim, more than {PRIOR_MARGIN_DB} dB below the {:.2} dB they gave",
                    p.pilot_mer_db
                );
                None
            }
        }
        _ => None,
    };
    let acq = match reuse {
        Some(p) => p,
        None => acquire_input(x, &xi, fs, carrier_offset_hz, trim)?,
    };
    let mut d = design(fs, acq.carrier_offset_hz);
    d.trim = acq.trim;
    let (z, period) = run(&xi, &d)?;
    let period = period / isdb_frontend::params::ONE as f64;
    debug!(
        samples = xi.len(),
        symbols = z.len(),
        symbol_rate_hz = if period > 0.0 { fs / period } else { 0.0 },
        "front end: {} samples to {} symbols; timing loop settled at {:.1} baud",
        xi.len(),
        z.len(),
        if period > 0.0 { fs / period } else { 0.0 }
    );
    Ok((
        Symbols {
            z,
            carrier_offset_hz: acq.carrier_offset_hz,
            symbol_rate_hz: if period > 0.0 { fs / period } else { 0.0 },
            survey_says_isdbs: acq.survey_says_isdbs,
            trim: acq.trim,
        },
        acq,
    ))
}
