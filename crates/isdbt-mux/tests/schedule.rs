#![allow(missing_docs)]
//! The OFDM [`Schedule`] is the multiplexer's former frame model, number for
//! number, and a multiplexer built from it (`with_schedule`) or by `new`
//! produces the same packets; `next_period` is the carried slots of a
//! period; `Schedule::validate` refuses inconsistent schedules.

mod common;

use common::*;
use isdbt_core::{Layer, Mode, Params};
use isdbt_fec::MultiplexPattern;
use isdbt_mux::capacity::layer_bps;
use isdbt_mux::mux::clocks_to_ticks;
use isdbt_mux::si::control_layer;
use isdbt_mux::{Multiplexer, MuxError, Schedule, System};

/// Mode 1 GI 1/4 without partial reception: a second layout next to the
/// bench one (other frame length, PCR default and control layer).
fn mode1_params() -> Params {
    use isdbt_core::{CodeRate, GuardInterval, LayerConfig, Modulation, Segments, TimeInterleave};
    let layer = |segments, modulation, code_rate| {
        Some(LayerConfig {
            segments: Segments::new(segments).unwrap(),
            modulation,
            code_rate,
            time_interleave: TimeInterleave::K1,
        })
    };
    Params::new(
        Mode::M1,
        GuardInterval::G1_4,
        false,
        [
            layer(3, Modulation::Qam16, CodeRate::R1_2),
            layer(10, Modulation::Qam64, CodeRate::R7_8),
            None,
        ],
    )
    .unwrap()
}

#[test]
fn ofdm_schedule_is_the_multiplex_frame() {
    for params in [bench_params(), mode1_params()] {
        let s = Schedule::ofdm(&params);
        s.validate().unwrap();
        let pattern = MultiplexPattern::new(&params);
        assert_eq!(s.slots.len(), pattern.len());
        for (j, (&got, want)) in s.slots.iter().zip(pattern.slots()).enumerate() {
            assert_eq!(got, want.map(Layer::index), "slot {j}");
            // B31 §3.2.2: one TSP per 408 clocks, PCR byte 20 clocks in
            assert_eq!(s.start[j], 408 * j as u64);
            assert_eq!(s.pcr_byte[j], 408 * j as u64 + 20);
        }
        let spf = params.samples_per_frame() as u64;
        assert_eq!(s.period, spf);
        assert_eq!(s.period_ticks(), params.frame_ticks_27mhz());
        assert_eq!(
            s.period_seconds().to_bits(),
            params.frame_seconds().to_bits()
        );
        for k in [0u64, 1, 7, 1000, 1 << 30] {
            for j in [0, 1, s.slots.len() - 1] {
                let c = k * spf + 408 * j as u64;
                assert_eq!(s.slot_time(k, j), clocks_to_ticks(c));
                assert_eq!(s.pcr_time(k, j), clocks_to_ticks(c + 20));
            }
        }
        assert_eq!(s.lanes(), 3);
        for l in Layer::ALL {
            let tsp = params.layers()[l.index()].map_or(0, |c| c.tsp_per_frame(params.mode()));
            assert_eq!(s.lane_slots(l.index()), tsp);
            assert_eq!(s.lane_active(l.index()), tsp > 0);
            assert_eq!(
                s.lane_bps[l.index()].to_bits(),
                layer_bps(&params, l).to_bits()
            );
        }
        // B31 Part 2 Table 3-5 / Table 3-1
        assert_eq!(s.default_pcr_per_period, params.mode().scale());
        assert_eq!(s.control_lane, control_layer(&params));
        assert_eq!(s.system, System::Terrestrial(params.clone()));
        assert_eq!(s.params(), Some(&params));
    }
    // bench: partial reception → PAT/NIT on layer A
    assert_eq!(Schedule::ofdm(&bench_params()).control_lane, 0);
    assert_eq!(Schedule::ofdm(&bench_params()).lane_name(1), "layer B");
}

#[test]
fn with_schedule_equals_new() {
    let dir = tmpdir("schedule_with_schedule");
    let cfg = test_config(&dir, "");
    let params = bench_params();
    let mut a = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let mut b = Multiplexer::with_schedule(&cfg, Schedule::ofdm(&params), &dir).unwrap();
    assert_eq!(a.schedule(), b.schedule());
    assert_eq!(a.params(), b.params());
    assert_eq!(a.pattern(), b.pattern());
    for _ in 0..12 {
        assert_eq!(a.next_frame_full(), b.next_frame_full());
    }
}

#[test]
fn next_period_is_the_carried_slots() {
    let dir = tmpdir("schedule_next_period");
    let cfg = test_config(&dir, "");
    let params = bench_params();
    let mut a = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let mut b = Multiplexer::new(&cfg, &params, &dir).unwrap();
    let slots = a.schedule().slots.clone();
    let carried = slots.iter().flatten().count();
    for k in 0..12 {
        let (full, _) = a.next_frame_full();
        let want: Vec<_> = full
            .iter()
            .zip(&slots)
            .filter_map(|(p, s)| s.map(|_| *p))
            .collect();
        let got = b.next_period();
        assert_eq!(got.len(), carried);
        assert!(got == want, "period {k}");
    }
    assert_eq!(b.frames(), 12);
}

#[test]
fn validate_names_the_rule_broken() {
    let ok = Schedule::ofdm(&bench_params());
    let broken = |f: &dyn Fn(&mut Schedule)| {
        let mut s = ok.clone();
        f(&mut s);
        match s.validate() {
            Err(MuxError::Schedule { what }) => what,
            other => panic!("{other:?}"),
        }
    };
    assert!(broken(&|s| s.slots.clear()).contains("no slot"));
    assert!(
        broken(&|s| {
            s.start.pop();
        })
        .contains("starts")
    );
    assert!(broken(&|s| s.den = 0).contains("unit ratio"));
    assert!(broken(&|s| s.start.swap(0, 1)).contains("decrease"));
    assert!(broken(&|s| *s.start.last_mut().unwrap() = s.period).contains("period"));
    assert!(broken(&|s| s.slots[0] = Some(3)).contains("lane 3"));
    assert!(broken(&|s| s.lane_bps.push(0.0)).contains("at most 3"));
    assert!(broken(&|s| s.control_lane = 5).contains("control lane"));
    assert!(broken(&|s| s.default_pcr_per_period = 0).contains("PCR"));
    // and the multiplexer refuses it before reading the configuration
    let dir = tmpdir("schedule_validate");
    let cfg = test_config(&dir, "");
    let mut s = ok.clone();
    s.den = 0;
    assert!(matches!(
        Multiplexer::with_schedule(&cfg, s, &dir),
        Err(MuxError::Schedule { .. })
    ));
}

/// A schedule that leaves a lane without slots makes that layer inactive
/// for the configuration, as the transmission parameters do.
#[test]
fn a_lane_without_slots_is_an_inactive_layer() {
    let dir = tmpdir("schedule_inactive");
    let cfg = test_config(&dir, "");
    let mut s = Schedule::ofdm(&bench_params());
    for x in &mut s.slots {
        if *x == Some(0) {
            *x = None;
        }
    }
    let e = Multiplexer::with_schedule(&cfg, s, &dir).err().unwrap();
    assert!(
        matches!(
            e,
            MuxError::InactiveLayer {
                layer: Layer::A,
                ..
            }
        ),
        "{e}"
    );
}
