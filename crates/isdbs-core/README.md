# isdbs-core

ISDB-S transmission parameters: the system of ITU-R BO.1408-1 Annex 1
(ARIB STD-B20) with the frame layout of MIC Ordinance No. 87 of 2011,
Chapter 5 §2 (annexed tables 45–51). No signal processing: geometry,
codes and the TMCC information, shared by the receiver and the
transmitter of `isdbs-phy`.

## Spec scope

| Module | Clause | Contents |
|---|---|---|
| `frame` | BO.1408-1 Table 3; annexed tables 45, 50, 51; Ordinance Art. 52 ¶3 | N = 48, M = 8, B = 203, 39 936 symbols per frame (192 header + 192 × (203 + 4)), 319 488 per superframe; W1/W2/W3 and their inner-coded 20-bit tails; symbol positions of main-signal and burst symbols; 28.860 Mbaud and 27 MHz ticks per symbol (450/481 exactly) |
| `combination` | Table 1, Table 2; annexed tables 48, 49 | the seven modulation-code combinations, their words, efficiency, slot units (with dummy slots) and puncturing (P1, P0) pairs |
| `slots` | §3.2, §8, Figs. 4, 9; Table 2 | each combination's run of slots and its range of main-signal symbols; the symbol at which packet k of relative TS r (and any byte of it) starts in the superframe |
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
interleaver alignment (confirmed by RS(204,188) on live signals), the
meaning of the "other information" values (pending), and the on-air time
of a packet (the symbol its first bit is sent in, before interleaving).

## Position in the stack

- Depends on: `isdb-fec`, `thiserror`.
- Used by: `isdbs-phy` (receiver and transmitter), `isdb-cli` (`isdb iq
  tmcc`, `isdb rx`; `TmccInfo::assign` for `isdb tx` / `isdb loopback
  --system isdbs`).

## Key types and entry points

| Item | Role |
|---|---|
| `frame::{FRAME_SYMBOLS, HEADER_SYMBOLS, BURSTS, …}`, `main_symbol_position`, `burst_symbol_position` | frame geometry |
| `frame::{W1, W2, W3, W1_CODED_TAIL, …}` | sync words |
| `Combination` (`word`, `from_word`, `slot_unit`, `efficiency`, `puncturing`) | the seven assigned words of Table 1 / table 48 |
| `CombinationWord::{Assigned(Combination), Dummy, Reserved(ReservedCode)}` (`from_word`, `word`) | all 16 words of Table 1, lossless; a `Reserved` word can only be decoded (private constructor), so `==` is wire equality; the `word` of a `tmcc::CombinationEntry` |
| `TmccInfo::{decode, encode, validate, slot_combinations, slot_ts_ids}` | the TMCC information |
| `TmccInfo::assign` | the TMCC information of a slot layout: (combination, slots) in any order and (TS_ID, sent slots) per relative TS; the other fields at their observed values |
| `tmcc::{headers, decode_blocks, derandomise}` | a superframe's 8 header blocks ↔ information |
| `slots::runs` | `Run { combination, slots, valid_slots, symbols }` per combination |
| `slots::{packet_symbol, packet_byte_symbol, packet_slot, ts_slots, slot_byte_symbol, bit_symbol}` | on-air position of packet k of relative TS r: its (frame, slot) and the superframe symbol its first byte (or byte b, e.g. 9 for the PCR) starts at, before the inter-frame interleaver |
| `frame::{SUPERFRAME_SYMBOLS, SYMBOL_RATE, TICKS_PER_SYMBOL_NUM, TICKS_PER_SYMBOL_DEN, symbols_to_ticks}` | superframe length and the exact symbol → 27 MHz tick conversion |
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
  `spec_superframe_time` (319 488 symbols = 11 059 200/37 ticks at 28.86
  Mbaud),
  `spec_fig_6_interleaver` (the positions Fig. 6 prints),
  `spec_dispersal_alignment` (PRBS from the second byte, sync bytes
  skipped with the sequence running), `spec_sent_slots`.
- Live words: `live_words_decode_consistently` (a BS and a CS TMCC word
  received from the lab's satellite IF, RS-clean, round trip);
  `assign_reproduces_the_live_words` (`TmccInfo::assign` of the BS layout,
  TC8PSK 2/3 × 48 with three TSs of 16 slots, and of the CS layout, QPSK
  3/4 × 48 with one TS of 36, is the received word but for the order of
  change / the CS uplink value).
- Builder: `assign_mixed_layout` (TC8PSK 2/3 × 44 + BPSK 1/2 × 4 given
  least efficient first: sorted, slots shared in slot order, dummy and
  FFFFh entries, encode/decode and header round trips), `assign_errors`
  (every error variant).
- Packet slot time (`slots`): `bench_layout_slot_symbols` (TC8PSK 2/3 ×
  48: packet 48 f + s at 39 936 f + 192 + 828 s, 6.65 µs to 0.14 µs after
  the uniform grid), `slot_lengths_per_combination` (812, 1 082⅔, 1 624,
  3 248 symbols per valid slot; a unit fills 812 · u), and
  `packets_of_mixed_layouts`; against the transmitter in
  `isdbs-phy/tests/slot_time.rs` (ten layouts: each packet in the slot
  `packet_slot` names, and flipping the first bit of a slot's byte changes
  the transmitted symbols first where `slot_byte_symbol` says).
- Property tests (`tests/props.rs`): `prop_tmcc_info_lossless`,
  `prop_tmcc_signal_corrects_8_bytes`, `prop_slot_assignment_fills_the_frame`,
  `prop_assign_matches_slot_tables` (any layout and TS split: valid,
  lossless, each TS's slots in slot order, `None` on the dummy slots);
  in `mainsig`: `prop_interleave_round_trip`, `prop_superframe_round_trip`
  (RS-coded packets → dispersal, interleaving → back).
