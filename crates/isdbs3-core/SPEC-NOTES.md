# isdbs3-core — spec interpretation notes

Specs: ARIB STD-B44 v2.1-E1 (2016-03-25) "Transmission System for Advanced
Wide Band Digital Satellite Broadcasting (ISDB-S3)", and v2.0-E1 where
noted; ITU-R BO.2098-0 Annex 1 (System F); MIC Ordinance No. 87 of 2011,
Chapter 5 §3. MIC Notification No. 304 of 2011, annexed table 3 (the
binding TMCC layout B44 §3.11 reproduces) is not available in full
(SPECS.md §1.2). "Lab recordings" are the 61.44 Msps satellite IF
recordings of BS7, BS11 and BS17 of 2026-10-25 (DC block removed;
VERIFICATION.md "ISDB-S3 receiver").

## ldpc-tables: transcription of the parity-check address tables
- Spec: B44 §3.4 (2) "Appendix 1 Bit position of parity check", one table
  per rate (pp. 16–35 of 2.1-E1), with the worked example for 81/120.
- Transcription: `tools/b44-ldpc-tables.py` reads the `pdftotext -layout`
  extraction, keeps the lines made only of integers between one "In case
  of coding rate" heading and the next, and checks r rows for rate r/120,
  every position below n − k and no repeat within a row. The 2.0-E1 and
  2.1-E1 extractions give byte-identical tables.
- Confirmation: `ldpc::tests::spec_example_81_120_first_parity_addresses`
  (the worked example's i₀ row); the rate-61/120 code decodes the live
  TMCC of all three lab transponders on every frame (see "tmcc-*"); the
  93/120 code decodes their main signal (see "main-ldpc"). Status:
  confirmed (live data) for 61/120 and 93/120; the other nine rates are
  checked only by the structure tests.

## tmcc-rate-table: Table 3.11-2 prints 101/120 twice
- Spec: B44 2.1-E1 Table 3.11-2: "1001 101/120", "1010 101/120",
  "1011 109/120"; 105/120 is missing. SPECS.md §1.4 records a MIC
  amendment of Notification 304's annexed table 3 that inserted 93/120 at
  0111 (shifting 97/120 … 109/120 up by one) and added 16APSK.
- Interpretation: codes 0001 … 1011 are the eleven rates of Table 3.4-2 in
  order; 1010 = 105/120 (the 2.1-E1 entry is a typo). This is the
  amended numbering (0111 = 93/120); the pre-amendment one had 0111 =
  97/120.
- Confirmation: the live TMCC of BS7, BS11 and BS17 says modulation 0100
  (16APSK), rate 0111; the main signal decodes with the 93/120 LDPC code
  ("main-ldpc"), not 97/120. 1010 is not exercised. Status: confirmed
  (live data) for 0111 = 93/120; 1010 = 105/120 is an interpretation.

## dispersal-output: which stage the B44 PRBS outputs
- Spec: B44 Figs. 3.6-1, 3.6-2, 3.6-3: stages 1…25 (1…15), the XOR of the
  last stage and stage 22 (14) fed back into stage 1; the line to the
  "switch for addition control" leaves the **last stage** (25 / 15), not
  the feedback node as in B31 Fig. 3-8 and BO.1408 Fig. 5.
- Interpretation: output = the last stage's bit before the shift
  (`isdb_fec::prbs::B44Prbs`), for all three generators; initial strings
  read stage 1 first.
- Confirmation: after the TMCC LDPC converges, 80 readings were tried
  (four initial states, feedback or last-stage output, start offsets 0, 1,
  2, 8, 15, bit order reversed or not; `isdbs3-phy` example `tmccbch`):
  only the B44 TMCC state with last-stage output and no offset gives
  BCH-clean data, with 0 corrections, on every frame of the three
  transponders. The slot generator's reading is confirmed by the main
  signal (see "main-dispersal"). Status: confirmed (live data) for TMCC
  and slot; the pilot generator follows the same figure form
  (see "pilot").

## tmcc-bit-order: bit order of the TMCC information fields
- Spec: B44 §3.11 Figs. 3.11-1 to 3.11-9 give field order and widths, not
  bit order.
- Interpretation: fields in figure order, each MSB first; the 9 422 bits
  are the BCH message highest power first (§3.4 (1)).
- Confirmation: on the lab recordings every frame parses to a consistent
  allocation (one mode, 16APSK 93/120 × 120 slots, modes 2–8 = 1111 /
  1111 / 0 / 0 exactly as §3.11 (2) prescribes for unused modes), stream
  types 0x02 (TLV) for relative streams 0–2 and 0xFF elsewhere, packet
  formats all zero (§3.11 (4) for TLV), 40 slots per stream, extension
  identification 0 with the 3 598-bit field all ones (§3.11 (9)).
  Status: confirmed (live data).

## frame-marker: which synchronisation word marks the frame
- Spec: B44 Fig. 3.3-1: the odd modulation slots #1, #3, …, #119 start
  with a "frame synchronization signal" (slot #1 labelled FSync; slots #3
  and #119 are labelled "frame sync signa!", which the text extraction
  and the drawing leave ambiguous between a typo and "!FSync"); the note
  gives FSync = 52F866h, !FSync = AD0799h; even slots carry SSync =
  36715Ah.
- Interpretation: slot #1 of each frame carries FSync, the other odd slots
  !FSync. With the π/2-BPSK reference of §3.8 (bit 0 at +45° on odd
  symbols), the SSync slots give the carrier phase, and the odd slot whose
  FSync correlation has the opposite sign to the other 59 of its parity
  in a 120-slot period is slot #1.
- Confirmation: on the lab recordings exactly one odd slot in 120 has that
  sign, and the TMCC read from it as slot #1 decodes on every frame
  ("tmcc-*"); any other choice of slot #1 shifts the TMCC bits and fails
  LDPC. Status: confirmed (live data).

## interleaver-rows: 32APSK rows
- Spec: B44 Table 3.7-1 prints 9 876 rows for 32APSK; Figs. 3.7-5/6 print
  8 976.
- Interpretation: 8 976 = 44 880 / 5. Status: interpretation (32APSK is
  not on air in the lab recordings).

## interleaver-direction: "below 2/5" in Table 3.7-2
- Spec: B44 Table 3.7-2 columns "below 2/5, 1/2, 3/5, 2/3, 3/4, 7/9, over
  4/5"; 1/3 and 2/5 have no column of their own.
- Interpretation: "below 2/5" covers 41/120 (1/3) and 49/120 (2/5):
  backward read-out; 32APSK at 7/9 (93/120) is also backward. Status:
  interpretation for the low rates; 16APSK 93/120 forward is confirmed by
  the main-signal decode ("main-ldpc").

## codeword-placement: where each slot's codeword sits in the symbols
- Spec: B44 Fig. 3.3-1 numbers the "parts of main signal" 1 … 7 920
  continuously across modulation slots #1 … #120, 136 symbols each;
  Tables 3.2-2/3: valid and dummy slots per unit of five; Fig. 3.2-3.
- Interpretation: the frame's main symbols are one stream; each valid
  slot's interleaved codeword takes 44 880 / bits consecutive symbols of
  it, in slot order; dummy slots take none (a unit of five modulation
  slots holds exactly the unit's valid codewords:
  `params::tests::spec_modulation_main_symbols_fill_whole_codewords`).
  Valid slots come first in each unit (§3.2 (2)).
- Confirmation: 16APSK 93/120 × 120 on the lab recordings: all 96
  codewords of every whole frame decode at these positions (4 512 of 4 512
  over BS7, BS11, BS17; "main-ldpc"). Status: confirmed
  (live data) for one 16APSK mode; mixed modes and dummies in other
  modulations are interpretations.

## main-ldpc: the main signal's inner code
- Spec: B44 §3.4 (2), §3.7, §3.8.
- Confirmation: on the lab recordings (BS7 15 frames, BS11 16, BS17 16),
  every codeword of 16APSK 93/120 converges (mean 2.9–3.0 layered min-sum
  iterations, scale 0.8) with the Fig. 3.8-1 labels, forward read-out of
  the 4-column interleaver and the 93/120 table: 4 512 of 4 512. Status:
  confirmed (live data).

## main-dispersal: order of BCH and energy dispersal in a slot
- Spec: B44 Fig. 3.2-1: step 2 "Error correction Outer code addition",
  step 3 "Energy dispersal"; §3.6 (1): dispersal over header, data, BCH
  parity and stuffing; Table 3.2-1: the 6 stuffing bits follow the BCH
  parity.
- Interpretation: the BCH word is header + data (k_bch = 176 + D bits)
  with its 192 parity bits; the stuffing is outside it; the receiver
  removes the dispersal (one X²⁵ + X²² + 1 sequence per frame, running
  across the valid slots in order, last-stage output) before BCH.
- Confirmation: BCH-clean with 0 corrections on all 4 512 codewords above;
  BCH applied before removing the dispersal, or including the stuffing,
  fails on every slot. The decoded headers read 0000h FFFF… (Appendix 1:
  head-TLV indication, undefined area all ones) and the data starts with
  a TLV header byte 7Fh. Status: confirmed (live data).

## constellation-labels: 16APSK and 32APSK labels
- Spec: B44 Fig. 3.8-1 (drawing; the text extraction loses positions).
- Interpretation: read from the rendered page (2.1-E1 p. 44): 16APSK
  inner ring [1100] 45°, [1110] 135°, [1111] 225°, [1101] 315°; outer
  ring from 15° in 30° steps [0100] [0000] [1000] [1010] [0010] [0110]
  [0111] [0011] [1011] [1001] [0001] [0101]; 32APSK as in
  `constellation.rs`. QPSK [C1 C0] 00 45°, 10 135°, 11 225°, 01 315°;
  8PSK Gray from [001] at 0° counter-clockwise.
- Confirmation: 16APSK by the main-signal decode ("main-ldpc"). Status:
  confirmed (live data) for 16APSK; QPSK, 8PSK and 32APSK labels are
  interpretations (not on air in the lab recordings).

## pilot: pilot content and dispersal
- Spec: B44 §3.9: "signal points 0000, 0001, …1111 in that order twice for
  16APSK"; §3.6 (3): X¹⁵ + X¹⁴ + 1 from `100000000101100`, 0°/180°, clocked
  over pilot symbols only, one period per frame; Fig. 3.3-1: 32 pilot
  symbols after the 24 sync symbols of every modulation slot.
- Interpretation: pilot symbol t of a slot carries label t mod 2^bits of
  the slot's modulation; the PRBS is the B44 form (last-stage output),
  restarted every frame, 3 840 bits per frame.
- Confirmation (`isdbs3-phy` example `pilotcheck`, BS11, one frame):
  averaging the sign-corrected pilots per label gives the 16 labels on two
  rings (inner 0.43–0.45, outer 0.95–1.07 of the sync amplitude), each
  within 3.6° of its nominal angle; without the PRBS signs the averages
  fall to 0.04 of that size. Inner/outer radius ratio 2.51 against γ =
  2.87 for 93/120 (transponder compression). Demapping against the
  learned points raises the main-signal MER by 0.7 dB with every codeword
  still decoding. Status: confirmed (live data) for 16APSK.
