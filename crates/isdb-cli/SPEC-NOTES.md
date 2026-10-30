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
  (discontinuity_indicator), §2.4.3.7 (PTS/DTS, eq. 2-11/2-12), §2.4.2.2
  eq. 2-1..2-3, Annex D.0.4.
- Text: §2.4.3.3 "The continuity_counter shall not be incremented when the
  adaptation_field_control of the packet equals '00' or '10'"; "may be
  discontinuous when the discontinuity_indicator is set to '1'".
- Interpretation: a looped file is one endless TS, so a seam is not marked
  with a discontinuity_indicator (that would need a new time base and
  receivers to reset); instead each pass n ≥ 1 is moved onto the time line
  of the passes before it, as `isdbt-mux` does for media looping and for
  the `ts-program` relay
  ([isdbt-mux SPEC-NOTES "Media looping"](../isdbt-mux/SPEC-NOTES.md#media-looping),
  ["Rebroadcasting a recorded program"](../isdbt-mux/SPEC-NOTES.md#rebroadcasting-a-recorded-program-kind--ts-program-srcrelayrs)),
  but packet by packet (`src/pace.rs`, "Loop seams"):
  - the reader thread tags each packet with its pass (the reopenings so
    far). The pacer's `PcrClock` is fed the raw packets and bridges the
    backward PCR step at the seam at the rate of the previous PCR interval
    (§2.4.3.5 handling, so the due times run on; the bridge is not reported
    as a discontinuity). The seam gap is therefore the file's own CBR
    continuation, one PCR interval on the bench TS;
  - at the first PCR of the reference PID in pass n, with bridged time t and
    raw value R, the pass offset is O_n = round₃₀₀(C + t − R) mod 2^33·300,
    where C = PCR_out − S(j) is the output's time base so far (the first
    PCR's value, or the base in force after a discontinuity of the file
    itself, so that the seam continues whatever time base the output is
    on). O_n is a multiple of 300 so that PTS/DTS move by exactly O_n/300
    90 kHz ticks and keep their distance to the PCR; its rounding
    e_n = O_n − (C + t − R), |e_n| ≤ 150 ticks, is added to the pass's due
    times, which keeps every PCR on the slot grid (PCR_out = C + S(j));
  - each placed packet of pass n: PCR_out = PCR_in + O_n + Δ (every PCR PID:
    O_n is a duration, the file period, the same for every program); the
    PTS and DTS of a PES header that starts in the packet + O_n/300 mod 2^33
    (Table 2-21 prefixes and markers kept; a header the packet cuts before
    its PTS/DTS is counted as `pes_truncated` and left alone); the
    continuity_counter + c mod 16, c fixed per PID at its first packet in
    the pass so that this packet continues the last one sent: one more than
    the last payload packet's counter if it carries a payload, equal to it
    if it is adaptation-only. A constant c keeps every relation inside the
    pass (increments, AF-only repeats, a duplicate pair). The plan
    (pcr-pacing.md §7.6) wrote "last_out + 1 if the last output packet had a
    payload": that misnumbers a pass that starts with an adaptation-only
    packet after a payload one; the rule above is §2.4.3.3's;
  - pass 0, and every packet of `--feed file`, has O_0 = 0, e_0 = 0, c = 0:
    no field but the PCR is modified. A PCR discontinuity inside a pass
    (indicator set) passes with Δ (and the pass's O_n); OPCR is not moved.
  Not covered: SI times (TOT/TDT, EIT p/f) restart with each pass, and the
  content at the seam is the file's (a PES the file cuts stays cut, and the
  PTS step across the seam is what the file's end leaves: on a 4 s mux,
  132 ms of video and 601 ms of audio not yet sent at its end).
- Confirmation: `pace.rs` `loop_seam_is_seamless` (a CBR file with a PES PID
  carrying the PCR, an AF-only PCR PID, PSI and a one-packet PID, looped 3×
  through `PacketStream::spawn` with `Feed::Loop`: every PCR exactly on the
  slot grid of its time base, PTS within one 90 kHz tick of the endless
  file's, 0 CC events, `isdb_analyze` without `transport.*` / `pes.*` /
  `pcr.*` finding above info, 0 discontinuities, CBR-fit max under 40 ns,
  pass 0 byte-identical but for the PCR) and the CC unit tests;
  `FEED=loop tools/isdbs-pacing-e2e.sh DIR 4` (the bench service muxed for
  4 s, 3.14 passes through `tx --iq` → `rx`, with and without `MEDIA=1`):
  0 CC errors, 0 PCR discontinuities, CBR-fit max 18–19 ns rms 11 ns
  (13–17 ns at the seams), no PES time stamp going back, no byte changed
  outside PCR/PTS/DTS/CC (VERIFICATION.md "ISDB-S loop seams, offline").
  (status: confirmed offline; pending a real receiver on the bench.)
