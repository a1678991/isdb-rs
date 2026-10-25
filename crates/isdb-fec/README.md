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
| `prbs` | §3.5, Fig. 3-8 | Annex 1 §4, §6.4; annexed tables 48, 50, 51 | `Prbs15` (X¹⁵ + X¹⁴ + 1, `100101010000000`), `Prbs9` (X⁹ + X⁴ + 1, `111101101`, the ISDB-S burst) |

BO.1408-1 and B31 give the same RS field and generator (B31 writes the
factors as (x − λⁱ), the same in characteristic 2), the same mother code
and the same 15-bit PRBS. The ISDB-S annexed tables define the RS(64,48)
shortening as 191 prepended zero bytes, like the 51 of RS(204,188).

The readings of the generator figures (stage numbering, where the output
is taken, which end of the initial string is stage 1) are the ones
recorded in the `isdbt-fec` [SPEC-NOTES](../isdbt-fec/SPEC-NOTES.md); the
9-stage burst generator of annexed table 51 is drawn the same way.

## Position in the stack

- Depends on: `thiserror`.
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
- The `isdbt-fec` suite (RS, Table 3-8 puncturing, Viterbi over AWGN,
  frame round trips, the gr-isdbt oracle) runs through this code.
