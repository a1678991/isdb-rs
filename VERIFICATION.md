# PHY verification against the gr-isdbt oracle

Differential test of the Rust ISDB-T PHY (`isdbt-fec`, `isdbt-ofdm`, driven
through `isdbt-phy`) against gr-isdbt (GNU Radio 3.10.9.2, gr-isdbt commit
`56b2556`, container `arib-sdr`). Per README level 5, gr-isdbt is an
oracle only: it can show that the two implementations disagree, and ARIB
STD-B31 v2.2 decides which one is right. Run date: 2026-10-08, Rust tree
after the Part 2 Figs. 3-8/3-9 alignment change (`isdbt-ofdm/src/latency.rs`).

## Result

- **Both directions are byte-exact in every configuration tested.** That is
  9 runs (8 parameter sets: Modes 1/2/3; GI 1/4, 1/8, 1/16, 1/32; QPSK,
  16QAM, 64QAM; all five code rates; time-interleave lengths 0/1/2/4
  (Mode 3), 0/2/4/8 (Mode 2), 8/16 (Mode 1); with and without partial
  reception; 1, 2 and 3 layers). No RS word was corrected or uncorrectable
  in any steady-state frame, and TMCC parity checked on both sides.
- **Carrier-level comparison.** With identical TS input, the two
  modulators' OFDM symbols are identical (one complex gain, tolerance 1 %
  of RMS) on every data carrier, SP, CP and band-edge carrier. The only
  carriers that differ are the TMCC and AC carriers, and every differing
  bit is explained by one of the gr-isdbt deviations D1–D3 below.
- **No discrepancy in the data path, so no Rust code was changed.** The four
  disagreements found (D1–D4) are all on the gr-isdbt side, as the B31
  text shows. Receivers do not need any of them to decode (see each
  entry).
- **Pending items in SPEC-NOTES are now settled by real-receiver evidence
  (through the oracle).** These were the carrier-rotation numbering, the
  2-symbol frame alignment, and the frame alignment of energy dispersal
  and puncturing. gr-isdbt's output was decoded by a consumer USB tuner
  (MyGica S270) with PER 0 over a conducted link (Mode 3, GI 1/4, partial
  reception, A 1 seg QPSK 1/2, B 12 seg 64QAM 3/4). Row `m3g4p_bench`
  below shows that the Rust chain is byte-exact and carrier-identical with
  that gr-isdbt chain.

## Matrix

Layer spec: `segs, modulation, rate, I` (I is the interleaving length; the
Rust tools take the TMCC index). "gr→Rust" means gr-isdbt TX
(`grisdbt_tx.py`) decoded by `rx_file`. "Rust→gr" means Rust TX (`tx_file`
or `rust_tx3`) decoded by `grisdbt_rx.py`. "Rust→Rust" is the loopback.
Packet counts are per layer A/B/C. *mm* = mismatching packets after
content anchoring (`ts_compare.py`). RS figures are clean/corrected-bytes/
uncorrectable words summed over the valid frames. MER is the minimum over
those frames (noise-free float32, so it measures numerical precision only).
"Carriers ≠" is the `iq_compare.py` result.

Inputs: row 0 used packets of a live terrestrial capture (`a_in.ts`,
`b_in.ts`, 20 frames; not distributed). Rows 1–8 use synthetic TS with a
unique packet index in each payload (`run_matrix.py`).

| # | config | layers | gr→Rust: valid fr., packets (mm) | RS (A; B; C) | MER dB | TMCC parity | Rust→gr: packets (mm), gr TMCC | Rust→Rust packets (mm) | carriers ≠ |
|---|---|---|---|---|---|---|---|---|---|
| 0 | live-capture packets, M3 GI1/8 partial | A 1,QPSK,2/3,2; B 12,64QAM,3/4,2 | 16; 1024 / 41472 (0) | 1024/0/0; 41472/0/0 | 118.6 / 125.0 | Ok 16/16 | 1087 / 44038 (0); OK 19, NOT OK 0 | 1024 / 41472 (0) | 104 AC + 52 TMCC |
| 1 | `m3g8p_q23_64q34` | same as #0 | 12; 768 / 31104 (0) | 768/0/0; 31104/0/0 | 118.6 / 125.0 | Ok 12/12 | 831 / 33670 (0); 15 / 0 | 768 / 31104 (0) | 104 AC + 52 TMCC |
| 2 | `m3g4p_bench` M3 GI1/4 partial | A 1,QPSK,1/2,4; B 12,64QAM,3/4,2 | 11; 528 / 28512 (0) | 528/0/0; 28512/0/0 | 109.6 / 128.0 | Ok | 575 / 33670 (0); 15 / 0 | 528 / 28512 (0) | 104 AC + 52 TMCC |
| 3 | `m3g4p_q12_16q34` M3 GI1/4 partial | A 1,QPSK,1/2,4; B 12,16QAM,3/4,1 | 11; 528 / 19008 (0) | 528/0/0; 19008/0/0 | 109.6 / 128.1 | Ok | 575 / 22447 (0); 15 / 0 | 528 / 19008 (0) | 104 AC + 52 TMCC |
| 4 | `m3g16_64q78_i0` M3 GI1/16 | A 13,64QAM,7/8,0 | 7; 22932 (0) | 22932/0/0 | 132.6 | Ok | 16353 (1 = gr start-up packet, see D6); 6 / 1 (first frame, D6) | 22932 (0) | 104 AC + 52 TMCC |
| 5 | `m2g8p_q56_64q12` M2 GI1/8 partial | A 1,QPSK,5/6,8; B 12,64QAM,1/2,0 | 17; 680 / 14688 (0) | 680/0/0; 14688/0/0 | 116.3 / 124.7 | Ok | 719 / 18988 (0); 23 / 0 | 680 / 14688 (0) | 52 AC + 26 TMCC |
| 6 | `m1g32p_16q23_q78` M1 GI1/32 partial | A 1,16QAM,2/3,16; B 12,QPSK,7/8,8 | 29; 928 / 7308 (0) | 928/0/0; 7308/0/0 | 120.8 / 135.3 | Ok | 959 / 8565 (0); 39 / 0 | 928 / 7308 (0) | 26 AC + 13 TMCC |
| 7 | `m3g8_3lay` M3 GI1/8 | A 3,QPSK,1/2,4; B 5,16QAM,2/3,2; C 5,64QAM,5/6,1 | 11; 1584 / 7040 / 13200 (0) | 1584/0/0; 7040/0/0; 13200/0/0 | 126.8 / 122.0 / 125.0 | Ok | 1726 / 8313 / 15588 (0); 15 / 0 | 1584 / 7040 / 13200 (0) | 104 AC + 52 TMCC |
| 8 | `m2g16p_3lay` M2 GI1/16 partial | A 1,QPSK,3/4,2; B 4,16QAM,1/2,8; C 8,64QAM,2/3,4 | 17; 612 / 3264 / 13056 (0) | 612/0/0; 3264/0/0; 13056/0/0 | 132.0 / 132.0 / 133.1 | Ok | 755 / 3454 / 15352 (0); 23 / 0 | 612 / 3264 / 13056 (0) | 52 AC + 26 TMCC |

Notes on the matrix:

- gr→gr reference for row 0: gr-isdbt decodes its own file with A 1075
  and B 44002 packets, both exact. The first decoded packet is source
  packet 2603 for B (byte-deinterleaver warm-up; gr-isdbt has no frame
  alignment at its output).
- Frame accounting. The Rust modulator's first output frame is on-air
  frame −1, so it carries the advanced data-frame symbols 1–2 (SPEC-NOTES
  "latency"). gr-isdbt's first frame is on-air frame 0, because its
  `skiphead(2)` drops symbols 1–2. Rust file frame f+1 therefore lines up
  with gr file frame f. The exception is a layer with I = 0, which lines
  up with gr file frame f+1 (see D4). `rx_file` labels frames from the
  frame where it locked, so its first valid output is source frame 1 for
  gr input and source frame 0 for Rust input, consistently.
- Carrier-level differences (`iq_compare.py`). In every row:
  - AC carriers: Rust B1–B203 = all 1, gr-isdbt = all 0 (D2).
  - TMCC carriers:
    - B1–B16: sync-word phase (w0/w1 swapped). It only shows when the
      compared files are an odd number of frames apart. Table 3-21:
      "Frame numbers are assigned for convenience of description". This
      is not a discrepancy.
    - B17–B19 (D1).
    - B67 and the parity bits that follow from it, in partial-reception
      configurations only (D3).
  - B'0 (the DBPSK reference W_i) is equal on every TMCC and AC carrier.
  - B20–B66 and B68–B121 are equal: modulation, rate, I and segment count
    are encoded identically, as is the countdown (1111) and the reserved
    bits (1).
- Rotation-numbering sensitivity. `rx_rot` decodes gr-isdbt files with
  `RotationNumbering::GroupRelative`. Every layer-B word fails on row 0
  (B 0/0/41472), and every layer-B and C word fails on row 8 (B 0/0/3264,
  C 0/0/13056). With the default `DataSegmentNumber`, every word is clean.
  The test would therefore have caught a wrong rotation numbering.

Coverage gaps. These combinations were not tested:

- DQPSK, differential segment groups, and rotation numbering inside a
  DQPSK group: gr-isdbt implements only coherent modulation.
- Mode 1 with I = 0 or 4.
- AC payload content, emergency-alarm flag, and parameter switching with
  a countdown. gr-isdbt has none of these (D2).
- Channel impairments (noise, CFO, timing offset, multipath). All runs are
  noise-free.

## Discrepancies

All four are on the gr-isdbt side, and none required a change to Rust.

### D1. TMCC segment-type bits B17–B19 on coherent segments
- gr-isdbt: `tmcc_encoder_impl.cc` sets B17, B18 and B19 to 1 on every
  segment ("Our implementation supports coherent modulation only"). That
  is the code for differential modulation.
- Rust: sends 000 on coherent segments and 111 on differential segments
  (`tmcc::segment_type_bits`).
- B31 §3.15.5: "This signal is used to determine whether a segment is a
  differential or coherent modulation and consists of a 3-bit word. “111”
  and “000” are assigned to this signal for a differential and coherent
  modulation, respectively."
- Resolution: Rust is correct. The Rust receiver tolerates gr-isdbt's
  value, since it detects segment kinds from the pilots. The USB tuner decoded
  gr-isdbt output, so that receiver ignores these bits. No change.

### D2. AC carriers unmodulated in gr-isdbt
- gr-isdbt: `pilot_signals_impl.cc` writes the pilot value W_i on every AC
  carrier in every symbol. Differentially, that is information 0
  throughout.
- Rust: DBPSK with B'0 = W_i and stuffing 1 (`AcFrame::stuffing`).
- B31 §3.13.4: "AC is transmitted by means of modulating the pilot carrier
  of a type similar to CP through DBPSK, and its reference for
  differential modulation is provided at the top frame symbol … If there
  is no additional information on broadcasting, information 1 is inserted
  as stuffing bits."
- Resolution: Rust is correct. No change.

### D3. TMCC B67 (next-information partial-reception flag)
- gr-isdbt sends B67 = 0 while its current flag B27 = 1. Its next-
  information layer fields (B68–B106) are copies of the current ones.
- Rust repeats the current information, so B67 = B27.
- B31 §3.15.6.4: "Note that the content of this flag is set to “1” if
  there is no next information." Table 3-22 lists B67 as the "Partial-
  reception flag" of the next information. Under both readings (no next
  information, or next = current) the value is 1 in these configurations.
- Resolution: Rust is correct. The parity bits differ as a consequence,
  yet each side's parity checks on the other side's frames. The two
  difference-set encoders therefore agree. No change.

### D4. Time-interleaving delay adjustment for I = 0
- gr-isdbt: in `time_interleaver_impl.cc`, the buffer for carrier i holds
  `I·m_i + 1 + sync_delay` with `sync_delay = 204 − ((95·I) % 204)`. That
  is a delay adjustment of 204 symbols (one whole frame) when I = 0. For
  I > 0 it equals Table 3-12 (for example 109 / 14 / 28 in Mode 3).
- Rust: adjustment from Table 3-12, which is 0 for I = 0.
- B31 Table 3-12 lists I = 0 with 0 delay-adjustment symbols and 0 delayed
  frames in Modes 1, 2 and 3. §3.11.1 adds: "The resulting difference in
  delay time must be corrected on the transmitting side using the number
  of symbols or the delay appropriate for each layer shown in Table 3-12,
  such that the total number of transmission and reception delays is a
  multiple of the number of frames."
- Effect: an I = 0 layer from gr-isdbt is one frame late relative to the
  other layers. Row 5 shows it: gr→Rust anchors A at source frame 1 and B
  (I = 0) at frame 0, while the Rust loopback has both at frame 0. Each
  layer still decodes, because FEC frames stay aligned to OFDM frames.
  Only the inter-layer timing in the reproduced TS is affected.
- Resolution: Rust is correct. No change. `iq_compare.py --seg-offsets`
  compensates for this.

### Oracle limitations observed (not disagreements)
- D5. A typo in gr-isdbt's TMCC encoder writes layer C's next-information
  I = 0 code at B87 (layer B's next I field) instead of B100. This was
  found by code inspection and not exercised: no test used layer C with
  I = 0.
- D6. gr-isdbt receiver start-up:
  - Its first output packet is unusable: the energy descrambler passes
    data through before the first `frame_begin` tag. This is the 1
    mismatch in row 4; gr→gr on the same configuration shows the same
    thing (22777 packets, the first one wrong).
  - In row 4 only, its TMCC decoder printed one "TMCC NOT OK" on the first
    frame of the Rust file, and then OK on every frame. The DBPSK bits of
    that first Rust frame (on-air frame −1) were decoded independently
    with numpy. They equal the later frames bit for bit, apart from the
    sync-word alternation. With the first or first two frames cut off, gr
    reports OK from its first frame. This is a gr acquisition artefact.

## Agreements that settle pending SPEC-NOTES items

| Item (crate SPEC-NOTES) | Evidence | New status |
|---|---|---|
| Carrier rotation numbering (`isdbt-ofdm`, "AMBIGUOUS") | gr-isdbt rotates by the absolute data-segment number (`rotated[(c − segment) mod nc]`, segment 0..12). The Rust default matches byte-exactly; `GroupRelative` fails every word. The S270 decoded gr-isdbt's partial-reception layer B. | confirmed (coherent groups) |
| 2-symbol frame alignment, Part 2 Fig. 3-8 (`isdbt-ofdm` latency) | gr-isdbt's `examples/tx_demo.grc` and the earlier gr-isdbt bench transmitter use `skiphead(2)` after the frequency interleaver. The S270 decoded that output with PER 0. Rust agrees in every row. | confirmed |
| Energy dispersal / puncturing reset at the OFDM frame (`isdbt-fec`) | gr-isdbt's receiver resets the PRBS at the TMCC-derived `frame_begin` tag. Cross-decoding is byte-exact in both directions. | confirmed |
| Mapping bit order b0.. (`isdbt-ofdm`) | Carrier-identical modulator output for QPSK, 16QAM and 64QAM. | confirmed (DQPSK not covered) |
| TMCC difference-set code bit order (`isdbt-ofdm`) | Parity agrees with gr-isdbt both ways, on two different information words. | agrees with an independent implementation; off-air check still pending |

These statuses were recorded in
[`crates/isdbt-ofdm/SPEC-NOTES.md`](crates/isdbt-ofdm/SPEC-NOTES.md)
(including the ["oracle (gr-isdbt) deviations"](crates/isdbt-ofdm/SPEC-NOTES.md#oracle-gr-isdbt-deviations)
section, which summarises D1–D6) and
[`crates/isdbt-fec/SPEC-NOTES.md`](crates/isdbt-fec/SPEC-NOTES.md). No code was changed. `cargo test
--release -p isdbt-fec -p isdbt-ofdm` passes, and clippy on both crates is
clean.

## Reproduction

The oracle tools are in `oracle/`. All data goes to `target/oracle-matrix/`
(override with `ISDB_RS_ORACLE_OUT`, which must be inside the repository so
that the container sees it):

| tool | purpose |
|---|---|
| `grisdbt_tx.py` / `grisdbt_rx.py` | gr-isdbt TX/RX (run inside the GNU Radio container). TX supports layer C. |
| `ts_compare.py` | anchors decoded packets on a unique source packet, then compares them in sequence |
| `iq_compare.py` | carrier-level diff of two cf32 files, with DBPSK bit attribution for TMCC/AC |
| `run_matrix.py` | the whole matrix (rows 1–8) |
| `rust_tx3/` | stand-alone crate, not a workspace member. `rust_tx3` is `tx_file` with a layer C; `rx_rot` runs the rotation-numbering check. |

The container (`arib-sdr` by default, `ISDB_RS_ORACLE_CONTAINER`) needs GNU
Radio 3.10 with gr-isdbt (tested: GNU Radio 3.10.9.2, gr-isdbt `56b2556`,
Ubuntu 24.04) and this repository mounted at `/work`.

```sh
# from the repository root
cargo build --release -p isdbt-phy --examples
cargo build --release --manifest-path oracle/rust_tx3/Cargo.toml --target-dir target/oracle
docker start arib-sdr

# Row 0 layout with your own per-layer TS files a_in.ts / b_in.ts
mkdir -p target/oracle-matrix/row0 && cd target/oracle-matrix/row0
D="docker exec -w /work/target/oracle-matrix/row0 arib-sdr python3 /work/oracle"
$D/grisdbt_tx.py --out gr_tx.cf32 --mode 3 --guard 8 --oneseg \
    --a 1,4,1,2 --b 12,64,2,2 --a-ts a_in.ts --b-ts b_in.ts
../../release/examples/rx_file gr_tx.cf32 rust_rx 2> rust_rx.log
python3 ../../../oracle/ts_compare.py a_in.ts rust_rx.a.ts
python3 ../../../oracle/ts_compare.py b_in.ts rust_rx.b.ts
../../release/examples/tx_file rust_tx.cf32 20 3 8 1 1,qpsk,2/3,2 a_in.ts 12,64qam,3/4,2 b_in.ts
for l in a b; do
  $D/grisdbt_rx.py --iq rust_tx.cf32 --layer $l --out grrx_rust.$l.ts \
      --mode 3 --guard 8 --oneseg --a 1,4,1,2 --b 12,64,2,2
  python3 ../../../oracle/ts_compare.py ${l}_in.ts grrx_rust.$l.ts
done
python3 ../../../oracle/iq_compare.py rust_tx.cf32 gr_tx.cf32 3 8 --skip 4
../../oracle/release/rx_rot gr_tx.cf32 data    # all clean
../../oracle/release/rx_rot gr_tx.cf32 group   # layer B all uncorrectable

# Rows 1-8 (about 2 minutes; results in target/oracle-matrix/<cfg>/, summary on stdout)
cd ../../..
python3 oracle/run_matrix.py            # or: python3 oracle/run_matrix.py m3g4p_bench
```

## Open questions

- An off-air capture of a real broadcaster is still the only independent
  check for the TMCC/AC parity bit order and for the DQPSK path. Japanese
  broadcasts use partial reception, so a capture would also re-confirm the
  rotation numbering. HackRF receive was out of scope here, and no off-air
  ISDB-T IQ capture has been tested.
- D3 (B67) and D1 (B17–B19) mean the bench transmitter that real receivers
  accepted was not TMCC-conformant. Those receivers have therefore not yet
  been shown to accept the spec-conformant TMCC that Rust sends. Expect
  that to be fine, but the first Rust over-the-cable test settles it.
  **Update 2026-10-08: settled for one configuration.** The Rust TX
  (`arib tx --mux-config`, spec-conformant TMCC and modulated AC) was
  received over a conducted link by a consumer USB tuner (MyGica S270; PER
  0; `arib ts validate` of its capture: 0 errors) and was registered by a
  channel scan and played on a Sony BDZ-series Blu-ray recorder (see the
  receiver compatibility notes in
  [`crates/arib-mux/SPEC-NOTES.md`](crates/arib-mux/SPEC-NOTES.md)).
  Remaining caveat: only Mode 3, GI 1/8, partial reception, layer A 1 seg
  QPSK 2/3 TI 2, layer B 12 seg 64QAM 3/4 TI 2 was tested on receivers.
  Other modes, GIs, DQPSK, three-layer and non-partial-reception TMCC have
  not been shown to a real receiver.
- Behaviour under impairments (AWGN, CFO, sample-rate offset, multipath)
  was not differentially tested.
