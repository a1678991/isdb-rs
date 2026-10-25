# isdbs-core — spec interpretation notes

Specs: ITU-R BO.1408-1 (04/2002), Annex 1 and its Appendix 1 (the system
using TC8PSK, i.e. ARIB STD-B20); MIC Ordinance No. 87 of 2011
(標準テレビジョン放送等のうちデジタル放送に関する送信の標準方式), revision of
2024-05-23, Chapter 5 §2 (Arts. 50–56) and annexed tables 45–51, read from
the e-Gov XML and its figures (`H23F11001000087-085` … `-095`). ARIB
STD-B20 itself (sold, Japanese only) and MIC Notification No. 304 of 2011
(the TMCC information layout, not online in full) are not available
(SPECS.md §1.2). "Lab recordings" are the satellite IF recordings of
2026-10-20 (9 BS and 12 110° CS transponders, VERIFICATION.md "ISDB-S
frame sync and TMCC").

## frame-layout: where the TMCC and the bursts sit in a frame
- Spec: BO.1408-1 §6.3, §7, §8, Fig. 12 and Table 3 give the parts and the
  totals (39 936 symbols per frame, 38 976 of main signal, a burst of 4
  symbols every 203) but leave the layout to "the system". Annexed table
  45 figure: "１９２シンボル" for W1 / "ＴＭＣＣ信号の一部" / W2 or W3, then the
  main signal with a burst after every "２０３シンボル"; annexed table 51
  figure: the same, "４シンボル" bursts; table 45 note 4: "各フレームの第１
  スロットから第１２スロットまでの各先頭１バイトは、フレーム同期信号及び電力
  拡散されたＴＭＣＣ信号で置き換える".
- Interpretation: frame = 192 header symbols (12 bytes W1 | 8 TMCC bytes |
  W2/W3 at rate 1/2 BPSK), then 192 × (203 main-signal symbols + 4 burst
  symbols). The first burst follows the first 203 main symbols; the frame
  ends with a burst.
- Confirmation: `frame::tests::spec_table_3_geometry` (the counts add up
  to Table 3); on the lab recordings the W1 tail correlates at header
  symbols 12–31 and the W2/W3 tail at 172–191 on every frame, and every
  burst symbol matches at the computed positions (next entry). Status:
  confirmed (live data).

## burst-data: the data the burst PRBS is added to
- Spec: BO.1408-1 §7: "The data for burst before modulation shall be
  randomized with an appropriate random sequence"; annexed table 51:
  "位相基準バースト信号用の９次の電力拡散信号（Ｘ９＋Ｘ４＋１、初期値：
  １１１１０１１０１）は … １フレームを周期として発生する … 電力拡散信号の発生
  及び加算処理は位相基準バースト信号期間のみ行う". What it is added to is
  not stated.
- Interpretation: the burst signal before randomisation is all zeros, so
  the burst symbols are BPSK (table 46: 0 → 0°) of the PRBS bits; the PRBS
  is restarted at every frame and clocked only on burst symbols, 768 bits
  per frame. Generator drawn like Fig. 3-8 of B31 / Fig. 5 of BO.1408
  (stage 1 = leftmost character of the initial value, output = the
  feedback node), taps at stages 4 and 9 (figure `-094`).
- Confirmation: on the lab recordings all 30 720 burst symbols of 40
  frames agree in sign with this sequence (BS-1 and ND2, after a
  W1-derived phase; 0 disagreements), and the burst-aided carrier recovery
  built on it gives the expected MER on every transponder. Status:
  confirmed (live data).

## coded-tails: the transmitted order of the inner code's two outputs
- Spec: annexed table 45 note 1: the coded W1, W2, W3 end in the 20 bits
  "ＥＣＤ２８ｈ … ０Ｂ６７７ｈ … Ｆ４９８８ｈ"; table 49 note 3: "二相位相変調に
  ついては内符号化後にＣ０、Ｃ１の順に送出する"; BO.1408-1 §9: "With regard to
  BPSK, the two encoded bits (P0 and P1) shall be transmitted in the order
  of P1 and P0" with P1 = X (171₈), P0 = Y (133₈) at rate 1/2 (Table 2).
- The names C0/C1 of table 49 do not say which generator each is; BO.1408
  says X first.
- Interpretation: BPSK sends X (171₈) then Y (133₈) for each input bit.
  The printed tails are reproduced exactly by this order (and not by Y
  then X) from any encoder state, since a word's first 6 bits flush the
  memory.
- Confirmation: `frame::tests::spec_w_words_and_coded_tails`; the TMCC of
  every lab recording decodes (RS-clean) with this order. Status:
  confirmed (spec vector and live data).

## tmcc-prbs: where the TMCC energy dispersal starts and which bytes it skips
- Spec: BO.1408-1 §6.4: "The pseudo-random sequence shall be initiated at
  the third byte (just after the sync word) of the first block. The first
  bit … applied to the first bit (i.e., MSB) of the third byte of the first
  block. The pseudo-random sequence shall be added to the data, except the
  timing reference words." Annexed table 50 別記 1: "１スーパーフレームを周期
  とし、先頭の２バイトを除き … Ｗ１、Ｗ２及びＷ３には加算は行わないが、擬似乱数
  符号系列の発生は継続する".
- Interpretation: one X¹⁵+X¹⁴+1 sequence per superframe, its first byte
  XORed onto the first TMCC byte of frame 1, clocked through every later
  header byte (96 per superframe after the first two), applied to the
  TMCC bytes only.
- Confirmation: `tmcc::tests::spec_tmcc_prbs_alignment`; RS(64,48)
  succeeds with 0 corrections on the strong lab transponders. Status:
  confirmed (live data).

## tmcc-layout: bit layout of the 384-bit TMCC information
- Spec: BO.1408-1 Fig. 7 (field order: order of change, modulation-code
  combination for each slot, relative TS ID for each slot, table relative
  TS ID → MPEG TS_ID, other information), Fig. 8 (CM entries of a 4-bit
  Table 1 word and "number of slots assigned"), Fig. 9 (relative TS IDs
  "sequentially from slot No. 1"), Fig. 10 (16-bit TS_IDs "from the
  relative TS ID number 0"), Appendix 1 Table 3 items 8, 9, 12, 13 (CM = 4,
  TM = 8; other information = order of change 5 bits, emergency flag 1 bit,
  uplink information 4 bits, reserved 62 bits; 384 bits in all). The
  binding layout, MIC Notification No. 304 of 2011 annexed table 2, is not
  available.
- Interpretation: MSB first, in Fig. 7 order; order of change (5) at the
  front as Fig. 7 draws it; slot counts 6 bits (the smallest width for
  0..48) and relative TS IDs 3 bits (0..7 for TM = 8); then emergency (1),
  uplink (4) and the 62 reserved bits. 5 + 4·10 + 48·3 + 8·16 + 1 + 4 + 62
  = 384.
- Confirmation: on all 21 lab transponders this reading gives slot counts
  summing to 48 in whole units of annexed table 48, combinations in
  descending efficiency (BS: TC8PSK 2/3 × 48; CS: QPSK 3/4 × 48),
  relative TS IDs in runs of equal slot counts, unused TS_ID entries FFFFh,
  TS_IDs that follow a per-transponder numbering, and identical
  information in every superframe. Another split of the same widths could
  not give all of these at once on every transponder. Status: confirmed
  (live data); the meaning of the individual fields beyond BO.1408 is not
  (next entries).

## tmcc-other: the "other information" field values
- Observed on the lab recordings: emergency flag 0 everywhere; uplink
  information 0010 on every BS transponder, 0100 on every CS one; of the
  62 "reserved" bits the first is 0 and the other 61 are 1 on every
  transponder.
- Interpretation: none. The fields are decoded and kept raw
  (`TmccInfo::{emergency, uplink, reserved}`); the 0 at the first reserved
  bit suggests a defined flag that BO.1408-1 does not describe. Status:
  pending (needs STD-B20 or Notification 304).

## dummy-slots: which slots of a unit are the dummy ones
- Spec: annexed table 48 別記 3 gives the unit and its valid / dummy slot
  counts; BO.1408-1 Fig. 4 shows the dummy slots after the valid ones
  ("one dummy slot is inserted for three active slots"), but only for a
  single unit at the end of the frame.
- Interpretation: within each unit the valid slots come first, then its
  dummy slots, unit after unit (`TmccInfo::slot_combinations`,
  `mainsig::sent_slots`). Dummy slots are not transmitted (§8) but count
  in the frame for the energy dispersal and the interleaver (they keep
  their slot positions in the 48 × 204-byte frame).
- Confirmation: on the 110° CS transponders (QPSK 3/4 × 48: 36 valid
  slots in units of 4) every slot decodes RS(204,188)-clean with this
  placement, whose PRBS bytes depend on the slot positions: ND2–ND20,
  ≈ 140 000 packets, 0 uncorrectable (VERIFICATION.md "ISDB-S main signal
  to TS"). A different placement would put the dispersal of a dummy
  position on a sent slot and fail every RS word there. Status: confirmed
  (live data).

## ts-unused: TS_ID value of unused table entries
- Spec: BO.1408-1 Fig. 10 lists TM entries without saying what an unused
  one holds.
- Observed: FFFFh in every unused entry of every lab transponder
  (`tmcc::UNUSED_TS_ID`). Status: observation (operational).

## main-dispersal: the main signal's energy dispersal
- Spec: BO.1408-1 §4, Fig. 5: "Loading of the sequence … shall be
  initiated at the second byte of every super-frame. The first bit of the
  output … shall be applied to the … MSB of the second byte of slot No. 1
  in frame No. 1. The PRBS shall be added to the data except to the first
  byte (MPEG sync byte) of every slot." Annexed table 48 別記 1:
  "各スーパーフレームの先頭の１バイトを除き … 周期内のＴＳ同期バイトには
  加算は行わないが、擬似乱数符号系列の発生は継続する".
- Interpretation: one X¹⁵+X¹⁴+1 sequence over the superframe's 8 × 48 ×
  204 bytes in slot order (dummy slots included), from the second byte;
  the 384 sync-byte positions get no dispersal but still clock the PRBS
  (`mainsig::dispersal`). Dispersal is applied before the interleaver
  (Fig. 1 order: energy dispersal, then interleaver), so the receiver
  removes it after de-interleaving, at the original byte positions.
- Confirmation: `mainsig::tests::spec_dispersal_alignment`; RS(204,188)
  succeeds on every packet of 19 lab transponders. Status: confirmed
  (live data).

## main-interleaver: read and write order of the inter-frame interleaver
- Spec: BO.1408-1 §5 and Fig. 6: "The data in the original frame is read
  out in the inter-frame direction, i.e., in the order of A1,1; A2,1;
  A3,1; …, where Ai,j represents the byte data at j-th slot in i-th
  frame, to form the interleaved frame. The data in the interleaved frame
  is read out in the byte direction"; Fig. 6 prints the first interleaved
  row as A1,1 A2,1 A3,1 … A3,26 and the second starting at A4,26. Annexed
  table 48 別記 2: "８×２０３バイトのブロックインターリーブ … スーパーフレーム
  方向で各フレームのスロット番号が同一のスロット間".
- The text's "j-th slot" conflicts with the figure: the figure's index
  runs over the 203 bytes of one slot (A1,1 … A1,203), and the annexed
  table interleaves the slots of the same number across the 8 frames.
- Interpretation: per slot number, an 8 × 203 matrix (row = frame, column
  = byte after the sync byte) read column by column and written row by
  row into the same slot of the 8 transmitted frames
  (`mainsig::interleave_position`); the sync byte is not interleaved.
- Confirmation: `mainsig::tests::spec_fig_6_interleaver` (the positions
  Fig. 6 prints); RS(204,188)-clean decoding of every lab transponder
  that has the SNR. Status: confirmed (spec figure and live data).

## qpsk-map: which QPSK axis carries P1
- Spec: annexed table 46 text: "四相位相変調 ０，０ （＋）４５度 ／ ０，１
  （－）４５度 ／ １，０ （＋）１３５度 ／ １，１ （－）１３５度" with
  "Ｐ１及びＰ０"; figure `-086` labels the points "[Ｐ１Ｐ０]" with 01 at
  +135° and 10 at −45°, which is the other axis assignment. BO.1408-1
  Fig. 15 b) labels "(P1, P0)" with 10 at upper left and 01 at lower
  right, as the text.
- Interpretation: the text and BO.1408: P1 = 0 ⇒ I > 0, P0 = 0 ⇒ Q > 0
  (`demod::qpsk_point`).
- Confirmation: only this reading decodes ND2 (288 of 288 packets
  RS-clean in the first superframe; the swapped axes and the mirrored
  spectrum give 0), `examples/mapprobe`. Status: confirmed (live data);
  figure `-086` as extracted disagrees.

## tc8psk-map: which inner-code output is C1
- Spec: annexed table 49 figure: B1 → C2 uncoded; B0 → the K = 7 encoder
  → "C1" (upper output) and "C0" (lower output) → puncturing; table 46:
  8PSK points labelled [C2 C1 C0]. BO.1408-1 Fig. 15 a) labels the same
  points "(W, Y, X)", i.e. C1 = Y (133₈) and C0 = X (171₈); Fig. 14 draws
  X on top.
- Interpretation: C1 = Y (133₈), C0 = X (171₈), as BO.1408-1 Fig. 15
  (`demod::Mapping::tc8psk_c1_is_y`, the default). For QPSK and BPSK the
  pair is (P1, P0) = (X, Y) as Table 2 says.
- Confirmation: BS3: 19 968 of 19 968 packets RS-clean with C1 = Y, 0 with
  C1 = X; BS1 the same (`examples/mapprobe`). Status: confirmed (live
  data).

## tc8psk-decoding: how the uncoded bit is decided
- Spec: BO.1408-1 §9 and annexed table 49 only define the transmitter.
- Design (not a spec reading): the 8 points of table 46 are set-partitioned
  by (C1, C0): for each pair the two values of C2 are antipodal. Viterbi
  runs on the coded bit with the branch metric max(|Re z·p̄|) over each
  subset (the likelier of its two points), and C2 is decided afterwards
  from the sign of Re z·p̄ against the subset of the decoded (C1, C0)
  (`demod::decode_frame`). Bytes are rebuilt MSB first, alternately B1
  (C2) and B0 (BO.1408-1 §9: "B1 and B0 are two successive bits of a
  byte data (B1 represents the higher order bit)").
- Confirmation: RS-clean decoding of every BS transponder (unit test at
  12 dB SNR: fewer than 0.5 % byte errors before RS). Status: design.

## inner-code-frames: one encoder through the frame, known at both ends
- Spec: BO.1408-1 §6.3: the W words' first 6 bits flush the encoder
  memory; Fig. 12 and Fig. 14: one encoder for TMCC and main signal.
- Interpretation: the encoder runs on from the header's W2/W3 into the
  main signal and on into the next frame's W1. The decoder starts in the
  state the W2/W3 word leaves (its last six bits) and ends on the next
  W1, which it knows (`isdb_fec::ViterbiDecoder::with_state`,
  `push_known`). Bursts are not coded (they are inserted after the inner
  coder, Fig. 1).
- Confirmation: RS-clean decoding of the last slot of every frame (the
  bytes the known end protects). Status: confirmed (live data).

## unassigned-slots: slots whose relative TS ID names an unused entry
- Observed: on four BS transponders some slots carry a relative TS ID
  whose TS_ID entry is FFFFh; all their packets are null packets (PID
  0x1FFF) and decode RS-clean.
- Interpretation: such slots belong to no TS; `isdb rx` counts them and
  writes no file for them. Status: observation (operational).
