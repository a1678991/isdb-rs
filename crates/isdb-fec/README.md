# isdb-fec

Channel-coding blocks that more than one ISDB system uses: the shortened
Reed–Solomon codes of the RS(255,239) mother code over GF(2⁸), the K = 7
convolutional code with generators 171₈ / 133₈ and its soft-decision
Viterbi decoder, and the energy-dispersal PRBS generators. The parts that
differ between systems stay in the system crates: puncturing, where a PRBS
restarts, byte interleaving and frame layout are in `isdbt-fec` (ISDB-T)
and `isdbs-core` (ISDB-S).

## Spec scope

| Module | ISDB-T: ARIB STD-B31 v2.2 | ISDB-S: ITU-R BO.1408-1, MIC Ordinance No. 87 of 2011 | Contents |
|---|---|---|---|
| `rs` | §3.3 | Annex 1 §2, App. 1 Table 3 item 10; annexed table 49 items 1–2 | `Rs::new(n)`: RS(n, n − 16), p(x) = 0x11D, g(x) = ∏(x + λⁱ), i = 0..15, λ = 02h; `RS_204_188` (TS packets), `RS_64_48` (ISDB-S TMCC); errors-only t = 8 decoder |
| `conv` | §3.8, Fig. 3-10 | Annex 1 §9, Fig. 13; annexed table 49 item 3 | `ConvEncoder`, `branch_output`, `ViterbiDecoder` (streaming, f32 LLRs) |
| `prbs` | §3.5, Fig. 3-8 | Annex 1 §4, §6.4; annexed tables 48, 50, 51 | `Prbs15` (X¹⁵ + X¹⁴ + 1, `100101010000000`), `Prbs9` (X⁹ + X⁴ + 1, `111101101`, the ISDB-S burst), `B44Prbs` (ISDB-S3 dispersals) |
| `bch` | — | — (ISDB-S3: ARIB STD-B44 v2.1 §3.4 (1), §3.5) | binary BCH over GF(2¹⁶), t = 12, any shortening |
| `ldpc` | — | — (ISDB-S3: B44 §3.4 (2), §3.5) | quasi-cyclic IRA LDPC from an address table: encoder, syndrome, floating-point layered normalised min-sum (the reference) |
| `ldpc::fixed`, `simd` | — | — | fixed-point layered offset min-sum, W codewords per SIMD lane group, bit-exact with its one-lane form (below) |

BO.1408-1 and B31 give the same RS field and generator (B31 writes the
factors as (x − λⁱ), the same in characteristic 2), the same mother code
and the same 15-bit PRBS. The ISDB-S annexed tables define the RS(64,48)
shortening as 191 prepended zero bytes, like the 51 of RS(204,188).

The readings of the generator figures (stage numbering, where the output
is taken, which end of the initial string is stage 1) are the ones
recorded in the `isdbt-fec` [SPEC-NOTES](../isdbt-fec/SPEC-NOTES.md); the
9-stage burst generator of annexed table 51 is drawn the same way.

## Fixed-point LDPC and SIMD

`ldpc::fixed::Decoder::<W>` decodes W codewords of one code at once, one
per lane: `i8` input LLRs (`quantise`: round(llr · scale), ±127), `i8`
check-to-variable messages and a-posteriori values, saturating; checks in
order; at each check, sign parity and the two smallest |L − r| (first edge
on ties), magnitudes minus `offset` (offset min-sum), floored at 0; after
each iteration the hard decisions are checked against every parity check
and a lane whose word is a codeword stops. `decode_all` streams any number
of codewords through the W lanes and refills a lane as soon as it stops,
so a slow or failing codeword does not hold the others.

The arithmetic is defined per lane, so a codeword decodes to the same
bits, in the same number of iterations, alone (W = 1, the `CpuScalar`
reference of plan §7.2) and in any lane of any W, on either back end of
`simd`:

| Back end | When | How |
|---|---|---|
| portable | any compiler (stable, the default) | branch-free loops over `[i8; W]` (mask blends, min/max, saturating ops), which LLVM vectorises for the target: SSE2 on plain x86-64, NEON on aarch64, AVX2 / AVX-512 when built for them, WASM SIMD |
| `std::simd` | the build script compiles a probe of the `std::simd` API the kernels use, i.e. a nightly whose `portable_simd` matches; `ISDB_FEC_NO_NIGHTLY_SIMD` turns it off | the same operations on `Simd<i8, W>` (`cfg(isdb_fec_portable_simd)`) |

There is no `unsafe` and no target-specific code. Runtime dispatch to AVX2
(`#[target_feature]`) would need `unsafe`, which the workspace denies; a
build with `RUSTFLAGS="-C target-cpu=native"` gets the wider instructions
instead.

Measured on ISDB-S3 16APSK 93/120 codewords of a live transponder
(`isdbs3-phy` `examples/ldpcspeed`, Ryzen 9 7950X, one thread, 16 lanes):

| Build | Codewords/s per thread |
|---|---|
| float reference (`Code::decode`) | 166 |
| fixed, one lane (scalar reference) | about 460 |
| fixed, 16 lanes, stable, x86-64 baseline (SSE2) | 4 678 |
| fixed, 16 lanes, stable, `-C target-cpu=x86-64-v3` (AVX2) | 5 739 |
| fixed, 16 lanes, nightly `std::simd`, baseline | 4 834 |
| fixed, 16 lanes, nightly `std::simd`, AVX2 | 6 153 |

16 lanes (one 128-bit vector of `i8`) is the fastest width on the
baseline and within 2 % of 32 with AVX2; AVX-512 adds nothing over AVX2
(the message memory, edges × W bytes, is the limit). On the live
recordings it decodes as well as the float reference: input scale 1,
offset 1 fails 5 of 9 216 near-threshold codewords where float fails 6,
at 2.59 against 2.53 mean iterations; scale 2 saturates the posteriors
and fails, offset 2 slows convergence (`examples/ldpcfixed`).

## Position in the stack

- Depends on: `thiserror` (build script: `rustc` only, for the nightly
  probe).
- Used by: `isdbt-fec` (re-exports `rs`, `conv` and the 15-bit PRBS under
  its own module paths, unchanged), `isdbs-core` and `isdbs-phy` (ISDB-S
  TMCC and main signal).

## Key types and entry points

| Item | Role |
|---|---|
| `Rs::new(n)`, `RS_204_188`, `RS_64_48` | `parity`, `encode_in_place`, `syndromes`, `decode` (corrected byte count or `Uncorrectable`) |
| `ConvEncoder::push` | one input bit → the mother-code pair (X, Y) |
| `ViterbiDecoder::{new, new_unknown_state, push, flush}` | mother-code LLR pairs in, bits out, `traceback` steps late |
| `Prbs15`, `Prbs9` | `next_bit` (and `next_byte`, MSB first, for `Prbs15`), `reset`, `state_string` |

## Tests

- `spec_primitive_element`, `spec_generator_roots`: λ = 2 has order 255;
  g(x) has exactly the roots λ⁰..λ¹⁵.
- `spec_shortening_equivalence`: for both RS(204,188) and RS(64,48), the
  shortened codeword with its zero prefix is a (255,239) codeword.
- `spec_correct_up_to_8`, `spec_failure_beyond_8`: both lengths.
- `spec_impulse_response`: 171₈ / 133₈ taps; `viterbi_rate_half_streaming`.
- `spec_prbs15_first_bits` (0x03, 0xF6), `spec_prbs9_first_bits`
  (0 1 0 0 0 0, derived by hand), both checked against an independent
  recurrence; `spec_periods` (2¹⁵ − 1, 2⁹ − 1).
- `tests/ldpc_lanes.rs`: `prop_every_lane_decodes_as_the_scalar_reference`
  (any mix of easy, hard and undecodable codewords in 16- and 32-lane
  batches), `prop_streamed_codewords_decode_as_the_scalar_reference`
  (more codewords than lanes, refilled), saturated and zero inputs, and
  lanes stopping at different iterations: each codeword's bits,
  iterations and convergence equal its one-lane decode. Both property
  tests fail on an injected lane-dependent fault and on stale messages in
  a refilled lane. Run on stable and nightly.
- The `isdbt-fec` suite (RS, Table 3-8 puncturing, Viterbi over AWGN,
  frame round trips, the gr-isdbt oracle) runs through this code.
