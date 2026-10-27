//! IQ samples to ISDB-S3 symbols through the bit-exact front end of
//! `isdb-frontend` (the CPU model of the FPGA stage), with the carrier
//! offset from the `isdb-iq` survey and the output trim measured on the
//! synchronisation symbols (`isdb_frontend::s3sync::best_trim`).

use crate::Cplx;
use isdb_frontend::fast::FastFrontend as Frontend;
use isdb_frontend::params::{Design, ParamError, Params};
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};

/// ISDB-S3 in `isdb_iq::survey::KNOWN_SYSTEMS`.
pub fn system() -> &'static isdb_iq::survey::KnownSystem {
    &KNOWN_SYSTEMS[1]
}

/// How the front end's output trim is chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trim {
    /// Measured on the sync symbols of the first 0.15 s.
    Auto,
    /// This many symbol periods.
    Fixed(f64),
}

/// The front end's output.
#[derive(Debug, Clone)]
pub struct Symbols {
    /// One complex value per symbol (front-end `zk`, i16 scale).
    pub z: Vec<Cplx>,
    /// Carrier offset removed by the mixer, Hz.
    pub carrier_offset_hz: f64,
    /// Whether the survey matched ISDB-S3.
    pub survey_says_isdbs3: bool,
    /// Output trim used, symbol periods.
    pub trim: f64,
}

/// Largest mean carrier phase step between consecutive SSync slots (two
/// modulation slots, 18 592 symbols) of a candidate carrier, rad: about
/// 140 Hz of residual offset at 33.756 Mbaud ([`pick_carrier`]).
pub const MAX_PHASE_STEP: f64 = 0.5;

/// Most carrier candidates [`pick_carrier`] runs through the front end
/// (the survey's distinct estimates; each run is 0.05 s of signal on its
/// own thread).
pub const MAX_CANDIDATES: usize = 8;

/// Carrier lines of a survey that agree within this much count as one
/// candidate (the ±R/2 and z⁴ lines of one carrier agree within a few Hz).
const SAME_CARRIER_HZ: f64 = 2_000.0;

/// The survey's distinct carrier estimates, strongest first. Each line of
/// the survey is one estimate; lines that agree within
/// 2 kHz are one candidate (scored by their summed level),
/// so a carrier corroborated by several lines ranks above a single
/// unrelated line of the same strength.
pub fn carrier_candidates(sv: &isdb_iq::survey::Survey) -> Vec<f64> {
    let mut groups: Vec<(f64, f64, usize)> = Vec::new(); // (offset, level sum, lines)
    for c in &sv.carrier_lines {
        let level = c.line.level_db.max(0.0);
        match groups
            .iter_mut()
            .find(|g| (g.0 - c.offset_hz).abs() < SAME_CARRIER_HZ)
        {
            Some(g) => {
                g.1 += level;
                g.2 += 1;
            }
            None => groups.push((c.offset_hz, level, 1)),
        }
    }
    groups.sort_by(|a, b| b.1.total_cmp(&a.1));
    groups.into_iter().map(|g| g.0).collect()
}

/// Of the `candidates` (strongest first), the carrier the front end locks
/// on: each (up to [`MAX_CANDIDATES`]) runs through the front end (trim 0) over the
/// first 0.05 s, and the one with the best sync-symbol Es/N0 wins among
/// those whose carrier phase stands still between slots. A candidate far
/// off leaves the sync words uncorrelated (Es/N0 near 0 dB or no slot
/// sync at all). One a few tens of kHz off does not (a 24-symbol word
/// hardly rotates, and each slot's phase is measured anew), but its phase
/// then turns by a near-constant amount from one SSync slot to the next
/// (+1.6 rad for 48 kHz on a live block, against ±0.01 rad for the right
/// carrier), which breaks the TMCC and the main signal: so a candidate
/// whose mean phase step exceeds [`MAX_PHASE_STEP`] ranks below every
/// candidate within it. With no candidate the carrier is taken as 0.
pub fn pick_carrier(xi: &[[i16; 2]], fs: f64, candidates: &[f64]) -> Result<f64, ParamError> {
    let n = samples_in(0.05, fs).min(xi.len());
    let score = |cfo: f64| -> Result<(bool, f64), ParamError> {
        let mut fe = Frontend::new(Params::design(&design(fs, cfo))?)?;
        let mut out = Vec::new();
        fe.process(&xi[..n], &mut out);
        let z: Vec<num_complex::Complex64> = out
            .iter()
            .skip(3 * isdb_frontend::s3sync::SLOT)
            .map(|s| num_complex::Complex64::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        Ok(
            isdb_frontend::s3sync::measure(&z).map_or((false, f64::NEG_INFINITY), |r| {
                (r.phase_step_mean.abs() <= MAX_PHASE_STEP, r.snr_db)
            }),
        )
    };
    // the candidates run independently, one thread each; the first of
    // equal scores wins, as in a sequential search. Every candidate is
    // tried: at 40 Msps the survey's aliases can outrank the carrier (it
    // was 4th of 5 on a live BS11 recording, plan §0.16), and a survey
    // gives at most a handful of distinct estimates.
    let tried = &candidates[..candidates.len().min(MAX_CANDIDATES)];
    let scores: Vec<(bool, f64)> = std::thread::scope(|sc| {
        let handles: Vec<_> = tried.iter().map(|&c| sc.spawn(move || score(c))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("carrier thread"))
            .collect::<Result<_, _>>()
    })?;
    // a still phase first, then the Es/N0
    let mut best: Option<(f64, (bool, f64))> = None;
    for (&cfo, &sc) in tried.iter().zip(&scores) {
        if best.is_none_or(|b| (sc.0 && !b.1.0) || (sc.0 == b.1.0 && sc.1 > b.1.1)) {
            best = Some((cfo, sc));
        }
    }
    Ok(best.map_or(0.0, |b| b.0))
}

/// The design for ISDB-S3 at `fs` with carrier offset `cfo`.
pub fn design(fs: f64, cfo: f64) -> Design {
    let mut d = Design::for_system(system(), fs);
    d.carrier_offset = cfo;
    d
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

/// Run the ISDB-S3 front end over unit-scale samples `x` at `fs`.
pub fn symbols(
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
    /// Sync-symbol Es/N0 the front end reached with them on the excerpt
    /// the acquisition measured, dB.
    pub sync_snr_db: f64,
    /// Whether the survey matched ISDB-S3.
    pub survey_says_isdbs3: bool,
}

/// How far below the prior's Es/N0 a block may measure before the
/// acquisition is redone, dB.
pub const PRIOR_MARGIN_DB: f64 = 3.0;

/// The sync-symbol Es/N0 of the front end with design `d` on the first
/// 0.05 s of `xi` (−∞ without slot sync).
fn excerpt_snr(xi: &[[i16; 2]], fs: f64, d: &Design) -> Result<f64, ParamError> {
    let n = samples_in(0.05, fs).min(xi.len());
    let mut fe = Frontend::new(Params::design(d)?)?;
    let mut z = Vec::new();
    fe.process_each(&xi[..n], |s| {
        z.push(num_complex::Complex64::new(
            f64::from(s.zk[0]),
            f64::from(s.zk[1]),
        ));
    });
    let z = z.get(3 * isdb_frontend::s3sync::SLOT..).unwrap_or(&[]);
    Ok(isdb_frontend::s3sync::measure(z).map_or(f64::NEG_INFINITY, |r| r.snr_db))
}

/// The acquisition of ISDB-S3 on the samples `x`: the carrier (the
/// survey's candidates tried through the front end, [`pick_carrier`],
/// unless `carrier_offset_hz` is given) and the output trim (searched on
/// the first 0.15 s, unless fixed). The front end then runs with these on
/// the CPU ([`symbols_with_prior`]) or in a radio's FPGA (`isdb rx` and
/// `isdb symbols` with a symbol front end).
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
    let cfo = match carrier_offset_hz {
        Some(f) => f,
        None => pick_carrier(xi, fs, &carrier_candidates(&sv))?,
    };
    let mut d = design(fs, cfo);
    let (t, snr) = match trim {
        Trim::Fixed(t) => {
            d.trim = t;
            (t, excerpt_snr(xi, fs, &d)?)
        }
        Trim::Auto => {
            let n = samples_in(0.15, fs).min(xi.len());
            let cands: Vec<f64> = (-10..=2).map(|i| f64::from(i) * 0.025).collect();
            isdb_frontend::s3sync::best_trim(&xi[..n], &d, &cands)?
        }
    };
    Ok(Prior {
        carrier_offset_hz: cfo,
        trim: t,
        sync_snr_db: snr,
        survey_says_isdbs3: sv.system.is_some_and(|m| m.system.name == system().name),
    })
}

/// [`symbols`], reusing the carrier and trim of an earlier block of the
/// same signal (`prior`) when they still hold: the front end runs over
/// the first 0.05 s with them, and if its sync-symbol Es/N0 is within
/// [`PRIOR_MARGIN_DB`] of the prior's, the survey, the carrier choice and
/// the trim search are skipped. Otherwise (or without a prior, or with
/// `carrier_offset_hz` / a fixed trim given) the full acquisition runs.
/// Returns the symbols and the acquisition for the next block.
pub fn symbols_with_prior(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
    prior: Option<&Prior>,
) -> Result<(Symbols, Prior), ParamError> {
    let xi = to_input(x);
    let reuse = match (prior, carrier_offset_hz, trim) {
        (Some(p), None, Trim::Auto) => {
            let mut d = design(fs, p.carrier_offset_hz);
            d.trim = p.trim;
            (excerpt_snr(&xi, fs, &d)? >= p.sync_snr_db - PRIOR_MARGIN_DB).then_some(*p)
        }
        _ => None,
    };
    let acq = match reuse {
        Some(p) => p,
        None => acquire_input(x, &xi, fs, carrier_offset_hz, trim)?,
    };
    let mut d = design(fs, acq.carrier_offset_hz);
    d.trim = acq.trim;
    let mut fe = Frontend::new(Params::design(&d)?)?;
    let mut z = Vec::with_capacity(count(xi.len() as f64 / d.sps()) + 16);
    fe.process_each(&xi, |s| {
        z.push(Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1])));
    });
    Ok((
        Symbols {
            z,
            carrier_offset_hz: acq.carrier_offset_hz,
            survey_says_isdbs3: acq.survey_says_isdbs3,
            trim: acq.trim,
        },
        acq,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdb_iq::survey::{Band, CarrierLine, Line, Survey};

    fn line(k: u32, shift_hz: f64, offset_hz: f64, level_db: f64) -> CarrierLine {
        CarrierLine {
            k,
            shift_hz,
            line: Line {
                freq_hz: f64::from(k) * offset_hz + shift_hz,
                level_db,
            },
            offset_hz,
        }
    }

    fn survey_of(lines: Vec<CarrierLine>) -> Survey {
        Survey {
            sample_rate_hz: 40e6,
            samples: 1 << 22,
            power_dbfs: -10.0,
            band: Band {
                level_db: -60.0,
                center_hz: 0.0,
                width_3db_hz: 33e6,
                width_20db_hz: None,
            },
            symbol_line: Line {
                freq_hz: 6.24e6,
                level_db: 20.0,
            },
            carrier_lines: lines,
            system: None,
        }
    }

    #[test]
    fn a_carrier_two_lines_agree_on_ranks_above_a_stronger_single_line() {
        // Modelled on a live 8-bit BS7 block (plan §0.8), where the +R/2
        // window held an unrelated line 0.3 dB stronger than the true −R/2
        // line and the strongest-line rule mixed 361 kHz off. In that
        // block no other line agreed with the true carrier, so only the
        // front-end trial of `pick_carrier` found it; here the z⁴ line
        // agrees, and the ranking alone puts it first.
        let r = 33.7561e6;
        let sv = survey_of(vec![
            line(2, 0.0, -1_946_882.0, 20.1),
            line(2, r / 2.0, 222_812.0, 26.1),
            line(2, -r / 2.0, -138_498.0, 25.8),
            line(4, 0.0, -138_510.0, 15.0),
            line(8, 0.0, -1_218_299.0, 13.7),
        ]);
        assert_eq!(sv.best_carrier().unwrap().offset_hz, 222_812.0);
        let c = carrier_candidates(&sv);
        assert_eq!(c[0], -138_498.0);
        assert_eq!(c[1], 222_812.0);
        assert_eq!(c.len(), 4);
    }

    #[test]
    fn every_distinct_estimate_stays_a_candidate() {
        let sv = survey_of(vec![
            line(2, 0.0, 10_000.0, 10.0),
            line(4, 0.0, 10_500.0, 5.0),
            line(8, 0.0, 500_000.0, 30.0),
        ]);
        assert_eq!(carrier_candidates(&sv), vec![500_000.0, 10_000.0]);
        assert!(carrier_candidates(&survey_of(Vec::new())).is_empty());
    }
}
