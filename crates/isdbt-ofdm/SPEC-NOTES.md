# isdbt-ofdm: spec interpretation notes

Interpretations of ARIB STD-B31 v2.2 (§3.9–3.16, Attachment Ch. 6) made
by this crate.  Quotes are from `specs/ARIB-STD-B31-v2.2.txt`; figure and
table readings were checked against the PDF page images (PDF page =
printed page + 10).  Status "confirmed" means confirmed by a second
statement of the spec itself (asserted in a test); "pending" means it
still needs a real-world capture or receiver check.

## mapping: bit order within a carrier symbol
- Spec: §3.9.3, Figs. 3-12/3-14/3-16/3-18 ("S/P", inputs "b0,b1,b2,…").
- Interpretation: the serial-to-parallel converter takes consecutive input
  bits as b0, b1, …, b(S−1) of one carrier symbol (b0 first).  As an
  integer label b0 is the MSB, so labels read like the figures' "(b0,b1,…)".
- Confirmation: confirmed for QPSK/16QAM/64QAM by the gr-isdbt oracle:
  byte-exact cross-decoding in both directions and carrier-identical
  modulator output for identical input (VERIFICATION.md).  gr-isdbt's own
  output (same bit order) was decoded by a MyGica S270 with PER 0 for QPSK
  and 64QAM over a conducted link.  DQPSK is not
  covered (gr-isdbt has no DQPSK).

## mapping: which branches are delayed (Fig. 3-18)
- Text: "the delay elements shown in Fig. 3-18 are inserted into b1 to b5
  for bit interleaving" (§3.9.3.4); the pdftotext layout of Fig. 3-18
  places the label "b0" next to the 24-bit element.
- Interpretation: b0 undelayed; b1..b5 delayed 24/48/72/96/120 bits.  16QAM:
  b1..b3 by 40/80/120; QPSK/DQPSK: b1 by 120.  Each branch carries one bit
  per carrier symbol, so an n-bit element is an n-symbol delay.
- Confirmation: confirmed — §3.9.2 "delays equivalent to 120 carrier
  symbols" and Table 3-9 (`(adjustment / S) + 120 = 2·nc·N`, i.e. exactly
  "2 OFDM symbols") hold for every modulation and N (test
  `spec_table_3_9_total_two_symbols`).

## mapping: delay adjustment (Table 3-9)
- Text: "corrected at the bit interleaving input side through the addition
  of the delay-adjustment value … such that the total transmission and
  reception delays are equal to 2 OFDM symbols."
- Interpretation: a pure delay of the layer's bit stream by X bits before
  the S/P.  X is a multiple of S, so it is implemented as X/S symbols added
  to each branch delay (bit-exact equivalent).  The spec does not define
  the initial delay-line content; the mapper fills its delay lines with
  pseudo-random bits (`CarrierMapper::with_seed`, one seed per layer), see
  "Start-up fill" in the latency section below.

## mapping: constellations (Figs. 3-15, 3-17, 3-19) and Table 3-11
- I carries b0 (and b2, b4), Q carries b1 (b3, b5): "I (level corresponding
  to b0, b2, b4)", "Q (Level corresponding to b1, b3, b5)".
- Per axis Gray code read from the figures: first axis bit = sign (0 → +);
  16QAM magnitude bit 0 → 3, 1 → 1; 64QAM magnitude bits 00 → 7, 01 → 5,
  11 → 3, 10 → 1.  Every label of every figure is asserted
  (`spec_fig_3_15_qpsk`, `spec_fig_3_17_16qam`, `spec_fig_3_19_64qam`).
- Normalisation Z/√2, Z/√2, Z/√10, Z/√42 ("√" lost in extraction, visible in
  the PDF); unit mean power asserted.

## mapping: π/4-shift DQPSK reference and recursion
- Text: "(Ij, Qj) and (Ij-1, Qj-1) represent the output symbol and the OFDM
  symbol immediately preceding the output symbol" (§3.9.3.1; Fig. 3-12
  feeds the output back through "Delay").
- Interpretation: the recursion runs per carrier-symbol position of the
  layer stream across consecutive OFDM symbols (delay = one OFDM symbol =
  nc·N carrier symbols).  Because time interleaving is a fixed per-position
  delay of whole OFDM symbols and frequency interleaving a fixed per-symbol
  permutation, this is the same as a per-on-air-carrier recursion across
  consecutive on-air symbols; the receiver therefore detects
  differentially per carrier right after the FFT (`d = y_j·y*_{j−1}`).
  Table 3-10 labels coincide with the QPSK labels of Fig. 3-15
  (00 → +π/4 = (+,+), 01 → −π/4, 10 → 3π/4, 11 → −3π/4), so the QPSK
  max-log demapper is applied to `d`.
- Initial reference: not specified; the modulator starts every position
  at a pseudo-random diagonal point (±1, ±1) (Fig. 3-13); the start-up fill
  of the time-interleaver lines / frame buffer ends in that point (see the
  latency section), so after modulator start-up every carrier's first mapped
  symbol has its reference on air.  Irrelevant in steady state.
- Confirmation: loopback (self-consistent); real-world pending.

## combine / time_interleave: index i of m_i and the delay adjustment
- Text: Fig. 3-23 "Provided that mi = (i × 5) mod 96", branches "0 … nc−1",
  "nc is 96, 192, and 384".
- Interpretation: `i` is the carrier-symbol index within the data segment
  (0..nc−1) in every mode; m_i repeats every 96.  TX delay of position i =
  A + I·m_i (A = Table 3-12 adjustment, "conducted on signals prior to time
  interleaving", merged into the same delay line); RX delay I·(95 − m_i).
- Confirmation: confirmed — Table 3-2 "I=4 (380 symbols)" = 95·I, Table
  3-12 `A + 95·I = frames·204` for every entry, Fig. 3-24 (Mode 1, I = 8:
  carrier 19 → 760, carrier 20 → 32) (tests `spec_table_3_12_frame_multiple`,
  `spec_fig_3_28b_randomize_direction`).

## freq_interleave: groups
- Text: "data-segment numbers 0 to 12 are assigned sequentially to the
  partial-reception portion, differential modulations …, and coherent
  modulation"; "Inter-segment interleaving is not conducted on the
  partial-reception portion".
- Interpretation: with the partial-reception flag set, data segment 0 forms
  its own group (only rotated and randomised) even if it is DQPSK; the
  remaining DQPSK segments form the differential group, the rest the
  coherent group; inter-segment interleaving runs within each group with
  n = group size (Fig. 3-26: output segment k position i ← S_{i·n+k}).

## freq_interleave: carrier rotation direction and segment number (AMBIGUOUS)
- Text: Fig. 3-27 maps the row "S'0,0,k S'1,0,k … S'95,0,k" (↓) to
  "S'(k mod 96),0,k S'(k+1 mod 96),0,k …"; "carrier rotation by segment
  number"; "S'i,j,k represents the carrier symbol of the kth segment
  following inter-segment interleaving".
- Direction: after rotation, position i holds S'_{(i+k) mod nc} (read
  top-to-bottom as drawn).
- Which k: Fig. 3-26 numbers the segments of a group 0..n−1, while "segment
  number" elsewhere in B31 means the data/OFDM segment number 0..12.  The
  two readings differ only for groups that do not start at data segment 0
  (e.g. the coherent group after a partial-reception segment).
- Interpretation: default `RotationNumbering::DataSegmentNumber` (k = data
  segment number 0..12, the literal "segment number");
  `RotationNumbering::GroupRelative` is available in `FrequencyInterleaver`,
  `Modulator::with_numbering` and `DemodConfig::rotation`.
- Confirmation: confirmed for the coherent group after a partial-reception
  segment: gr-isdbt's frequency interleaver rotates by the absolute data
  segment number (`rotated[(c - segment) mod nc] = derotated[c]`, segment
  = 0..12), the Rust chain cross-decodes byte-exactly with it, and with
  `GroupRelative` every layer-B/C RS word of the same gr-isdbt file fails
  (`oracle/rust_tx3` `rx_rot`).  gr-isdbt's partial-reception output was
  decoded by a MyGica S270 (layer B 64QAM, PER 0, conducted link), so a
  real receiver uses
  the data-segment number too.  Differential (DQPSK) groups not covered.

## freq_interleave: Table 3-13 direction
- Text: "These tables show which carriers are assigned, as a result of
  carrier randomizing, to carrier-rotated data arranged in ascending order
  of carrier number."
- Interpretation: the symbol at rotated position `before` goes to carrier
  `after` (`out[T[i]] = in[i]`).
- Confirmation: confirmed by Fig. 3-28 (b) (Mode 1, segment 0, I = 8,
  "Number of delayed symbols" vs carrier): the plot shows ≈515 at carrier 0,
  ≈270 at carrier 1 and 0 at carrier 80; with this direction the delays are
  8·m_32 = 512, 8·m_26 = 272 and 8·m_0 = 0 (the inverse direction would
  give 128, 336, 432).  Test `spec_fig_3_28b_randomize_direction`.
- Transcription: generated from the text extraction, every table asserted
  to be a permutation, 19 entries spot-checked against the PDF images.

## frame: carrier numbering, SP positions, data order
- Text: Fig. 3-30 (Mode 1): SP at carriers 0 and 12 in symbol 0, 3 in
  symbol 1, 6 in symbol 2, 9 in symbol 3, 0 in symbol 4; "inserted … once
  every 12 carriers … and once every 4 symbols".
- Interpretation: SP at carrier c of a coherent segment iff
  `c ≡ 3·(n mod 4) (mod 12)`, n = symbol number in the frame (0..203).
  Data symbols S_{i,n} fill the remaining carriers in ascending carrier
  order (Fig. 3-30 shows S0,0 at carrier 1 and S0,1 at carrier 0).  The
  figure has obvious typos ("S9,2 S9,2", "S2,4 S2,4", "S5,3 S5,3", "S5,201
  S5,202"); the regular pattern is used.  Differential segments: CP at
  carrier 0, data from carrier 1 (Fig. 3-29).
- TMCC/AC positions: Tables 3-14/3-15 columns are segment numbers in
  frequency order; their values never coincide with an SP position or each
  other (asserted: exactly nc data carriers in every segment, kind and SP
  phase).  AC1 positions of Tables 3-14 and 3-15 are equal (asserted, as
  §3.12.1 states).
- Coherent segments have no CP (Table 3-1); the pilot closing a coherent
  segment at its upper edge is the next segment's carrier 0 (SP every 4th
  symbol, or the CP of a differential segment) or the band-edge carrier
  ("The receiver uses this CP as the high-frequency end SP", §3.12.1).

## frame: pilot PRBS (Fig. 3-31, Tables 3-16/3-17/3-19)
- Text: "Initial Value … (Arranged in Ascending Order of Bits from Left to
  Right)"; Note: "Each of the initial values … matches the value obtained by
  setting all bits to an initial value of 1s and continuously generating all
  carriers in the entire band, starting with the leftmost carrier (carrier 0
  of segment 11)".
- Interpretation: registers D1..D11 printed left to right; output W = D11
  read before the shift; feedback D9 ⊕ D11 into D1.  W_i of carrier i of a
  segment is generated from that segment's Table 3-16 value.
- Confirmation: confirmed — for all 3 modes, running from all ones across
  the band reproduces all 13 initial values of Table 3-16, and the next
  output equals Table 3-19 for the band-edge carrier (Mode 1 W=1 → (−4/3,0);
  Modes 2/3 W=0 → (+4/3,0)).  Test `spec_table_3_16_prbs_initial_values`.
- Amplitudes: SP/CP/edge BPSK W=1 → −4/3, W=0 → +4/3 (Table 3-17).

## frame: TMCC / AC DBPSK
- Text: "B'0 = Wi (reference for differential modulation); B'k = B'k-1 ⊕
  Bk"; "signal points (+4/3, 0) and (-4/3, 0) for information 0 and 1".
- Interpretation: on each TMCC/AC carrier, symbol 0 carries pilot_value(W_k)
  of that carrier; symbols 1..203 carry B'n.  B0 of Table 3-20/3-32 is not
  information.  Every frame restarts at B'0 = W.

## frame / ofdm: spectrum arrangement, IFFT bin, scaling
- Text: Fig. 3-32 order 11, 9, 7, 5, 3, 1, 0, 2, 4, 6, 8, 10, 12 in
  ascending frequency; §3.14.1 "k: Carrier number … 0 assigned to carrier 0
  of segment 11", Ψ = exp(j2π (k−Kc)/Tu (t − Tg − nTs)), Kc = 702/1404/2808.
- Interpretation: band carrier k = position·C + c (+ the edge carrier
  13·C); baseband frequency (k − Kc)/Tu, i.e. IFFT bin (k − Kc) mod N (no
  spectral inversion; Kc at DC).  The guard interval is the last Ng IFFT
  samples (§3.14.2; equals Ψ's (t − Tg) phase reference).
- Scaling (not specified by B31): x = g/√K · IFFT_unnormalised(c), so a
  spectrum of unit-power carriers has unit mean sample power; the actual
  mean is (1/K)Σ|c|², slightly above 1 because of the 4/3 pilots.  The
  receiver FFT multiplies by √K/N so carriers come back at their spec
  values.

## tmcc: bit order, sync alternation, next information
- Fields are MSB first in ascending B order (Table 3-28 "B28–B30 … 001 QPSK"
  with B28 leftmost).
- Sync: Table 3-21 frame numbers are "for convenience"; the modulator sends
  w0 in its frame 0, 2, 4 … and w1 in odd frames.  Receivers accept either.
- Segment type B17–B19 is per segment (111 differential / 000 coherent), so
  TMCC carriers of different segment kinds differ in these 3 bits only.
- `TmccInfo::from_params` sets next information = current information (no
  switching announced), countdown 1111, system 00, emergency 0, B107–B121
  all 1.  ("the content of this flag is set to '1' if there is no next
  information" is available as `TmccParams::NONE`.)

## tmcc: Table 3-30 code '100' and the unused-layer test
- Spec: ARIB STD-B31 v2.2 §3.15.6.5–3.15.6.8, Tables 3-28..3-31.
- Text: Table 3-30 lists "100 — Not used", "101–110 — Reserved", "111 —
  Unused hierarchical layer"; §3.15.6.7 adds that "100" is "assigned to
  the use of digital terrestrial sound broadcasting system and are not
  used in this system". Tables 3-28/3-29/3-31 have only "Reserved" and
  "Unused hierarchical layer" rows. Each clause says the field is all
  ones "for an unused hierarchical layer, or when there is no next
  information".
- Interpretation: '100' of Table 3-30 is `InterleaveCode::Reserved` carrying 0b100
  (one variant for every code this system does not assign, so the TMCC
  word stays lossless; the sound-broadcasting meaning is documented on
  the variant, not modelled). A layer is "unused" when all four fields are
  all ones (`TmccLayer::UNUSED`, `is_unused`); `TmccLayer::config` decides
  on the modulation field alone ('111' → `None`), so a word with '111'
  modulation and a non-'111' other field is treated as unused, while a
  '111' rate / interleave / segment code under an assigned modulation is
  an error. The tables do not say which of these mixed words a receiver
  should accept.
- Confirmation: the two broadcast words of `tests/broadcast.rs` decode to
  the named variants (`spec_table_3_28_…`–`spec_table_3_31_…`,
  `broadcast_words_decode_to_the_broadcast_configuration`); the mixed
  words have not been seen off air (status: pending real-world check).

## dsc: difference-set code bit order (AMBIGUOUS)
- Text: "B20 to B121 … are error-correction coded by means of the shortened
  code (184,102) of the difference-set cyclic code (273,191)", g(x) given.
- Interpretation: systematic, first transmitted information bit (B20) =
  highest-degree coefficient, parity = m(x)·x^82 mod g(x) sent highest
  degree first (B122 = x^81) — the conventional serial divider, as drawn for
  the AC CRC in Fig. 3-35.  The reversed bit order would also give a valid
  (reciprocal) cyclic code, so this cannot be settled from the text.
- Self-checks: g(x) divides x^273 + 1; the 17-element Singer difference set
  of PG(2,16) gives parity checks orthogonal to g(x) (a constant, re-derived
  by the Singer construction in the tests),
  majority-logic decoding corrects 8 errors (tests).
- Confirmation: agrees with the independent gr-isdbt implementation
  (polynomial division with B20 as the highest-degree coefficient, parity
  B122 = x^81): the Rust receiver reports `ParityStatus::Ok` on every
  gr-isdbt TMCC frame and gr-isdbt's syndrome check passes on every Rust
  frame after its acquisition, for 8 parameter sets; in the 6
  partial-reception sets the two sides' information words differ
  (gr-isdbt's B67, VERIFICATION.md D3) and both parities still check, so
  the two encoders agree on the code.  Confirmed against broadcasters:
  every TMCC frame of seven stations (via a cable feed) passes the parity
  check as received, and one station's word equals the Rust encoder's
  (`tests/broadcast.rs`, VERIFICATION.md "Broadcast reception").  Whether
  consumer receivers check the parity is unknown.

## ac: content, order and seismic word
- No information: "information 1 is inserted as stuffing bits" (§3.13.4) →
  all AC bits 1 (which also reads as configuration identification 111 =
  "additional information on the transmission control").
- Serial AC data (Attachment §6.2.3.2/§6.3.4.2): "mapped sequentially,
  regardless of AC1 or AC2, to AC carriers in the order of lower to higher
  frequencies … then … the next symbol"; symbol 0 not used.
- Seismic warning (§3.16.6): "transmitted by the AC carriers of the segment
  No. 0 … must be the same in all AC carriers within the segment No. 0" →
  every AC1 and AC2 carrier of segment 0 carries the same 204-bit word.
  B1–B16 = the TMCC sync word of the same frame (config. id 001/110 + 13-bit
  sync, §3.16.4/§3.16.6.1).  CRC (Fig. 3-35, registers 0) over B21–B111,
  remainder MSB first in B112–B121.  Parity: shortened (187,105) code over
  B17–B121 with the same bit-order convention as TMCC (pending, as above).
- Page "0" area bits are kept as a 56-bit integer with B56 as MSB.

## latency: frame alignment of the bit-interleaving delay (Part 2, Figs. 3-8/3-9)
- Spec: ARIB STD-B31 v2.2 Part 2, Fig. 3-8 "Time Interleaving and Delay
  Adjustment (Example of Processing on the Transmitting Side)" and Fig. 3-9
  (receiving side), PDF pages 114–115.
- Text (Fig. 3-8, row "Combining of hierarchical layers"): "(Formation of
  OFDM symbol by 1-frame buffer; S-1,B,n is a symbol made up of B'''-2,12 to
  B'''-1,11.) (Two-symbol delay is caused by bit interleaving.  Therefore,
  frame synchronization is delayed by two symbols.)"; the bracket "OFDM-frame
  formation" starts at S-1,A,3 / S-1,B,3.  Fig. 3-9: "Bit de-interleaving
  (start bit delayed by 120 bits)", after which the first symbol of the OFDM
  frame is S''-9,A,1 / S''-5,B,1.
- Interpretation: the OFDM frame does not start with bit-interleaver output
  symbol 1 but with symbol 3, i.e. data-frame symbols 1–2 (after the
  2-symbol bit-interleave delay of §3.9.2) go on air as the last two symbols
  of the previous OFDM frame.  The modulator advances each layer's
  bit-interleaved carrier-symbol stream by 2 OFDM symbols with a one-frame
  buffer (`ModulatorOptions::bit_interleave_frame_alignment`, default on),
  so after the receiver's bit deinterleaver data-frame symbol `s` coincides
  with OFDM-frame symbol `s` (plus F_l whole frames of time interleaving).
  Consequences: the modulator emits on-air frame k in the call receiving
  input frame k+1 (its first output is on-air frame −1); LLRs of on-air
  frame m belong to input frame m − L with L = max_l F_l; the receiver's
  per-layer alignment FIFO is 204·(L − F_l) symbols (see `src/latency.rs`).
- Confirmation: figure-level (the figures are examples for layer A DQPSK
  1/2 5 seg I=16 and layer B 64QAM 7/8 8 seg I=8); asserted by
  `prop_bit_frame_alignment`.  gr-isdbt's TX does the same 2-symbol advance
  (a `skiphead` of 2 data symbols after its frequency interleaver, in its
  own examples/tx_demo.grc and in the bench scripts); its output was decoded
  by a MyGica S270 with PER 0, and the Rust receiver/transmitter
  cross-decode byte-exactly with gr-isdbt in Modes 1-3, all GIs, all
  rates and I lengths (VERIFICATION.md).  Status: confirmed (receiver
  evidence via the oracle, and a broadcast decoded RS-clean in both
  layers, VERIFICATION.md "Broadcast reception").
- Start-up fill (not in the spec): the mapper's bit-delay lines start with
  pseudo-random bits and its DQPSK references at pseudo-random diagonal
  points (±1, ±1) (one seed per layer); the time-interleaver lines and the
  one-frame buffer start with a pseudo-random sequence of constellation
  points (DQPSK: a π/4-shift random walk ending at that position's start-up
  reference, so its first mapped symbol has its reference on air).  A fill
  that is identical across carriers (zero bits → the same constellation
  point everywhere, or a common DQPSK reference) adds up coherently in the
  IFFT: the first frames had ≈ 40 dB PAPR and clipped in 8-bit DACs.  Test
  `papr_first_frames` asserts < 15 dB for the first 3 frames (measured
  10.5–12.3 dB).  Note that all-zero *coded bits* (bypassing energy
  dispersal) still give 30–38 dB in steady state; real coded bits are
  energy-dispersed (§3.5).

## oracle (gr-isdbt) deviations
Summary of where the gr-isdbt oracle (commit `56b2556`) departs from
B31 v2.2; full evidence, quotes and run matrix in
[VERIFICATION.md](../../VERIFICATION.md#discrepancies).  In every case the
Rust implementation follows the B31 text and no Rust code was changed.
- D1. TMCC segment type B17–B19: gr-isdbt sends 111 (differential) on every
  segment; Rust sends 000 on coherent and 111 on differential segments
  (`tmcc::segment_type_bits`, §3.15.5).  See "tmcc" above.
- D2. AC carriers: gr-isdbt leaves them unmodulated (pilot value W_i in
  every symbol, i.e. information 0); Rust sends DBPSK with B'0 = W_i and
  stuffing 1 (`AcFrame::stuffing`, §3.13.4).  See "ac" above.
- D3. TMCC B67 (next-information partial-reception flag): gr-isdbt sends 0
  while B27 = 1; Rust repeats the current information (B67 = B27,
  §3.15.6.4 / Table 3-22).  Both parities still check, see "dsc" above.
- D4. Time-interleave delay adjustment for I = 0: gr-isdbt adds a whole
  frame (204 symbols); Rust uses Table 3-12 (0 for I = 0,
  `time_interleave_adjustment_symbols`).  Only inter-layer timing of the
  reproduced TS is affected.
- D5 (oracle limitation, not exercised): a gr-isdbt typo writes layer C's
  next-information I = 0 code at B87 instead of B100.
- D6 (oracle limitation): gr-isdbt receiver start-up artefacts (first
  output packet unusable; one "TMCC NOT OK" on the first Rust frame in one
  run).  See
  [VERIFICATION.md](../../VERIFICATION.md#oracle-limitations-observed-not-disagreements).
