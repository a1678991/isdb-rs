//! IQ samples to ISDB-S3 symbols through the bit-exact front end of
//! `isdb-frontend` (the CPU model of the FPGA stage), with the carrier
//! offset from the `isdb-iq` survey and the output trim measured on the
//! synchronisation symbols (`isdb_frontend::s3sync::best_trim`).

use crate::Cplx;
use isdb_frontend::fixed::Frontend;
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
/// on: each of the first few runs through the front end (trim 0) over the
/// first 0.05 s, and the one with the best sync-symbol Es/N0 wins. A wrong
/// candidate leaves the sync words uncorrelated (Es/N0 near 0 dB or no
/// slot sync at all), the right one gives the signal's SNR less the
/// untrimmed timing loss, so the margin is large. With no candidate the
/// carrier is taken as 0.
fn pick_carrier(xi: &[[i16; 2]], fs: f64, candidates: &[f64]) -> Result<f64, ParamError> {
    let n = ((0.05 * fs) as usize).min(xi.len());
    let score = |cfo: f64| -> Result<f64, ParamError> {
        let mut fe = Frontend::new(Params::design(&design(fs, cfo))?)?;
        let mut out = Vec::new();
        fe.process(&xi[..n], &mut out);
        let z: Vec<num_complex::Complex64> = out
            .iter()
            .skip(3 * isdb_frontend::s3sync::SLOT)
            .map(|s| num_complex::Complex64::new(f64::from(s.zk[0]), f64::from(s.zk[1])))
            .collect();
        Ok(isdb_frontend::s3sync::measure(&z).map_or(f64::NEG_INFINITY, |r| r.snr_db))
    };
    // the candidates run independently, one thread each; the first of
    // equal scores wins, as in a sequential search
    let tried = &candidates[..candidates.len().min(4)];
    let scores: Vec<f64> = std::thread::scope(|sc| {
        let handles: Vec<_> = tried.iter().map(|&c| sc.spawn(move || score(c))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("carrier thread"))
            .collect::<Result<_, _>>()
    })?;
    let mut best: Option<(f64, f64)> = None;
    for (&cfo, &snr) in tried.iter().zip(&scores) {
        if best.is_none_or(|b| snr > b.1) {
            best = Some((cfo, snr));
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

/// Run the ISDB-S3 front end over unit-scale samples `x` at `fs`.
pub fn symbols(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
    trim: Trim,
) -> Result<Symbols, ParamError> {
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let xi = to_input(x);
    let cfo = match carrier_offset_hz {
        Some(f) => f,
        None => pick_carrier(&xi, fs, &carrier_candidates(&sv))?,
    };
    let mut d = design(fs, cfo);
    d.trim = match trim {
        Trim::Fixed(t) => t,
        Trim::Auto => {
            let n = ((0.15 * fs) as usize).min(xi.len());
            let cands: Vec<f64> = (-10..=2).map(|i| f64::from(i) * 0.025).collect();
            isdb_frontend::s3sync::best_trim(&xi[..n], &d, &cands)?.0
        }
    };
    let mut fe = Frontend::new(Params::design(&d)?)?;
    let mut out = Vec::new();
    let mut z = Vec::with_capacity((xi.len() as f64 / d.sps()) as usize + 16);
    for chunk in xi.chunks(1 << 16) {
        out.clear();
        fe.process(chunk, &mut out);
        z.extend(
            out.iter()
                .map(|s| Cplx::new(f64::from(s.zk[0]), f64::from(s.zk[1]))),
        );
    }
    Ok(Symbols {
        z,
        carrier_offset_hz: cfo,
        survey_says_isdbs3: sv.system.is_some_and(|m| m.system.name == system().name),
        trim: d.trim,
    })
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
