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
    let cfo = carrier_offset_hz.unwrap_or_else(|| sv.best_carrier().map_or(0.0, |c| c.offset_hz));
    let mut d = design(fs, cfo);
    let xi = to_input(x);
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
