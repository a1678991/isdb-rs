# isdbt-ofdm

ISDB-T symbol-domain processing: the transmit chain from coded bits to
complex baseband (carrier modulation, layer combining, time and frequency
interleaving, OFDM frame with pilots/TMCC/AC, IFFT and guard interval) and
the receiver that inverts it (synchronisation, channel estimation, TMCC
decoding, deinterleaving, soft demapping) down to per-layer LLRs.
Everything above the coded-bit interface (RS, energy dispersal, byte
interleaving, convolutional code) is in [`isdbt-fec`](https://github.com/a1678991/isdb-rs/tree/main/crates/isdbt-fec).

## Spec scope

ARIB STD-B31 v2.2 (`specs/ARIB-STD-B31-v2.2.txt`):

| Module | Clause / tables |
|---|---|
| `mapping` | §3.9: delay adjustment Table 3-9, bit interleaving Figs. 3-12/3-14/3-16/3-18, π/4-shift DQPSK Table 3-10 / Fig. 3-13, QPSK/16QAM/64QAM Figs. 3-15/3-17/3-19, normalisation Table 3-11; receiver max-log demapping, differential detection, bit deinterleaving |
| `combine` | §3.9.5 Fig. 3-20, §3.10 Fig. 3-21 |
| `time_interleave` | §3.11.1, Figs. 3-22/3-23, Table 3-12 |
| `freq_interleave` | §3.11.2, Figs. 3-25..3-27, Table 3-13 |
| `frame`, `tables` | §3.12–3.14: segment structure Figs. 3-29/3-30, Tables 3-14/3-15 (CP/TMCC/AC positions), pilot PRBS Fig. 3-31 / Tables 3-16/3-17/3-19, spectrum arrangement Fig. 3-32 |
| `ofdm` | §3.14.1–3.14.2: IFFT, guard interval; receiver FFT |
| `tmcc` | §3.15, Tables 3-20..3-31 |
| `dsc` | §3.15.6.9 (184,102) and §3.16.6.7 (187,105) shortened difference-set cyclic code |
| `ac` | §3.13.4, §3.16 (seismic warning), Attachment Ch. 6 (AC carrier order); AC CRC Fig. 3-35 |
| `latency` | Part 2 Figs. 3-8/3-9 frame alignment |
| `rx` | receiver; uses the pilot grid of §3.12/§3.13 (synchronisation and equalisation methods are not specified by B31) |

## Position in the stack

- Depends on: `isdbt-core` (modes, guard intervals, `Params`, Table 3-x
  quantities), `num-complex`, `rustfft`.
- Used by: `isdbt-phy` (TX/RX pipelines), `isdb-cli` (channel model,
  receiver configuration and metrics).

## Key types and entry points

| Item | Role |
|---|---|
| `Modulator::new(Params)` / `with_numbering` / `with_options` | TX chain for fixed parameters |
| `Modulator::modulate_frame(layer_bits, &TmccInfo, &AcFrame)` | one OFDM frame: `bits_per_frame(layer)` = `nc·S·N·204` coded bits (0/1) per active layer → `204·(Ng+N)` samples at 512/63 MHz |
| `Modulator::modulate_frame_carriers` | the same frame in the carrier domain (204 × K values) |
| `tmcc::TmccInfo::from_params` | TMCC with next = current information, countdown 1111 |
| `ac::AcFrame::stuffing` / `from_stream` / `with_seismic` | AC content (stuffing 1, serial data, §3.16.6 seismic word) |
| `Demodulator::new(DemodConfig)` / `push(&[C32])` | receiver; any chunking; yields one `DemodFrame` per on-air frame once locked |
| `DemodFrame` | `llr` per layer, `valid`, `latency_frames`, `tmcc` (with parity status), `params`, `ac_bits`, `seismic`, `metrics` (CFO, MER per layer, noise, timing drift), `start_sample` |
| `latency::{latency_frames, end_to_end_frames, ti_frames}` | frame-alignment contract with the FEC layer |
| `freq_interleave::RotationNumbering` | carrier-rotation segment numbering (default `DataSegmentNumber`) |

One frame of a single-layer Mode 1 signal, then demodulated:

```rust
use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave};
use isdbt_ofdm::{ac::AcFrame, latency, tmcc::TmccInfo, DemodConfig, Demodulator, Modulator};

let a = LayerConfig {
    segments: 13,
    modulation: Modulation::Qam16,
    code_rate: CodeRate::R1_2,
    time_interleave: TimeInterleave(1),
};
let params = Params::new(Mode::M1, GuardInterval::G1_8, false, [Some(a), None, None])?;
let mut m = Modulator::new(params.clone());
assert_eq!(m.bits_per_frame(Layer::A), 96 * 4 * 13 * 204); // nc·S·N·204
let tmcc = TmccInfo::from_params(&params);
let ac = AcFrame::stuffing(m.layout());

let mut x = 1u32; // any coded bits
let mut iq = Vec::new();
for _ in 0..latency::end_to_end_frames(&params) + 3 {
    let bits: Vec<u8> = (0..m.bits_per_frame(Layer::A))
        .map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; (x & 1) as u8 })
        .collect();
    let frame = m.modulate_frame([&bits, &[], &[]], &tmcc, &ac)?;
    assert_eq!(frame.len(), params.samples_per_frame());
    iq.extend(frame);
}

let mut rx = Demodulator::new(DemodConfig::default());
let frames = rx.push(&iq);
assert!(frames.iter().any(|f| f.valid));
assert!(frames.iter().all(|f| f.tmcc.parity.is_ok() && f.params == params));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Design notes

- **Conventions.** Bits are `u8` 0/1; soft bits are `f32` LLRs
  `ln P(b=0)/P(b=1)` (positive ⇒ 0); samples are `C32`
  (`num_complex::Complex32`) at 512/63 MHz. Bit `b0` of a carrier symbol is
  the first bit taken from the stream and the MSB of its integer label.
  Output scaling `g/√K` gives unit mean power for unit-power carriers; the
  receiver FFT returns carriers at their spec values.
- **Frame alignment** (`latency.rs`, B31 Part 2 Figs. 3-8/3-9). The
  modulator advances each layer's bit-interleaved stream by the 2 OFDM
  symbols of bit-interleaving delay through a one-frame buffer: the call
  that receives input frame `k + 1` emits on-air frame `k` (its first
  output is on-air frame −1). The demodulator adds a per-layer FIFO of
  `204·(L − F_l)` symbols, so the LLRs yielded with on-air frame `m` are
  those of input frame `m − L` for every layer (`L = max_l F_l`, Table
  3-12); end-to-end delay in modulator calls is `1 + L`.
  `DemodFrame::valid` is false for the first `L + 1` frames after lock.
  `ModulatorOptions::bit_interleave_frame_alignment = false` reproduces the
  unaligned stream for experiments only.
- **Start-up fill** (not in the spec). Mapper delay lines, DQPSK references,
  time-interleaver lines and the one-frame buffer start with a
  deterministic pseudo-random fill of valid constellation points, so the
  first frames have ordinary PAPR instead of ≈ 40 dB.
- **Receiver** (`rx/`): mode/GI/timing/fractional CFO by guard-interval
  autocorrelation; integer CFO from the AC1 + band-edge continual carriers;
  segment kinds from differential-only continual positions; SP phase;
  frame sync from DBPSK-decoded TMCC (w0/w1 confirmed by the (184,102)
  parity, which also yields the `Params`). Tracking: common-phase-error
  correction from continual carriers, timing-drift estimation with a
  Kaiser-windowed sinc resampler, SP channel estimation (time
  interpolation over `sp_time_window` SP occurrences per side, cubic
  frequency interpolation, `rx/chanest.rs`) or per-carrier differential
  detection for DQPSK, then frequency/time deinterleaving, max-log
  demapping weighted by channel state, bit deinterleaving and the
  alignment FIFO. After an abrupt parameter change the receiver
  re-acquires and reports the new `Params`.
- **Tables** in `src/tables.rs` are generated from the text extraction
  and spot-checked against the PDF images in `tests/spec.rs`.

## Verification

| Test | What it proves |
|---|---|
| `tests/spec.rs` | spec vectors: every label of Figs. 3-15/3-17/3-19, Table 3-11 unit power, Table 3-10 DQPSK, Table 3-13 permutations + PDF spot checks, Fig. 3-28 (b) randomising direction, Figs. 3-26/3-27 composed, Table 3-1/3-2 carrier counts, Tables 3-14/3-15, Figs. 3-29/3-30 segment structure, Table 3-16 PRBS initial values regenerated across the band (+ Table 3-19), Table 3-21 sync alternation decoded from the TMCC carriers, Tables 3-22..3-31 field placement, §3.15.6.9 parity vectors, §3.16.6 seismic word / CRC / parity, §3.14.2 sample counts |
| `tests/prop.rs` | proptest inverse pairs: map/demap, TMCC and seismic round trips with up to 8 bit errors, frequency, bit and time (de)interleaving identities, AC stream round trip, noiseless TX → RX loopback over random valid `Params`; `prop_bit_frame_alignment` asserts the Figs. 3-8/3-9 contract |
| `tests/loopback.rs` | Modulator → channel (CFO 2.3 carriers, echo, 30 dB SNR, sample offset) → Demodulator with zero bit errors: Mode 3 hierarchical, Mode 1 three layers (DQPSK + coherent) starting mid-frame, Mode 2 long interleave, all-DQPSK Mode 3 GI 1/32, AC seismic + serial AC + emergency flag, +40 ppm sampling-clock offset, parameter switch with re-acquisition, arbitrary chunking with noise lead-in, acquisition helpers, alignment option |
| `tests/loopback.rs` (ignored) | `stress_long_echo_and_seeds` (6 seeds; −10 dB echo at half the GI; Mode 3 at 20 ppm; BER < 1e-4) and `perf_realtime_factor` (prints TX/RX real-time factors) |
| `tests/papr.rs` | PAPR < 15 dB for the first 3 frames of a fresh modulator in 6 configurations with random coded bits (all-zero input, which bypasses energy dispersal, is only reported) |
| in-source | `dsc` generator divides x²⁷³+1 and the difference set is perfect; `mapping` demap/label consistency; `delay` delay-line behaviour |

```text
cargo test --release -p isdbt-ofdm
cargo test --release -p isdbt-ofdm --test loopback -- --ignored --nocapture
```

Independent check: the gr-isdbt oracle (TX ↔ RX cross-decoding byte-exact
in both directions, carrier-identical modulator output except for the
documented gr-isdbt TMCC/AC deviations D1–D4) —
[VERIFICATION.md](https://github.com/a1678991/isdb-rs/blob/main/VERIFICATION.md). No test needs captures or files
outside the repository.

## Known limitations

- **DQPSK** (differential segments, rotation numbering inside a
  differential group, DQPSK initial reference) is verified only by
  self-consistent loopback; gr-isdbt has no DQPSK and no off-air capture
  exists.
- **TMCC/AC difference-set code bit order** agrees with gr-isdbt but is
  not settled by the spec text; an off-air frame is still pending.
- **Not cross-checked against the oracle:** AC payload content, the
  emergency-alarm flag and parameter switching with a countdown (gr-isdbt
  has none), Mode 1 with I = 0 or 4, and behaviour under impairments
  (only tested in software loopback).
- Real receivers (a MyGica S270 USB tuner and a Sony BDZ-series recorder,
  over a conducted link) have accepted this crate's conformant TMCC
  (B17–B19, B67, where gr-isdbt deviates) in one configuration only:
  coherent Mode 3, GI 1/8, partial reception 1 + 12 segments
  ([VERIFICATION.md](https://github.com/a1678991/isdb-rs/blob/main/VERIFICATION.md#real-receivers)).
- `TmccInfo::from_params` never announces a parameter switch; switching
  requires a caller-built `TmccInfo`.

Interpretations of the spec text, with quotes and confirmation status:
[SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-ofdm/SPEC-NOTES.md). Workspace overview: [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
