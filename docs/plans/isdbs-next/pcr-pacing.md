# PCR-paced placement in the ISDB-S producer (isdbs-next.md work item 1): investigation and design

Investigation only, 2026-10-30. Repository `/home/a1678991/dev/isdb-rs` at `371e72b`, clean.
Nothing transmitted; the board was not touched. Offline measurements use a `bench60.ts`
regenerated in the scratchpad by the same steps as `tools/isdbs-bench-tx.sh` (the bench
WORKDIR's copy was not found under `~`, `/tmp` or `/var/tmp`) and a placement model
(`pacing_sim.py`, §6) whose output was checked with the repository's own
`isdb ts inspect` / `validate`.

## 0. Summary

- **The ISDB-T `Pacer` is frame-granular and cannot be reused as it is.** It puts every packet
  due before the end of OFDM frame *k* into frame *k* from the layer's first TSP onwards, then
  pads with nulls (`input.rs:264-311`, `pad` at `:56-62`). Inside a frame the packets are
  front-loaded, so a PCR can go out up to about one frame early (231 ms in mode 3). Applied
  per ISDB-S superframe, the same rule gives **5.5 ms max / 3.2 ms rms** of PCR error on the
  bench TS and PAT/PMT intervals up to 0.104 s (§6). That is better than today's 12–30 ms rms
  but far outside ±500 ns. Its `PcrClock` (`isdbt-mux/src/pcrclock.rs`) is generic and is the
  part worth reusing: eq. 2-4 interpolation, wrap, discontinuity bridging and `Anchor` for
  PTS/DTS.
- **Placement at slot resolution is necessary but not enough.** One TS packet slot lasts
  28.83 µs at 48 slots/frame. Putting each packet in the first slot at or after its due time
  leaves a uniform quantisation error: **14.3 µs max / 8.3 µs rms** (§6), more than "a few
  µs".
- **PCR restamping makes it exact.** Add ΔPCR = (slot time − due time) to each placed PCR
  (H.222.0 Annex D.0.4, "correction of PCRs is necessary in a re-multiplex operation",
  ΔPCR = del_act − del_const). With that, the residual is **18 ns max / 11 ns rms**, the
  PCR-implied rate is **52.170000 Mbit/s** and `isdb ts validate` shows no timing finding
  (§6).
- **Time grid: uniform per TS.** Packet *k* of relative TS *r* is at k·T_sf/(8·N_r), not at its
  symbol time on air. The depth-8 byte interleaver spreads every packet of a superframe over
  all 8 frames (`mainsig.rs:94-97`), so a receiver has to buffer one superframe and re-time
  its output anyway. Real BS transponders already fit this grid within 21–68 ns
  (VERIFICATION.md "ISDB-S main signal to TS"), including BS1, whose three TSs each take a
  contiguous third of every frame.
- **CPU and FPGA paths stay identical by construction.** Placement happens in
  `TsInputs::take` (`txs.rs:509`). Both `Modulate` variants then encode the same `SfPackets`
  (`txs.rs:826-835` → `:657-669`).
- **Loop seam: shift, do not mark.** Shift PCR, PTS and DTS by a constant per pass and
  renumber the continuity counters per PID, as the mux's `ts-program` relay does
  (`relay.rs:45-64`). On `bench60.ts`, 9 of the 11 PIDs would otherwise have a CC error at
  each seam (§4). The seam has to be tagged by the reader thread (`source.rs:148-164`):
  today it is only a counter.
- **Validation gap.** `validate` checks the PCR interval, PCR accuracy (global CBR fit,
  > 500 ns = warning) and section repetition, but **not the PCR-implied rate against the
  on-air rate**. Today's `--feed file` path passes validate cleanly even though it plays the
  29.96 Mbit/s TS at 52.17 Mbit/s, 1.74× too fast (§5, §6). Acceptance needs that check.
- Proposed split (§9): **P1** retime helpers in `isdb-ts` → **P2** `SlotPacer` core
  (`isdb-cli/src/pace.rs`) → **P3** loop seam (pass tag + per-pass shift + CC) → **P4**
  `txs.rs` integration with file/loop default, probe, `--placed-ts` dump, SPEC-NOTES → **P5**
  live/FIFO pacing → **P6** validation tooling and the bench script. P1, P2 and P6 can run in
  parallel; the bench acceptance needs P1–P4 and P6 (bench switched to `--feed loop` on the
  file, no `pv`/FIFO).

## 1. The ISDB-T `Pacer` (`--mux-ts`)

### 1.1 Inputs

- **Time base:** `PcrClock<Packet>::new(None)` (`input.rs:208`) locks onto the first PID
  carrying a PCR (`pcrclock.rs:159`, `pid.get_or_insert`); other PIDs' PCRs are then ignored
  (test `other_pcr_pids_are_ignored_once_locked`). There is no PMT lookup, unlike the
  analyzer's `timebase` (`isdb-analyze/src/inspect.rs:272-279`, first program's PCR_PID).
- **Rate estimation:** none of its own and no CBR fit. Each packet's time is H.222.0 eq. 2-4
  between consecutive PCRs of the reference PID (`pcrclock.rs:158-221`), so the rate is the
  local rate of each PCR interval (`ticks_per_packet = d/n`, `:190`). Packets before the
  first PCR get t = 0 (`:171-182`). After the last PCR (at `finish`), or with more than
  200 000 pending packets (`MAX_PENDING`, `:43`), packets are extrapolated at the last
  interval's rate (`:223-240`).
- **Rate check (regular files only):** `probe_rates` (`input.rs:419-502`) reads up to 30 s or
  128 MB (`PROBE_SECONDS`/`PROBE_BYTES`, `:719-722`) and computes the mean and 1-s peak per
  layer, nulls excluded. `check_input_rates` (`:732-793`) refuses mean > layer capacity unless
  `--allow-overrate`, warns when the 1-s peak exceeds it, and returns `false` (no pacing)
  when no PCR is found. The capacity comes from `capacity::layer_bps(params, …)`, so this
  check is tied to ISDB-T.

### 1.2 Mapping PCR time to frames

- `frame_ticks = params.frame_ticks_27mhz()` (`input.rs:209`; an exact integer,
  `isdbt-core/src/params.rs:576-580`). Frame *k* spans [k·F, (k+1)·F) from the first PCR
  (t = 0 ↔ start of the first data frame).
- `Pacer::take` (`input.rs:255-312`) pops timed packets in input order and drops nulls
  (`:286-289`). A packet with t ≥ limit (`:293`) is carried to a later frame. If the packet's
  layer is already full (`:297-301`), it is carried too and `saturated` is incremented.
  Otherwise it is **appended to the layer's vector**. `take_frame` pads each layer to its
  count with nulls at the end (`:327`, `pad` `:56-62`).
- **Granularity: one OFDM frame.** No packet is placed at a time inside the frame. Due
  packets take the layer's first TSPs and nulls follow, so the error is up to about
  (1 − fill) × T_frame early. The tests only assert per-frame counts
  (`mux_split_is_paced_by_pcr`, `:1101-1128`: 49–51 of 100 per frame). The `lag` status
  (`:303`, frame_start − t) measures lateness at frame resolution only. (By reading, not
  measured on ISDB-T. The same rule per ISDB-S superframe is measured in §6.)

### 1.3 Edge behaviour

| Case | Behaviour | Where |
|---|---|---|
| Overrun (layer full) | packet waits for the next frame, input order kept, lag grows; every packet sent | `input.rs:297-301`, test `paced_split_over_capacity_lags` `:1131-1156` |
| Underrun (nothing due) | null padding | `:327` |
| Input nulls | dropped, counted (`dropped_nulls`) | `:286-289` |
| PCR discontinuity (indicator, backwards, or step > 0.7 s) | time line continued at the previous interval's rate; counted, shown as `pcr-discontinuities` | `pcrclock.rs:187-201`, `MAX_PCR_GAP` `:37`; `input.rs:393-396` |
| PCR wrap (2^33·300) | unwrapped (`pcr_wrap_diff`) | `pcrclock.rs:188`, test `wrap_is_continuous_and_anchor_maps_timestamps` |
| Multiple programs | one reference PCR PID; all packets timed on it | `pcrclock.rs:159` |
| PCR values | **not modified** (no restamp) | — |
| Live feed | not paced (`paced = feed != Live`) | `input.rs:888` |
| End of input | `finish()`; the input ends when the clock is drained and a frame has no real packet | `input.rs:274-277`, `:328-334` |

### 1.4 Coupling to the OFDM frame

`Pacer` holds `frame_ticks` and `[usize; 3]` per-layer counts with a PID→layer `route`
(`input.rs:168-182`, `:255-262`). `status` and the rate check use `capacity::layer_bps(params)`.
Only `PcrClock` (in `isdbt-mux`, no ISDB-T type in its API) is system-agnostic. It has
4 unit tests (`pcrclock.rs:242-392`), which pass at `371e72b`.

## 2. How the ISDB-S producer assembles a superframe

### 2.1 Slot allocation

- `Layout::of` (`txs.rs:314-368`): the default combination is `tc8psk2/3:48`
  (`:316-317`). Sent slots per combination = n / unit × valid (`:331-338`, units from
  `Combination::slot_unit`, `isdbs-core/src/combination.rs:246-257`). A single `--ts` without
  `:SLOTS` gets every sent slot (`:339-343`). `TmccInfo::assign` (`isdbs-core/src/tmcc.rs:366-433`)
  sorts combinations by efficiency and gives each relative TS the next `ts[r].1` sent slots
  **in slot order** (`:420-430`). A TS's slots are therefore contiguous in every frame.
- `Transmitter::new` (`isdbs-phy/src/tx.rs:245-284`) derives `sent` (slot numbers),
  `sent_ts` and `per_frame[r]`. `packets_per_superframe(r) = per_frame[r] × 8`
  (`:321-323`). `build` (`:414-443`) takes packet `next[r]` of TS `r` for each sent slot,
  frame by frame and slot by slot (`:421-433`). **Packet k of TS r goes to frame ⌊k/N_r⌋,
  that TS's (k mod N_r)-th sent slot.** RS(204,188) is applied, then `frame_bytes` (energy
  dispersal and the depth-8 interleaver).
- Bench: one TS (0x4010), 48 sent slots/frame, **384 packets/superframe**.

### 2.2 Reader, queue and `Feed`

- One reader thread per file TS (`source.rs:131-196`) feeds a `sync_channel` of
  `queue_depth(n, 11.07 ms)` packets, about 2 s and at least 1024 (`txs.rs:458-465`,
  `source.rs:297-302`; 69 376 for 384/superframe). The thread resynchronises on 0x47,
  refuses scrambled packets, and with `Feed::Loop` reopens the file at EOF and bumps
  `stats.rewinds` (`source.rs:148-164`). **The rewind is not visible in the packet stream.**
- `PacketStream::next` (`source.rs:248-275`): `File`/`Loop` block (polling `stop` every
  50 ms). `Live` uses `try_recv` and never blocks.
- `TsInputs::take` (`txs.rs:509-552`), per relative TS: synthetic packets
  (`synthetic_packet`, `:399-420`), or **the next `counts[r]` packets in arrival order,
  input nulls kept**, padded with nulls (`:527-539`). It ends when every file TS has ended
  and none gave a packet (`:545-548`).
- Feed modes for ISDB-S: `--feed file` (default, `input.rs:815-817`) fills every slot from
  the file, so a CBR TS goes on air at 52.17 Mbit/s whatever its own rate. `loop` does the
  same with rewinds. `live` takes what is queued and pads. The bench uses `live` behind
  `pv -L RATE` (`tools/isdbs-bench-tx.sh:49-57`).

### 2.3 Producer, and where CPU and FPGA diverge

- `spawn` (`txs.rs:749-879`): the pre-roll (the last frame of a null superframe,
  `:790`), `lead` null superframes (`:792-815`), then data: `input.take(&counts)`
  (`:826`) → `m.data(&mut tx, &refs)` (`:835`) → `ProducedSf`. With `cfg.pace`, production
  is paced by the wall clock (`:767-775`). `pace` defaults to on only for live feeds into a
  non-realtime sink (`producer.rs:265-267`, `txs.rs:1240`). Into a radio, the producer is
  bounded only by `--queue-frames` (4, `tx.rs:133-136`) and the sink. The FPGA path keeps
  about 1.2 s of codes queued (commit `ecb1b41`).
- The paths diverge **only inside `Modulate`** (`txs.rs:617-688`): `Shaped` calls
  `tx.superframe(ts)` + `Shaper` (`:659-662`); `Codes` calls `tx.superframe_codes(ts)`
  (`:663-666`). Both come from the same `build` (`isdbs-phy/src/tx.rs:340-357`).
  **Placement upstream in `TsInputs::take` gives both paths the same packets.** The
  `isdb loopback --system isdbs` path uses the same `TsInputs` (`loopbacks.rs:211`).
- **Why the bench jitters today:** the producer runs about 1.2 s ahead of air, in bursts
  driven by the sink's back-pressure. Each superframe takes whatever `pv` has delivered since
  the last one, so the multiplexer's schedule is smeared by producer and sink burstiness:
  12–30 ms rms and PAT up to 0.21 s (VERIFICATION.md:1754-1763).

## 3. On-air timing of the slots

### 3.1 Constants (verified)

- Symbol rate: **28.860 Mbaud** (MIC Ordinance No. 87, line 815 of the local text,
  "毎秒二八・八六〇メガボー"; `isdb-iq/src/survey.rs:51`).
- Superframe: 8 × 39 936 = **319 488 symbols** (`isdbs-core/src/frame.rs:18-29`,
  `isdbs-phy/src/tx.rs:46`) = **11.0703 ms**. Frame: 1.38378 ms.
- In 27 MHz ticks: superframe = 319 488 × 450/481 = **11 059 200/37 ticks** (exact; 481 = 13·37
  and 319 488 = 2^13·3·13). One TS packet of a TS with N_r slots/frame lasts
  **1 382 400/(37·N_r) ticks**. N_r = 48: 28 800/37 = 778.378 ticks = **28.829 µs**.
- TS rate with N_r slots/frame: 8·N_r·1504 bit per superframe. 48 slots: 577 536 × 28.86e6 /
  319 488 = 28.86e6 × 47/26 = **52 170 000 bit/s exactly**. The plan's 52.17 Mbit/s and
  11.07 ms are confirmed; `txs.rs:1166-1180` logs the same.

### 3.2 Symbol position of each slot

- Frame layout (`frame.rs:7-15`, `:71-78`): 192 header symbols, then 192 × (203 main + 4
  burst). Main symbol m is at `192 + ⌊m/203⌋·207 + m mod 203`.
- `slots::runs` (`isdbs-core/src/slots.rs:33-53`): each combination's run covers a contiguous
  range of main symbols. Its units last whole multiples of 812 symbols
  (`symbols_per_unit`, `combination.rs:262`). Valid slot q of a run starts at main symbol
  `run.start + q·812·u/v` ((u, v) = slots and valid slots per unit). Its on-air start is
  `39 936·f + main_symbol_position(…)`. Slots with a lower-efficiency combination therefore
  last u/v × 812 symbols: 812 for TC8PSK, 1 082⅔ for QPSK 3/4, 1 624 for QPSK 1/2 and 3 248
  for BPSK 1/2.
- **Bench (TC8PSK 2/3 × 48, one TS):** slot s of frame f starts at symbol
  `39 936 f + 192 + 828 s`. 828 = 812 main + 4 bursts × 4. Against the uniform 832-symbol
  grid the offset is 192 − 4s symbols, a frame-periodic sawtooth from **6.65 µs down to
  0.14 µs**.
- **The interleaver makes the "on-air time of a packet" ill-defined.**
  `interleave_position(i, j)` (`mainsig.rs:94-97`) sends byte j of the packet in frame i to
  frame ⌊(8j + i)/203⌋. Byte 0 is always in frame 0 and byte 202 in frame 7. Every packet of
  a superframe completes at the receiver only in frame 7, so a receiver must buffer a
  superframe and re-time its output.

### 3.3 Which grid to pace to

**Use the uniform per-TS grid:** packet k of TS r at k × 1 382 400/(37·N_r) ticks. It is the
CBR stream a receiver outputs for that TS_ID. Evidence:

- Real BS transponders, decoded by `isdb rx` (packet index = slot order), fit a constant packet
  rate within **21–68 ns** (VERIFICATION.md:1049-1051; table `:1002-1004`, BS1 33–55 ns). On
  BS1 the three TSs each take 16 contiguous slots per frame, so their real slot times are
  bunched in thirds of a frame. Fitting within tens of ns at packet-index resolution means
  the broadcaster stamps PCRs to the uniform per-TS grid.
- The PIX-DT295 recording holds every slot of 0x4010, nulls included: 691 712 packets in
  20 s ≈ 52 Mbit/s (VERIFICATION.md:1752). So the receiver's output, and `validate`'s view of
  it, is that grid.
- It is the ISDB-S counterpart of the mux's ISDB-T timing model ("the TS … a 13-segment
  receiver reproduces: one TSP per 408 clocks", `isdbt-mux/SPEC-NOTES.md:24-41`).
- Counter-evidence to keep in mind: the CS TSs deviate **11–16 µs** from a CBR fit (§10, Q3).

## 4. Loop seam

### 4.1 `bench60.ts` at the seam (measured, `seam.py`, §11)

- 1 198 080 packets (260 OFDM frames × 4 608). Two PCR PIDs (0x01FF of program 0x0400 and
  0x01FE of 0x0588) every 57.834 ms; first PCR at packet 0. Period by packets ×
  ticks/packet = **1 623 978 720 ticks = 60.147360 s**. The gap from the last PCR across the
  seam equals one PCR interval exactly, so the file is seam-consistent in time:
  `PcrClock`'s "continue at the previous rate" (`pcrclock.rs:191-193`) bridges it with no
  timing error.
- **CC:** for **9 of 11 PIDs** (PAT, CAT, NIT, SDT, EIT, TOT, the PMTs…) the last CC + 1 ≠ the
  first CC, so each seam gives a CC error. The two AF-only PCR PIDs are fine because their CC
  does not increment. VERIFICATION.md:1765-1766 reports 3 CC errors and 1 PCR discontinuity
  per minute on the recording.
- **SI:** TOT/TDT and EIT p/f restart 60 s back at each pass. This is out of scope here,
  noted in §10.

### 4.2 What the `ts-program` relay does (`isdbt-mux/src/relay.rs`)

- Doc `relay.rs:38-64`: per packet, the arrival time comes from `PcrClock` on the program's
  PCR_PID. Mux time = that time + `base` (+ one loop period per pass), and **PTS/DTS are
  moved by the same constant**. The CC is **renumbered** and the AF is rebuilt **without the
  PCR**: the relay generates new PCRs from the mux frame clock, which is not a byte
  pass-through. The loop period is P = max(arrival span, per-PID PTS span + one step), and a
  pass starts at a video access point.
- Reusable pieces:
  - `Anchor::map90` / `map27` (`pcrclock.rs:61-79`, public);
  - the PTS/DTS field rewrite `write_times` / `pes_times` (`relay.rs:3236-3270`; private
    methods), with `read_ts` / `write_ts` (`relay.rs:774-795`, private) and the test
    `spec_table_2_21_timestamp_rewrite_keeps_prefix_and_markers` (`:4066`).
  These operate on assembled PES bytes, so P1 needs a packet-level equivalent.
- Not reusable as is: the relay's access-point cut-in, ES selection and PCR generation. Our
  relay keeps every packet and its PCRs.

## 5. What `isdb ts validate` / `inspect` check for timing

- `pcr.interval` (error, H.222.0 §2.7.2): PCR-value differences > 0.1 s per PCR PID
  (`isdb-analyze/src/validate.rs` around `:440-470`).
- `pcr.discontinuity` (warning): jumps > 5 s or backwards.
- `pcr.accuracy` (`validate.rs:485-507`): a **global least-squares fit** PCR time vs packet
  index. It runs only when null packets are present. Max |residual| > 500 ns is a warning,
  otherwise info with the value. **The rms is not reported by validate.**
- `inspect` prints, per PCR PID, `rate`, `CBR-fit residual max … ns rms … ns`
  (`inspect.rs:301-338`, text `:702-710`). **The CBR-fit residual asked for in the
  acceptance exists in `inspect`**, and its max in `validate`.
- Section repetition: `tables.frequency` (B10 Part 2 Table 5-2 criterion; PAT/PMT
  ≤ 100 ms), timed by eq. 2-4 on the first program's PCR_PID (`isdb-analyze/SPEC-NOTES.md`
  "section repetition time base").
- **Missing: the PCR-implied rate is never compared with the on-air rate.** Because time
  comes from the PCRs, a TS played at the wrong speed (today's `--feed file`: the 29.96 Mbit/s
  TS in 52.17 Mbit/s of slots) validates clean, with fit 0 ns and PAT 0.093 s (§6
  "arrival"). Add `--expect-rate BPS[:PPM]` to `validate`, pattern of `--expect-tsid`
  (`isdb-cli/src/ts.rs:128-166`, `Expectations`): a `pcr.rate` finding when
  |rate_bps − expected| > tolerance, from `Timebase::rate_bps`. A recording through a real
  receiver gets its own clock's ppm, so use a tolerance of about 100 ppm. An offline `--placed-ts`
  dump should match to < 1 ppm. Optionally print the rms in the `pcr.accuracy` text.

## 6. Offline measurement of placement rules (bench TS, 20 s, 48 slots)

`pacing_sim.py` (§11) reads `bench60.ts` and computes each packet's due time by eq. 2-4 on
PCR PID 0x01FF. It places packets on the 384-per-superframe grid under one of four rules and
writes the TS a receiver would output (every slot, nulls included). The figures below come
from `isdb ts inspect` / `validate` on those files:

| Rule | PCR-implied rate | CBR-fit max / rms (both PCR PIDs) | PAT interval (validate) | validate timing findings |
|---|---|---|---|---|
| `arrival` = today's `--feed file` (nulls kept, every slot filled) | 29.958 Mbit/s (**on air 52.17: 1.74× fast**) | 0 / 0 ns | 0.091–0.097 s (by PCR; 0.052–0.056 s real) | none (rate error undetected) |
| `sf` = ISDB-T `Pacer` rule per superframe | 52.159 | **5.55 ms / 3.20 ms** | up to 0.104 s | `pcr.accuracy` ×2, `tables.frequency` PAT + 2 PMT |
| `slot` = first slot ≥ due, nulls dropped, PCR as is | 52.170 | **14.3 µs / 8.3 µs** | 0.091–0.098 s | `pcr.accuracy` ×2 |
| `restamp` = `slot` + PCR += slot time − due | **52.170000** | **18 ns / 11 ns** | 0.091–0.098 s | **none** |

The input `bench60.ts` itself: 29.958294 Mbit/s, fit 0 ns, PCR every 0.058 s, PAT
0.091–0.097 s, 99.64 % null packets. Non-null traffic is about 108 kbit/s (SI and PCR only,
declared streams without media). The validate warnings left in every row are the input's
own four `pid.missing` warnings.

## 7. Design

### 7.1 Placement and restamp rule (per relative TS)

1. Drop input null packets (count them), as `MuxInput` does.
2. Time each packet with `PcrClock` (reference PID: the first PCR PID seen, or the PCR_PID
   of the first PAT program if P2 adds a cheap PAT/PMT sniff). Call the result due(p), in
   continuous 27 MHz ticks from the first PCR.
3. Slot clock of the TS: slot j (global, counted from the first data superframe) is at
   S(j) = j · 1 382 400/(37·N_r) ticks after the anchor, in exact integer/rational
   arithmetic (u128).
4. **Late strategy**, as the relay's "first free slot at or after its time" (`relay.rs:51-53`):
   j(p) = max(⌈(due(p) + A)·den/num⌉, next free slot). A is the anchor offset (§7.4). Input
   order is never changed.
5. Restamp every PCR field (any PID, `adaptation_field` with `PCR_flag`) by
   **Δ = round(S(j) − due(p) − A)** ticks, mod 2^33·300. In steady state Δ ∈ [0, one slot);
   with saturation Δ = lag. OPCR is left alone. This is H.222.0 Annex D.0.4 with
   del_const = 0. Δ is a duration, so it is valid for every program's time base: a second
   PCR PID with its own clock is corrected correctly.
6. Fill the superframe's N_r × 8 slots: the placed packets at their j, nulls elsewhere.
   Packets with j ≥ end of the superframe wait in a carry.

Error bound: |PCR_out(p) − (base + S(j) + O_pass)| ≤ 1 tick (37 ns) plus `PcrClock`'s
integer interpolation (≤ 1 tick).

### 7.2 Where it lives, and its API

New module **`crates/isdb-cli/src/pace.rs`**, system-agnostic. It does not depend on
`isdbt_core::Params` or ISDB-S types, only on `isdbt_mux::pcrclock` and the P1 helpers. It
is a new component rather than a copy of `Pacer`; ISDB-T could migrate to it later (§10).

```rust
/// Exact slot clock: slot j at ⌊j·num/den + ½⌋ ticks of 27 MHz after the anchor.
pub struct SlotClock { num: u64, den: u64 }
impl SlotClock {
    pub fn new(num: u64, den: u64) -> Self;
    /// ISDB-S TS with `slots_per_frame` sent slots: 1 382 400 / (37·N_r) ticks per packet.
    pub fn isdbs(slots_per_frame: usize) -> Self;
    pub fn time(&self, j: u64) -> u64;            // ticks (rounded)
    pub fn first_at_or_after(&self, t: u64) -> u64; // ⌈t·den/num⌉
    pub fn bps(&self) -> f64;                      // 188·8·27e6·den/num
}

/// A packet source with the loop pass of each packet (P3; pass 0 without loop).
pub trait TaggedSource {
    fn next(&mut self, stop: &AtomicBool) -> Option<(Packet, u32)>;
    fn ended(&self) -> bool;
}

pub struct SlotPacer { /* PcrClock<(Packet, u32)>, SlotClock, next_slot, carry,
                          Retimer (PCR Δ, per-pass offsets, PTS/DTS, CC), stats */ }
impl SlotPacer {
    pub fn new(slots: SlotClock, pcr_pid: Option<Pid>, mode: WaitMode) -> Self;
    /// The next `n` slots: exactly n packets, and how many came from the input.
    pub fn fill(&mut self, n: usize, src: &mut dyn TaggedSource, stop: &AtomicBool) -> (Vec<Packet>, usize);
    /// Input ended, clock drained, nothing carried.
    pub fn drained(&self) -> bool;
    pub fn stats(&self) -> PacerStats; // placed, dropped_nulls, lag/max_lag (ticks), saturated,
                                       // pcr_restamped, max |Δ|, discontinuities, passes, cc_fixed
    pub fn status(&self) -> String;    // " | PCR lag 0.00 s (max 0.00) in 0.11 Mbit/s (cap 52.17)"
}
pub enum WaitMode { Block /* file, loop */, Live { latency: Duration, now: Box<dyn Fn() -> Instant + Send> } }
```

`TsInputs` gains a third `Source` variant, `Paced { stream, pacer }` (`txs.rs:422-428`).
`take` calls `pacer.fill(counts[r], …)` for it, and its end test uses `pacer.drained()`
instead of `stream.ended()` (`:542-548`). Synthetic and unpaced file sources are unchanged.

### 7.3 Per feed mode

| Feed | Default | Behaviour |
|---|---|---|
| `file` (regular file) | paced if `probe_rates` finds a PCR (route all → 0) | block for input; at EOF `finish()`; end once `drained` and a superframe holds no real packet |
| `loop` | paced if a PCR is found | as `file`, plus the seam (§7.6) |
| `file` on a FIFO or stdin (`-`) | paced (no probe possible: decided by the first PCR, as `live`) | blocks like a file |
| `live` | paced when a PCR appears within the first 1 s (wall) / 64 k packets; else today's path | §7.7 |
| no PCR (any feed) | **today's behaviour** (arrival order, nulls kept) | logged once, as `check_input_rates` does (`input.rs:733-741`) |

Add a switch `--ts-pacing auto|pcr|arrival` (default `auto`) so the old rule stays reachable
for comparison and for inputs with broken PCRs. It is an ISDB-S-only option, so add it to
`TX_ISDBS_ONLY` (`txs.rs:153`).

### 7.4 Rate versus capacity, startup

- Capacity of TS r = `SlotClock::bps()` = 8·N_r·1504/T_sf. Probe file inputs
  (`probe_rates`; nulls are excluded already). Mean > capacity is refused unless
  `--allow-overrate` (reuse the flag, `input.rs:829-834`). A 1-s peak > capacity gives a
  warning. Factor `check_input_rates` (`input.rs:732-793`) into a capacity-agnostic core and
  keep the ISDB-T wrapper.
- Feed rate < capacity (the normal case; the bench uses 0.2 % of it): null padding.
- Feed rate > capacity: packets wait in order, lag grows, every packet is still sent, and
  restamping keeps PCRs accurate. **PTS−PCR shrinks by the lag**, so decoders underflow once
  the lag exceeds the stream's decode margin. Report `lag`/`max_lag` and a status warning as
  ISDB-T does (`input.rs:376-392`). Do not re-anchor silently.
- **Anchor (file/loop):** due time 0 (the first PCR) ↔ slot 0 of the first data superframe,
  so A = 0. Packets before the first PCR all get t = 0 (`pcrclock.rs:171-182`): they take
  slots 0… and the first PCR is late by their count. Restamping covers this. Optional
  refinement: A = (pre-PCR packet count) × slot, to keep them early.
- Pre-roll and lead superframes are unchanged and use no input (`txs.rs:790-815`). The slot
  index counts data superframes only.

### 7.5 Without PCRs

Return "unpaced" at the probe, or at the first PCR deadline for live inputs. `TsInputs` then
keeps `Source::File` (today's code, unchanged). For a paced source whose PCRs stop
mid-stream, `PcrClock` extrapolates after 200 000 pending packets (≈ 37 MB, `pcrclock.rs:43`).
That is acceptable for files. For live inputs, lower the threshold or time out on the wall
clock (P5).

### 7.6 Loop seam

- **Pass tag** (P3, `source.rs`): the reader thread counts reopenings and sends `(Packet, u32)`
  on the channel. `PacketStream::next` keeps its signature for ISDB-T callers and drops the
  tag. A new `next_tagged` implements `TaggedSource`. Cost: 4 bytes per queued packet.
- **Per-pass offset O_n** (27 MHz, a multiple of 300 so that PTS shifts exactly): when the
  first PCR of the reference PID in pass n ≥ 1 is pushed, set
  O_n = (anchor.t + base) − anchor.pcr (`clock.anchor()` after `push`; base = the raw value of
  the very first PCR), rounded to a multiple of 300. O_0 = 0. The pass's earlier packets are
  still pending in the clock, so O_n is known before any packet of pass n is popped. On
  `bench60.ts`, O_n = n × 1 623 978 720.
- On every placed packet of pass n:
  - PCR_out = PCR_in + O_n + Δ;
  - PES packets (`payload_unit_start_indicator`, payload starts `00 00 01`, stream_id with
    an optional header, `isdb_ts::pes::has_optional_header`): PTS/DTS += O_n/300 (mod 2^33),
    prefix and markers kept (Table 2-21), only when the header lies inside this packet;
    otherwise count it;
  - CC: per-PID offset c[pid]. At the first packet of a PID in pass n ≥ 1,
    c = (expected − cc_in) mod 16, where expected = last_out + 1 if the last output packet
    of the PID had a payload, else last_out (H.222.0 §2.4.3.3: AF-only packets do not
    increment). Output CC = (cc_in + c) mod 16. Byte-identical duplicates are not relevant to
    files.
- A real PCR discontinuity inside a pass (the indicator set) is passed through with Δ only;
  the receiver handles it per §2.4.3.5. A backwards step without the indicator and not at a
  seam is counted and passed through.
- Single pass (`--feed file`): O_0 = 0, so no field other than the PCR is ever modified.

### 7.7 Live / FIFO (P5)

- The producer is bounded only by the sink (≈ 1.2 s ahead of air on the FPGA path). With
  non-blocking `take` it would race ahead of a real-time writer, and every packet would be
  late after the first second. Live pacing therefore **blocks for input, bounded by a
  deadline**:
  - anchor: when the first PCR is timed while building superframe k_now, put due 0 at slot 0
    of superframe k_a = k_now + ⌈L/T_sf⌉. L is the live latency (default about 0.3 s: it
    covers one PCR interval ≤ 0.1 s plus arrival jitter). Record the wall time w_a;
  - building superframe k: read until the clock has timed a packet with due ≥ end(k), the
    input ends, or the wall clock passes w_a + (k + 1 − k_a)·T_sf + L + G, with grace G
    (≈ 0.2 s, below the sink's 1.2 s buffer). Then emit the superframe; late packets go
    later, restamped, and the lag is reported.
- Steady state: production follows the writer's pace, the radio's queue fills once at start
  (on-air start ≈ 1.2 s + L later than today), and the PCR schedule is independent of
  arrival jitter.
- Clock drift (writer vs Pluto DAC; the Pluto is ≈ +0.6 ppm vs BS, `isdb-s.md:44`): a faster
  writer is throttled by the FIFO's back-pressure (the reader blocks on the full queue). A
  slower writer drains the sink buffer: at 50 ppm a 1.2 s buffer lasts about 6.7 h, then
  deadline misses start to add lag. Report "input lead/lag vs schedule".
- Use an injectable time source (`WaitMode::Live { now }`) so that tests do not repeat the
  flaky wall-clock test noted in `isdb-s.md:243-251`.

### 7.8 Observability and offline acceptance

- `--placed-ts TEMPLATE` (`{ts_id}`), on `tx --system isdbs`: writes each relative TS's
  placed packets, nulls included, as the producer hands them to `Modulate`. Write it from
  `ProducedSf` with `record = true` in `drive`, or straight from `TsInputs`. It is the TS the
  tuner outputs, and it is the same for `--iq` and `--offload fpga`. The bench recording can
  then be checked **byte-exact** against it, after aligning the start.
- Status line per paced TS: `PCR lag`, `max`, input rate vs capacity, `nulls-dropped`,
  `restamped`, `passes`, `pcr-discontinuities`.

### 7.9 Spec bookkeeping

`isdb-cli` has no SPEC-NOTES (top-level `SPEC-NOTES.md:28-29`). Create
`crates/isdb-cli/SPEC-NOTES.md`, add it to the index, and record:

- (a) the ISDB-S TS timing model: uniform per-TS grid, evidence BS 21–68 ns, CS 11–16 µs open;
- (b) PCR restamping per H.222.0 Annex D.0.4 with the late strategy, del_const = 0;
- (c) the loop seam: constant shift of PCR/PTS/DTS by the pass period and CC renumbering,
  cross-referenced to `isdbt-mux` "Media looping" and the `ts-program` notes.

Doc comments cite H.222.0 §2.4.2.2 eq. 2-4, §2.4.3.3 (CC), §2.4.3.5, Annex D.0.4, and BO.1408-1
Table 3 / Ordinance Art. 52(3) for the slot clock.

## 8. Tests

Unit tests in `pace.rs`, synthetic streams built with `input.rs`'s `pcr_packet`
(`:1051-1063`) and `source.rs`'s `pkt`:

- `spec_slot_clock_isdbs`: `SlotClock::isdbs(48)`: 384 slots = 11 059 200/37 ticks (exact
  rational, so 37 superframes = 11 059 200 ticks), `bps() == 52_170_000`. N_r = 16 →
  1/3 of that rate.
- `cbr_input_is_placed_late_by_less_than_a_slot`: a CBR input at R_in ∈ {0.1, 10, 29.96,
  52.0} Mbit/s into 48 slots: for every placed packet, 0 ≤ S(j) − due < one slot; order kept;
  input nulls dropped.
- `prop_placed_pcr_error_below_one_slot`: random R_in < capacity, PCR interval 10–100 ms,
  PCR jitter ±slot, random start offsets and N_r ∈ 1..=48: placement error ∈ [0, slot)
  without saturation, every packet sent once, in order.
- `prop_restamped_pcr_is_on_the_slot_grid`: for every output PCR packet at global slot j:
  PCR_out − PCR_out(first) ≡ S(j) − S(j_first) mod 2^33·300, within ±2 ticks. Equivalently, a
  least-squares fit over the output (helper shared with `isdb-analyze` or re-implemented)
  gives max residual ≤ 100 ns and slope = `bps()` within 1 ppm.
- `two_pcr_pids_are_both_restamped`, `pcr_wrap_mid_stream` (raw PCR near 2^33·300),
  `discontinuity_indicator_passes_through_with_delta`, `opcr_untouched`.
- `over_capacity_lags_and_sends_everything` (counterpart of `paced_split_over_capacity_lags`).
- `no_pcr_is_reported_unpaced` (probe → `None`; `TsInputs` keeps arrival order).
- Loop (P3): `loop_seam_is_seamless`. A small CBR file (PCR, a PES PID with PTS/DTS, a
  section PID, an AF-only PCR PID) is looped 3× through `PacketStream::spawn` with
  `Feed::Loop`. Expect: no backwards PCR, the fit holds across seams, PTS steps continuous,
  CC continuous per PID (check with `isdb_ts::cc` or `isdb_analyze` on the concatenated
  output: 0 CC errors, 0 PCR discontinuities), pass 0 byte-identical to the input apart from
  the PCR.
- `txs.rs`: `paced_inputs_feed_both_paths_the_same_packets` (spawn with `Modulate::codes()`
  and `Modulate::shaped(..)`, `record = true`, compare `packets`); an end-to-end
  `placed_ts_of_a_cbr_file_validates` (generate a 3 s CBR TS in the test, run `TsInputs` +
  pacer, write the placed TS, run `isdb_analyze` validate: no `pcr.*` above info and no
  `tables.frequency`; `inspect` rate 52.17 Mbit/s ± 1 ppm); keep
  `inputs_pad_with_nulls_and_end_with_the_files` (no PCR, so the old path).
- Live (P5): `live_paced_with_mock_clock` covering anchor, latency, deadline miss → nulls and
  lag, recovery; `live_without_pcr_falls_back`.
- Bench acceptance (manual, recorded in VERIFICATION.md): `--feed loop` on `bench60.ts`
  (FPGA and `--iq` + `isdb rx`); `dt295 stream 20 s` → `isdb ts validate --expect-rate
  52170000:100` shows no `pcr.*` / `tables.frequency` finding; `inspect` fit max ≤ 1 µs
  (target ≤ 100 ns); the recording equals `--placed-ts` after aligning.

## 9. Implementation split (one worktree agent each)

| Piece | Scope and contract | Depends on |
|---|---|---|
| **P1** retime helpers (`isdb-ts`, e.g. `src/retime.rs` or in `packet.rs`/`pes.rs`) | `pcr_field(&[u8;188]) -> Option<usize>`; `add_to_pcr(&mut [u8;188], d: u64) -> bool` (mod 2^33·300, ext kept in 0..300); `shift_pes_timestamps(&mut [u8;188], d90: u64) -> PesShift {Shifted, NoHeader, Truncated}`; `continuity_counter` get/set. Spec doc comments (Tables 2-2, 2-6, 2-21). Tests `spec_table_2_6_pcr_add_wraps`, `spec_table_2_21_…_markers_kept`, sections and non-PUSI packets untouched, `prop_add_then_subtract_is_identity`. No `isdb-cli` change. | — |
| **P2** `SlotClock` + `SlotPacer` core (`isdb-cli/src/pace.rs`) | API of §7.2 with `WaitMode::Block` only and pass always 0; rule §7.1; stats/status; tests of §8 except loop and live. Must not touch `input.rs`'s `Pacer` or `txs.rs`. May use P1 through a local shim until P1 merges (the signatures are fixed above). | (P1 API) |
| **P3** loop seam | `source.rs`: pass tag in the reader thread, `next_tagged`, `TaggedSource` impl; existing `next()` unchanged (ISDB-T unaffected; the `source.rs` tests stay green). `pace.rs`: per-pass offset, PTS/DTS shift, per-PID CC renumbering (§7.6); test `loop_seam_is_seamless`. | P1, P2 |
| **P4** `txs.rs` integration | `Source::Paced`, probe and capacity check (refactor `check_input_rates` into a capacity-agnostic core and keep ISDB-T output identical), `--ts-pacing`, `--placed-ts`, `describe`/`status`, loopback unaffected; module doc (`txs.rs:1-36`), `--help`, `isdb-cli/SPEC-NOTES.md` + index; tests of §8 (`txs.rs`). The CPU/FPGA identity test is mandatory. | P2 (P3 for `loop` seam quality; P4 can land before P3 with seams as today) |
| **P5** live/FIFO pacing | `WaitMode::Live` (§7.7) with an injected clock, PCR detection with timeout and fallback, wiring for `--feed live`, tests with the mock clock; no wall-clock-sensitive asserts. | P2, P4 |
| **P6** validation and bench tooling | `isdb ts validate --expect-rate BPS[:PPM]` (`pcr.rate`, `Expectations`, `isdb-analyze` SPEC-NOTES entry); optionally the rms in the `pcr.accuracy` text. `tools/isdbs-bench-tx.sh`: default `--feed loop` on `bench60.ts` (keep the FIFO mode behind a flag for P5) and `--placed-ts`. `tools/isdbs-bench-check.sh`: `dt295 stream` + inspect + validate + compare with the placed TS. Move `pacing_sim.py` into `tools/` as the reference model. | — (P4 for `--placed-ts`) |

Merge order: P1 → P2 → P4 (bench acceptance possible with `--feed file`) → P3 (then `loop`)
→ P6 → bench → P5.

## 10. Risks and open questions

1. **Restamping changes the input bytes.** PCRs always change, and PTS/DTS/CC change at loop
   seams. This is sanctioned by H.222.0 Annex D.0.4 and is what any remultiplexer does, but
   it is a change of policy for "relay a TS file". Confirm, and keep `--ts-pacing arrival`
   for byte-exact relays.
2. **Grid choice for a multi-TS or mixed-combination layout.** The uniform per-TS grid is
   supported by BS (21–68 ns). The CS TSs fit only within 11–16 µs. Check whether CS PCRs fit
   the *true* symbol-time grid of §3.2 (QPSK 3/4: 1 082⅔ symbols per slot plus the header)
   better than the uniform one, using `isdb rx` on an `ISDB_RS_ISDBS_TS_IQ` CS recording plus
   a fit script. If they do, `SlotClock` needs a per-slot time table (the API already allows
   swapping `time()`).
3. **How the PIX-DT295 (TC90532) re-times its output** is unknown. The recording is
   index-based, so it does not matter for `validate`. A real-time decoder behind the tuner
   sees the tuner's own output smoothing. Acceptance is defined on the recording.
4. **Lag and over-capacity policy:** keep the lag (PTS margin shrinks) or re-anchor with
   `discontinuity_indicator` once the lag exceeds a bound? Proposed: keep it, report it, and
   refuse at the probe. Decide for live inputs (P5).
   *Decided in P5:* live inputs keep the lag too (late packets restamped, output PCRs on the
   grid, no re-anchor and no indicator); it shrinks when the writer sends its backlog, and
   `live-input lag` / `deadline-misses` report it (`isdb-cli/SPEC-NOTES.md`).
5. **Live:** default latency L and grace G; whether `--feed live` should pace by default when
   PCRs are present (the plan says yes); interaction with `pace` for `--iq` (both wait;
   harmless). Writer/DAC clock drift over multi-hour runs.
   *Decided in P5:* L = 0.3 s and G = 0.2 s, constants (`pace::LIVE_LATENCY`,
   `LIVE_GRACE`), no CLI flag; `--feed live` paces by default when a PCR comes within 1 s /
   65 536 packets (`--ts-pacing pcr` paces from whenever it comes). w_a is the first PCR's
   *arrival* (the reader thread's read time), and the input lag is measured at each PCR's
   arrival, so a producer paced to real time (`--iq`), which reads each packet L after it
   came, does not show the writer as late; drift shows as a growing lag (a 5 % slow writer in
   the tests: first miss after 9.5 s). With `pace` for `--iq` both wait, as expected:
   `tools/isdbs-pacing-e2e.sh` `FEED=live` (10 s, `pv -L` at the TS's rate) had no miss and
   an input lag within ±0.08 s.
6. **Reference PCR PID:** first PCR seen versus the PAT's first program PCR_PID. They are the
   same for the bench. For a multi-program TS with independent clocks, packet due times
   follow one program; Δ is still right per packet. Is a PMT sniff worth it?
7. **Loop seam content:** TOT/TDT/EIT go back by one period each pass, and SI versions
   repeat. That is out of scope here and belongs to the mux (a loop-aware file) or to the
   in-process ISDB-S mux (work item 3). PES headers split across packets (rare) are not
   shifted; the count is reported.
8. **ISDB-T follow-up:** `MuxInput`'s `Pacer` has the same frame-granular placement (by
   reading: up to about one 231 ms frame of PCR error). `SlotPacer` with the multiplex-frame
   TSP times of `isdbt-mux`'s timing model (408 clocks per TSP) would fix it. Measure first
   (`--mux-ts` with `bench60.ts`, `isdb rx`, validate), and track it separately.
9. **The 0.35 % rate seen on the recording** (51.99 vs 52.17 Mbit/s, VERIFICATION.md:1752)
   is bytes per wall second, not the PCR rate. After pacing, the PCR-implied rate of the
   recording must be 52.17 Mbit/s within the radio's ppm. If the wall rate really is 0.35 %
   low, that is a separate symbol-clock question.

## 11. Commands run

```sh
# bench TS (the steps of tools/isdbs-bench-tx.sh, in the scratchpad)
python3 -I - crates/isdbt-mux/examples/bench-minimal.toml bs-bench.toml <<'PY' … PY
target/release/isdb mux -c bs-bench.toml --seconds 60 --out-ts bench60.ts
#   260 frames (60.15 s); 1 198 080 packets; layer A 2 591, layer B 1 708 non-null
target/release/isdb ts inspect bench60.ts   # 29.958294 Mbit/s, PCR 0x01fe/0x01ff every 0.058 s, fit 0 ns, PAT 0.091–0.097 s
target/release/isdb ts validate bench60.ts  # 0 errors, 4 warnings (pid.missing: declared streams without packets)

# placement model and its outputs (20 s, 48 slots)
python3 -I pacing_sim.py bench60.ts 20 {arrival|sf|slot|restamp} out-MODE.ts
target/release/isdb ts inspect out-MODE.ts ; target/release/isdb ts validate out-MODE.ts   # §6

# loop seam
python3 -I seam.py bench60.ts   # period 1 623 978 720 ticks; 9 of 11 PIDs CC-discontinuous at the seam

cargo test --locked -p isdb-cli --bin isdb -- paced inputs_pad   # 4 passed
cargo test --locked -p isdbt-mux --lib pcrclock                  # 4 passed
```

The scripts are in the session scratchpad
(`/tmp/claude-1000/-home-a1678991-dev-isdb-rs/d8070956-98d1-4b16-a7a8-8e96244838b0/scratchpad/pacing_sim.py`
and `seam.py`). P6 should move `pacing_sim.py` into `tools/`.
