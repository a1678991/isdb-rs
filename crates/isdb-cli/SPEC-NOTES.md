# isdb-cli: spec interpretation notes

Interpretations made by the `isdb` binary itself, in the format of the
workspace `SPEC-NOTES.md`. Most of `isdb-cli` wires the spec crates
together and has nothing to interpret; the entries here are about how
`tx --system isdbs` times a TS file in its slots (`src/pace.rs`,
`src/txs.rs`; design in `docs/plans/isdbs-next/pcr-pacing.md`). Clause
numbers of H.222.0 are those of the 05/2006 edition.

## isdb-cli: ISDB-S TS timing model (uniform per-TS slot grid)
- Spec: ITU-R BO.1408-1 Table 3 and MIC Ordinance No. 87 Art. 52(3)
  (28.860 Mbaud); BO.1408-1 Annex 1 (superframe of 8 frames × 39 936
  symbols, slots per frame); H.222.0 §2.4.2.2 eq. 2-4 (byte arrival time
  between PCRs).
- Text: the Ordinance gives the symbol rate as "毎秒二八・八六〇メガボー";
  eq. 2-4: "t(i) = PCR(i″)/system_clock_frequency + (i − i″)/transport_rate(i)".
- Interpretation: neither BO.1408-1 nor B20 (as available) says at what
  time a receiver hands out the packets of one TS_ID; the depth-8
  inter-frame interleaver spreads every packet of a superframe over all 8
  frames, so a receiver must buffer a superframe and re-time its output.
  `isdb` takes the output of a TS with N_r sent slots per frame to be
  uniform: packet k at k · 1 382 400 / (37 · N_r) ticks of 27 MHz
  (`SlotClock::isdbs`; a superframe is 319 488 · 27e6 / 28.86e6 =
  11 059 200 / 37 ticks exactly), i.e. 52 170 000 bit/s for 48 slots, not
  the slots' true symbol times (which bunch a TS's packets into its part
  of each frame). Every sent slot carries one packet per frame whatever
  its modulation, so the rate is 1 086 875 bit/s per sent slot.
- Confirmation: the BS transponders decoded by `isdb rx` (packet index =
  slot order) fit a constant packet rate within 21–68 ns
  (VERIFICATION.md "ISDB-S main signal to TS"), including BS1, whose three
  TSs each take a contiguous third of every frame; the 110° CS TSs deviate
  11–16 µs from such a fit, which is open (pcr-pacing.md §10 Q2: their
  PCRs may follow the true slot times). (status: confirmed for BS; pending
  for CS.)

## isdb-cli: PCR restamping when a TS file is placed in slots
- Spec: H.222.0 Annex D.0.4; §2.4.2.2 eq. 2-1..2-3; §2.4.3.5.
- Text: "correction of PCRs is necessary in a re-multiplex operation,
  creating a new Transport Stream from one or more Transport Streams. This
  correction is accomplished by adding a correction term to the PCR"; the
  correction is ΔPCR = del_act − del_const.
- Interpretation: placing a TS on the slot grid is such a re-multiplex.
  Each packet's due time comes from its own PCRs by eq. 2-4 (`PcrClock`,
  reference PID = the first PID carrying a PCR, or the PID the start-up
  probe found); it goes into the first free slot at or after that time
  (the late strategy, input order never changed), and every PCR field (any
  PID) gets Δ = round(S(j) − due) added modulo 2^33 · 300, so del_act is
  the time the packet was moved and del_const = 0. Δ is a duration, so it
  is right for every program's time base in the TS. OPCR is not corrected
  (it is the source's clock); input null packets are dropped and the slots
  left over carry new ones; no other byte changes. A PCR discontinuity
  (§2.4.3.5) is bridged by the clock at the previous rate and the PCR,
  indicator included, passes through with Δ. `--ts-pacing arrival` keeps
  the byte-exact relay of every packet instead.
- Confirmation: `pace.rs` properties (placement error in [0, one slot),
  restamped PCRs on the slot grid within 2 ticks); a 3 s CBR file through
  `TsInputs` → `isdb_analyze` validate with no `pcr.*` finding above info,
  `pcr.rate` 52.17 Mbit/s within 1 ppm and a CBR-fit residual under 40 ns
  (`txs.rs` test `placed_ts_of_a_cbr_file_validates`), and the same through
  `isdb tx --iq` → `isdb rx` byte-exact against `--placed-ts`. (status:
  confirmed offline; pending a real receiver's PCR recovery on the bench.)

## isdb-cli: loop seams of a looped TS file (`--feed loop`)
- Spec: H.222.0 §2.4.3.3 (continuity_counter), §2.4.3.5
  (discontinuity_indicator), §2.4.3.7 (PTS/DTS), Annex D.0.4.
- Text: §2.4.3.3 "The continuity_counter shall not be incremented when the
  adaptation_field_control of the packet equals '00' or '10'".
- Interpretation (planned, piece P3 of pcr-pacing.md, §7.6): a seam is not
  marked with a discontinuity; each pass n is shifted by a constant O_n
  (27 MHz, a multiple of 300) on PCR, PTS and DTS, and the continuity
  counters are renumbered per PID so that they run on across the seam, as
  `isdbt-mux` does for media looping and for the `ts-program` relay
  ([isdbt-mux SPEC-NOTES "Media looping"](../isdbt-mux/SPEC-NOTES.md#media-looping),
  ["Rebroadcasting a recorded program"](../isdbt-mux/SPEC-NOTES.md#rebroadcasting-a-recorded-program-kind--ts-program-srcrelayrs)).
  Until then, a paced looped file keeps its seams as before: the PCR (after
  restamping), PTS/DTS and continuity counters jump back with the file at
  each rewind; the pacer's clock bridges the backward PCR step as a
  discontinuity, so the slot timing stays continuous.
- Confirmation: none yet (status: pending, P3).
