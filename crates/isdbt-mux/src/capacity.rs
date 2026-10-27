//! Layer capacities and layer layouts that fit a given transport-stream
//! rate.
//!
//! A layer carries `tsp_per_frame` 188-byte TSPs per OFDM frame (ARIB
//! STD-B31 §3.4 Table 3-3 × segments, [`LayerConfig::tsp_per_frame`]);
//! its TS rate is therefore `tsp_per_frame · 188 · 8 / frame_seconds`
//! bit/s ([`Params::frame_seconds`], Table 3-1 frame length). Everything
//! here is computed from `isdbt_core`; no rate is tabulated.
//!
//! [`suggest`] lists layer layouts (same mode, alternative guard intervals)
//! whose capacity for the stream reaches a required rate, so a
//! "does not fit" message can name configurations that do.

use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, Segments};

/// TS rate (bit/s of 188-byte packets) of one layer configuration.
pub fn layer_config_bps(mode: Mode, guard: GuardInterval, cfg: &LayerConfig) -> f64 {
    // frame_seconds depends only on mode and guard; build it without a
    // full Params (any valid layer set gives the same frame length).
    let samples = (mode.fft_size() + guard.samples(mode)) * isdbt_core::SYMBOLS_PER_FRAME;
    let frame_s = samples as f64 / isdbt_core::sample_rate_hz();
    cfg.tsp_per_frame(mode) as f64 * 188.0 * 8.0 / frame_s
}

/// TS rate (bit/s) of `layer` in `params`, 0 if the layer is unused.
pub fn layer_bps(params: &Params, layer: Layer) -> f64 {
    params.layers()[layer.index()]
        .map_or(0.0, |c| layer_config_bps(params.mode(), params.guard(), &c))
}

/// Short layer description, e.g. `12seg 64QAM 7/8`.
pub fn describe_layer(cfg: &LayerConfig) -> String {
    let m = match cfg.modulation {
        Modulation::Dqpsk => "DQPSK",
        Modulation::Qpsk => "QPSK",
        Modulation::Qam16 => "16QAM",
        Modulation::Qam64 => "64QAM",
    };
    let (n, d) = cfg.code_rate.ratio();
    format!("{}seg {m} {n}/{d}", cfg.segments)
}

/// `isdb` layer spec `SEGMENTS,MOD,RATE,TI` of a layer configuration.
pub fn layer_spec(cfg: &LayerConfig) -> String {
    let m = match cfg.modulation {
        Modulation::Dqpsk => "dqpsk",
        Modulation::Qpsk => "qpsk",
        Modulation::Qam16 => "16qam",
        Modulation::Qam64 => "64qam",
    };
    let (n, d) = cfg.code_rate.ratio();
    format!("{},{m},{n}/{d},{}", cfg.segments, cfg.time_interleave)
}

fn guard_name(g: GuardInterval) -> String {
    format!("1/{}", g.denominator())
}

/// One layer layout whose stream layer reaches the required rate.
#[derive(Debug, Clone)]
pub struct Suggestion {
    /// The complete transmission parameters.
    pub params: Params,
    /// The layer that would carry the stream.
    pub layer: Layer,
    /// Its TS rate (bit/s).
    pub bps: f64,
    /// Human-readable summary, e.g. `B 12seg 64QAM 7/8 = 19.66 Mbit/s (GI 1/8, partial reception kept)`.
    pub summary: String,
    /// The `isdb tx` / `isdb mux` options selecting it.
    pub options: String,
}

/// Every modulation / code rate, most robust first (B31 Part 2 Table 3-2
/// order restricted to coherent modulations).
fn coherent_by_robustness() -> Vec<(Modulation, CodeRate)> {
    let rates = [
        CodeRate::R1_2,
        CodeRate::R2_3,
        CodeRate::R3_4,
        CodeRate::R5_6,
        CodeRate::R7_8,
    ];
    let mut v = Vec::new();
    for m in [Modulation::Qpsk, Modulation::Qam16, Modulation::Qam64] {
        for r in rates {
            v.push((m, r));
        }
    }
    v
}

/// Layouts that carry `need_bps` on one layer, with the transmission's mode
/// kept: for each guard interval (the configured one first, then longer
/// symbols' shorter guards 1/16, 1/32), (a) the configured layer set with
/// only `layer` changed to the most robust modulation / code rate that
/// fits, (b) when partial reception is on, a single 13-segment layer
/// without it. At most `max` entries, configured guard interval first.
pub fn suggest(params: &Params, layer: Layer, need_bps: f64, max: usize) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let Some(cur) = params.layers()[layer.index()] else {
        return out;
    };
    let ti = cur.time_interleave;
    let mut guards = vec![params.guard()];
    for g in [GuardInterval::G1_16, GuardInterval::G1_32] {
        if g.denominator() > params.guard().denominator() && !guards.contains(&g) {
            guards.push(g);
        }
    }
    for g in guards {
        // (a) same layer set, change only `layer`.
        for (m, r) in coherent_by_robustness() {
            let cfg = LayerConfig {
                modulation: m,
                code_rate: r,
                ..cur
            };
            let mut layers = params.layers();
            layers[layer.index()] = Some(cfg);
            let Ok(p) = Params::new(params.mode(), g, params.partial_reception(), layers) else {
                continue;
            };
            let bps = layer_config_bps(params.mode(), g, &cfg);
            if bps >= need_bps {
                out.push(Suggestion {
                    summary: format!(
                        "{layer} {} = {:.2} Mbit/s (GI {}{})",
                        describe_layer(&cfg),
                        bps / 1e6,
                        guard_name(g),
                        if params.partial_reception() {
                            ", partial reception kept"
                        } else {
                            ""
                        }
                    ),
                    options: format!(
                        "{}--layer-{} {}",
                        if g != params.guard() {
                            format!("--gi {} ", g.denominator())
                        } else {
                            String::new()
                        },
                        ["a", "b", "c"][layer.index()],
                        layer_spec(&cfg)
                    ),
                    params: p,
                    layer,
                    bps,
                });
                break;
            }
        }
        // (b) one 13-segment layer, no partial reception.
        if params.partial_reception() || params.active_layers().count() > 1 {
            for (m, r) in coherent_by_robustness() {
                let cfg = LayerConfig {
                    segments: Segments::MAX,
                    modulation: m,
                    code_rate: r,
                    time_interleave: ti,
                };
                let bps = layer_config_bps(params.mode(), g, &cfg);
                if bps < need_bps {
                    continue;
                }
                let Ok(p) = Params::new(params.mode(), g, false, [Some(cfg), None, None]) else {
                    continue;
                };
                out.push(Suggestion {
                    summary: format!(
                        "A {} = {:.2} Mbit/s (GI {}, single layer, no partial reception / one-seg)",
                        describe_layer(&cfg),
                        bps / 1e6,
                        guard_name(g)
                    ),
                    options: format!(
                        "{}--partial false --layer-a {} --layer-b none --layer-c none",
                        if g != params.guard() {
                            format!("--gi {} ", g.denominator())
                        } else {
                            String::new()
                        },
                        layer_spec(&cfg)
                    ),
                    params: p,
                    layer: Layer::A,
                    bps,
                });
                break;
            }
        }
    }
    out.truncate(max);
    out
}

/// Multi-line report: `what` needs `need_bps` but `layer` of `params`
/// carries only its capacity; lists [`suggest`]ions (or says none fits).
pub fn over_capacity_message(params: &Params, layer: Layer, need_bps: f64, what: &str) -> String {
    let cap = layer_bps(params, layer);
    let cur = params.layers()[layer.index()]
        .map(|c| describe_layer(&c))
        .unwrap_or_else(|| "unused".into());
    let mut s = format!(
        "{what} needs {:.2} Mbit/s but layer {layer} ({cur}, mode {}, GI {}) carries {:.2} Mbit/s ({:.1} % of the need)",
        need_bps / 1e6,
        params.mode().number(),
        guard_name(params.guard()),
        cap / 1e6,
        100.0 * cap / need_bps.max(1.0)
    );
    let sug = suggest(params, layer, need_bps, 4);
    if sug.is_empty() {
        s += "\n  no ISDB-T layer layout with this mode carries that rate";
    } else {
        s += "\n  layouts that fit (computed from ARIB STD-B31 Table 3-3):";
        for x in sug {
            s += &format!("\n    {:<70} {}", x.summary, x.options);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use isdbt_core::TimeInterleave;

    fn bench() -> Params {
        let l = |segments: u8, modulation, code_rate| {
            Some(LayerConfig {
                segments: Segments::new(segments).unwrap(),
                modulation,
                code_rate,
                time_interleave: TimeInterleave::K2,
            })
        };
        Params::new(
            Mode::M3,
            GuardInterval::G1_8,
            true,
            [
                l(1, Modulation::Qpsk, CodeRate::R2_3),
                l(12, Modulation::Qam64, CodeRate::R3_4),
                None,
            ],
        )
        .unwrap()
    }

    #[test]
    fn spec_table_3_3_rates_mode3_gi_1_8() {
        let p = bench();
        // 2592 TSP/frame × 1504 bit / 0.231336 s
        assert!((layer_bps(&p, Layer::B) / 1e6 - 16.851).abs() < 0.001);
        assert!((layer_bps(&p, Layer::A) / 1e6 - 0.416).abs() < 0.001);
        let b78 = LayerConfig {
            code_rate: CodeRate::R7_8,
            ..p.layers()[1].unwrap()
        };
        assert!(
            (layer_config_bps(Mode::M3, GuardInterval::G1_8, &b78) / 1e6 - 19.660).abs() < 0.001
        );
        let a13 = LayerConfig {
            segments: Segments::MAX,
            ..b78
        };
        assert!(
            (layer_config_bps(Mode::M3, GuardInterval::G1_8, &a13) / 1e6 - 21.298).abs() < 0.001
        );
    }

    #[test]
    fn suggestions_fit_and_are_most_robust() {
        let p = bench();
        let s = suggest(&p, Layer::B, 19.10e6, 8);
        assert!(!s.is_empty());
        // first: B 12seg 64QAM 7/8 at GI 1/8 (5/6 gives 18.72 < 19.10)
        assert!(
            s[0].summary.starts_with("B 12seg 64QAM 7/8 = 19.66"),
            "{}",
            s[0].summary
        );
        assert_eq!(s[0].options, "--layer-b 12,64qam,7/8,2");
        // second: single 13-seg layer, 64QAM 5/6 = 20.28 Mbit/s fits 19.10
        assert!(
            s[1].summary.starts_with("A 13seg 64QAM 5/6 = 20.28"),
            "{}",
            s[1].summary
        );
        assert!(
            s.iter()
                .all(|x| x.bps >= 19.10e6 && layer_bps(&x.params, x.layer) == x.bps)
        );
        // nothing carries 30 Mbit/s in 6 MHz
        assert!(suggest(&p, Layer::B, 30e6, 8).is_empty());
        let m = over_capacity_message(&p, Layer::B, 19.10e6, "input");
        assert!(
            m.contains("16.85 Mbit/s") && m.contains("--layer-b 12,64qam,7/8,2"),
            "{m}"
        );
    }
}
