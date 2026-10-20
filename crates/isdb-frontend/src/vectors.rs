//! Test vectors for the FPGA implementation: the parameters (registers,
//! cosine table, filter banks), an input sample sequence and the model's
//! output for it (every [`Symbol`] field and the packed words). The HDL
//! simulation reads this JSON and must reproduce the outputs exactly.

use crate::control::{Encoder, Op, ctrl};
use crate::fixed::{Frontend, Symbol};
use crate::format::Packer;
use crate::params::{FORMAT_VERSION, LOOP_DELAY, PHASES, Params};
use crate::tables::cos_table;
use serde_json::{Value, json};

/// The JSON document for `params`, `input` and the model's outputs.
pub fn document(params: &Params, input: &[[i16; 2]]) -> Value {
    let mut f = Frontend::new(params.clone()).expect("valid parameters");
    let mut syms: Vec<Symbol> = Vec::new();
    f.process(input, &mut syms);
    let mut pk = Packer::new(params);
    let mut words = Vec::new();
    for s in &syms {
        pk.push(s.q, s.gain, s.period, &mut words);
    }
    // The host's configuration sequence, then start + run + symbols out.
    let mut enc = Encoder::new(0);
    let mut commands = enc.configure(params);
    commands.push(enc.word(Op::Ctrl, ctrl::START));
    commands.push(enc.word(Op::Ctrl, ctrl::RUN | ctrl::SYMBOLS));
    json!({
        "format_version": FORMAT_VERSION,
        "commands": commands,
        "loop_delay": LOOP_DELAY,
        "phases": PHASES,
        "params": params_json(params),
        "cos_table": cos_table(),
        "input": input.iter().map(|s| [s[0], s[1]]).collect::<Vec<_>>(),
        "symbols": syms.iter().map(symbol_json).collect::<Vec<_>>(),
        "words": words,
    })
}

/// The registers as JSON (field names as in [`Params`]).
pub fn params_json(p: &Params) -> Value {
    json!({
        "nco_step": p.nco_step,
        "taps": p.taps(),
        "h": p.h,
        "g": p.g,
        "period_nominal": p.period_nominal,
        "period_min": p.period_min,
        "period_max": p.period_max,
        "ted_shift": p.ted_shift,
        "kp": p.kp,
        "kp_shift": p.kp_shift,
        "ki": p.ki,
        "ki_shift": p.ki_shift,
        "integrator_max": p.integrator_max,
        "agc_init": p.agc_init,
        "agc_target": p.agc_target,
        "agc_shift": p.agc_shift,
        "bits": p.bits,
        "q_shift": p.q_shift,
        "block_log2": p.block_log2,
    })
}

fn symbol_json(s: &Symbol) -> Value {
    json!({
        "sample": s.sample,
        "phase": s.phase,
        "y": s.y,
        "dy": s.dy,
        "gain": s.gain,
        "period": s.period,
        "z": s.z,
        "dz": s.dz,
        "q": s.q,
        "e": s.e,
        "err": s.err,
    })
}
