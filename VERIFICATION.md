# Verification

Every layer of `isdb-rs` has to be checkable without trusting the
implementation itself. The evidence is organised as a ladder; each rung
checks something the rung below cannot:

1. **Spec vectors.** Values printed in the specifications (check values,
   tables, PRBS initial states, constellation points, frame lengths, TSP
   counts) are asserted directly in tests named after the table
   (`spec_table_3_3`, `spec_fig_3_15_qpsk`, …).
2. **Lossless round trips.** Every decoder keeps every bit (reserved bits
   included), and `encode(decode(x)) == x` is property-tested (`prop_*`,
   proptest) on generated structures and arbitrary byte input, and run over
   synthetic multiplexes and, optionally, over your own capture.
3. **Inverse pairs and software loopback.** Each TX stage has its RX inverse
   tested as an identity (interleavers, mappers, codes), and the whole PHY
   runs TX → channel impairments (AWGN, CFO, sample offset, echo) → RX with
   a byte-exact comparison (`isdbt-ofdm`/`isdbt-phy` loopback tests,
   `isdb loopback`).
4. **Independent oracle.** gr-isdbt (GNU Radio) is run as a separate program
   and cross-decodes with the Rust chain in both directions
   ([below](#phy-differential-test-against-gr-isdbt)). It detects
   disagreement; the specification decides who is right.
5. **Real receivers.** Consumer receivers accept the Rust transmitter over a
   conducted link ([below](#real-receivers)).
6. **Broadcast transmitters.** The Rust receiver decodes real
   broadcasters' signals ([below](#broadcast-reception)), which checks the
   receive chain against transmitters this project did not build; their
   TMCC words are kept as test vectors. On the satellite IF recordings,
   ISDB-S is demodulated to the TMCC on all 21 transponders
   ([below](#isdb-s-frame-sync-and-tmcc)) and to RS-clean TS packets on
   the 19 with the SNR for it ([below](#isdb-s-main-signal-to-ts));
   ISDB-S3 is only surveyed
   ([below](#satellite-if-survey)).

Coverage per clause, with the rung that confirms it, is in
[SPECS.md §2](SPECS.md#2-coverage-matrix).

## Running the tests

```sh
cargo test --workspace --release        # everything that needs no data or hardware
cargo test --workspace --doc            # doctests, including every crate README
cargo test -p isdb-cli -p isdb-radio --no-default-features   # no radio backend, no libhackrf / libiio
# heavy tests, ignored by default
cargo test --release -p isdbt-ofdm --test loopback -- --ignored --nocapture
cargo test --release -p isdbt-fec viterbi_throughput -- --ignored --nocapture
cargo test --release -p isdb-cli --test cli -- --ignored mux_ts_producer_throughput --nocapture
```

Tests that read real data (a terrestrial capture, a recording, media files)
take the path from an `ISDB_RS_*` environment variable and print `SKIP`
and pass when it is unset, so a fresh checkout passes without data. The
variables, what each enables and how to obtain your own capture legally
are in [docs/test-data.md](docs/test-data.md).

## PHY differential test against gr-isdbt

Differential test of the Rust ISDB-T PHY (`isdbt-fec`, `isdbt-ofdm`, driven
through `isdbt-phy`) against gr-isdbt (GNU Radio 3.10.9.2, gr-isdbt commit
`56b2556`, run in the container built from
[`oracle/Dockerfile`](oracle/Dockerfile)). gr-isdbt is an oracle only
(rung 4 above): it can show that the two implementations disagree, and ARIB
STD-B31 v2.2 decides which one is right. No gr-isdbt code is included in
this repository; the scripts in [`oracle/`](oracle/) import it at run time.
The results below are from the run of 2026-10-08.

### Result

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
- **No discrepancy in the data path.** The four disagreements found
  (D1–D4) are all on the gr-isdbt side, as the B31 text shows. Receivers do
  not need any of them to decode (see each entry).
- **Ambiguous spec readings settled.** The carrier-rotation numbering, the
  2-symbol frame alignment and the frame alignment of energy dispersal and
  puncturing agree with gr-isdbt, whose output was in turn decoded by a
  consumer USB tuner (MyGica S270) with PER 0 over a conducted link (Mode 3,
  GI 1/4, partial reception, A 1 seg QPSK 1/2, B 12 seg 64QAM 3/4). Row
  `m3g4p_bench` below shows that the Rust chain is byte-exact and
  carrier-identical with that gr-isdbt configuration.

### Matrix

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
`b_in.ts`, 20 frames). It is a development result: the capture is not
distributed, so row 0 cannot be rerun as is; the reproduction below takes
your own per-layer files. Rows 1–8 use synthetic TS with a unique packet
index in each payload (`run_matrix.py`) and are the reproducible evidence.

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

Rerun on 2026-10-10 in the container built from `oracle/Dockerfile`
(`python3 oracle/run_matrix.py`, about 100 s): rows 1–3 and 5–8 reproduced
every packet count, RS total, TMCC result and carrier difference above.
Row 4 was byte-exact as well, but its Rust→gr direction compared 26164
packets with no mismatch and TMCC OK 9 / NOT OK 0, so neither D6 start-up
artefact appeared in that run.

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

### Discrepancies

All four are on the gr-isdbt side, and none required a change to Rust.

#### D1. TMCC segment-type bits B17–B19 on coherent segments
- gr-isdbt: `tmcc_encoder_impl.cc` sets B17, B18 and B19 to 1 on every
  segment ("Our implementation supports coherent modulation only"). That
  is the code for differential modulation.
- Rust: sends 000 on coherent segments and 111 on differential segments
  (`tmcc::segment_type_bits`).
- B31 §3.15.5 defines a 3-bit segment-type word: "“111” and “000” are
  assigned to this signal for a differential and coherent modulation,
  respectively."
- Resolution: Rust is correct. The Rust receiver tolerates gr-isdbt's
  value, since it detects segment kinds from the pilots. The USB tuner decoded
  gr-isdbt output, so that receiver ignores these bits. No change.

#### D2. AC carriers unmodulated in gr-isdbt
- gr-isdbt: `pilot_signals_impl.cc` writes the pilot value W_i on every AC
  carrier in every symbol. Differentially, that is information 0
  throughout.
- Rust: DBPSK with B'0 = W_i and stuffing 1 (`AcFrame::stuffing`).
- B31 §3.13.4: AC is DBPSK-modulated with its differential reference at
  the first symbol of the frame, and "If there is no additional
  information on broadcasting, information 1 is inserted as stuffing
  bits."
- Resolution: Rust is correct. No change.

#### D3. TMCC B67 (next-information partial-reception flag)
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
- Broadcast evidence ([Broadcast reception](#broadcast-reception)): every
  station sends B67 = 1; six send no next information (B67–B106 all 1),
  one repeats the current information, and its TMCC word equals the one
  `isdb tx` sends.

#### D4. Time-interleaving delay adjustment for I = 0
- gr-isdbt: in `time_interleaver_impl.cc`, the buffer for carrier i holds
  `I·m_i + 1 + sync_delay` with `sync_delay = 204 − ((95·I) % 204)`. That
  is a delay adjustment of 204 symbols (one whole frame) when I = 0. For
  I > 0 it equals Table 3-12 (for example 109 / 14 / 28 in Mode 3).
- Rust: adjustment from Table 3-12, which is 0 for I = 0.
- B31 Table 3-12 lists I = 0 with 0 delay-adjustment symbols and 0 delayed
  frames in Modes 1, 2 and 3. §3.11.1 requires the transmitter to correct
  each layer's delay by the Table 3-12 amount so that the total
  transmission and reception delay is a whole number of frames.
- Effect: an I = 0 layer from gr-isdbt is one frame late relative to the
  other layers. Row 5 shows it: gr→Rust anchors A at source frame 1 and B
  (I = 0) at frame 0, while the Rust loopback has both at frame 0. Each
  layer still decodes, because FEC frames stay aligned to OFDM frames.
  Only the inter-layer timing in the reproduced TS is affected.
- Resolution: Rust is correct. No change. `iq_compare.py --seg-offsets`
  compensates for this.

#### Oracle limitations observed (not disagreements)
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

### Agreements that settle pending SPEC-NOTES items

| Item (crate SPEC-NOTES) | Evidence | New status |
|---|---|---|
| Carrier rotation numbering (`isdbt-ofdm`, "AMBIGUOUS") | gr-isdbt rotates by the absolute data-segment number (`rotated[(c − segment) mod nc]`, segment 0..12). The Rust default matches byte-exactly; `GroupRelative` fails every word. The S270 decoded gr-isdbt's partial-reception layer B. A broadcast (partial reception, coherent) decodes RS-clean in both layers ([below](#broadcast-reception)). | confirmed (coherent groups), also against a broadcast transmitter |
| 2-symbol frame alignment, Part 2 Fig. 3-8 (`isdbt-ofdm` latency) | gr-isdbt's `examples/tx_demo.grc` and a gr-isdbt transmitter flowgraph built from it use `skiphead(2)` after the frequency interleaver. The S270 decoded that output with PER 0. Rust agrees in every row, and decodes a broadcast RS-clean. | confirmed, also against a broadcast transmitter |
| Energy dispersal / puncturing reset at the OFDM frame (`isdbt-fec`) | gr-isdbt's receiver resets the PRBS at the TMCC-derived `frame_begin` tag. Cross-decoding is byte-exact in both directions. | confirmed |
| Mapping bit order b0.. (`isdbt-ofdm`) | Carrier-identical modulator output for QPSK, 16QAM and 64QAM. | confirmed (DQPSK not covered) |
| TMCC difference-set code bit order (`isdbt-ofdm`) | Parity agrees with gr-isdbt both ways, on two different information words. Every TMCC frame of seven broadcasters passes the Rust parity check as received (174 frames; [below](#broadcast-reception)), and one broadcaster's word equals the Rust encoder's. | confirmed, also against broadcast transmitters (no seismic AC word was available) |

These statuses are recorded in
[`crates/isdbt-ofdm/SPEC-NOTES.md`](crates/isdbt-ofdm/SPEC-NOTES.md)
(including the ["oracle (gr-isdbt) deviations"](crates/isdbt-ofdm/SPEC-NOTES.md#oracle-gr-isdbt-deviations)
section, which summarises D1–D6) and
[`crates/isdbt-fec/SPEC-NOTES.md`](crates/isdbt-fec/SPEC-NOTES.md).

### Reproduction

The oracle tools are in [`oracle/`](oracle/). All data goes to `target/oracle-matrix/`
(override with `ISDB_RS_ORACLE_OUT`, which must be inside the repository so
that the container sees it):

| tool | purpose |
|---|---|
| `grisdbt_tx.py` / `grisdbt_rx.py` | gr-isdbt TX/RX (run inside the GNU Radio container). TX supports layer C. |
| `ts_compare.py` | anchors decoded packets on a unique source packet, then compares them in sequence |
| `iq_compare.py` | carrier-level diff of two cf32 files, with DBPSK bit attribution for TMCC/AC |
| `run_matrix.py` | the whole matrix (rows 1–8) |
| `rust_tx3/` | stand-alone crate, not a workspace member. `rust_tx3` is `tx_file` with a layer C; `rx_rot` runs the rotation-numbering check. |

The container is built from [`oracle/Dockerfile`](oracle/Dockerfile)
(Ubuntu 24.04, GNU Radio 3.10.9.2, gr-isdbt `56b2556` built from upstream)
and must have this repository mounted at `/work`. It is named
`isdb-oracle` by default (`ISDB_RS_ORACLE_CONTAINER` overrides the name);
see [docs/test-data.md](docs/test-data.md#the-gr-isdbt-oracle).

```sh
# from the repository root
cargo build --release -p isdbt-phy --examples
cargo build --release --manifest-path oracle/rust_tx3/Cargo.toml --target-dir target/oracle
docker build -t isdb-oracle oracle/      # once; later just: docker start isdb-oracle
docker run -d --name isdb-oracle --user "$(id -u):$(id -g)" -v "$PWD":/work isdb-oracle

# Row 0 layout with your own per-layer TS files a_in.ts / b_in.ts
mkdir -p target/oracle-matrix/row0 && cd target/oracle-matrix/row0
D="docker exec -w /work/target/oracle-matrix/row0 isdb-oracle python3 /work/oracle"
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

## Real receivers

The Rust transmitter (`isdb tx --mux-config`, spec-conformant TMCC and
modulated AC, multiplex from `isdbt-mux`) was received over a conducted link
(HackRF One or PlutoSky R2 → coax → attenuators → receiver):

- **MyGica S270** (USB ISDB-T tuner): decoded with PER 0; `isdb ts validate`
  of its 12 s capture reported 0 errors (5 warnings).
- **A Sony BDZ-series recorder**: a channel scan registered the service and
  the recorder played it, once the digital copy control descriptor (0xC1)
  carried copy_control_type '01' in its user_defined bits. With '00' the
  recorder refuses playback. `isdbt-mux` therefore defaults to '01' and
  `isdb ts validate` warns (`si.copy_control`) on '00'.

- **Sony BDZ-ZW1500, data broadcasting (2026-10-12)**: with a local
  station's recording re-emitted under that station's own identifiers on
  its own channel (shielded bench), the recorder ran its BML browser on the
  station's relayed carousel and, with the carousel replaced by
  `isdbt-mux`'s `bml-carousel` carrying `examples/bml/startup.bml`,
  displayed our page over the picture. Under the placeholder network
  (0x79EE) or on another channel it never started the browser (d mark
  only, or "channel suspended"), including for the station's own
  carousel relayed unchanged: the gate is the receiver's channel memory,
  not the carousel. A service made entirely by this project (FFmpeg
  MPEG-2 video and AAC, the two example pages with remote-key navigation
  and a page transition) under the same identity worked the same way;
  the same identity on another channel gave "channel suspended" with no
  picture or sound. The generated Hybridcast launcher page loads and runs
  its script on it: its `getBrowserSupport('ARIB', 'APIGroup',
  'AITControlledApp.Start')` check reports no support, and the page says
  "This receiver has no Hybridcast engine"; starting the application
  itself needs a Hybridcast-capable TV.

- **A TV, from a PlutoSky R2 (2026-10-21)**: `isdb tx --mux-config
  bench.toml` (with `channels = [44]`) `--radio
  pluto:uri=ip:…,channel=2 --channel 44 --gain att=60 --rf-enable`,
  Tezuka v0.3.24, TX2 into the same conducted chain as the HackRF
  (fixed attenuators, DC block). The TV showed the test picture and
  played its tone. It received at `att=60` and `att=80`, not at `att=85`.
  Over Gigabit Ethernet the producer ran at 4.3× real time with 0
  underruns in 8.1 s on air; 1.7·10⁻⁵ of the components clipped at the
  default 10 dB backoff. The audio was the same non-conforming ADTS file
  (MPEG-4 ID '0', no CRC, fullness 0x7FF, sent with `adts_compliance =
  "warn"`), so this does not close the conforming-AAC item.

Only one configuration was shown to these receivers: Mode 3, GI 1/8, partial
reception, layer A 1 segment QPSK 2/3 TI 2, layer B 12 segments 64QAM 3/4
TI 2. This also shows that these receivers accept the spec-conformant TMCC
bits where gr-isdbt deviates (D1, D3). The receiver-specific findings and how
to reproduce the setup are in
[docs/transmitting.md](docs/transmitting.md#receiver-compatibility-notes)
and in the `isdbt-mux`
[receiver compatibility notes](crates/isdbt-mux/SPEC-NOTES.md#receiver-compatibility-notes).

## Broadcast reception

Real broadcasts were demodulated with `isdb rx --radio pluto` and from
IQ captures (2026-10-20): the seven terrestrial stations of one region in
Japan, from a cable (CATV) feed that retransmits the terrestrial channels
on their own channel numbers, into RX1 of a PlutoSky R2 (AD9361) over
Gigabit Ethernet at 512/63 MHz (Tezuka firmware v0.3.24; see
[below](#receiving-hardware) for why the radio's firmware matters). The
stations are not named here, as elsewhere in this project; their TMCC
words are in `crates/isdbt-ofdm/tests/broadcast.rs`.

All seven send the same configuration: Mode 3, GI 1/8, partial
reception, layer A 1 segment QPSK 2/3 time-interleave index 3, layer B
12 segments 64QAM 3/4 index 2.

- **TMCC, seven stations.** In 174 frames (6 s from each station) no
  frame failed the (184,102) difference-set parity check and none needed
  correction, so the parity bit order (B20 as the highest-degree
  coefficient, §3.15.6.9) is the broadcasters'. The sync word alternates
  w0/w1 on every frame pair, the segment type B17–B19 is 000 (coherent)
  everywhere, and the information is system 00, no switching (1111), no
  emergency alarm, phase-shift correction 111 and reserved bits all 1,
  identical in every frame. Taking each station's word and flipping any
  8 bits (200 patterns), the majority-logic decoder restores it every
  time.
- **Next information.** Six stations send "no next information": B67–B106
  all 1, which §3.15.6.4 and Tables 3-28..3-31 ("111 … or when there is
  no next information") define. One station repeats the current
  information, as `isdb tx` does; its TMCC word equals the one `isdb tx`
  transmits for the same configuration, bit for bit, parity included.
  Either way B67 is 1, which settles D3: gr-isdbt's B67 = 0 is the
  deviation.
- **AC.** No station sends a seismic-motion-warning word: the AC
  carriers of segment 0 carry configuration identification 111 and all-1
  stuffing on every station. Two stations send other data on AC
  carriers outside segment 0 (configuration identification 000, data on
  the AC1 carriers of the lowest-frequency segment, unchanged from frame
  to frame). Table 3-33 makes 000 and 111 "additional information on the
  transmission control of modulating wave", whose bit assignment §3.16.5
  leaves free, so this content cannot be checked against the standard.
- **Both layers RS-clean.** On the station received best, 12 s of signal
  (50 frames, 46 valid: the first frames after lock prime the
  deinterleavers) gave layer A 2944 packets, all clean, and layer B
  119 232 packets: 119 228 clean, 4 with one corrected byte each, 0
  uncorrectable; MER 21.6 dB in both layers, carrier offset
  +0.32 carriers, timing drift −0.006 samples per symbol. This exercises,
  against a broadcast transmitter, the rotation numbering of coherent
  segments, the two-symbol frame alignment, time, frequency, bit and byte
  deinterleaving, Viterbi, energy dispersal and RS.
- **Reproduced TS.** That station's single TS (§3.2.2) has 0 sync
  losses, 0 continuity-counter errors and 0 transport_error_indicator
  flags on all 32 PIDs, and its PCRs fit a constant 29.958294 Mbit/s with
  at most 84 ns residual (no discontinuities), so the re-multiplexed slot
  order and the null-packet positions are the broadcaster's. PAT, PMTs,
  NIT, SDT, EIT p/f and schedule and TDT/TOT parse with valid CRCs. The
  full-segment services are scrambled, as expected (every PES packet);
  the one-segment service is not, and its H.264 video and AAC PES parse
  (32 video PES over 10.3 s of PTS).
- **`isdb ts validate` on that TS**: 199 errors, all on scrambled PIDs
  (PES headers hidden by scrambling; section CRC and assembly errors on
  the three scrambled data-carousel PIDs) except one: the one-segment
  programme's PCR is sent once per OFDM frame, every 0.231 s, against
  H.222.0 §2.7.2's 0.1 s. PAT/PMT repeat about every 0.101 s, at the edge
  of the B10 Table 5-2 operational criterion; EIT schedule events carry
  an event_group_descriptor without a short_event_descriptor. These are
  the broadcaster's practice, not reception errors.
- **Other stations' payload.** Every station's TMCC and one-segment layer
  decode, but on this feed two stations give layer B MER ≈ 16.5 dB (all
  layer-B packets uncorrectable) whatever the RX gain (20–40 dB) and a
  third 18.1 dB (partly decodable). Since the same chain reaches 21.6 dB
  on the station above, the limit is in those channels on this feed, not
  yet identified.

### Receiving hardware

With the PlutoSky R2's factory firmware no channel locked: its 40 MHz
VCTCXO is tuned only by an ADF4001 charge pump that stays active without
a 10 MHz reference and drags the oscillator, which put about 1 rad of
common phase jitter between OFDM symbols at UHF (measured as the spread
of the per-symbol guard-interval phase: 0.19 carriers, against ≤ 0.009
carriers for this estimator on a simulated signal at 0 dB SNR). Tezuka
v0.3.22 and later three-state the pump without a reference; the spread
fell to 0.001–0.003 carriers and the channels locked. The radio and the
backend's own tests are described in
[docs/transmitting.md](docs/transmitting.md#adalm-pluto-and-plutosky-r2).

## Satellite IF survey

The satellite IF of the same cable feed (RX1 of the PlutoSky R2, Tezuka
v0.3.24) was recorded with `isdb capture` and surveyed with `isdb iq
survey` on 2026-10-20: every BS right-hand-circular transponder (12) and
every 110° CS transponder (12). This rung checks the capture path, the
channel tables and which system each transponder carries; the ISDB-S
frame sync and TMCC are in [their own section](#isdb-s-frame-sync-and-tmcc). The feed carries no left-hand-circular IF
(2.2–3.2 GHz: nothing).

**Capture length and contiguity.** One libiio buffer holds at most
16 515 072 samples (63 MiB; 16 531 456 fail with `ENOMEM` in iiod, with
1 or 4 kernel blocks), 282 ms at 57.72 Msps; Tezuka's maia-sdr recorder
holds 33 554 432 (128 MiB), 581 ms, and returns in about 5.5 s including
the download. Both are one DMA transfer. On recordings at 61.44 Msps,
where the |z|² line of the 28.86 Mbaud signal and its image are
resolved, the line's phase advances uniformly across the whole capture:
in 2^16-sample chunks it deviates at most 0.084 rad (maia, 546 ms) and
0.068 rad (libiio, 269 ms) from its mean step, while one lost sample
would move it by 2.95 rad. A loss of a multiple of 1024 samples would
move it by only 0.004 rad (R/fs·1024 is almost an integer) and the
z⁸ carrier line is too noisy to see it (its phase moves 0.5 rad rms per
1 ms chunk: the feed's conversion phase noise, ×8); the ISDB-S frame
sync of the demodulator settles that: every ISDB-S recording keeps exact
39 936-symbol frame spacing throughout ([below](#isdb-s-frame-sync-and-tmcc)). At 57.72 Msps (2 samples per
symbol) the line and its image coincide and this check does not apply.
The two methods agree in level (−10.9 dBFS on BS1 with the AGC at
33 dB), which confirms their full scales (2048 and 32768).

**Channel tables.** Every band centre measured lies within 1.3 MHz of
the `isdb-cli` `bands` IF table (BS-n 1049.48 + 19.18·(n − 1) MHz, ND-n
1613 + 20·(n − 2) MHz), and every transponder shows the same carrier
offset, −112 to −116 kHz (the feed's conversion), so the tables locate
each transponder.

**Classification** (4 Mi samples of each recording; dB above the median
of the searched bins; ISDB-S at 57.72 Msps where the ppm sign is
ambiguous, the rest at 61.44 Msps):

| Transponders | Symbol line | Strongest carrier line | System |
|---|---|---|---|
| BS1, 3, 5, 9, 13, 15, 19, 21, 23 | 28.86 Mbaud, +47…+49 dB | z⁸, +33…+41 dB (z⁴ +21…+27 dB): 8PSK main signal | ISDB-S |
| BS7, 11, 17 | 33.7561 Mbaud (+0.6 ppm), +32 dB | z² at +R/2, +27 dB (π/2-shift BPSK), confirmed by z⁴ | ISDB-S3 |
| ND2 … ND20 (10) | 28.86 Mbaud (−0.3…+0.3 ppm), +47…+50 dB | z⁴, +49…+57 dB (z⁸ +25…+43 dB): QPSK main signal | ISDB-S |
| ND22, ND24 | 28.86 Mbaud, +38…+39 dB | z⁴, +31…+33 dB | ISDB-S (about 10 dB lower SNR: the feed's tilt) |

The −3 dB widths are 23–26 MHz (BS ISDB-S), 25–31 MHz (CS) and 32–33 MHz
(ISDB-S3); the ISDB-S spectrum is not flat at the top on this feed, so
the −3 dB width is an estimate of the occupied band, not a roll-off
measurement.

Survey findings that shaped `isdb_iq::survey`: on a BS IF recording the
z⁸ line's copy at 8Δf + R was stronger than the line itself in one 73 ms
stretch (so z^k lines are searched within ±R/2), and in ISDB-S3
recordings an unrelated line of the same strength as the π/2-BPSK line
lies in the −R/2 window (so an offset counts when a second line kind
confirms it). The opt-in tests `isdb-iq` `tests/captures.rs`
(`ISDB_RS_ISDBS_IQ`, `ISDB_RS_ISDBS3_IQ`) pass on ND2, ND22, ND24, BS1
(maia and libiio recordings) and BS7, 11, 17.

## 8-bit captures and streaming over Gigabit Ethernet

Measured on 2026-10-20 with the same PlutoSky R2 (Tezuka v0.3.24, iiod
0.26; host libiio 0.26) on the same feed, receive only. The question was
how long a capture can be, and whether ISDB-S / ISDB-S3 can be streamed
live, without new FPGA code.

**Sample formats.** Tezuka's bitstream packs 8-bit samples when
`voltage0` is the only `cf-ad9361-lpc` channel enabled (`{Q8, I8}` per
16-bit word, maia-hdl `cs12_cs8mux.v`), and 12-bit samples with sync
bursts when `voltage1` is; maia-sdr's recorder has `IQ16bit`, `IQ12bit`
and `IQ8bit` modes. On BS1 at 40 Msps, against 16-bit captures of the
same signal:

| Path | Datatype | Power | Carrier offset | DC mean (8-bit LSB) |
|---|---|---|---|---|
| libiio 16 bits (full scale 2048) | `ci16_le` | −11.0 dBFS | −103.56 kHz | +0.003 |
| libiio 8 bits (Tezuka packing, full scale 128) | `ci8` | −11.0 dBFS | −103.55 kHz | +0.004 |
| maia `IQ16bit` (full scale 32768, low 4 bits always 0) | `ci16_le` | −11.0 dBFS | −103.82 kHz | +0.005 |
| maia `IQ12bit` (downloaded sign-extended, 2048) | `ci16_le` | −11.1 dBFS | −103.77 kHz | – |
| maia `IQ8bit` (full scale 128) | `ci8` | −10.8 dBFS | −103.24 kHz | −0.465 |

The same power gives the full scales; the same sign of the offset shows
that I is the first byte; the means show that Tezuka's 8-bit packing
rounds (with first-order error feedback, per its HDL) and maia-sdr's
8-bit mode truncates.

**maia-sdr recorder length.** Every mode stopped at exactly 128 MiB
(33 554 432 / 44 739 242 / 67 108 864 samples in 16 / 12 / 8 bits),
although the device tree reserves 256 MiB and
`/sys/class/maia-sdr/maia-sdr-recording/device/recording_size` reads
0x10000000: maia-hdl's `maia_iio_lite` configuration, which Tezuka
builds, ends the recorder's DMA at 0x0E000000 ("size only 0x8000000").
`maximum_duration` in maia-httpd's `/api` is the duration last
requested, not a capacity. A longer one-shot capture therefore needs a
bitstream built with a larger recorder range, not only a larger
reservation. The longest contiguous recordings are now 1.68 s (8 bits at
40 Msps); one libiio buffer holds 33 030 144 8-bit samples (63 MiB).

**40 Msps is enough for both systems.** With the AD9361's own filters at
40 Msps, the BS1 spectrum matches the 57.72 Msps one within 0.5 dB out
to ±16 MHz and within 2.5 dB at ±18–19 MHz, where it is already 16–22 dB
down (analog bandwidth 38–56 MHz made no difference). `isdb iq survey`
on full 8-bit maia recordings at 40 Msps: BS1 ISDB-S 28.86002 Mbaud
(line +45.7 dB), carrier −112.2 kHz from z⁸ (+42 dB), z⁴ and z²; BS7
ISDB-S3 33.75612 Mbaud (+24.7 dB), carrier −112.1 kHz from both
π/2-BPSK z² lines (+28 dB). At 40 Msps the two ±R/2 search windows met
across fs/2, so each saw both lines and the +R/2 one reported +3.0 MHz;
the windows are now kept apart (`isdb-iq` `pi2_bpsk_carrier_line` at
61.44 and 40 Msps). The opt-in `isdb-iq` capture tests pass on these
recordings (BS1: symbol clock continuous over 67 108 864 samples,
largest deviation 0.109 rad against 4.53 rad for one lost sample; BS7:
0.619 rad against 5.30; the libiio 8-bit buffer: 0.073 rad).

**Link throughput.** 40 Msps of 8-bit samples is 80 MB/s; the link does
not carry it:

| Path (PlutoSky → host, Gigabit Ethernet) | Rate |
|---|---|
| `iio_readdev` 0.26 over iiod | ≈ 38 MB/s at most, behind real time even at 8 MB/s offered |
| `isdb-iio` `start_rx_raw` over iiod (1 Mi-sample buffers, 4 kernel blocks) | ≈ 63–66 MB/s at most |
| on the board: `iio_readdev` (local, mmap) to `/dev/null` | real time at 80 MB/s, almost no CPU |
| on the board: `iio_readdev` piped into `socat` TCP | real time at 32 and 40 MB/s |
| on the board: `dd if=/dev/zero` piped into `socat` / busybox `nc` TCP | 45.6 / 29.7 MB/s, one CPU at 100 % system time |

The Zynq-7000 GEM has no TSO and its `maxmtu` is 1518 (no jumbo frames),
so the board's CPU and TCP stack, not the cable, set the ceiling; a
direct cable instead of a switch does not raise it. Streamed captures
(`isdb capture --radio pluto:…,recorder=stream,bits=8 --discard`) on
BS1 and BS7:

| Rate (8-bit samples) | MB/s | Duration | Losses reported | Samples discarded |
|---|---|---|---|---|
| 10 Msps | 20 | 20 s | 0 | 0 |
| 15 / 18 / 20 / 22 Msps | 30 / 36 / 40 / 44 | 30 s each | 0 | 0 |
| 20 Msps (BS1) | 40 | 300 s | 0 | 0 |
| 24 Msps (BS1) | 48 | 300 s | 9 | 142 606 336 (2.0 %) |
| 28 Msps (BS1) | 56 | 300 s | 7 | 114 294 784 (1.4 %) |
| 32 Msps (BS7) | 64 | 300 s | 14 | 476 053 504 (5.0 %) |
| 40 Msps (BS1, BS7) | 80 | 60 s | continuous after the first 2–4 Mi samples | all (≈ 65.5 MB/s arrived) |

16-bit samples at 512/63 MHz (32.5 MB/s, the ISDB-T rate, T27) streamed
60 s without loss. Losses above 40 MB/s come at random (single 30 s runs
at 28 and 32 Msps had none), so "loss-free" holds up to about 40 MB/s on
this board with its default services running.

**The loss detector.** The ADC core's overflow bit (`cf-ad9361-lpc`
register 0x80000088, bit 2, write 1 to clear) is sticky and also set
whenever no DMA transfer runs, so it is cleared once the stream is
running and read after every buffer. It was checked against an exact
reference: Tezuka's 12-bit mode replaces 8 samples every 262 144 with a
20-byte magic and a 32-bit counter, so a counter step other than 1 is a
loss. Streaming that mode through the same `isdb-iio` path (a scratch
program, not in the repository):

| 12-bit rate | MB/s offered / arrived | Counter jumps (losses) | Overflow flags | Jumps with no flag in the 15 buffers before | Largest flag-to-jump lag |
|---|---|---|---|---|---|
| 10 Msps, 20 s | 30 / 29.9 | 0 | 0 | 0 | – |
| 20 Msps, 30 s | 60 / 59.8 | 2 | 2 | 0 | 5 buffers |
| 22 Msps, 30 s | 66 / 62.7 | 38 | 239 | 0 | 12 buffers |
| 25 Msps, 30 s | 75 / 62.5 | 370 | 823 | 0 | 5 buffers |

Every loss was flagged before its data arrived, at most 12 buffers
earlier (the queue between the DMA and the reader is 4 kernel blocks,
iiod, the network and 8 host buffers); flags without a counter jump are
losses that did not swallow a sync burst. So a streamed capture drops the
buffer that finds the bit and the next 14, and what it writes after a
reported loss is contiguous again (`isdb-radio` README).

**Consequence.** Live ISDB-S / ISDB-S3 from this Pluto over Gigabit
Ethernet does not fit as samples (≥ 78 / 70 MB/s in 8 bits against
≈ 40 MB/s loss-free), nor as 8-bit symbols at one per symbol (58 / 68
MB/s); it needs fewer bits leaving the board (soft bits narrower than
8 bits, or decoded packets) or a network path that does not go through
the CPU. Offline work uses one-shot recordings (1.68 s at 40 Msps).
(Measured with Tezuka's services running; with them stopped the ceiling
is about 72 MB/s, and 5- or 6-bit symbols fit: next section.)

## Symbol front end: IQ to symbols on the CPU and in the FPGA

Measured and built on 2026-10-20 (PlutoSky R2, Tezuka v0.3.24, the same
satellite IF, receive only) to stream ISDB-S / ISDB-S3 continuously:
the front end of `isdb-frontend` (mixer, polyphase root-raised-cosine
matched filter, non-data-aided timing recovery, AGC, n-bit output) as a
bit-exact fixed-point model and the same stage in Amaranth HDL
(`fpga/`).

**Link ceiling with Tezuka's services stopped.** Tezuka starts a DATV
stack (`pluto_stream`, the `zynqrx` DVB-S2 receiver, MQTT, GPS and NFS
helpers). With those stopped (board ≈ 99 % idle, no QSPI interrupts;
`/mnt/jffs2/autorun.sh` on this board stops them at every boot),
streamed captures (`isdb capture --radio pluto:…,recorder=stream,bits=8
--discard`, BS1):

| 8-bit rate | MB/s | 30 s | 300 s |
|---|---|---|---|
| 20 Msps | 40 | no loss | no loss |
| 24 / 28 Msps | 48 / 56 | no loss | 28 Msps: 2 losses (while the host compiled and ran analyses) |
| 32 Msps | 64 | no loss | no loss |
| 36 Msps | 72 | no loss | no loss |
| 40 Msps | 80 | losses from the first buffers | – |

16-bit samples: no loss up to 16 Msps (64 MB/s); losses at 18 Msps
(72 MB/s). A raw TCP send from the board with the tools on it (`dd` into
`socat` with 64 KiB buffers): 59.8 MB/s with one stream (58 % of the two
CPUs), 69.5 MB/s with two streams pinned to the two CPUs (89 %); `socat`
with its default 8 KiB buffer reaches 32.7 MB/s. UDP through `socat`
(one datagram per write) stays below 21 MB/s. This host's interface has
an MTU of 1460, so 1500-byte frames from the board are dropped (TCP
adapts through its MSS). So the board's CPU sets a ceiling near 70 MB/s
whatever the transport, below the 78 MB/s that 8-bit IQ for ISDB-S needs
((1 + 0.35)·28.86 Msps).

**Sample rate and analog bandwidth.** The 40 Msps recordings of the
previous section used `isdb capture`'s default analog bandwidth,
0.8 × the rate (32 MHz), which cuts the band edges. 8PSK
decision-directed MER of BS1 after the front end (32 taps):

| Recording | 8PSK MER |
|---|---|
| 57.72 Msps 16-bit (46 MHz) / 61.44 Msps 16-bit (49 MHz) | 14.86 / 15.23 dB |
| 40 Msps 8-bit, 56 MHz | 14.94 dB |
| 40 Msps 8-bit, 40 MHz | 13.86 dB |
| 40 Msps 8-bit, 32 MHz (maia and libiio) | 13.01 / 13.02 dB |
| 36 Msps 8-bit, 56 MHz (fits the link as IQ) | 13.99 dB |

**Front end on the recordings** (`isdb-frontend` `examples/analyze.rs`,
40 Msps, 56 MHz):

- **ISDB-S (BS1):** the timing loop settles at +0.68 ppm (the board's
  clock offset). The fixed-point and floating-point outputs agree within
  0.01 dB. MER against taps: 8 / 12 / 16 / 24 / 32 taps give
  13.75 / 14.33 / 14.65 / 14.86 / 14.94 dB.
- **ISDB-S3 (BS7, roll-off 0.03):** the non-data-aided timing
  detector's slope shrinks with the roll-off:
  - at a loop bandwidth of 2·10⁻⁴ R it wanders (−265 ppm with 32 taps);
  - at 2·10⁻⁵ to 10⁻⁶ R it locks at +0.68 ppm with a period jitter of
    3·10⁻⁴ to 6·10⁻⁵ samples;
  - ring MER barely changes from 24 to 64 taps.

  The constellation is not identified without TMCC (three-ring fit
  shares 0.18 / 0.33 / 0.49), so its MER is not given.
- **Output quantiser** (no clipping):

  | Bits | ISDB-S 8PSK MER change | ISDB-S3 ring MER change | ISDB-S / ISDB-S3 MB/s |
  |---|---|---|---|
  | 4 | −1.12 dB | −0.45 dB | 29.0 / 33.9 |
  | 5 | −0.32 dB | −0.07 dB | 36.2 / 42.4 |
  | 6 | −0.07 dB | −0.01 dB | 43.4 / 50.8 |

  Chosen then: 6 bits for ISDB-S, 5 for ISDB-S3 (≈ 43 MB/s, 60 % of
  the ceiling). Superseded on 2026-10-25 (below): the ISDB-S3 figures
  were taken through an untrimmed filter at about 9 dB, where the
  quantiser's cost hides; ISDB-S3 now uses 6 bits.

The opt-in tests `isdb-frontend` `tests/captures.rs` (`ISDB_RS_ISDBS_IQ`,
`ISDB_RS_ISDBS3_IQ`) check this on any recording: timing lock within
5 ppm of the survey, fixed point within 0.1 dB of floating point,
ISDB-S MER above 10 dB, and a lossless read-back of the words.

**FPGA implementation.** `fpga/` (Amaranth 0.5.10):

- **Simulation:** reproduces the model bit for bit. Every symbol's
  phase, filter outputs, gain, period, scaled outputs, output components,
  timing and AGC errors, and every output word are compared on:
  - recordings of BS1 (32 taps, 2887 symbols) and BS7 (64 taps,
    2532 symbols);
  - synthetic signals (16 and 32 taps);
  - with sample strobes 4–7 cycles apart;
  - with the coefficients loaded through the port;
  - with the whole configuration sent as the host's `gpio_out` command
    words (ID word and a register readback checked).

  One-LSB changes to a coefficient, a loop gain, the AGC target or the
  NCO step make the comparison fail, so it is not vacuous.
- **Resources:** Yosys `synth_xilinx` estimates 46 DSP48E1, 3.6k LUT,
  4.5k FF and 9 RAMB18 for 32 taps, beside Tezuka's 106 / 220 DSP and
  23k / 53k LUT.
- **Not done:** a bitstream, timing closure and a hardware test (they
  need Vivado; `fpga/README.md` lists the steps). On the current
  bitstream, `isdb symbols --probe` reads `up_adc_gpio_in` = 0: no front
  end. `isdb symbols` falls back to the CPU on one contiguous capture
  (0.2 s of BS1: 8PSK MER 14.84 dB, words read back without loss).
- **Configuration cost:** an iiod register access takes 0.62 ms (500
  reads), so configuring 32 taps (about 4,250 commands) takes about
  2.6 s (format 1; format 2 below: about 6,360 commands, 3.9 s).

### Timing trim and output width (format 2, 2026-10-25)

ISDB-S3 recordings of BS7, BS11 and BS17 at 61.44 Msps with 56 MHz
analog bandwidth (the DC-block-free set of 2026-10-25), measured with
the SNR on the 24 known π/2-BPSK synchronisation symbols of every
9 296-symbol modulation slot (`isdb iq s3sync`; ARIB STD-B44 v2.1
Fig. 3.3-1). The format-1 front end gave 8.8–9.5 dB against an in-band
SNR of about 17 dB from the spectrum.

- **Loss budget** (`isdb-frontend` `examples/s3budget.rs`): the timing
  loop locks −0.153 / −0.165 / −0.181 symbol (±0.02 rms) from the
  instant of best SNR found per slot from the sync symbols, steadily over
  each recording and the same at 40 and 61.44 Msps, 8 and 16 bits, 32 to
  56 MHz analog bandwidth and on the older 2026-10-20 recordings. That
  costs 5.1–6.4 dB. A brick-wall filter against the neighbouring
  transponders changes nothing, 32 instead of 128 taps costs 0.4 dB at
  ideal timing, jitter (0.004 symbol rms at 10⁻⁶ R) and fixed point
  under 0.3 dB together. On noiseless synthetic signals (QPSK, and
  ISDB-S3 slot structure with 8PSK or 16APSK data) the detector locks
  within ±0.05 symbol (`examples/tedbias.rs`): the offset comes from the
  received pulse, upstream of the front end. ISDB-S shows the same
  effect at −0.05 … −0.10 symbol.
- **Fix:** format 2 adds an output bank `k` (the matched filter a trim
  later inside the same window); the output and the AGC use it, the
  timing detector still `h` and `g`. The host measures the trim on
  0.1–0.15 s of IQ (`s3sync::best_trim` on the sync words,
  `isdbs_phy::frontend::measure_trim` on the ISDB-S bursts) and
  `isdb symbols` / `isdb iq symbols` apply it (`--trim auto`, the
  default).
- **Result,** whole recordings, 32 taps:

  | | Trim | Untrimmed | Trimmed | 6-bit words read back |
  |---|---|---|---|---|
  | BS7 | −0.168 | 9.31 dB | 14.65 dB | 14.57 dB |
  | BS11 | −0.165 | 9.46 dB | 14.67 dB | |
  | BS17 | −0.180 | 8.83 dB | 14.58 dB | |

  Per-slot ideal timing reaches 15.4 / 14.7 / 14.7 dB: the trim recovers
  all but 0.1–0.8 dB of the timing loss; the rest of the gap to the
  spectrum's estimate is intersymbol interference for an equaliser on
  the host.
- **Modulation:** the data symbols after the trim form two rings with
  shares 0.26 / 0.74, 4 angle clusters on the inner and 12 on the outer
  ring: 16APSK (4 + 12). Ring ratio 2.2–2.3, below STD-B44 Table 3.8-2's
  γ ≈ 2.7–3.0, as Annex A describes for a saturated transponder (outer
  points pulled inward). The code rate needs the TMCC.
- **Output width** (the bit-exact model's own quantiser, trimmed):

  | Bits | 16APSK ring MER change | Sync Es/N0 change | ISDB-S3 MB/s |
  |---|---|---|---|
  | 4 | −0.6 dB | −1.0 dB | 33.9 |
  | 5 | −0.16…−0.19 dB | −0.27 dB | 42.4 |
  | 6 | −0.04 dB | −0.07 dB | 51.0 |
  | 7 | −0.01 dB | −0.02 dB | 59.4 |

  Nothing reaches full scale at any width. Chosen: 6 bits for ISDB-S3 as
  for ISDB-S: 51.0 MB/s, 73 % of the 70 MB/s ceiling. With 14.6 dB the
  16APSK rates up to about 7/9 (12.3 dB in STD-B44 Table A2.2-4) keep a
  2 dB margin; 5 bits would spend 0.2–0.3 dB of it.
- **ISDB-S with the trim** (2026-10-25 set, `tools/isdbs-*-acceptance.sh`):
  burst MER +0.1 … +2.3 dB on all 21 transponders (median 1.3 dB, ND2
  12.6 → 14.9 dB); every frame header and TMCC RS-clean, 0 bytes
  corrected; TS acceptance unchanged (38 TSs, 319 584 packets, 0
  uncorrectable, 0 continuity errors), corrected packets 85 → 33. BS1
  8PSK decision-directed MER 16.4 dB.
- **FPGA:** the format-2 HDL (54-bit `h | g | k` coefficient words, a
  third accumulator pair, AGC and quantiser on `zk`) is bit-exact with
  the model on four vector sets (two with a trim), on trimmed recording
  vectors of BS7 and BS1, through the port load with irregular strobes
  and through `gpio_out`; one LSB off in `k` fails. Yosys: 64 DSP48E1,
  3.9k LUT, 4.6k FF, 1 RAMB18 + 8 RAMB36 at 32 taps (170 / 220 DSP
  beside Tezuka's build). Configuration is about 6,360 commands (3.9 s).
- **Opt-in test:** `isdb-frontend` `tests/captures.rs`
  `isdb_s3_trim_recovers_the_sync_snr` (`ISDB_RS_ISDBS3_IQ`, a 56 MHz
  recording): trim measured on 0.1 s, more than 3 dB gain, above 13 dB,
  6 bits within 0.15 dB.

### Format 3: CRC-32C trailer (2026-10-26)

- **Change:** the block trailer becomes two words, the standard CRC-32C
  (CRC-32/ISCSI) of the sequence number (4 bytes, little endian) and the
  payload words (little endian); the arithmetic is version 2's. The
  reader accepts version-3 blocks on the CRC alone; versions 1 and 2
  keep the CRC-16 plus the framing check (`isdb-frontend` README, "The
  `Symbols` boundary").
- **Spec values:** check value `0xE3069283` on "123456789"; the
  slicing-by-8 CRC equals a bitwise one on every length 0–63 at both
  alignments; the trailer equals the CRC-32C of its byte string. The HDL
  word step equals a bit-serial register on 303 random states and words.
- **Loss handling:** the version-3 property test (random cut position
  and length, 7 widths) delivers every untouched block, including the
  one right before a loss; the version-2 property test keeps its
  framing exception and its recorded seeds. The two found cuts replay
  correctly on both. Fault injection: with the trailer check skipped the
  version-3 property test fails (minimal `bits = 2, cut_at = 9, cut_len =
  1`) and a grid of 208 payload cuts is caught 148 times.
- **CLI:** a 4 500-block version-3 file of BS17 (`isdb iq symbols`)
  reads back with 0 loss events and the same 14.51 dB sync Es/N0 as the
  version-2 file of the same recording, which still reads; with 1 000
  words cut out mid-stream, exactly the 2 touched blocks are lost (no
  framing cost).
- **Host speed** (`examples/crcspeed`, one core, 7950X): CRC-32C 2 831
  MB/s against 173 MB/s for the bitwise CRC-16 (link 50.6 MB/s); the
  `Reader` on a clean stream 558 MB/s (v3) against 137 MB/s (v2).
- **FPGA:** bit-exact on the regenerated default vector set, trailers
  included; a missing final XOR fails at the first trailer word. Yosys:
  +279 LUT and +17 FF over format 2 (4 175 LUT, 4 583 FF, 64 DSP48E1).
  The CRC's widest output bit is a 22-input XOR (two LUT6 levels).
- **Decoded output unchanged:** ISDB-S3 BS7/11/17 and ISDB-S BS1/ND2
  byte-identical to the previous build (`tools/rx-compare.sh`); ISDB-S
  acceptance 38 TSs, 319 584 packets, 33 corrected, 0 uncorrectable.

### Format 4: one sample per clock, and the PlutoSky R2 image (2026-10-26)

The block's clock on the PlutoSky R2 was measured on the board
(read-only): `cf-ad9361-lpc` register 0x80000054 (`adc_clk_count`) =
0x1999A → l_clk = 160.0 MHz; register 0x80000044 bit 2 (`r1_mode`) = 0
on both AD9361 cores; Tezuka's plutoskyr2 build is LVDS, so
`util_ad9361_divclk` (the block's clock) is l_clk / 4 = 40 MHz = the
sample rate. The format-3 HDL assumed four clocks per sample. Format 4:

- **Model** (`isdb-frontend`): loop delay 24 symbols (was 6) and the
  detector banks `h`, `g` non-zero on their centre 8 taps only (the output
  bank `k` keeps all 32). ISDB-S3 trimmed sync-symbol Es/N0 on the
  2026-10-25 BS7 / 11 / 17 recordings: loop delay 6 → 24: 14.65 / 14.67 /
  14.58 → 14.69 / 14.71 / 14.58 dB; detector on 32 → 8 centre taps
  (`examples/tedspan.rs`): 14.69 → 14.89 (BS7), 14.69 → 14.91, 14.59 →
  14.78 dB. Through the CLI (`isdb iq s3sync`, 6 bits): 14.79 / 14.81 /
  14.69 dB; live 40 Msps BS7 13.93 dB. A detector on every 2nd or 4th
  symbol only (time-shared multipliers) was tried and rejected: 1.5–8 dB
  with cycle slips on ISDB-S3 (`examples/loopdiag.rs`).
- **Receivers unchanged or better:** ISDB-S acceptance 38 TSs, 319 584
  packets, 33 corrected, 0 uncorrectable, 0 CC, validate 0 errors (as
  before); ISDB-S3 BS7 / 11 / 17 LDPC and BCH clean, equalised sync Es/N0
  18.0 / 18.0 / 17.3 dB (was 17.7 / 17.7 / 17.0), main MER 15.7 / 15.9 /
  15.3 dB. Decoded streams differ from format 3's (the loop changed), as
  expected.
- **HDL** (`fpga/`): fully pipelined, a sample every clock. Bit-exact
  against the format-4 vectors with a sample on every clock, with
  `util_wfifo`'s bursts of eight, with irregular strobes and the
  coefficient load port, configured through `gpio_out`, and on 30 000
  samples of live BS7 (25 317 symbols) and BS1 (14 092 symbols); one LSB
  off in `k` or in a centre tap of `g` fails the comparison.
- **Image:** Tezuka v0.3.24 plutoskyr2 with the block and without the
  DVB-S2 transmitter, Vivado 2026.1: WNS +0.100 ns, WHS +0.022 ns, ADI's
  timing check passes; 178 / 220 DSP48E1 (the block 112), 21 694 LUT,
  28 421 FF, 62 BRAM36. `isdb_top.bin` 2 785 408 bytes, sha256
  `479141e02d8a151dd8475ac196b658beee7820ad9c298123191688aaa3ed1fd3`.
  **Not yet run on the board** (deployment and the on-board checks:
  [fpga/README.md](fpga/README.md)).

## ISDB-S frame sync and TMCC

`isdb iq tmcc` (front end → `isdbs-phy` frame sync, burst-aided carrier,
TMCC Viterbi, `isdbs-core` energy dispersal and RS(64,48)) was run on
every ISDB-S recording of the 2026-10-20 satellite IF set
(`tools/isdbs-tmcc-acceptance.sh`; the opt-in test
`isdbs-phy` `tests/captures.rs` with `ISDB_RS_ISDBS_TMCC_IQ` runs the same
checks). BS1 is the 40 Msps 8-bit recording with 56 MHz analog bandwidth
(0.42 s), the other BS ones 57.72 Msps (0.58 s), the CS ones 61.44 Msps
(0.55 s). TS_IDs are not station identities; no service is named.

| Transponder | Frames (headers with intact sync words) | Slips | Superframes, RS(64,48) ok | Bytes corrected | Pilot MER | Combination | TS_IDs (slots) |
|---|---|---|---|---|---|---|---|
| BS1 | 303 (303) | 0 | 37 / 37 | 0 | 14.7 dB | TC8PSK 2/3 × 48 | 4010, 4011, 4012 (16 each) |
| BS3 | 419 (419) | 0 | 52 / 52 | 0 | 14.0 dB | TC8PSK 2/3 × 48 | 4030 (24), 4631, 4632 (12 each) |
| BS5 | 419 (419) | 0 | 52 / 52 | 0 | 14.8 dB | TC8PSK 2/3 × 48 | 4450, 4451 |
| BS9 | 420 (420) | 0 | 51 / 51 | 0 | 14.8 dB | TC8PSK 2/3 × 48 | 4090, 4092 |
| BS13 | 419 (419) | 0 | 51 / 51 | 0 | 14.0 dB | TC8PSK 2/3 × 48 | 40D0, 40D1, 46D2 |
| BS15 | 419 (419) | 0 | 52 / 52 | 0 | 14.5 dB | TC8PSK 2/3 × 48 | 40F1, 40F2, 48F3 |
| BS19 | 419 (419) | 0 | 51 / 51 | 0 | 14.0 dB | TC8PSK 2/3 × 48 | 4730, 4731, 4732, 4733 |
| BS21 | 419 (419) | 0 | 51 / 51 | 0 | 14.4 dB | TC8PSK 2/3 × 48 | 4750, 4751, 4752 |
| BS23 | 419 (419) | 0 | 52 / 52 | 0 | 14.3 dB | TC8PSK 2/3 × 48 | 4770, 4971, 4973 |
| ND2 | 394 (394) | 0 | 48 / 48 | 0 | 13.0 dB | QPSK 3/4 × 48 | 6020 (36) |
| ND4 | 394 (394) | 0 | 48 / 48 | 0 | 12.8 dB | QPSK 3/4 × 48 | 7040 |
| ND6 | 394 (394) | 0 | 49 / 49 | 0 | 13.0 dB | QPSK 3/4 × 48 | 7060 |
| ND8 | 394 (394) | 0 | 48 / 48 | 0 | 12.1 dB | QPSK 3/4 × 48 | 6080 |
| ND10 | 394 (394) | 0 | 48 / 48 | 0 | 12.5 dB | QPSK 3/4 × 48 | 60A0 |
| ND12 | 393 (393) | 0 | 48 / 48 | 0 | 11.9 dB | QPSK 3/4 × 48 | 70C0 |
| ND14 | 394 (394) | 0 | 49 / 49 | 0 | 11.6 dB | QPSK 3/4 × 48 | 70E0 |
| ND16 | 393 (393) | 0 | 48 / 48 | 0 | 9.8 dB | QPSK 3/4 × 48 | 7100 |
| ND18 | 394 (394) | 0 | 49 / 49 | 0 | 9.3 dB | QPSK 3/4 × 48 | 7120 |
| ND20 | 394 (394) | 0 | 48 / 48 | 0 | 8.2 dB | QPSK 3/4 × 48 | 7140 |
| ND22 | 394 (367) | 0 | 48 / 48 | 5 | 0.7 dB | QPSK 3/4 × 48 | 7160 |
| ND24 | 394 (374) | 0 | 48 / 48 | 1 | 1.0 dB | QPSK 3/4 × 48 | 7180 |

- **Every superframe of every transponder decodes**, with one TMCC per
  transponder throughout the recording: combination counts summing to 48
  in whole slot units, unused TS_ID entries FFFFh, emergency flag 0,
  uplink information 0010 (BS) / 0100 (CS). This is the evidence for the
  TMCC information layout, which the binding text (MIC Notification 304
  of 2011) would otherwise settle (`isdbs-core` SPEC-NOTES "tmcc-layout").
- **Frame layout and burst data.** The coded W1 tail sits at header
  symbols 12–31 and the W2 (frame 1) / W3 tail at 172–191 of every frame;
  all 30 720 burst symbols of 40 frames (BS1, ND2) are BPSK of the
  X⁹+X⁴+1 PRBS alone, restarted per frame. The spacing of consecutive
  frames is exactly 39 936 symbols across every recording (0 slips), so
  none of these captures lost samples, including multiples of 1024 that
  the |z|² check above cannot see.
- **Pilot MER** is the MER of the burst symbols after the carrier
  estimate, against their BPSK points: a true SNR, unlike the decision-
  directed figures of the front end. It falls along the CS band (ND2 13 dB
  to ND20 8 dB) with the feed's tilt, and ND22 / ND24 are at about 1 dB
  (their burst symbols project 6070 of 8111 rms on the expected sign),
  near the limit of QPSK 3/4: their TMCC still decodes (BPSK r = 1/2 and
  RS(64,48)), their main signal is not expected to (Phase 3).
- **Sync detector.** The 20-symbol W1 tail alone scores 0.99 on frames of
  the strong transponders, but random data reaches 0.75–0.84 somewhere in
  every frame length and ND22's frames score 0.01–0.6. Correlating all
  808 known symbols of a frame (tails and bursts, coherently within each
  group): frames ≥ 0.81 on every transponder, random data ≤ 0.55 over
  20 000 offsets, ±1 symbol ≈ 0.47 (examples `syncstat`, `offsets`).
- Main-signal decision MER (8PSK on BS, QPSK on CS) is within 0.3 dB of
  the pilot MER where the SNR is high enough for decisions (BS1 14.4 dB
  against 14.7; ND2 12.8 against 13.0).

## ISDB-S main signal to TS

`isdb rx --system isdbs` on the same recordings, one TS per TS_ID, each
checked with `isdb ts inspect` and `isdb ts validate`
(`tools/isdbs-ts-acceptance.sh`, 2026-10-25; release build). The
recordings are 0.42–0.58 s long: 37–52 superframes each, which at the
TMCC's slot counts give 4 700–14 100 packets per TS. TS_IDs are
transponder numbering, not station identities.

| Transponder | TS_ID: slots/frame, packets | RS corrected / uncorrectable | CC errors | PCR fit (max) | scrambled |
|---|---|---|---|---|---|
| BS1 (40 Msps) | 4010: 16, 4736 · 4011: 16, 4736 · 4012: 16, 4736 | 0 / 0 | 0 | 33–55 ns | 88–98 % |
| BS3 | 4030: 24, 9984 · 4631: 12, 4992 · 4632: 12, 4992 | 0 / 0 | 0 | 42–54 ns | 71–90 % |
| BS5 | 4450: 24, 9984 · 4451: 24, 9984 | 0 / 0 | 0 | 35–52 ns | 79–92 % |
| BS9 | 4090: 18, 7344 · 4092: 14, 5712 · unassigned: 16 | 0 / 0 | 0 | 39–68 ns | 89–96 % |
| BS13 | 40D0: 16 · 40D1: 16 · 46D2: 16, 6528 each | 0 / 0 | 0 | 27–45 ns | 74–98 % |
| BS15 | 40F1: 22, 9152 · 40F2: 12 · 48F3: 12, 4992 each · unassigned: 2 | 0 / 0 | 0 | 29–57 ns | 79–82 % |
| BS19 | 4730–4733: 12 each, 4896 each | 0 / 0 | 0 | 28–45 ns | 84–88 % |
| BS21 | 4750: 14 · 4751: 12 · 4752: 14 · unassigned: 8 | 0 / 0 | 0 | 30–66 ns | 87–93 % |
| BS23 | 4770 · 4971 · 4973: 12 each, 4992 each · unassigned: 12 | 0 / 0 | 0 | 21–47 ns | 79–88 % |
| ND2–ND12 | one TS each: 36 slots, 13 824–14 112 packets | 0 / 0 | 0 | 11–16 µs | 67–89 % |
| ND14, ND16, ND18, ND20 | one TS each: 36 slots, 13 824–14 112 packets | 1, 1, 4, 6 / 0 | 0 | 12–14 µs | 87–91 % |
| ND22, ND24 | 36 slots, 13 824 | — / 13 824 (all) | — | — | — |

- **19 of 21 transponders give RS-clean TS**: 311 520 packets, 12
  corrected (on the weaker CS channels) and none uncorrectable; 0
  continuity errors and 0 transport_error_indicator in every TS. ND22 and
  ND24 (pilot MER ≈ 1 dB) decode their TMCC but not their main signal,
  as expected for QPSK 3/4 at that SNR: a re-capture with more gain or
  another feed is needed for them.
- **All 21 after removing a DC block (2026-10-25).** The cause of the
  low SNR of ND22 / ND24 was a DC block in the RX1 feed: re-captured
  without it (`tools/isdbs-recapture.sh`, same settings as above; DC on
  the cable checked absent first), ND22 rose from 0.7 to 7.8 dB pilot MER
  and ND24 from 1.0 to 7.5 dB, every frame header decodes, and both give
  RS-clean TS (14 112 packets each; 24 and 51 corrected, 0
  uncorrectable). Over the 21 recordings: 38 TSs, 319 584 packets, 85
  corrected, 0 uncorrectable, 0 continuity errors, 0
  transport_error_indicator, `isdb ts validate` 0 errors. Only the top of
  the band changed: BS1–BS23 (1.05–1.48 GHz) +0.0…+0.7 dB, ND2–ND20
  (1.61–1.97 GHz) −0.4…+0.6 dB (the spread between captures), ND22 and
  ND24 (2.01 and 2.05 GHz) about +7 dB, so this block cut sharply near
  2 GHz rather than losing a little everywhere. Without a DC block the
  AD9361 input is exposed to any LNB supply that a receiver on the same
  feed switches on later; a replacement should be specified to 2.5–3 GHz
  and re-checked with the script.
- **The TMCC's TS_IDs are right**: the PAT transport_stream_id of every
  TS equals the TMCC's TS_ID for its slots. Slots whose relative TS ID
  names an unused (FFFFh) entry (BS9, 15, 21, 23) carry null packets only
  and are not written.
- **PSI/SI and scrambling**: every TS carries PAT and PMTs; SDT, EIT, TOT,
  NIT and CAT appear where their repetition interval fits in the 0.6 s
  recording. 67–98 % of the packets are scrambled (video, audio and
  some data; transport_scrambling_control ≠ 00, with ECM PIDs in the
  PMTs): recognised and left as they are (no descrambling, project
  policy). `isdb ts validate` reports 0 errors on every decoded TS.
- **PCR**: the BS TSs fit a constant packet rate within 21–68 ns, so the
  decoder adds no timing error. The CS TSs deviate 11–16 µs from a CBR
  fit (4 programs, PCR every 66.7 ms with ±0.2 ms spread): their measured
  TS rate (39.13 Mbit/s) equals 36 slots × 188 bytes per 1.3838 ms frame,
  and the continuity counters run clean, so this is the broadcaster's
  multiplexing, not a decoding error.
- **Constellation readings** that the texts leave open were settled by
  RS success (`examples/mapprobe`, `isdbs-core` SPEC-NOTES): TC8PSK takes
  C1 = 133₈ and C0 = 171₈ (BO.1408-1 Fig. 15 (W, Y, X); the reverse gives
  0 of 19 968 packets on BS3); QPSK puts P1 on I (annexed table 46 text;
  figure `-086` draws the other assignment); no spectrum inversion. The
  dummy slots of a unit follow its valid slots (all 36 sent slots of the
  CS transponders decode), and the interleaver follows BO.1408-1 Fig. 6.
- **validate on satellite TSs** misfired before this work: it parsed
  scrambled payloads as sections and PES (hundreds of CRC, pointer and
  PES-start errors), and reported mandatory tables and CAT as missing
  from captures shorter than their repetition interval. Both are fixed
  (`isdb-analyze`: scrambled payloads are not parsed; such absences are
  Info), with synthetic tests.
- **Throughput** (BS3, 0.581 s of signal, 32-core host): front end
  1.6 s (0.36× real time, one thread), frame sync / carrier / TMCC 0.95 s
  (0.61×, one thread), main signal → TS 0.16 s (3.7×, every core; 7.5
  Msym/s per core). Reception of a recording runs at about 0.2× real
  time; the sequential front end and carrier tracker are the limits
  (`examples/throughput`). From a radio, `isdb rx` takes one contiguous
  capture after another (over Gigabit Ethernet ISDB-S cannot be streamed
  without losses), so the TS has a gap between blocks; continuous
  reception needs the FPGA symbol front end.

## ISDB-S3 receiver

`isdb rx --system isdbs3` (or `auto`, from the survey) on the 61.44 Msps,
56 MHz recordings of BS7, BS11 and BS17 of 2026-10-25 (DC block removed;
`isdb capture --freq BS7 --rate 61.44M --bw 56M`), one TLV stream file
per stream id, each checked with `isdb tlv inspect` (release build). The
recordings are 0.55 s long: 15–16 whole frames of 33.05 ms. Stream ids
are transponder numbering, not station identities.

| Transponder | TMCC | Sync Es/N0 → equalised | Main MER | LDPC / BCH | TLV streams: slots, packets | Head-indication mismatches | MPU with scramble info |
|---|---|---|---|---|---|---|---|
| BS7 | 15/15; 16APSK 93/120 × 120 | 14.65 → 17.72 dB | 15.57 dB | 1 440 / 1 440 clean, 2.01 it | B070: 40, 1 514 · B071: 40, 1 518 · B072: 40, 1 445 | 0 | 98.8–98.9 % |
| BS11 | 16/16; same | 14.68 → 17.67 dB | 15.69 dB | 1 536 / 1 536 clean, 1.98 it | B0B0: 40, 1 616 · B0B2: 40, 1 618 · unassigned: 40 (null packets) | 0 | 98.8–98.9 % |
| BS17 | 16/16; same | 14.56 → 17.02 dB | 15.21 dB | 1 536 / 1 536 clean, 2.09 it | B110: 40, 1 643 · B111: 40, 1 584 · B112: 40, 1 695 | 0 | 98.8–98.9 % |

- **Every frame and every codeword decodes.** The TMCC decodes on every
  whole frame (0 LDPC iterations, 0 BCH corrections at 14.6 dB) and says
  16APSK, code 0111, in all 120 slots, three relative streams of type TLV
  with 40 slots each (BS11's relative stream 1 is unassigned: type FFh,
  id FFFFh, its slots carry TLV null packets). All 4 512 main-signal
  codewords of the 47 whole frames are LDPC- and BCH-clean with 0 BCH
  corrections.
- **Readings settled on the signal** (`isdbs3-core` SPEC-NOTES): the
  B44 energy-dispersal generators output their **last stage**, not the
  feedback node (80 readings tried after the TMCC LDPC converged; only
  this one is BCH-clean); Table 3.11-2's code 0111 = 93/120 (the
  amended numbering; the 93/120 LDPC code decodes the main signal) and
  its printed "1010 = 101/120" is read as 105/120; the frame head is the
  odd slot whose FSync correlation has the opposite sign to the other 59
  of its parity; the BCH word of a slot is header + data + parity, with
  the dispersal (which also covers the stuffing) removed first; the
  16APSK labels of Fig. 3.8-1 and the forward read-out of the 4-column
  interleaver at 93/120; the 32 pilot symbols carry the slot's
  constellation in label order with the pilot PRBS's 0°/180° signs. The
  LDPC address tables were transcribed from the standard by
  `tools/b44-ldpc-tables.py` (2.0-E1 and 2.1-E1 give identical tables),
  and a spec test checks that Table 3.4-1's BCH polynomials are the
  minimal polynomials of α¹ … α²³.
- **Transponder compression**: the pilot-learned 16APSK points have an
  inner/outer radius ratio of 2.51 against γ = 2.87 for 93/120, each
  label within 3.6° of its nominal angle (`examples/pilotcheck`).
  Demapping against them adds 0.7 dB of MER.
- **Equaliser**: an 11-tap symbol-spaced least-squares equaliser trained
  on each frame's 6 720 sync and pilot symbols removes the intersymbol
  interference the timing trim leaves: sync Es/N0 +2.5 to +3.1 dB (to
  17.0–17.7 dB, the in-band SNR estimated from the spectrum) and main
  MER +1.3 to +1.7 dB on the data symbols, which were not used to train
  it; LDPC iterations fall from about 3.0 to 2.0 (`examples/eqgain`).
  Main tap 1.05, the others ≤ 0.10.
- **TLV**: 12 633 TLV packets in the 8 assigned streams, 0
  head-indication mismatches, 0 bytes skipped, 0 IP errors; every UDP
  payload (header-compressed IP, context 1, and 15–16 IPv6 packets per
  stream) is an MMTP packet. The 160 undefined bits of every slot header
  are ones.
- **MMT-SI**: PA messages carrying MPT and PLT on every stream; M2
  section messages with MH-EIT (p/f and schedule), MH-SDT (actual,
  other), MH-TOT, MH-AIT, ECM and EMM; data transmission messages.
  ISO/IEC 23008-1 is not available, so only what ARIB STD-B32 Part 3 and
  STD-B60 define is parsed. No TLV-SI (TLV-NIT, AMT) appears in these
  0.55 s recordings; the TLV-SI section finder is tested on synthetic
  sections only.
- **Scrambling**: 98.8–98.9 % of the MPU payloads carry the MMTP
  scramble-information extension (B60 Table 6-9 type 0001h; its content
  is in ARIB STD-B61, not available), and ECM / EMM are present:
  recognised and left as they are (no descrambling, project policy).
- **Is any of it unscrambled? No (2026-10-26, `isdb-tlv` example
  `freeca`).** Carrying the extension does not by itself mean
  scrambled, and the signalling says the opposite: every MH-SDT service
  (8) and MH-EIT event (106) on BS7/11/17 has `free_CA_mode` 0 ("not
  scrambled"), with `running_status` 0 ("undefined") — fields the
  broadcasters leave at 0. The media itself decides: the payload of all
  14 media MMTP packet ids reads 7.98–8.000 bits per byte of entropy,
  like the known-scrambled ISDB-S PID 0x0140 on BS1 (TS
  scrambling_control set: 8.000), while clear 8K AV1 video reads 6.65
  and the clear signalling 2.77. The scramble extension's first byte
  differs by stream (0xE0, 0xF0, 0xF8) and alternates 0xE0 / 0xF8 in
  equal halves on the busiest video stream, as even / odd key switching
  would (B61 not available to confirm). No service on these three
  transponders was found in the clear.
- **ISDB-S unchanged**: `tools/isdbs-ts-acceptance.sh` on the same day's
  21 ISDB-S recordings still gives 38 TSs, 319 584 packets, 0
  uncorrectable.
- **Throughput** (BS11, 32-core host, `examples/ldpcspeed`), against the
  2 905 codewords per second (100 Mbit/s of slot data) of 16APSK 93/120 ×
  120:

  | LDPC decoder | 1 thread | 32 threads |
  |---|---|---|
  | float reference (layered normalised min-sum) | 166 cw/s | 2 742 cw/s = 0.94× |
  | fixed-point, 16 SIMD lanes, stable, x86-64 baseline (the default) | 4 678 cw/s | 33 076 cw/s = 11.4× |
  | same, built with `-C target-cpu=x86-64-v3` (AVX2) | 5 739 cw/s | 35 602 cw/s = 12.3× |
  | same, nightly `std::simd` back end, AVX2 | 6 153 cw/s | 37 754 cw/s = 13.0× |

  The fixed-point decoder (`isdb-fec` `ldpc::fixed`) gives each codeword
  the same bits and iterations with 16 lanes as with one: on generated
  codewords (`isdb-fec` `tests/ldpc_lanes.rs`) and slot for slot on
  recordings (`isdbs3-phy` `tests/captures.rs`
  `isdbs3_ldpc_simd_equals_scalar_on_recordings`). Against the float
  reference on live-path recordings (40 Msps 8-bit libiio, about 1 dB
  below the 61.44 Msps ones): 5 against 6 failed of 9 216 codewords, 2.59
  against 2.53 mean iterations; neither recovers more failures past 50
  iterations (5 at 50, 100 and 200 for fixed; float 6 at 50, 5 at 200),
  so the residual failures need more SNR, not more iterations
  (`examples/ldpcfixed`). The whole receiver on a 1.65 s live-path
  recording: 37.8 s before, 19.9 s now (0.083× real time): the trim
  search and carrier trial run their candidates on threads (10.6 → 1.3 s
  per block); the single-threaded front end (0.33× real time) and slot
  sync (1.7 s per 0.83 s block, run twice) are the bottleneck now
  (`examples/chaintime`). From a radio `isdb rx` takes one contiguous
  capture after another.
- **Carrier choice** (2026-10-25): a live 8-bit BS7 block decoded no TMCC
  (24 of 48 frames) because the survey's +R/2 window held an unrelated
  line 0.3 dB stronger than the π/2-BPSK line, and the strongest line
  was taken: 361 kHz off, sync Es/N0 5.5 dB. The carrier is now the
  candidate the front end locks on best (sync Es/N0 on 0.05 s, lines
  that agree within 2 kHz grouped): 48 of 48 frames, 13.6 dB
  (`examples/blockcarrier`). A second case (2026-10-25, plan §0.10): on
  the second block of the live BS11 recording a candidate 48 kHz off won
  by 0.6 dB of sync Es/N0 and decoded no TMCC (24 of 48 frames over the
  recording). A 24-symbol sync word hardly rotates at that offset and
  each slot's phase is measured anew, so the sync Es/N0 cannot tell, but
  the phase then turns +1.6 rad from one SSync slot to the next (the
  right carrier: ±0.01 rad). Candidates whose mean phase step exceeds
  0.5 rad now rank below the others: every segment of the three live
  recordings acquires on its own with every frame's TMCC
  (`isdbs3_every_segment_acquires`, `ISDB_RS_ISDBS3_LIVE_IQ`;
  `examples/blockacq`).

## Satellite receive speed

Plan §0.10 (2026-10-25): the stages that kept `isdb rx --system isdbs3`
and `--system isdbs` below real time, made faster without changing their
output.

**Bit-exactness.** The front end the receivers run is
`isdb-frontend` `fast::FastFrontend`; `fixed::Frontend` stays the
reference the FPGA matches. They give the same `Symbol`s (every field):
property tests over random designs, widths, trims, rates, input
amplitudes up to full-range `i16` and arbitrary chunking
(`tests/fast.rs`), and on recordings (`ISDB_RS_ISDBS3_IQ`,
`ISDB_RS_ISDBS_IQ`: 3.4 M and 1.9 M symbols identical). The FPGA
vectors (`mise run fpga:vectors`, made with the reference) are
byte-identical to those of the previous build and `mise run fpga:test`
passes. Everything else that was parallelised computes each value as
the sequential code did (`position_mags_equal_corr_bit_for_bit`,
first-maximum searches split between threads). Decoded TLV streams and
TS files, and the receivers' summaries, are byte-identical to the
previous build on BS7/11/17 (61.44 Msps), BS1 (40 Msps), BS3, BS9, ND2
and ND22 (`tools/rx-compare.sh`), and `tools/isdbs-ts-acceptance.sh`
still gives 38 TSs, 319 584 packets, 33 corrected, 0 uncorrectable.
Recordings of several blocks change after the first block: later blocks
reuse the first block's acquisition (below).

**One 0.826 s ISDB-S3 block** (live-path recording, 40 Msps, 32-thread
host; real time = 0.826 s):

| Stage | Before | After | What changed |
|---|---|---|---|
| front end | 2.57 s (one thread) | 1.13 s (one thread) | linear delay line, fused banks, 16-bit products in `i32` on split coefficients, width as a const generic (`FastFrontend`); symbols handed to a closure |
| slot sync | 1.74 s, run twice | 0.17 s once + 0.07 s | the per-position sync statistic computed once in vectorised form on every core; no second search after the equaliser (`synchronise_at`) |
| carrier choice + trim | 1.26 s every block | 0.83 s on the first block; later blocks verify the first block's in 0.05 s | `symbols_with_prior` |
| equaliser | 0.21 s | 0.04 s | chunks on every core |
| TMCC, main symbols, demap, LDPC, BCH | 2.7 s (frame by frame) | 0.36 s | every frame prepared in parallel, all codewords of the block in one pool (`decode_frames`) |
| receive (whole) | 5.85 s | 0.60 s | |

**ISDB-S** (BS1, 1.68 s at 40 Msps): 16.5 s → 2.9 s. The trim search ran
its 11 candidates (each a front end and a receive) one after another
(7.6 s); they now run at once (0.65 s), and frame acquisition and the
per-frame derotation share out their work.

**Whole chain, recordings.** `isdb rx` reads, runs the front end and
receives at once (`isdb-cli` `satpipe`), up to three blocks' front ends
at a time (each block depends only on its samples and the first block).

| Recording | Before | After |
|---|---|---|
| ISDB-S3, 1 block, 0.546 s at 61.44 Msps | 6.9–7.2 s | 2.3–2.4 s |
| ISDB-S3, 2 blocks, 1.65 s at 40 Msps | 19.4–20.0 s | 4.2–4.3 s |
| ISDB-S3, 12 blocks, 9.9 s at 40 Msps (live blocks repeated) | — | 10.9 s (0.91×; one front end at a time: 0.52×) |
| ISDB-S, 1 block, 1.68 s at 40 Msps | 16.1–16.5 s | 2.9 s |
| ISDB-S, 1 block, 0.546 s at 61.44 Msps | 10.3–13.3 s | 1.5–1.7 s |

A single contiguous block is bounded by its front end, one sequential
stream (the timing loop): 1.13 s per 0.826 s of ISDB-S3 at 40 Msps
(0.73× real time) on this host at the x86-64 baseline, about 0.9 s with
`-C target-cpu=native`. Splitting one stream between threads would
change its output at the seams, so it is not done.

**Live** (receive only, PlutoSky R2, libiio 8-bit captures of 0.826 s at
40 Msps, 5 s of signal): ISDB-S3 BS7, 7 blocks: 168/168 TMCC, 16 128
slots, 13 LDPC failures, front end 1.33 s and receiver 0.60 s per block;
ISDB-S BS1, 7 blocks: 516 superframes, 198 144 packets, 0
uncorrectable, front end 0.93 s and receiver 0.29 s per block. A block
arrived every 2.05 s, which is the capture alone (`isdb capture
--discard`: 2.03 s per block, the single-buffer transfer at 32.5 MB/s):
the decoder keeps up with the blocks the radio delivers. The signal
between captures is not received: continuous reception needs a link
that carries the samples (≈ 70 MB/s loss-free over iiod, below 40 Msps
8-bit) or the FPGA symbol path, which is designed but not deployed.

**SIMD dispatch and the front end (plan §0.11, 2026-10-25).**

- *Runtime AVX2 dispatch without `unsafe`*: not achievable for these
  kernels. A call into a `#[target_feature]` function from code without
  the feature is `unsafe` on stable 1.99 and nightly 1.101 alike
  (E0133). `pulp` 0.22 hides that call behind a safe token, but a kernel
  inlined into its `#[target_feature]` op is unrolled into scalar code
  by LLVM (front end 2.8–3.9 s per block instead of 1.0 s), a kernel
  left out of line does not get the op's features (no gain), and our
  own `#[target_feature]` kernel cannot be called from the generic op
  without `unsafe`. AVX-512 in pulp needs Rust 1.89 (MSRV is 1.88). The
  wider instructions come from a build with `-C target-cpu=native`.
- *LDPC*: the iteration in its own function, 4 615 → 5 380 codewords/s
  per thread (A/B in one session, BS11), same results.
- *Front end*: a chunked mixer (whole chunk mixed into the delay line,
  then the symbol loop) was 11 % faster through `process` on both
  systems but 7–13 % slower in the ISDB-S receiver's acquisition
  (`process_each` with its callback) and 4 % slower over a 6-block
  ISDB-S file; it was reverted (`1664f09`). The front end stays the
  single-stream limit: ISDB-S3 0.99 s per 0.826 s block (0.83× real
  time), ISDB-S 1.88 s per 1.68 s at 40 Msps (0.89×).
- *Whole pipeline on long files* (`tools/sigmf-repeat.py`: the live
  BS7 recording ×6 = 12 blocks, 9.91 s; BS1 40 Msps ×6 = 6 blocks,
  10.07 s): ISDB-S3 0.92× → 0.96× real time with the LDPC change
  (27 648 slots, 12 LDPC failures before and after); ISDB-S 1.27× real
  time.

**AVX2 / AVX-512 front end (plan §0.14, 2026-10-26).** With the user's
approval the front end got run-time dispatch with exactly one `unsafe`
call (`isdb-frontend` `simd::run`, right after detecting the loop's CPU
features; the workspace's `unsafe_code = "deny"` is unchanged, the
exception is named in the workspace `Cargo.toml`). Kernels: `vpmaddwd`
via `std::arch` inside `#[target_feature]` functions, the loop around
them shared. Default AVX2; AVX-512 on request (`ISDB_SIMD=avx512`;
needs Rust ≥ 1.89, probed by `build.rs`), because it is slower on this
Zen 4.

- *Bit-exactness*: `tests/fast.rs` runs every instruction set the host
  supports (baseline, AVX2, AVX-512) through `process` and
  `process_each`: property tests, every padded width 8 … 64, the wide
  input kernel, recordings (live BS7 3 375 613 and BS1 1 878 908 symbols
  identical on all three). Fault injection: ±1 in one lane of the AVX2
  or of the AVX-512 kernel fails 3–4 tests. Decoded output
  byte-identical to `c03a2aa` (`tools/rx-compare.sh`): ISDB-S3 BS7, BS11,
  BS17 (61.44 Msps) and live BS7; ISDB-S BS1, BS3, ND2, ND22 and BS1 at
  40 Msps. ISDB-S acceptance: 38 TSs, 319 584 packets, 0 uncorrectable.
- *Front end, one core* (`examples/feisa.rs`, best of 3, load ≈ 3.5),
  through `process_each` (what the receivers call): ISDB-S3 0.826 s
  block baseline 0.78 s (1.06×), **AVX2 0.69 s (1.19×)**, AVX-512 0.77 s;
  ISDB-S 0.419 s block baseline 0.34 s (1.25×), **AVX2 0.30 s (1.40×)**,
  AVX-512 0.33 s. Before §0.14: ISDB-S3 0.99 s (0.83×). One contiguous
  ISDB-S3 stream's sequential stage is now above real time on this CPU.
- *A trap found on the way*: the kernels' helpers as `#[target_feature]`
  functions (which cannot be `#[inline(always)]`) were left out of line
  by LLVM in the `process_each` loop only, where AVX2 then ran 2.8×
  *slower* than the baseline (2.2 s); as macros they expand in each
  kernel. Also, a plain reduction loop written generically is left scalar
  when inlined into a `#[target_feature]` function; the baseline dot
  product is now written as `pmaddwd` computes it, which LLVM
  vectorises (baseline 1.07 → 0.99 s through `process`).
- *Whole pipeline on long files* (same files as above, contended: the
  FPGA image build ran in parallel, load 10–16): ISDB-S3 0.95–0.96×,
  ISDB-S 1.30–1.33× real time for every instruction set (decoding
  identical: 12 LDPC failures; 444 superframes). With three front ends
  and the receiver threads sharing a loaded machine the instruction set
  hardly shows in the wall time (summed front-end CPU −9 % with AVX2);
  the single-stream figure above is the load-independent one.

## Continuous reception through the FPGA front end

2026-10-26, PlutoSky R2 with the format-4 bitstream (`fpga/README.md`),
receive only, satellite IF through RX1 (no DC block), 40 Msps, 56 MHz.
`isdb rx --system isdbs|isdbs3 --radio pluto:uri=ip:192.0.2.30 --channel
… --seconds 30` with the default `--offload auto` (plan §0.16).

| | ISDB-S, BS1 | ISDB-S3, BS7 |
|---|---|---|
| acquisition | carrier −158.6 kHz, burst MER 16.4 dB | carrier −157.9 kHz, sync Es/N0 13.5 dB |
| link | 43.4 MB/s (6 bits) | 50.8 MB/s (6 bits) |
| decoded | 2 716 superframes, TMCC and main signal on all; 21 732 frames, 0 slips | 873 frames, TMCC on 872; 83 712 slots, 1 LDPC failure, 0 BCH failures |
| output | 3 TS × 347 648 packets, all RS-clean; 0 continuity errors, 0 TEI (`isdb ts inspect`) | 3 TLV streams, 0 head-indication mismatches |
| losses | 0 | 1 (one DMA overflow, 40 blocks ≈ 5 ms; a reported discontinuity) |
| receiver | 12.3 s for 30 s (2.5× real time) | 18.3 s for 30 s (1.6× real time) |

- **The first gapless TS from the Pluto**: until now a radio delivered
  0.83 s blocks every 2 s (the ≥ 39 Msps IQ does not fit the link).
- **Offline on recorded words** (`isdb symbols --out`, then `isdb rx
  --symbols`): 10 s of BS7 decodes to 301 frames (TMCC on all; the 302nd
  is the partial last one), 28 896 slots LDPC-clean, 0 lost slots in the
  three TLV streams, at 1.42× real time; 10 s of BS1 to 902 superframes,
  3 TS × 115 456 packets RS-clean, 0 continuity errors, 2.1× real time.
- **Chunking**: the receiver runs on chunks of 24 frames (ISDB-S3) and
  keeps what starts at its last whole frame for the next call; 8 / 16 /
  24 frames decoded the 10 s BS7 words at 0.81× / 1.07× / 1.14× real time
  with the same result (each call synchronises and trains the equaliser
  anew); a slot-offset hint (chunks start on a slot head) then saved the
  0.15 s search per chunk: 1.42×. A loss cut into the words file recovers
  at the next frame (300 of 300 frames, TMCC on all).
- **Losses on ISDB-S3, not ISDB-S** (resolved, below): a device-side
  stall every 10.2 s, which the 51 MB/s ISDB-S3 stream's four kernel
  buffers did not cover; and the "lost lock after a loss" was the carrier
  drift, not the loss.
- **The frame markers are right**: over the 10 s stream `isdb iq s3sync`
  counts FSync 302 / !FSync 17 854 = 18 156 odd slots, one marker per 120
  slots, and the receiver finds and decodes the same frames. The FPGA
  front end has no carrier loop: over 10 s the residual offset turned the
  carrier by 1.06 rad per SSync slot pair (+306 Hz), which the per-slot
  phase of the receiver follows.

### Losses and drift over minutes (plan §0.18)

- **The lost lock was carrier drift.** The FPGA front end's NCO stays at
  the acquisition's offset and the signal drifted: −97 Hz → −830 Hz in
  30 s on one run, +1 537 Hz in 60 s on another (`examples/s3drift` on
  recorded words). The receiver tells the frame marker from !FSync
  against the two SSync slots one slot either side; their sum vanishes at
  |Δf| = Rs / (4 · 9 296) ≈ 908 Hz, where the word's sign is noise and
  the frame heads land anywhere (no TMCC). A 40 s recording with two
  losses decoded 904 of 957 frames with three resyncs, every frame lost
  from 30 s on. The streaming receiver now tracks the residual from the
  SSync phase steps and removes it before synchronising
  (`RxOptions::carrier_hz`): 1 207 of 1 207 frames, 0 resyncs, each loss
  costing about one frame. Synthetic sync tests at ±908 Hz (heads lost
  without the offset removed, found with it).
- **The losses were on the device.** With no decoder at all, writing the
  words to disk at low load, a 45 s ISDB-S3 stream lost 64–106 blocks
  (≈ 8–13 ms) at 10.15, 20.35, 30.54 and 40.73 s — every 10.19 s
  (`tools/symbols-losses.py`); a 43.4 MB/s ISDB-S stream lost nothing.
  iiod used ≈ 72 % of a Pluto core at 50.8 MB/s, and no other process
  on the board showed anything periodic. With 8 or 16 kernel buffers of
  1 Mi words (`kbuffers`, now 16 by default: 660 ms of slack at
  50.8 MB/s) 45 s ran without a loss.
- **The stall is BBR's PROBE_RTT (2026-10-26).** Tezuka sets the board's
  TCP congestion control to BBR (`/etc/init.d/S96networkcong`, with the
  `fq` qdisc and 4 MiB send buffers). BBR re-measures the minimum RTT
  every 10 s by holding in-flight data to 4 packets for ≥ 200 ms, during
  which iiod's sends block and the FPGA's DMA buffers fill. Evidence,
  `isdb symbols --offload fpga` to disk, no decoder, `kbuffers=4`
  (165 ms of slack), BS7:
  - the loss lands at the same point of every stream: **10.152 s, symbol
    342 704 128** (± one block), in 5 of 5 runs under BBR, losing 29–201
    blocks (the part of a ≈ 170–200 ms pause beyond the slack);
  - with `cubic` set at runtime (`sysctl -w
    net.ipv4.tcp_congestion_control=cubic`): **0 losses** in 2 of 2 30 s
    runs; the same setup back under BBR straight after lost 52 blocks at
    10.152 s again;
  - ruled out on the way: a free-running board task (the loss is at a
    fixed stream position, not a fixed time of day; the board's watchdog,
    chrony, update.sh and maia-sdr IRQ are periodic on their own clocks),
    the Pluto's memory reclaim (all `allocstall`/`pgscan`/`pgsteal`
    counters 0), a fixed DMA buffer count (half-size buffers stall at
    the same time, as four back-to-back overflows 20 ms apart — the
    pause's true length), and the host NIC (its drop counters for this
    traffic did not move during a stalled stream).

  The "every 10.19 s" pattern above and the 43.4 MB/s ISDB-S stream's
  apparent immunity follow from the pause's length against the slack:
  at 43.4 MB/s, 4 × 2 MiB is 193 ms, just enough most of the time;
  small buffers showed ISDB-S losses too. The default 16 buffers absorb
  it at both rates; setting `cubic` on the board removes it.
  `tools/pluto-sample.sh` samples the board's interrupts, softirqs and
  process CPU time at ≈ 5 ms intervals (read-only) for this kind of
  search.
- **The host keeps draining.** The parser thread reads the words whatever
  the decoder does; its queue to the decoder is bounded to four chunks
  (≈ 3.2 s of ISDB-S3, ≈ 1.1 s of ISDB-S), beyond which whole blocks are
  dropped and reported (`blocks_dropped`).

60 s live, receive only, after the changes (48 busy threads on the host
from 22 to 42 s in the "induced" runs; other sessions loaded it too):

| | load | radio losses | host drops | decoded | output |
|---|---|---|---|---|---|
| ISDB-S3 BS7 | normal (1–11) | 0 | 0 | 1 816 / 1 816 frames with TMCC, 0 LDPC failures; drift tracked to +1 537 Hz | 3 TLV streams |
| ISDB-S3 BS7 | induced | 0 | 11 (≈ 5.6 s) | 1 634 frames, all with TMCC, 1 LDPC failure, 0 resyncs | queue ≤ 3.2 s |
| ISDB-S3 BS7, `kbuffers=4` | induced | 2 (157 blocks) | 14 | 1 624 frames, all with TMCC | — |
| ISDB-S BS1 | normal (≈ 11) | 0 | 0 | 5 423 / 5 423 superframes | 3 TS × 694 144 packets, 0 CC errors, 0 TEI |
| ISDB-S BS1 | induced | 0 | 37 (≈ 8 s) | 4 660 superframes, all decoded, 0 resyncs | queue ≤ 1.1 s |

Before the changes, at normal load: an ISDB-S3 loss every 10.2 s, and
frames lost from ≈ 30 s on (§0.17's run: TMCC on 657 of 836 frames).
The host drops under induced load are the decoder sharing the cores with
48 busy threads (≈ 0.9× real time then); they are reported
discontinuities, not stalls of the capture.

## Open questions

- **Broadcast coverage.** Seven stations of one region were received
  through a cable feed (above). Still unchecked against a broadcaster:
  - the seismic-warning AC word: none of the seven sends one. Its
    (187,105) parity is the TMCC code shortened differently (§3.16.6.7:
    "as is the case with TMCC") and runs through the same encoder and
    decoder, so the bit-order convention confirmed on TMCC carries over;
    the B17–B121 span, the CRC-10 (§3.16.6.6) and the field layout do not;
  - DQPSK and differential segments: every station is coherent (segment
    type 000), and gr-isdbt has no DQPSK, so they rest on loopback alone;
  - reception from an antenna, and the cause of the 16.5 dB layer-B MER
    of two stations on this feed.
- **Receiver coverage.** Other modes, guard intervals, DQPSK, three-layer
  and non-partial-reception TMCC have not been shown to a real receiver.
- **Impairments.** Behaviour under AWGN, CFO, sample-rate offset and
  multipath was tested Rust → Rust only (software loopback), not
  differentially against the oracle.
