# isdbt-fec

Bit-domain channel coding of ISDB-T (ARIB STD-B31 v2.2 §3.2–3.8), both
directions: the TS re-multiplexer's multiplex-frame pattern (computed by
simulating the spec's model receiver), RS(204,188) outer code, energy
dispersal, delay adjustment, byte interleaving and the punctured K = 7
convolutional code with a soft-decision Viterbi decoder. `LayerEncoder` /
`LayerDecoder` run the whole per-layer chain one OFDM frame at a time:
188/204-byte TSPs in, coded bits out (TX); LLRs in, RS-corrected TSPs out
(RX).

## Spec scope

ARIB STD-B31 v2.2, Chapter 3 and the reference Part 2 figures:

| Module | Clause | Contents |
|---|---|---|
| `remux` | §3.2, §3.4, Tables 3-5/3-6, Figs. 3-3..3-5 | `MultiplexPattern`: model-receiver slot pattern of a multiplex frame; TX `assemble`, hierarchical `divide`, RX `reproduce_ts` |
| `rs` | §3.3 | shortened RS(204,188) over GF(2⁸)/0x11D, encoder and t = 8 decoder |
| `energy` | §3.5, Fig. 3-8 | PRBS 1 + X¹⁴ + X¹⁵, initial state `100101010000000`, reset every OFDM frame |
| `delay` | §3.6, Table 3-7 | delay adjustment FIFO of T − 11 slots |
| `byte_interleave` | §3.7, Fig. 3-9 | 12-path convolutional byte (de)interleaver, 17-byte unit |
| `conv` | §3.8, Fig. 3-10, Table 3-8 | 171₈/133₈ mother code, puncturing, soft Viterbi |
| `layer` | §3.4–3.8, Fig. 3-2 | per-layer frame pipeline and the one-frame latency contract |

The bit interleaving and everything after it (§3.9–3.16) is in
`isdbt-ofdm`.

## Position in the stack

- Depends on: `isdb-fec` (the RS code, the K = 7 mother code and Viterbi
  decoder, and the 15-bit PRBS, shared with ISDB-S; `rs`, `conv` and
  `energy` keep their APIs here and delegate), `isdbt-core` (`LayerConfig`,
  `Mode`, `Params`, Table 3-7 / coded-bit figures).
- Used by: `isdbt-phy` (TX/RX pipelines: `LayerEncoder`, `LayerDecoder`,
  `MultiplexPattern`, `LATENCY_FRAMES`), `isdbt-mux` (`MultiplexPattern`
  for multiplex-frame output and null packets) and `isdb-cli`.

## Key types and entry points

- `LayerEncoder::new(mode, &layer)`; `encode_frame_188(&[[u8; 188]])` or
  `encode_frame(&[[u8; 204]])` with exactly `tsps_per_frame()` packets →
  `coded_bits_per_frame()` bits (u8 0/1) in carrier-modulator order.
- `LayerDecoder::new(mode, &layer)` (or `with_viterbi` for a custom
  traceback or an unknown start state); `decode_frame(&[f32])` →
  `DecodedFrame { tsps, status: Vec<RsStatus>, stats: RsStats }`;
  `tsps_188()` strips parity.
- `MultiplexPattern::new(&params)`: `slots()`, `count(layer)`,
  `null_count()`, `assemble`, `divide`, `reproduce_ts`.
- Stage-level APIs: `rs::{encode, decode}`, `energy::Prbs`,
  `delay::DelayLine`, `byte_interleave::{ByteInterleaver, ByteDeinterleaver}`,
  `conv::{ConvEncoder, Puncturing, ViterbiDecoder, encode_punctured,
  depuncture, decode_punctured}`.

One-frame round trip with hard-decision LLRs (Mode 1, one segment, QPSK
1/2 → 12 TSPs per frame):

```rust
use isdbt_core::{CodeRate, LayerConfig, Mode, Modulation, Segments, TimeInterleave, TSP};
use isdbt_fec::{FecError, LayerDecoder, LayerEncoder};

fn main() -> Result<(), FecError> {
    let layer = LayerConfig {
        segments: Segments::new(1).unwrap(),
        modulation: Modulation::Qpsk,
        code_rate: CodeRate::R1_2,
        time_interleave: TimeInterleave::K0,
    };
    let mut enc = LayerEncoder::new(Mode::M1, &layer);
    let mut dec = LayerDecoder::new(Mode::M1, &layer);
    assert_eq!(enc.tsps_per_frame(), 12); // Table 3-3

    let frames: Vec<Vec<[u8; TSP]>> = (0..2u8)
        .map(|k| (0..12u8).map(|i| { let mut p = [k ^ i; TSP]; p[0] = 0x47; p }).collect())
        .collect();
    let mut out = Vec::new();
    for f in &frames {
        let bits = enc.encode_frame_188(f)?;
        // LLR = ln P(b=0)/P(b=1): positive means 0.
        let llr: Vec<f32> = bits.iter().map(|&b| if b == 0 { 1.0 } else { -1.0 }).collect();
        out.push(dec.decode_frame(&llr)?);
    }
    // One-frame latency (§3.6): decoder call 1 returns encoder call 0.
    assert_eq!(out[1].tsps_188(), frames[0]);
    assert_eq!(out[1].stats.clean, 12);
    Ok(())
}
```

The crate emits one [`tracing`](https://docs.rs/tracing) event, at
`debug`, when a multiplex pattern is built (mode, guard, slots per layer);
nothing per packet.

## Design notes

- **Conventions.** Coded bits are `u8` 0/1; soft bits are `f32` LLRs
  `ln P(b=0)/P(b=1)`, 0.0 for erased (punctured) positions.
- **Slots (§3.4).** A layer frame is cut into 204-byte slots starting at
  the byte after the sync byte; energy dispersal, byte-interleaver path 0
  and the puncturing reset are all aligned to slot 0 byte 0 of the frame.
- **Latency contract.** Delay adjustment (T − 11 slots) plus the
  interleaver/deinterleaver pair (11 slots) delay every byte by exactly
  one frame: `LayerDecoder::decode_frame` call k returns the TSPs given to
  `LayerEncoder` call k − 1, frame-aligned (`layer::LATENCY_FRAMES = 1`).
  The convolutional encoder state carries across frames; the Viterbi
  decoder keeps path metrics but releases all decisions at each frame end,
  so it adds no frame latency. The symbol-domain delays are `isdbt-ofdm`'s
  responsibility (its `latency` module); the integrator must hand RX call k
  exactly the LLRs of TX call k.
- **Encoder start-up.** B31 does not define the initial memory contents.
  `LayerEncoder::new` primes the delay line and interleaver with one
  virtual frame of dispersed null transmission TSPs
  (`null_transmission_tsp`), so the first coded frame has no long zero runs
  (which caused ~37 dB PAPR on an 8-bit DAC). RX call 0 of a fresh
  decoder is warm-up output.
- **Model receiver.** `MultiplexPattern::new` is a clock-level simulation
  of Fig. 3-4 (one clock = one FFT sample) and asserts its own consistency
  (every packet read inside its S3 interval, counts = Tables 3-3/3-5).
- RS failures are reported per packet (`RsStatus::Uncorrectable`, bytes
  left as received); setting the transport_error_indicator is done by
  `isdbt-phy`.

## Verification

`cargo test -p isdbt-fec` (the workspace test profile is opt-level 2). No
captures or fixtures are needed.

| Test | Proves |
|---|---|
| `src/rs.rs` `spec_3_3_*` | primitive element, generator roots, shortening = (255,239) with 51 zero bytes, corrects ≤ 8 byte errors, fails beyond |
| `src/energy.rs` `spec_fig_3_8_*`, `spec_3_5_sync_skipped_register_advances` | first PRBS bytes 0x03 0xF6 (hand-derived), period 2¹⁵ − 1, sync byte not XORed but register advances |
| `src/delay.rs` `delay_is_exact_slots` | FIFO delay in whole slots |
| `src/byte_interleave.rs` `spec_fig_3_9_*`, `pair_latency` | path delays 17·j, memory sizes, pair delay = 11 × 204 bytes |
| `src/conv.rs` `spec_fig_3_10_impulse_response`, `spec_table_3_8_*` | taps 171₈/133₈, Table 3-8 transmission order; Viterbi noiseless at all rates, chunked streaming, AWGN BER improvement |
| `src/conv.rs` `viterbi_throughput` (`#[ignore]`) | throughput measurement only: `cargo test --release -p isdbt-fec -- --ignored --nocapture viterbi_throughput` |
| `src/layer.rs` `tx_slot_provenance`, `one_frame_latency_small_layer`, `first_frame_has_no_long_runs` | the documented slot provenance inside TX frame k, the one-frame latency, the start-up priming |
| `src/remux.rs` `spec_fig_3_3_and_3_5_example`, `assemble_divide_roundtrip` | Fig. 3-5 configuration: n = 1152, A = 60, B = 504, frame starts `B B null` and ends with null; divide ∘ assemble = identity |
| `tests/remux_patterns.rs` | slot count = Table 3-5 and per-layer counts = Table 3-3 for every single-layer, every 1 + 12 partial-reception and a set of 3-layer configurations in all modes/GIs; Mode 2/3 sub-frame periodicity; independence from the read-instant tie rule; `print_reference_patterns` prints the Fig. 3-3/3-8 comparison (`--nocapture`) |
| `tests/props.rs` | LayerEncoder → LayerDecoder identity for every mode × modulation × rate and for full-size Mode 3 layers (3276 TSPs); mid-stream join with an unknown Viterbi state; recovery from 0.3 % sign flips + 3 % erasures; proptests for byte-interleave identity, RS correction ≤ 8, noiseless Viterbi, random layer round trips |

End-to-end, through `isdbt-phy` and `isdbt-ofdm`, the chain cross-decodes
byte-exactly with the gr-isdbt oracle in both directions in 8 parameter
sets ([VERIFICATION.md](https://github.com/a1678991/isdb-rs/blob/main/VERIFICATION.md)); this settled the PRBS
and puncturing reset points.

## Known limitations

- The multiplex-frame pattern is only partially confirmed: it reproduces
  the start and end of Fig. 3-3, but "#1151 = TSP_A" is not reproducible
  and Part 2 Fig. 3-8 is treated as schematic. Status in SPEC-NOTES:
  pending a real-world check against a broadcast's re-multiplexed TS.
- The 16 bytes after each TSP are always the RS(204,188) parity. The
  204-byte *broadcasting* TS of Part 2 §5.5 (8 dummy bytes of
  ISDB-T_information, 8 bytes of (204,196) parity, and the IIP) is
  `isdbt_phy::broadcast_ts`, not this crate.
- The encoder start-up content and the null packet bytes used in null
  slots (H.222.0 null, 0xFF payload) are implementation choices, not spec
  text.
- Viterbi decisions near each frame end are made without look-ahead into
  the next frame (traceback flushed per frame).

Spec interpretations, quoted text and confirmation status:
[SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-fec/SPEC-NOTES.md). Workspace overview: [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
