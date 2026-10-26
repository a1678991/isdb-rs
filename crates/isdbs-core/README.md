# isdbs-core

ISDB-S transmission parameters: the system of ITU-R BO.1408-1 Annex 1
(ARIB STD-B20) with the frame layout of MIC Ordinance No. 87 of 2011,
Chapter 5 §2 (annexed tables 45–51). No signal processing: geometry,
codes and the TMCC information, shared by the receiver (`isdbs-phy`) and
the planned modulator.

## Spec scope

| Module | Clause | Contents |
|---|---|---|
| `frame` | BO.1408-1 Table 3; annexed tables 45, 50, 51 | N = 48, M = 8, B = 203, 39 936 symbols per frame (192 header + 192 × (203 + 4)); W1/W2/W3 and their inner-coded 20-bit tails; symbol positions of main-signal and burst symbols |
| `combination` | Table 1, Table 2; annexed tables 48, 49 | the seven modulation-code combinations, their words, efficiency, slot units (with dummy slots) and puncturing (P1, P0) pairs |
| `slots` | §3.2, §8, Fig. 4 | each combination's run of slots and its range of main-signal symbols |
| `tmcc` | §6, Figs. 7–11, App. 1; annexed table 50 | the 384-bit TMCC information codec, validation, per-slot combination and TS_ID, and the TMCC signal of a superframe (RS(64,48), energy dispersal, sync words) |
| `burst` | §7; annexed table 51 | the 768 burst bits of a frame (X⁹ + X⁴ + 1 from `111101101`) |
| `mainsig` | §§2–5, §8, Figs. 5–6; annexed table 48 別記 1–3 | the slots sent (dummy slots dropped), the superframe energy dispersal, the depth-8 inter-frame interleaver, and a superframe's RS codewords from the inner decoder's bytes (and back) |

The RS code, the K = 7 code and the PRBS generators are `isdb-fec`.
Every reading that is not settled by the text is in
[SPEC-NOTES.md](SPEC-NOTES.md): the frame layout and the burst data
(confirmed on live signals), the bit order of the BPSK pair, the TMCC
energy dispersal and the TMCC information layout (confirmed on live
signals: the binding MIC Notification No. 304 of 2011 is not available),
the place of dummy slots in a unit, the main-signal energy dispersal and
interleaver alignment (confirmed by RS(204,188) on live signals), and the
meaning of the "other information" values (pending).

## Position in the stack

- Depends on: `isdb-fec`, `thiserror`.
- Used by: `isdbs-phy` (receiver), `isdb-cli` (`isdb iq tmcc`, `isdb rx`).

## Key types and entry points

| Item | Role |
|---|---|
| `frame::{FRAME_SYMBOLS, HEADER_SYMBOLS, BURSTS, …}`, `main_symbol_position`, `burst_symbol_position` | frame geometry |
| `frame::{W1, W2, W3, W1_CODED_TAIL, …}` | sync words |
| `Combination` (`word`, `from_word`, `slot_unit`, `efficiency`, `puncturing`) | the seven assigned words of Table 1 / table 48 |
| `CombinationWord::{Assigned(Combination), Dummy, Reserved(u8)}` (`from_word`, `word`) | all 16 words of Table 1, lossless; the `word` of a `tmcc::CombinationEntry` |
| `TmccInfo::{decode, encode, validate, slot_combinations, slot_ts_ids}` | the TMCC information |
| `tmcc::{headers, decode_blocks, derandomise}` | a superframe's 8 header blocks ↔ information |
| `slots::runs` | `Run { combination, slots, valid_slots, symbols }` per combination |
| `burst::frame_bits` | burst data |
| `mainsig::{sent_slots, dispersal, interleave, deinterleave, codewords, frame_bytes}` | main signal bytes of a superframe |

## Tests

- Spec vectors: `spec_table_3_geometry` (Table 3, equation (3), the
  layout adds up to 39 936), `spec_w_words_and_coded_tails` (W3 = ¬W2;
  the printed coded tails from any encoder state), `spec_table_1_words`,
  `spec_fig_8_combination_words` (all 16 words round-trip through
  `CombinationWord`: seven assigned, 1111 dummy, the rest reserved),
  `spec_table_48_slot_units` (units from E_k / E_max, 812-symbol slots),
  `spec_table_2_puncturing` (each kept bit sent once),
  `spec_tmcc_prbs_alignment`, `spec_table_51_burst_bits`,
  `spec_fig_4_examples` (runs fill the main signal),
  `spec_fig_6_interleaver` (the positions Fig. 6 prints),
  `spec_dispersal_alignment` (PRBS from the second byte, sync bytes
  skipped with the sequence running), `spec_sent_slots`.
- Live words: `live_words_decode_consistently` (a BS and a CS TMCC word
  received from the lab's satellite IF, RS-clean, round trip).
- Property tests (`tests/props.rs`): `prop_tmcc_info_lossless`,
  `prop_tmcc_signal_corrects_8_bytes`, `prop_slot_assignment_fills_the_frame`;
  in `mainsig`: `prop_interleave_round_trip`, `prop_superframe_round_trip`
  (RS-coded packets → dispersal, interleaving → back).
