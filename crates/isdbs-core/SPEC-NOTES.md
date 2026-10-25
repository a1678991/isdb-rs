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
  dummy slots, unit after unit (`TmccInfo::slot_combinations`).
- Confirmation: none yet; it matters only when the TS packets are
  demultiplexed (Phase 3: on the CS transponders, QPSK 3/4 units of 4
  slots, every fourth slot would be a dummy). Status: pending (Phase 3,
  by RS success per slot).

## ts-unused: TS_ID value of unused table entries
- Spec: BO.1408-1 Fig. 10 lists TM entries without saying what an unused
  one holds.
- Observed: FFFFh in every unused entry of every lab transponder
  (`tmcc::UNUSED_TS_ID`). Status: observation (operational).
