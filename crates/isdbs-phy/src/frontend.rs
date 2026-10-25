//! IQ samples to ISDB-S symbols through the bit-exact front end of
//! `isdb-frontend` (the CPU model of the FPGA stage), with the carrier
//! offset taken from the `isdb-iq` survey.

use crate::Cplx;
use isdb_frontend::fixed::Frontend;
use isdb_frontend::params::{Design, ParamError, Params};
use isdb_iq::survey::{KNOWN_SYSTEMS, Options, survey};

/// ISDB-S in `isdb_iq::survey::KNOWN_SYSTEMS`.
pub fn system() -> &'static isdb_iq::survey::KnownSystem {
    &KNOWN_SYSTEMS[0]
}

/// What [`symbols`] measured and produced.
#[derive(Debug, Clone)]
pub struct Symbols {
    /// The symbols (front-end `z`, i16 scale), one per symbol.
    pub z: Vec<Cplx>,
    /// Carrier offset removed by the front end's mixer, Hz.
    pub carrier_offset_hz: f64,
    /// Symbol rate the timing loop settled at (mean over the second half),
    /// baud.
    pub symbol_rate_hz: f64,
    /// Whether the survey matched ISDB-S.
    pub survey_says_isdbs: bool,
}

/// Run the ISDB-S front end over unit-scale samples `x` at `fs`.
/// `carrier_offset_hz` overrides the survey's estimate.
pub fn symbols(
    x: &[num_complex::Complex<f32>],
    fs: f64,
    carrier_offset_hz: Option<f64>,
) -> Result<Symbols, ParamError> {
    let sv = survey(&x[..x.len().min(1 << 22)], fs, &Options::default());
    let cfo = carrier_offset_hz.unwrap_or_else(|| sv.best_carrier().map_or(0.0, |c| c.offset_hz));
    let mut d = Design::for_system(system(), fs);
    d.carrier_offset = cfo;
    let p = Params::design(&d)?;
    let mut fe = Frontend::new(p)?;
    let mut out = Vec::new();
    let mut z = Vec::with_capacity((x.len() as f64 / d.sps()) as usize + 16);
    let mut psum = 0.0;
    let mut pn = 0usize;
    for chunk in x.chunks(1 << 16) {
        let xi: Vec<[i16; 2]> = chunk
            .iter()
            .map(|v| {
                [
                    (v.re * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                    (v.im * 2048.0).round().clamp(-2048.0, 2047.0) as i16,
                ]
            })
            .collect();
        out.clear();
        fe.process(&xi, &mut out);
        for s in &out {
            z.push(Cplx::new(f64::from(s.z[0]), f64::from(s.z[1])));
            if z.len() * 2 > (x.len() as f64 / d.sps()) as usize {
                psum += f64::from(s.period);
                pn += 1;
            }
        }
    }
    let period = psum / pn.max(1) as f64 / isdb_frontend::params::ONE as f64;
    Ok(Symbols {
        z,
        carrier_offset_hz: cfo,
        symbol_rate_hz: if period > 0.0 { fs / period } else { 0.0 },
        survey_says_isdbs: sv.system.is_some_and(|m| m.system.name == system().name),
    })
}
