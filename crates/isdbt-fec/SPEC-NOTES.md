# isdbt-fec — spec interpretation notes

Spec: ARIB STD-B31 v2.2 (E1). "Part 2" below means the reference part
(Chapter 3 of Part 2, PDF pp. 105–115, "Channel-coding scheme for
hierarchical transmission" etc.). Figures were read from the PDF pages
because `pdftotext` garbles them.

## remux: model receiver timing (multiplex-frame pattern)
- Spec: §3.2.1, §3.2.2.1, §3.2.2.2, Figs. 3-3, 3-4, 3-5, Tables 3-5, 3-6.
- Text: "The arrangement of transmission TSPs within a multiplex frame is
  determined in advance to ensure that it is identical to that of the TSs
  that will be reproduced by the model receiver shown in Fig. 3-4." /
  "In Modes 2 and 3, switching of S3 and S4 is performed at 1/2 OFDM-frame
  intervals (102 OFDM-symbol intervals) and 1/4 OFDM-frame intervals (51
  OFDM-symbol intervals), respectively."
- Paraphrase of the rest of §3.2.2: clocks are FFT sampling clocks; in
  each OFDM symbol the data carriers of layer A, then B (then C) are input
  one per clock, followed by a null signal for the remaining carriers
  (Fig. 3-5 example: 480 + 768 carriers, then 1056 null). Layer X stores
  B_X,k = 2·([k·S_X·R_X] − [(k−1)·S_X·R_X]) bits for its k-th carrier.
  S1 moves to another layer buffer once one TS packet has been input
  (408 bytes: a 204-byte TSP at the mother code rate 1/2, the spec's
  footnote), transfer being instantaneous. The TS
  reproduction section looks at the selected TS buffer every TS packet
  period (408 clocks) and reads one packet out if a full one is there,
  else sends a null packet. In Mode 1, S3 switches at the start of the
  OFDM frame and S4 follows three TS packet periods (408 × 3 clocks) later.
- Interpretation (implemented literally in `MultiplexPattern::new`):
  1. Symbol = `fft_size + guard` clocks; the data carriers of segments in
     ascending data-segment order (layer A, B, C) occupy the first clocks,
     one carrier per clock, then the null signal. (Fig. 3-5 puts the null
     signal at the end of the symbol period.)
  2. k counts layer-X data carriers from the start of the multiplex frame;
     cumulative stored bits = 2·[k·S·R]; a TSP is complete when the total
     reaches a multiple of 3264 bits (408 bytes). A datum input during clock
     c is complete at time c + 1.
  3. The completed TSP goes to the TS buffer of the section S3 selects at
     that moment (one S3 interval = 204/scale symbols). Packets of all
     layers share one FIFO per section in completion order.
  4. "More data than the size of one TS packet" is read as "at least one
     complete TS packet" (a strict ">" would leave the last packet of every
     interval unread, contradicting the fixed per-frame TSP counts).
  5. Multiplex-frame slot j = the read at 1224 + 408·j clocks after the
     model receiver's frame input began ("that is, at the beginning of an
     OFDM frame": the S4 switch defines the multiplex-frame start). Only
     the section selected by S4 is read; the other one holds its packets.
  6. The simulation asserts that every packet produced in an S3 interval
     is read before S4 leaves that section, and that the per-layer counts
     equal Table 3-3 × N and the slot count equals Table 3-5. This holds
     for every configuration in the test sweep (all modes/GIs, all
     single-layer, all 1+12 partial-reception, and a set of 3-layer splits).
  7. The tie rule of item 2 (c + 1 vs c) changes no pattern in the sweep.
- Confirmation:
  - Fig. 3-3 (Mode 1, GI 1/8; layers not stated, assumed to be the
    Fig. 3-5 configuration DQPSK 1/2 × 5 + 64QAM 7/8 × 8): figure shows
    "#1 TSP_B, #2 TSP_B, #3 TSP_null … #1151 TSP_A, #1152 TSP_null". The
    simulation gives `BB...BB...BBB..ABB...` at the start (matches #1–#3)
    and `…BB...ABBB...` at the end (#1152 null matches; #1151 is null, not
    A). With FIFO order the last packet of the frame is B504 (completed
    after A60), so "#1151 = TSP_A" is not reproducible; the figure is read
    as schematic. Status: partially confirmed.
  - Part 2 Fig. 3-8 (same configuration) shows the multiplexer output
    `B B B B N N N B B B N N A B … B B N N N A B B B`. No single read
    phase reproduces both ends: in a scan of the phase offset the start
    matches only for offsets ≈ 2272–2600 clocks (≈ one symbol), the end only
    for ≈ −200…+210 clocks. The figure is therefore schematic (its
    elisions do not keep the timing), and it is not used as a test vector.
  - Mode 2/3 patterns repeat every n/2 resp. n/4 slots (each S3 interval
    carries an integral number of each layer's TSPs), consistent with
    Part 2 §3.2.4 Figs. 3-4/3-5 ("Same pattern").
  - Status: pending real-world check (compare against the layer sequence
    of a live broadcast's re-multiplexed TS, if a tuner exposing the null
    slots / IIP TSP_counter is available).

## remux: null TSP content in the reproduced TS
- Spec: §3.2.2.2 "transmits a null packet"; §3.2.1 "16-byte null data".
- Interpretation: B31 does not fix the bytes of the null packet; the
  reproduced TS uses the H.222.0 null packet (PID 0x1FFF, payload only,
  0xFF payload). Status: convention.

## layer: one-byte shift of the frame (slot structure)
- Spec: §3.4 "containing all bytes from the byte next to the TS
  synchronization byte to the next synchronization byte … Note also that
  OFDM-frame synchronization shifts by one byte, to the beginning of the
  information bytes." Fig. 3-7. Part 2 §3.3: "Switching between
  hierarchical layers is performed every 204 bytes (starting with the byte
  next to the TS synchronization byte (47H))."
- Interpretation: a layer frame is T slots of 204 bytes; slot m = bytes
  1..203 of transmission TSP m followed by a sync byte. Strictly that sync
  byte is TSP m+1's; since sync bytes are 0x47 and never dispersed, TSP m's
  own sync byte is used (identical bytes). On RX the slot's last byte
  becomes the TSP's first byte before RS decoding. Status: confirmed by
  Fig. 3-7 and Part 2 §3.3 text.

## energy: PRBS register bit order and output tap
- Spec: §3.5, Fig. 3-8.
- Text: "The initial value of the register in the PRBS-generating circuit
  must be “100101010000000” (arranged in ascending order of bits, from left
  to right)". Fig. 3-8 (PDF p. 34): stages D1…D15 left to right; the
  outputs of D14 and D15 go to an XOR whose output loops back to the input
  of D1; "output" is tapped at that loop node, before D1.
- Interpretation: stage i holds the i-th character (D1 = '1', D2 = '0',
  D3 = '0', D4 = '1', D5 = '0', D6 = '1', D7 = '0', D8 = '1', D9…D15 = '0').
  Each clock: out = D14 ⊕ D15; shift D_i → D_{i+1}; D1 ← out. Equivalent
  recurrence: o_t = o_{t−14} ⊕ o_{t−15} with o_{−i} = D_i initially.
  Hand derivation of the first 16 bits: o0 = D14⊕D15 = 0, o1 = D13⊕D14 = 0,
  o2..o5 = 0, o6 = D8⊕D9 = 1, o7 = D7⊕D8 = 1, o8 = D6⊕D7 = 1, o9 = D5⊕D6 = 1,
  o10 = D4⊕D5 = 1, o11 = D3⊕D4 = 1, o12 = D2⊕D3 = 0, o13 = D1⊕D2 = 1,
  o14 = o0⊕D1 = 1, o15 = o1⊕o0 = 0 → bytes 0x03, 0xF6 (MSB first).
  Period 2¹⁵ − 1 verified. Status: confirmed (circuit and independent
  recurrence agree; period is maximal).

## energy: frame alignment and position in the chain
- Spec: §3.5 "this value must be initialized every OFDM frame. At this
  time, the beginning of an OFDM frame must be the MSB of the byte next to
  the transmission TSP's synchronization byte. Note also that the shift
  register must also perform shifting during the synchronization byte."
  Fig. 3-2 block order per layer: "Byte → Bits, Energy dispersal, Delay
  adjustment, Bits → Byte, Byte interleaving, Byte → Bits, Convolutional
  coding". (Part 2 Fig. 3-6 names one box "Delay adjustment, energy
  dispersal, byte interleaving", order not significant there.)
- Interpretation: dispersal is applied to the layer frame *before* the
  delay adjustment, PRBS reset at slot 0 byte 0 (MSB first), advancing 8
  clocks per byte including the sync byte (slot offset 203), which is not
  XORed. On RX the stream after byte deinterleaving is exactly the
  pre-delay TX stream one frame later (see "latency"), so the RX resets the
  PRBS at each RX frame start. Status: confirmed by Fig. 3-2 order and by
  the gr-isdbt oracle (its energy descrambler resets at the OFDM-frame
  tag; byte-exact cross-decoding both ways in 8 parameter sets, and
  gr-isdbt's output with the same reset point was decoded by a MyGica S270
  with PER 0) — see the workspace `VERIFICATION.md`.

## delay: Table 3-7 adjustment
- Spec: §3.6, Table 3-7 ("12 × N − 11" … "252 × N − 11").
- Interpretation: a FIFO of (Table 3-7 value) slots of the layer's own
  stream (null TSPs of other layers do not count), TX side only. Table
  value = T − 11 for every row (tested for all rows, all N, all modes).
  Filler for the initial FIFO content: see "layer: encoder start-up
  content" below.
- Confirmation: Part 2 Fig. 3-8 (DQPSK 1/2 × 5: T = 60, delay 49; 64QAM
  7/8 × 8: T = 504, delay 493) shows B_{0,1} replaced by B_{−1,12} and
  A_{0,1} by A_{−1,12}, B_{0,504} by B_{0,11}: exactly a T − 11 slot delay
  of the layer's own packets. Status: confirmed.

## byte_interleave: path-0 alignment
- Spec: §3.7 "the byte next to the synchronization byte must pass through
  a reference path that causes no delay"; Fig. 3-9 (path j: 17·j bytes).
- Interpretation: path = slot byte offset mod 12, path 0 at slot byte 0
  (the byte after the sync byte). The sync byte (offset 203, 203 mod 12 =
  11) takes path 11 (11 slots of delay) and still lands on a sync offset.
  The commutator runs continuously (204·T is a multiple of 12, so it is at
  path 0 at every frame start without reset). Deinterleaver path j holds
  17·(11 − j) bytes. Status: confirmed by the text; Part 2 Fig. 3-9 "Byte
  de-interleaving (start byte delayed by 11 packets for the same
  hierarchical layer)".

## conv: Fig. 3-10 taps and encoder state
- Spec: §3.8, Fig. 3-10 (PDF p. 37).
- Reading: X adder inputs: data input, outputs of D1, D2, D3, D6; Y adder
  inputs: data input, outputs of D2, D3, D5, D6. With the undelayed input
  as the MSB of the 7-bit mask this is 171₈ = 1111001₂ and 133₈ =
  1011011₂, consistent with the stated G1/G2. Impulse response test:
  X = 1111001, Y = 1011011.
- Encoder state: not stated; the encoder starts at zero and is never
  reset (only the puncturing pattern is reset, §3.8).

## conv: puncturing transmission order and reset point
- Spec: §3.8 "the puncturing pattern must be reset such that the patterns
  shown in Table 3-8 are initiated by frame synchronization", Table 3-8.
- Interpretation: the transmission sequence is taken verbatim from Table
  3-8 (it equals "Xᵢ then Yᵢ for each kept i"). The pattern restarts with
  the first information bit of the layer frame, i.e. the MSB of slot 0 byte
  0 after byte interleaving (the byte after a sync byte, §3.4 shift). For
  every legal layer the frame's information bit count 1632·T is a multiple
  of the puncturing period, so the reset never truncates a period.
  Part 2 Fig. 3-8: "Convolutional coding (puncturing reset by frame
  synchronization)" on the same frame-aligned TSP sequence. Status:
  confirmed.

## layer: latency / alignment contract
- Spec: §3.6 "such that all delays, including transmission and reception
  delays caused by byte interleaving (11 transmission TSPs), are one frame
  in length."
- Interpretation: delay adjustment (T − 11 slots) + interleaver (path j:
  j slots) + deinterleaver (11 − j slots) = T slots = one frame for every
  byte. `LayerDecoder::decode_frame` call k returns the TSPs of
  `LayerEncoder::encode_frame` call k − 1, slot-aligned; call 0 returns
  warm-up packets (the encoder's priming null frame, see below). The Viterbi decoder (`ViterbiDecoder`, src/conv.rs) runs
  continuously across frames: its path metrics are kept from one frame to
  the next (the encoder is not terminated), and at every frame end
  `LayerDecoder::decode_frame` calls `ViterbiDecoder::flush`, which traces
  back from the currently best state and releases all buffered decisions,
  so no frame of latency is added (the last `traceback` bits of each frame
  are decided without look-ahead into the next frame). TX frame k carries, at byte b of slot m (path j = b mod 12),
  pre-delay byte b of slot m + 11 − j of frame k − 1 (or slot m + 11 − j −
  T of frame k) — tested. Confirmation: Part 2 Figs. 3-8/3-9 show the same
  frame-aligned one-frame relation (RX "Byte de-interleaving … delayed by
  11 packets" restoring B_{n,1} at frame start). Status: confirmed.

## layer: encoder start-up content (unspecified implementation choice)
- Spec: B31 does not say what the delay-adjustment FIFO (§3.6) and the
  byte-interleaver path memories (§3.7, Fig. 3-9) contain when a
  transmitter starts; it only defines the steady-state processing.
- Problem: zero-filled memories are never energy-dispersed, so a fresh
  encoder's first frame began with a ~39 000-bit run of zero coded bits,
  giving ~37 dB PAPR peaks in the first on-air frames.
- Choice: `LayerEncoder::new` runs one virtual frame of T null
  transmission TSPs (H.222.0 null packet, PID 0x1FFF, 0xFF payload, plus
  RS(204,188) parity) through energy dispersal, the delay line and the
  byte interleaver, and discards the output (the convolutional encoder is
  not run and stays in the zero state). One frame suffices: the delay line
  (T − 11 slots) is refilled from that frame's last slots, and the
  interleaver paths (≤ 11 slots deep) from the delay line's output in its
  last 11 slots, which are themselves virtual-frame slots 0..10.
- Consequence: the first coded frame is statistically like any other
  (test: no run of equal coded bits longer than 64 in the first two frames
  for five configurations); the one-frame latency is unchanged, with "TX
  frame −1" = the null frame. RX call 0 of a fresh decoder returns the null
  transmission TSPs in slots 11..T−1 and corrupt slots 0..10 (zeroed
  deinterleaver memory) — still warm-up, not data.
- Status: implementation choice, not spec-derived.
