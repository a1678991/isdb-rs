//! Test vectors for the FPGA implementations. Receive ([`document`]): the
//! parameters (registers, cosine table, filter banks), an input sample
//! sequence and the model's output for it (every [`Symbol`] field and the
//! packed words). Transmit ([`tx_document`]): the interpolator's design
//! and registers, the configuration as command words, the DMA words and
//! stalls, and every output sample with its internals. The HDL simulation
//! reads this JSON and must reproduce the outputs exactly.

use crate::control::{Encoder, Op, ctrl, tx};
use crate::fixed::{Frontend, Symbol};
use crate::format::Packer;
use crate::params::{FORMAT_VERSION, LOOP_DELAY, PHASES, ParamError, Params};
use crate::shape::fixed::{Interpolator, TxDesign, TxParamError, TxParams, TxSample};
use crate::tables::cos_table;
use crate::txformat;
use serde_json::{Value, json};

/// The JSON document for `params`, `input` and the model's outputs; the
/// [`ParamError`] of [`Params::validate`] if `params` are not valid.
pub fn document(params: &Params, input: &[[i16; 2]]) -> Result<Value, ParamError> {
    let mut f = Frontend::new(params.clone())?;
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
    Ok(json!({
        "format_version": FORMAT_VERSION,
        "commands": commands,
        "loop_delay": LOOP_DELAY,
        "phases": PHASES,
        "params": params_json(params),
        "cos_table": cos_table(),
        "input": input.iter().map(|s| [s[0], s[1]]).collect::<Vec<_>>(),
        "symbols": syms.iter().map(symbol_json).collect::<Vec<_>>(),
        "words": words,
    }))
}

/// The registers as JSON (field names as in [`Params`]).
pub fn params_json(p: &Params) -> Value {
    json!({
        "nco_step": p.nco_step,
        "taps": p.taps(),
        "h": p.h,
        "g": p.g,
        "k": p.k,
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
        "yk": s.yk,
        "gain": s.gain,
        "period": s.period,
        "z": s.z,
        "dz": s.dz,
        "zk": s.zk,
        "q": s.q,
        "e": s.e,
        "err": s.err,
    })
}

/// The transmit vectors (TX format [`txformat::FORMAT_VERSION`]): the
/// interpolator of `design` ([`TxParams::design`]) after START, fed
/// `words` in order with `stalls` (`(word_index, n)`: before word
/// `word_index` is delivered, `n` requests find no word,
/// [`Interpolator::stall`]), run until every word and stall has been
/// requested and up to the sample whose carry would request the next
/// word (so the counters end on a word boundary). The document:
///
/// ```text
/// {"format": 1,
///  "design": {sample_rate, symbol_rate, roll_off, span, phases, backoff_db},
///  "params": {step, shift, phases, span, a: [[2S] × P], b, id_word},
///  "gpio": [configure_tx, CTRL START, CTRL RUN|SELECT from Encoder::new(0)],
///  "words": [u32 Q << 16 | I, as sent],
///  "stalls": [[word_index, n], ...],
///  "samples": [{n, frac, q, line, sum_i, sum_q, out_i, out_q, wrap, code}, ...],
///  "counters": {consumed, missed, first_miss, samples}}
/// ```
///
/// Per sample: `frac` before the update, the row `q`, the line (2S codes,
/// newest first) as used, the sums before rounding, the 12-bit outputs,
/// `wrap` 1 when the update carried and `code` the code that entered then
/// (null without a carry; 0 for a missed symbol). The counters are the
/// registers' 32-bit values (`first_miss` 0 without a miss).
///
/// # Errors
///
/// The [`TxParamError`] of the design.
pub fn tx_document(
    design: &TxDesign,
    words: &[u32],
    stalls: &[(u64, u32)],
) -> Result<Value, TxParamError> {
    let params = TxParams::design(design)?;
    let mut m = Interpolator::new(params.clone())?;
    let mut stalls_sorted = stalls.to_vec();
    stalls_sorted.sort_by_key(|&(i, _)| i);
    let mut next = stalls_sorted.iter().peekable();
    for (i, &w) in (0u64..).zip(words) {
        while let Some(&&(_, n)) = next.peek().filter(|&&&(at, _)| at == i) {
            m.stall(n);
            next.next();
        }
        m.push_words(&[w]);
    }
    // stalls after the last word (and any at a later index)
    for &(_, n) in next {
        m.stall(n);
    }
    let mut samples = Vec::new();
    while !(m.input_done() && m.next_requests()) {
        samples.push(tx_sample_json(&m.output()));
    }
    let mut enc = Encoder::new(0);
    let mut gpio = enc.configure_tx(&params);
    gpio.push(enc.word(Op::Ctrl, tx::ctrl::START));
    gpio.push(enc.word(Op::Ctrl, tx::ctrl::RUN | tx::ctrl::SELECT));
    Ok(json!({
        "format": txformat::FORMAT_VERSION,
        "design": {
            "sample_rate": design.sample_rate,
            "symbol_rate": design.symbol_rate,
            "roll_off": design.roll_off,
            "span": design.span,
            "phases": design.phases,
            "backoff_db": design.backoff_db,
        },
        "params": tx_params_json(&params),
        "gpio": gpio,
        "words": words,
        "stalls": stalls_sorted.iter().map(|&(i, n)| [i, u64::from(n)]).collect::<Vec<_>>(),
        "samples": samples,
        "counters": {
            "consumed": low32(m.consumed()),
            "missed": low32(m.missed()),
            "first_miss": low32(m.first_miss().unwrap_or(0)),
            "samples": low32(m.samples()),
        },
    }))
}

/// The transmit registers as JSON.
pub fn tx_params_json(p: &TxParams) -> Value {
    json!({
        "step": p.step,
        "shift": p.shift,
        "phases": p.phases(),
        "span": p.span(),
        "a": p.a,
        "b": p.b,
        "id_word": p.id_word(),
    })
}

fn tx_sample_json(s: &TxSample) -> Value {
    json!({
        "n": s.n,
        "frac": s.frac,
        "q": s.q,
        "line": s.line(),
        "sum_i": s.sum[0],
        "sum_q": s.sum[1],
        "out_i": s.out[0],
        "out_q": s.out[1],
        "wrap": u8::from(s.wrap),
        "code": s.code,
    })
}

/// A counter's 32-bit register value: its low 32 bits.
fn low32(v: u64) -> u32 {
    // The registers are 32 bits wide and wrap: truncation is the value.
    #[allow(clippy::cast_possible_truncation)]
    let r = v as u32;
    r
}
