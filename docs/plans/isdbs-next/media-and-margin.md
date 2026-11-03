# ISDB-S next, items 3 and 4: media in the bench service, the in-process multiplexer, level margin

Investigation only, 2026-10-30. Repository `~/dev/isdb-rs` at `371e72b`;
tuner driver `~/dev/PIX-DT295` at `72872e5`. Nothing was transmitted, no board or tuner
command was run. Local runs: `isdb mux` of the media bench service (12 s, scratchpad),
ffprobe / ffmpeg on that file, and a shell test of the bench script's FIFO loop.
**[confirm]** marks what a bench run still has to settle. Item 1 (PCR-paced placement) is
investigated separately; §3.6 only states the interface this work needs from it.

## 0. Summary

- **Media is on this host**, `ISDB_RS_MEDIA_DIR` is not set: `crates/isdbt-mux/examples/media/`
  (git-ignored, `.gitignore:27`) holds `video.m2v` (MPEG-2 MP@ML-ish 1440×1080i, 29.97 fps,
  300 frames, 14.97 MB ≈ 11.96 Mbit/s ES) and `audio.m2a` (AAC-LC 48 kHz 2 ch, 470 frames,
  FFmpeg ADTS: protection_absent 1, fullness 0x7FF, MPEG-4 ID). The same pair is in
  `target/bml-test/media/`. The bench WORKDIR of 2026-10-29 (`bs-bench.toml`, `bench60.ts`) is
  gone (not found anywhere on the disk).
- **Media mode works offline today**: bench-minimal.toml with the media kept, TS_ID 0x4010 and
  `--adts-compliance warn` multiplexes cleanly (T-STD ok, 0 late PES, 0 CC events). Its TS is
  29.96 Mbit/s CBR with ≈ 12.4 Mbit/s non-null: 24 % of the BS layout's 52.17 Mbit/s.
  Decoded video pictures of the muxed TS are byte-identical to the source's (357 of 358; the
  last is truncated by the file end); decoded audio frames identical except 4 after each
  media loop seam (AAC overlap state). That gives an end-to-end bit-exact check for the
  recording (§2.4).
- **ffmpeg on a recording does not need item 1**: arrival-order placement keeps packet order,
  so PES integrity is untouched; only `isdb ts validate` timing findings and real-time
  decoders (TV, recorder) need item 1.
- **Bug in `tools/isdbs-bench-tx.sh`**: `exec` drops the EXIT trap, and the
  `while :; do cat …; done | pv` loop spins for ever once its reader is gone (reproduced:
  9 554 `cat` runs in 3 s). Fix: `while cat bench60.ts; do :; done` (exits on SIGPIPE,
  verified) and no `exec`.
- **In-process ISDB-S multiplexer** (`tx --system isdbs --mux-config`): isdbt-mux's timing is
  "slot j of OFDM frame k at `k·samples_per_frame + 408·j` clocks" (`mux.rs:1-13`,
  `:1263-1266`); 52 references in `mux.rs`, 16 in `si.rs`, plus `capacity.rs`. Proposal: a
  `Schedule` (one period's slots with lane, exact start times, period) built either from
  `Params`/`MultiplexPattern` or from an ISDB-S `TmccInfo` + relative TS; `Multiplexer::new`
  keeps its signature (≈ 70 call sites untouched). Its real gain over item 1: PCR stamped at
  the exact slot time (0 jitter by construction), where placing a foreign PCR schedule into
  28.8 µs slots leaves ≈ ±14 µs (§3.6). Size ≈ 1 000–1 300 lines with tests and docs.
- **The tuner already reads C/N and the error counters every poll once locked**
  (`tc90532.rs:560-577`); `status --isdbs --watch N` polls every 110 ms. What looked like
  "C/N read once" is the dB conversion: at att 45 the raw 0xBC reads 0x0B66–0x0B7A
  (2 918–2 938) in the 2026-10-29 transcripts, below the tc90522 polynomial's floor of 3 000,
  so `cnr_db()` is `None` and the summary prints the misleading "C/N not read (the vendor
  reads it only once acquiring)" (`demod.rs:451`). The signal is above the formula's range;
  the absolute C/N scale of the TC90532 is open.
- **Attenuation**: set once at open (`pluto.rs:2227-2242`); no run-time path in `isdb tx`.
  A restart costs ≈ 9 s (block configuration 8 s + 0.58 s fill) and a re-acquisition. The
  sweep can instead write `ad9361-phy` `out_voltage0_hardwaregain` with `iio_attr` while
  `isdb tx` streams **[confirm]**, which keeps the tuner locked and gives the hold threshold
  and C/N slope continuously.
- **Expected range problem**: TC8PSK 2/3 needs ≈ 8.4 dB Es/N0 (loopback, VERIFICATION.md:1152-1156);
  with only the 30 dB pad, the lock threshold may lie beyond att ≈ 80, where the Pluto's TX1
  output stops scaling 1 dB/dB (TX1 in-band power 58.7 / 66.4 / 77.6 dB at att 89.75 / 80 / 70,
  PIX-DT295 `docs/stream-links.md:1802`). The sweep must plan for a second pass with +20 dB of
  fixed attenuation.

## 1. The bench script today (item 3a)

`tools/isdbs-bench-tx.sh` (59 lines):

| Lines | What |
|---|---|
| 9-13 | `WORKDIR [ATT_DB] [PLUTO_URI]`; ATT default 45 "about −38 dBm at the port, −68 dBm after a 30 dB pad" (an estimate, §5.3) |
| 32-46 | Python edit of `crates/isdbt-mux/examples/bench-minimal.toml` → `bs-bench.toml`: drops every line starting `source =` or `frames_per_pes` (also `[clock] source = "system"`, harmless: it is the default), turns `mpeg2-video` / `aac-adts` into `declared` with stream_type 0x02 / 0x0F, `transport_stream_id = 0x79EE` → `0x4010` (first occurrence: `[transport]`; `network_id` stays 0x79EE) |
| 47-49 | `isdb mux -c bs-bench.toml --seconds 60 --out-ts bench60.ts`: default layout Mode 3 GI 1/8, A 1 seg QPSK 2/3, B 12 seg 64QAM 3/4; `--out-ts` is the 13-segment multiplex-frame TS, null slots included: 4 608 TSPs per 231.336 ms frame = 29.958 Mbit/s |
| 51 | `RATE = size / (260 × 0.231336)` = 3 744 786 B/s (60 s → ⌈259.4⌉ = 260 frames) |
| 52-55 | FIFO `ts.fifo`, `( while :; do cat bench60.ts; done \| pv -q -B 7520 -L $RATE > ts.fifo ) &`, `trap 'kill $PACER' EXIT` |
| 57-59 | `exec isdb tx --system isdbs --radio pluto[:uri=…] --channel BS1 --freq-unrestricted --offload fpga --rf-enable --gain att=$ATT --ts 0x4010=$WORK/ts.fifo --feed live --status-secs 5` (default combination tc8psk2/3:48, one TS with all 48 slots) |

Notes:

- `--feed live` (`source.rs:59-61`): never blocks the modulator, pads with nulls. The ISDB-S
  `TsInputs::take` (`txs.rs:509-575`) keeps the input's own null packets (the ISDB-T `MuxInput`
  drops them, `input.rs:232-235`), so the 29.96 Mbit/s FIFO fills 57.4 % of the 384 packets
  per superframe and the producer pads the rest.
- Loop seam: `cat` restarts the 60 s file: one PCR/PTS discontinuity and 3 CC errors per
  minute (VERIFICATION.md:1765-1766).
- **Shutdown bug.** `exec` (l. 57) replaces the shell, so the EXIT trap (l. 55) never runs.
  When `isdb tx` ends, `pv` gets EPIPE and exits, and the `while :; do cat …; done` loop then
  restarts `cat` for ever, each one dying of SIGPIPE at once. Reproduced in the scratchpad
  with `head -c 10` as reader: the loop ran 9 554 iterations in 3 s and never ended;
  `while cat f; do :; done` ended at once. Fix in the media-mode change (§2.1).

## 2. Media mode (item 3b)

### 2.1 Script changes

`tools/isdbs-bench-tx.sh [--media] WORKDIR [ATT_DB] [PLUTO_URI]`:

1. `--media` takes `MEDIA=${ISDB_RS_MEDIA_DIR:?…}`; refuse when `$MEDIA/video.m2v` or
   `$MEDIA/audio.m2a` is missing. (On this host:
   `ISDB_RS_MEDIA_DIR=$HERE/crates/isdbt-mux/examples/media`; docs/test-data.md:66-91 has
   the FFmpeg recipe that made them.)
2. TOML: keep bench-minimal.toml's media streams; replace only
   `source = "media/video.m2v"` / `"media/audio.m2a"` with absolute `$MEDIA/…` paths (the
   TOML lives in WORKDIR, relative sources resolve against it), keep `frames_per_pes`, TS_ID
   0x4010 as today. Write `bs-bench-media.toml` (do not reuse `bs-bench.toml`/`bench60.ts`:
   the cache test on l. 47 would pick the media-free TS).
3. Feed the FIFO from a live multiplexer instead of a looped file: no 60 s seam, no 2 GB file,
   timestamps continuous across the media loop (docs/mux-config.md:225):
   ```sh
   RATE=3744786   # 4608 × 188 B / 0.231336 s, Mode 3 GI 1/8
   ( "$ISDB" mux -c bs-bench-media.toml --adts-compliance warn --seconds 86400 --out-ts - \
       | pv -q -B 7520 -L "$RATE" > ts.fifo ) &
   ```
   `isdb mux` writes frame by frame through a `BufWriter` (`isdb-cli/src/mux.rs:171-197`,
   `isdbt-mux/src/output.rs:11-15`); without `--report` it keeps nothing, so the pipe's
   back-pressure paces it. `--adts-compliance warn` is needed: the FFmpeg ADTS is refused by the
   default `conform` (checked: 470/470 frames ID '0', protection_absent '1', fullness 0x7FF).
4. Both modes: the file loop becomes `while cat bench60.ts; do :; done`; run `isdb tx` without
   `exec`, then `kill $PACER` and wait (or `kill 0` of a process group started with `setsid`;
   never `pkill -f`, see isdbs-next.md "Pitfalls").

### 2.2 What the multiplexer does with the media (checked locally)

`isdb mux -c bs-bench-media.toml --adts-compliance warn --seconds 12 --out-ts media12.ts --report`
(scratchpad, 0.06 s CPU for 12 s of stream):

| | |
|---|---|
| layout | A 1 seg QPSK 2/3 (64 TSP/frame), B 12 seg 64QAM 3/4 (2 592 TSP/frame) = 18.25 Mbit/s for B |
| ES input | `mpeg2-video`: MPEG-2 video ES (sequence header → 0xC8 / component type derived, `video.rs`); `aac-adts`: ADTS AAC, B32 Part 2 §5.2 gate (`adts_compliance`) |
| PCR | dedicated PID 0x01FF (`pcr_pid`, adaptation-field-only packets), `pcr_per_frame` default = Mode scale 4 at fixed positions (`mux.rs:815-870`), value = slot time + 20 clocks (`mux.rs:1404-1418`); report: 208 PCRs in 12 s, max interval 57.8 ms |
| PES / T-STD | 895 PES, 0 late; T-STD 0x0111 and 0x0112 ok (0 TB/buffer overflow, 0 underflow); 0 continuity events |
| SI intervals | PAT 97.5 ms, PMT 0x01F0 90.4 ms, SDT 1.80 s, EIT 1.81 s, NIT 9.0 s, BIT one in 12 s |
| packets | layer B 134 784, 35 695 null (26.5 %); layer A 3 328, 2 807 null |
| non-null rate | (134 784 − 35 695 + 521) × 1 504 bit / 12.03 s ≈ **12.45 Mbit/s** (video ≈ 12.2 incl. PES/TS overhead, audio ≈ 0.19, PCR/SI the rest) |
| TS rate | 29.958 Mbit/s (null slots included) |

Against the bench combination (TC8PSK 2/3 × 48, one TS): 384 packets per superframe of
319 488 symbols / 28.86 Mbaud = 11.0703 ms → 34 688 packets/s → **52.17 Mbit/s**. The
FIFO TS fills 57.4 %, its non-null content 23.9 %. Any TS up to 52.17 Mbit/s fits; nothing
in the media mode is near the limit (the mux itself warns above 95 % of layer B,
`mux.rs:983-1000`).

Observation: the media loop period is the longest stream (`mux.rs:593`): audio 470 × 1024 /
48 000 = 10.027 s, video 300 × 1001/30 000 = 10.010 s, so every 10.03 s the video DTS steps
4 503 instead of 3 003 (90 kHz) once (seen at packet 300 of the 12 s file). Expected by the
design; the PTS check must allow it. (Equal lengths would need 640 video and 1 001 audio
frames, 21.355 s.)

### 2.3 Recording with the tuner

`dt295 stream --raw-config` passes every component of a service without CA in the clear
(ISDB-T, the same bench.toml media, 2026-10-21: video 0x0111 79 903 packets, audio 0x0112
1 382, `sc 00`, 0/0/0 sync/TEI/CC, ffprobe mpeg2video + aac; PIX-DT295
`docs/stream-links.md:1777-1822`). Not yet shown for ISDB-S: the 2026-10-29 recording had no
ES packets to pass **[confirm]**.

```sh
D=~/dev/PIX-DT295/target/release/dt295
$D status --isdbs                       # STATE 0x0011 (else tools/power-cycle.sh 1-2 1 3, then `dt295 init`)
$D stream --isdbs bs1 --tsid 4010 --raw-config --seconds 30 --out "$WORK/rec-media.ts"
```

`stream` tunes, waits for lock (5 s), records `--seconds`, stops to STATE 0x0011
(`main.rs:1682-1735`); its built-in check prints sync losses, TEI and CC errors and
`clear output: …`. `status` cannot run concurrently (the device lock,
`$XDG_RUNTIME_DIR/dt295/<serial>.lock`).

### 2.4 Checks on the recording (commands tried on the muxed file)

```sh
R=$WORK/rec-media.ts; M=$ISDB_RS_MEDIA_DIR
isdb ts inspect "$R" > rec.inspect.txt          # PIDs, CC, PCR, PES, SI
isdb ts validate "$R" --show warning             # timing findings until item 1
ffprobe -v error -show_entries program=program_id:stream=index,codec_name,width,height,r_frame_rate,sample_rate,channels -of compact "$R"
# decode errors, program 0x0400 = 1024 only (the one-seg service declares H.264/AAC without packets)
ffmpeg -hide_banner -nostats -v error -err_detect crccheck+bitstream+buffer -i "$R" -map 0:p:1024 -f null - 2> dec.err; wc -l < dec.err
# DTS monotonic per stream (steps 3003 video, 1920 audio; one 4503 per 10.03 s media seam)
ffprobe -v error -select_streams p:1024:v:0 -show_entries packet=pts,dts -of csv=p=0 "$R" > v.csv
ffprobe -v error -select_streams p:1024:a:0 -show_entries packet=pts,dts -of csv=p=0 "$R" > a.csv
# content: every decoded picture / audio frame must be one of the source's
ffmpeg -v error -i "$M/video.m2v" -map 0:v -f framemd5 src-v.md5
ffmpeg -v error -i "$R" -map 0:p:1024:v -f framemd5 rec-v.md5
ffmpeg -v error -i "$M/audio.m2a" -map 0:a -f framemd5 src-a.md5
ffmpeg -v error -i "$R" -map 0:p:1024:a -f framemd5 rec-a.md5
```

On `media12.ts` (the multiplexer's output, standing in for a recording): 2 decode-error lines,
both from the last picture cut by the end of the file (none with `-t 11`); DTS strictly
increasing, steps 3 003 (+ one 4 503) and 1 920; **357 of 358 decoded pictures are in the
source's framemd5 set** (the cut last one is not), 533 of 537 audio frames (the 4 after the
seam). Stream-copy packet hashes do not match (`-c copy`: the raw demuxers frame the ES
differently), so compare decoded frames.

Acceptance for the recording: 0 sync/TEI/CC from `dt295`; `isdb ts inspect` CC 0 except none
expected (no file seam in live mode); decode errors only in the first GOP (open-GOP leading
B pictures before the first I) and the last picture; every decoded picture in the source set
and in cyclic order; audio frames in the source set except ≤ 4 after the recording start and
each media seam; DTS monotonic as above. Put the decode and hash comparison in a committed
tool (`tools/media-decode-check.py`, run with `python3 -I`), not in /tmp.

Item 1 is not needed for these checks. It is needed for `isdb ts validate` without timing
findings (PCR jitter 12–30 ms rms, PAT/PMT intervals 0.17–0.21 s today) and for a TV or
recorder, whose clock recovery would not follow that jitter.

## 3. In-process multiplexer for ISDB-S (item 3c)

### 3.1 How ISDB-T does it today

- `isdb tx --mux-config FILE` → `relay.rs:349-391` resolves it (with `--auto-layout`) →
  `MuxConfigInput::open` (`input.rs:598-611`) → `isdbt_mux::Multiplexer::new(&cfg, params, base)`.
- `FrameInput::take_frame(counts: [usize; 3])` (`input.rs:30-54`); the mux impl
  (`input.rs:651-670`) returns `mux.next_frame()`: exactly `tsp_per_frame` packets per active
  layer, nulls included, so no pacing is needed: PCR/PTS/DTS are made for those slots.
- ISDB-S refuses `--mux-config` (`txs.rs:143`, `TX_ISDBT_ONLY`).

### 3.2 What in isdbt-mux assumes the OFDM frame

| Where | Assumption |
|---|---|
| `mux.rs:1-20`, `:54` `SLOT_CLOCKS = 408`, `:55` `PCR_BYTE_CLOCKS = 20`, `:66-70` `clocks_to_ticks` (× 1701/512) | B31 §3.2.2 model receiver: one 204-byte TSP per 408 clocks of 512/63 MHz, 2 clocks per byte |
| `Multiplexer { params: Params, pattern: MultiplexPattern, frame }` (`mux.rs:164-190`); `new(cfg, &Params, base)` (`:218-221`) | the period is an OFDM frame, slots = `MultiplexPattern::slots()` (`isdbt-fec/src/remux.rs:60-69`, `Slot = Option<Layer>`) |
| `slot_time(k, j)` (`:1263-1266`); PCR value in `decide` (`:1408-1418`) | `k·samples_per_frame + 408·j` |
| `next_frame` / `next_frame_full` / `step` (`:1274-1366`); `layer_slices` (`:1836`) | output is `[Vec<Packet>; 3]` per OFDM frame |
| sections: `frame_ticks = params.frame_ticks_27mhz()` (`:736`), first-transmission stagger `frame_ticks / pattern.count(layer)` (`:794`) | |
| PCR: `per_frame` default `params.mode().scale()` (`:815-816`), must divide `pattern.count(layer)` (`:837-843`), interval check `frame_ticks / per_frame ≤ 100 ms` (`:844-852`), positions `offset + q·step` (`:864`) | PCR positions per OFDM frame |
| relay capacity: `capacity::layer_bps(params, id)` (B31 Table 3-3), `over_capacity_message`, `suggest` (layouts) (`:893-981`, `capacity.rs:17-26, 99, 201`); `emit_relay` `layer_bps` (`:1438-1439`); `check_relay_lateness` `frame_seconds` (`:1369`) | layer capacity of an ISDB-T layer |
| video capacity warning `tsp_per_frame × 184 × 8 / frame_seconds` (`:983-1000`) | |
| inactive-layer checks `params.layers()[l].is_none()` (`:257, :479, :753`) | layers A/B/C |
| `si.rs:34, 253-258` `control_layer` (partial reception → A, else most robust), `:726-738` default table layers, `:1021` robustness rank, `:1089-1090` terrestrial delivery system descriptor GI/mode, `:1131` partial reception descriptor | terrestrial SI (item 2's area) |
| `config.rs:44-46` `LayerName` A/B/C; `error.rs:173` `OverCapacity { layer: isdbt_core::Layer }` | |

Not affected: `tstd.rs`, `relay.rs` (works on layer index and 27 MHz times),
`video.rs`/`audio.rs`, `carousel.rs`, `analysis.rs` (no `Params` use).

### 3.3 The ISDB-S timeline

- One relative TS r of the TMCC's `TmccInfo` (`isdbs-core/src/tmcc.rs:366-432`; r owns a
  contiguous run of sent slots, `:417-429`). Each ISDB-S TS is an independent transport
  stream (own TS_ID, PAT, SI), so one `Multiplexer` per relative TS, one lane.
- Packet k of TS r in superframe s: frame f = k / n_r, its (k mod n_r)-th own sent slot
  (`isdbs-phy/src/tx.rs:400-433` `build`: per frame, sent slots in order).
- Slot timing: `isdbs_core::slots::runs` (`slots.rs:17-60`) gives each combination run's
  main-signal symbol range; a valid slot of a run spans `symbols.len() / valid_slots` main
  symbols (812 for TC8PSK; dummy slots take none); `frame::main_symbol_position`
  (`frame.rs:72-75`) adds the 192-symbol header and the 4-symbol bursts every 203.
  Time of symbol n of superframe s: `(s·319 488 + f·39 936 + pos) × 27e6 / 28.86e6`
  = × 450/481 exactly, integer-split like `clocks_to_ticks`.
- PCR byte: byte 10 of the 188-byte packet is on-air byte 9 of the slot (the sync byte is not
  sent, `frame.rs:20-23`); offset = its main-symbol index (bytes × symbols per byte of the
  slot's combination: 4 for TC8PSK 2/3) mapped through `main_symbol_position`.
- Receiver model: the inter-frame interleaver spreads each slot over the 8 frames
  (`mainsig.rs:76-94`, depth 8 block); a block deinterleaver delays every byte by one
  superframe, so "arrival = pre-interleave slot time + constant" holds. B20 has no ISDB-T-like
  model receiver: record this as a SPEC-NOTES interpretation **[confirm against B20/B21]**.

### 3.4 Proposed abstraction

A data type, not a trait (the mux needs only numbers; no generic churn):

```rust
// isdbt-mux/src/schedule.rs
pub struct Schedule {
    /// One period's slots in transmission order: Some(lane) carried by this
    /// multiplexer, None not carried (ISDB-T null TSP; ISDB-S another TS's slot).
    pub slots: Vec<Option<usize>>,
    /// Start of each slot and of byte 10 of its packet, in `unit`s from the period start.
    pub start: Vec<u64>, pub pcr_byte: Vec<u64>,
    /// Period length in units, and units → 27 MHz ticks as num/den
    /// (ISDB-T: samples_per_frame, 1701/512; ISDB-S: 319 488 symbols, 450/481).
    pub period: u64, pub num: u64, pub den: u64,
    /// Bit/s per lane (capacity checks), lanes active, default PCRs per period,
    /// control lane (PAT/NIT default), and the delivery system for the SI.
    pub lane_bps: Vec<f64>, pub default_pcr_per_period: usize, pub control_lane: usize,
    pub system: System,   // Terrestrial(Params) | Satellite { info: TmccInfo, relative_ts }
}
impl Schedule {
    pub fn ofdm(params: &Params) -> Self;                       // from MultiplexPattern
    pub fn satellite(info: &TmccInfo, relative_ts: usize) -> Self; // from slots::runs (isdbs-core)
    pub fn slot_time(&self, period: u64, j: usize) -> u64;      // 27 MHz ticks
}
```

- `Multiplexer::new(cfg, &Params, base)` stays (= `with_schedule(cfg, Schedule::ofdm(p), base)`);
  `Multiplexer::with_schedule` is new; `next_period() -> Vec<Packet>` (the lane's packets) is
  new; `next_frame*`, `params()`, `pattern()` stay for the OFDM case (`params()` →
  `Option<&Params>` only if a caller needs it in the satellite case).
- Satellite rules at load: services/streams/tables must not name layer B/C (one lane);
  `pcr_per_frame` reads as "per period" (one PCR per superframe = 11.07 ms is the natural
  default; positions need `n_r·8` divisible by it); capacity messages without ISDB-T layout
  suggestions; SI by `system` (NIT satellite delivery system descriptor etc. is item 2).
- isdbt-mux would depend on isdbs-core (small, no heavy deps), or `Schedule::satellite` lives
  in isdb-cli/isdbs-core and the mux only sees the data struct (preferred: no dependency).
- isdb-cli: `txs.rs` `Source::Mux(Box<Multiplexer>)` (`:422-428`), filled per superframe by
  `next_period()` in `TsInputs::take` (`:509-575`, real count = non-null); CLI: drop
  `mux_config` from `TX_ISDBT_ONLY` (`:143`), allow `--mux-config FILE` repeated, each TOML
  one TS (TS_ID from its `[transport]`, slots from `--ts ID:SLOTS` when more than one TS);
  `isdb loopback --system isdbs --mux-config`; optionally `isdb mux --system isdbs --out-ts`
  (a TS file timed for the superframe).

### 3.5 Size

| Piece | Lines (code + tests) |
|---|---|
| `Schedule` + OFDM constructor, `mux.rs` routed through it (≈ 52 sites), `si.rs` system switch (≈ 16), capacity/error | 450–600 |
| satellite timeline (slot symbol offsets, ticks, PCR byte) + tests against `Transmitter::build` placement | 120–180 |
| isdb-cli: `Source::Mux`, option checks, describe/status, tests (loopback byte-exact SI + PCR exactness) | 250–350 |
| docs: transmitting.md, mux-config.md, isdbt-mux README / SPEC-NOTES (receiver model) | 100–150 |

Total ≈ 1 000–1 300 lines; 2–3 agent contracts (Schedule refactor with ISDB-T output
byte-identical before/after; satellite timeline; CLI wiring). The refactor's guard: every
existing isdbt-mux / isdb-cli test unchanged and `isdb mux` output byte-identical for
bench.toml.

### 3.6 Interface needed from item 1

Item 1 and this item both need "on-air time of packet k of relative TS r in superframe s".
Put it once in isdbs-core (e.g. `slots::packet_symbol(info, r, k) -> usize` within the
superframe, plus `SUPERFRAME_SYMBOLS` and the 450/481 tick ratio); the pacer inverts it (due
PCR time → first free slot at or after it), the mux's `Schedule::satellite` tabulates it.
Note for item 1's acceptance: placing packets of a foreign PCR schedule into 28.83 µs slots
quantises their arrival by up to one slot (≈ 8 µs rms), above H.222.0's ±500 ns, unless the
pacer re-stamps PCR at the chosen slot (PCR_base/ext rewrite + the T-STD lateness check). The
in-process mux stamps from the slot time and has no such error.

## 4. What the tuner reports (item 4d)

Driver: `dt295-frontend/src/tc90532.rs` (satellite core at I²C 0x22), `isdbs.rs`,
`dt295-device/src/tune.rs`, `dt295-cli/src/main.rs`, `demod.rs`.

| Command | Reads |
|---|---|
| `tune --isdbs bs1 --tsid 4010 [--lock-seconds N]` | tune sequence (`isdbs.rs:137-162`), then polls every 110 ms until `c3 = 00` or the limit (`tune.rs:226-245`), prints each round (`main.rs:1170-1189`); **stops at lock**, so it gives time-to-lock and one C/N/error read. Exit ≠ 0 without lock |
| `status --isdbs [--watch N] [--read-reg R[:LEN]]` | one round, or one every 110 ms for N s, no retune (`main.rs:1283-1340`); extra registers every round regardless of lock (`tune::read_extra`); summary: rounds per lock state, C/N mean/min/max, 0xBA range, first/last/max error counters (`demod.rs:415-472`) |
| `stream … --raw-config` | tune + lock poll once, then only EP 0x81 reads with IRQ service (`stream.rs:621-660`); no quality poll during the recording |

Per round (`tc90532.rs:552-577`): 0xC3 (lock: 0x00 locked; tc90522 bits: 7 level low,
6:5 no carrier, 4 no lock, `:903-935`), 0xBA ("0x58/0x5A locked, meaning unknown"; logged as
the AGC byte); **once 0xC3 = 0**: 0xBC 2 B (C/N raw), 0xEB 3 B and 0xF0 3 B (INFERRED error
counters of the two hierarchical layers; tc90522 reads two 5-byte records from 0xEB).
C/N in dB: tc90522 polynomial on `x = raw − 3000`, `None` below 3000 (`:938-951`).

Evidence from the 2026-10-29 transcripts (`~/dev/PIX-DT295/captures/linux-20261029-*.i2c.txt`):
`status` 181755 read 0xBC in all 27 rounds, 0x0B66–0x0B7A, c3 = 00 every round; `tune`
181740 0x0B6F; `stream` 181758 / 182110 0x0B72. Polynomial: raw 3 000 → 58.9 dB,
3 500 → 36.5, 4 000 → 31.1, 5 000 → 25.7, 7 235 → 20.5 (the off-air BS-15 reading,
`docs/first-tune.md:20`), 10 000 → 17.2, 20 000 → 10.4, 30 000 → 5.6. A thermal estimate
(−174 dBm/Hz + 74.6 dB(28.86 MHz) + NF ≈ 10 dB ≈ −89 dBm) gives C/N ≈ 11–21 dB at −78 to
−68 dBm, yet the raw value is past the formula's 59 dB end: the TC90532's scale (or this
formula for it) is not right at the top. The sweep calibrates raw against att (§6).

Continuous C/N and BER are feasible with what exists:
`dt295 status --isdbs --watch N --read-reg bc:2 --read-reg eb:10` gives raw C/N even when not
locked and both 5-byte error records every 110 ms. Driver additions worth making (PIX-DT295
repo, separate commits):

1. `demod.rs:451`: say "C/N above the formula's range (0xBC < 3000), raw min…max" instead of
   "not read"; keep raw 0xBC min/mean/max in the summary.
2. `SatelliteStatus`: read 0xEB as 10 bytes; `ber()` = errors / (period × 204 × 8) per layer
   (tc90522 semantics, INFERRED; mark so).
3. A machine-readable line (`--csv` or `--json`) for `status`, so scripts do not parse the
   human format.
4. Optional: a quality poll every ~1 s inside `stream`'s read loop (the vendor polls the lock
   over I²C during streams, `docs/stream-links.md:472-473`), to log C/N/BER next to TEI/CC.

## 5. Attenuation in `isdb tx` (item 4e)

### 5.1 How it is set

- `--gain att=N` → `tx_settings` (`pluto.rs:765-790`): 0..89.75 dB in 0.25 dB steps.
- Open: `program` writes `hardwaregain = −89.75` on `ad9361-phy` `voltage0` (TX1; `voltage1`
  for TX2, `pluto.rs:394-396`) before anything else (`:2188-2193`); after the stream channels
  are checked, `set_level` writes `−att` and reads it back into the open line (`:2227-2242`),
  called last in `open_tx` (`:918`) and `open_tx_symbols` (`:1240`).
- Close / error: `safe_state` → 89.75 dB, TX LO powerdown (`:2288-2301`).
- No run-time setter: `QueuedSink` / `SymbolSink` have no gain call; the `isdb tx` key commands
  are playback only (`keys.rs:12-21`).

### 5.2 Changing it while transmitting

- **Restart**: ≈ 8 s block configuration (13 315 commands through iiod) + start at half the
  kernel queue (580 ms), docs/transmitting.md:602-606; the tuner loses lock each time. Gives
  the acquisition threshold, not the hold threshold.
- **External write while streaming** (no code):
  `iio_attr -u ip:<PLUTO_IP> -q -o -c -- ad9361-phy voltage0 hardwaregain -55`
  (libiio 0.26 on this host; the `--` keeps getopt from reading `-55` as options
  **[confirm the exact argument order once, with a read: no value]**). The attribute is the
  same sysfs file `set_level` writes; iiod serves several clients; the DMA stream is
  untouched. **[confirm]** on the bench: one 45 → 46 step while transmitting, `isdb tx`
  status lines show no new underrun/MISSED, raw 0xBC moves. `isdb tx`'s close still sets
  89.75.
- **In-process** (optional, later): a `set_gain` on the Pluto sinks and a schedule option
  (`--att-schedule 45:30,50:30,…`) or a stdin command; ≈ 150 lines in isdb-radio + isdb-cli.

### 5.3 Expected level at the tuner

`P_tuner(att) = P_port(0) − att − 30 dB (pad) − L` (DC block, cables ≈ 0.5–1 dB).
`P_port(0)` for this waveform is not measured anywhere in the repo. The script's
"−38 dBm at the port at att 45" (`isdbs-bench-tx.sh:11-12`) implies +7 dBm at att 0: the
AD9361's full-scale CW figure (data sheet ≈ +7–8 dBm near 1 GHz, from memory **[confirm]**),
but the FPGA block's RMS is 10 dB below full scale (`shape/fixed.rs:23, :132`), which gives
≈ −3 dBm at att 0 unless the PlutoSky R2 has a PA (transmitting.md:88-90 mentions variants
with a 10 dB PA). So at att 45: −78 dBm (no PA) to −68 dBm (script); each att step is −1 dB
while the AD9361 scales linearly. Linearity at the top is limited: TX1 → RX2 in-band power
58.7 / 66.4 / 77.6 dB at att 89.75 / 80 / 70 (`stream-links.md:1802`): 11.2 dB for 70 → 80
but 7.7 dB for 80 → 89.75 (leakage or RX floor). Treat att > 80 as unreliable for slopes.

Range check: the tuner's lock needs roughly TC8PSK 2/3 Es/N0 ≈ 8.4 dB (loopback PASS at 7 dB
over 40 MHz, VERIFICATION.md:1152-1156) or less for frame lock alone (BPSK header). If
att 45 is ≥ 25 dB above that, the threshold sits at att ≳ 70–80, at or past the linear end.
Plan a second pass with +20 dB of fixed attenuation (50 dB pad) overlapping the first.

## 6. Level sweep: procedure and script (item 4f)

### 6.1 Procedure

0. Bench check (printed and acknowledged by a `--conducted` flag): TX1 → ≥ 30 dB pad → DC
   block → PIX-DT295; nothing else on TX1; `--rf-enable` only through
   `tools/isdbs-bench-tx.sh`.
1. Tuner up: `dt295 status --isdbs` STATE 0x0011 (else power-cycle + `init`).
2. Transmitter: `tools/isdbs-bench-tx.sh WORK 45 ip:<PLUTO_IP>` in its own process group, pid
   in a file; wait for "FPGA transmit block running"; `dt295 tune --isdbs bs1 --tsid 4010`
   must lock.
3. Baseline at 45: `status --watch 10` + `stream --raw-config --seconds 10` (TS counters).
4. **Hold sweep** (live `iio_attr`, tuner never retuned): att 45, 50, 55, 60, 65, 70, then 1 dB
   steps from the first step where raw 0xBC leaves its floor (> 3 000) or an error counter
   moves, 0.25 dB steps within 2 dB of lock loss. Per step: write, read back (abort on a
   mismatch > 0.125 dB), settle 1 s, `status --isdbs --watch 10 --read-reg bc:2 --read-reg eb:10`.
   Stop going up at the first step with any round `c3 ≠ 00` (lock lost) or at 89.75.
5. **Hysteresis**: from the loss point step back down 0.25 dB at a time with `status --watch 5`
   until `c3 = 00` again without a retune (records whether the TC90532 re-acquires by itself).
6. **Acquisition threshold**: at the loss point and 1, 2, 3 dB below it, `dt295 tune
   --lock-seconds 10` (fresh acquisition, time to lock from its poll lines).
7. **TS errors**: at the baseline, every 5 dB and at each of the last 3 locked steps,
   `dt295 stream --raw-config --seconds 10` (sync/TEI/CC; keep the TS only when not clean).
   `stream` retunes, so do it after the hold data of that step.
8. Restore att 45 (trap on every exit), leave the broadcast in its documented state or stop it
   (`kill` the pid from the file; never `pkill -f`).
9. If the hold sweep reached 80 still locked: repeat 4–7 with +20 dB of pad from att 45, and
   join the passes on their overlap.

Dwell: 10 s status (≈ 90 rounds) + 1 s settle per step; ≈ 25–35 steps → 7–10 min, plus
≈ 15 s per stream step.

Lock loss: any round with `c3 ≠ 00`, classified by the tc90522 bits (NoSignal / Searching /
Sync); `tune` exit status ≠ 0 for acquisition. BER-onset: first step where an 0xEB/0xF0 error
count is non-zero; TS-onset: first step with TEI or CC > 0 in `stream`.

### 6.2 Script design: `tools/isdbs-level-sweep.sh`

```text
tools/isdbs-level-sweep.sh --conducted --uri ip:<PLUTO_IP> --tx-pid FILE \
    [--from 45] [--to 89.75] [--coarse 5] [--fine 1] [--finest 0.25] \
    [--dwell 10] [--stream-every 5] [--allow-below-45] OUTDIR
```

- Refuses without `--conducted`; refuses a start below 45 unless `--allow-below-45` (only for
  an explicit overload measurement); caps at 89.75; requires a live `isdb tx --system isdbs`
  pid (from the file, checked with `kill -0`) before any write; never starts or stops the
  transmitter itself unless asked (keeps `--rf-enable` in one place).
- `trap` restores the starting attenuation on EXIT/INT/TERM; every write is read back.
- Output: `OUTDIR/sweep.csv`, one row per step:
  `utc, att_set, att_read, rounds, locked_rounds, c3_states, bc_raw_min/mean/max,
  cn_db_mean (formula, blank out of range), ba_min/max, eb_err_first/last/max,
  f0_err_first/last/max, ber_hi/lo (INFERRED), tune_lock_ms, ts_bytes, sync_loss, tei, cc,
  tx_underruns, tx_missed`; raw `dt295` and `isdb tx` logs per step beside it
  (`OUTDIR/step-<att>.status.txt`, `.tune.txt`, `.stream.txt`). OUTDIR outside git (logs);
  the script itself is committed.
- Parses the human `status` lines today (`+ t ms c3=.. ba=.. Locked bc=…`, `--read-reg`
  lines); switch to the driver's CSV/JSON once §4 item 3 exists.
- A small `tools/isdbs-level-sweep.py` (run with `python3 -I`) turns the CSV into the
  VERIFICATION.md table and fits raw 0xBC (and the formula's dB) against att over the
  linear range.

### 6.3 Recording the result in VERIFICATION.md

The ISDB-T level figures are prose today, in "Real receivers" (VERIFICATION.md:350-361):
"It received at `att=60` and `att=80`, not at `att=85`" (TV, PlutoSky TX2, UHF 44,
64QAM 3/4). docs/transmitting.md:88-95 repeats it. The tabular precedent is the PIX-DT295
ISDB-T sweep (`~/dev/PIX-DT295/docs/bench-validation.md:282-296`):

```text
| txvga | backoff | Level vs. baseline | Lock | Time to 0x80 = 00 | 0x80 progression | C/N mean (min…max) | 0x82 | Error regs A/B/C at end | Run |
| 0 | 18 dB | −8 dB | yes | 1.22 s | 2F 2E 06 02 00 | 23.9 (21.1…24.7) | A2 | 0/0/0 | r4-vga0-bo18 |
| 0 | 20 dB | −10 dB | **no** | — | 2F 2E 06, then 04 | 22.6 (21.5…23.4) | A5 | 0/0/0 | r4-vga0-bo20 |
```

Proposed: a new section "ISDB-S transmitter in the FPGA: level margin into the PIX-DT295
(date)" directly after "into the PIX-DT295" (VERIFICATION.md:1735-1766), with a one-line
cross-reference added to the ISDB-T sentence at :357 and to transmitting.md:93-95, and this
table:

```text
| TX att | Level vs. att 45 | Est. tuner input | Hold (c3 = 00 rounds) | Fresh tune: lock in | 0xBC raw mean (min…max) | C/N (tc90522 formula) | 0xBA | Errors 0xEB / 0xF0 (max) | TS 10 s: sync / TEI / CC | Run |
| 45 | 0 dB | −78…−68 dBm | 90/90 | 121 ms | 2930 (2918…2938) | above range | 63 | 0 / 0 | 0 / 0 / 0 | s1-45 |
```

followed by: the hold threshold, the acquisition threshold, the hysteresis, the TS-error
onset (all in att and in dB below att 45), the raw-vs-att slope over the linear range, the
second-pass offset if one was needed, and the caveats (absolute level unmeasured; att > 80
not linear; the 0xEB/0xF0 and polynomial semantics INFERRED). A one-line ISDB-T comparison on
the same chain (`isdb tx --mux-config` UHF into the DT295 terrestrial side, `dt295 status
--watch`) would make the "next to the ISDB-T figures" literal; optional.

## 7. Implementation split

| # | Piece | Depends on | Where |
|---|---|---|---|
| A | bench script: `--media`, live `isdb mux` feed, loop/`exec` fix | — | isdb-rs `tools/` |
| B | `tools/media-decode-check.py` (§2.4 checks, framemd5 set membership, DTS steps) | — | isdb-rs `tools/` |
| C | bench run: record + B; VERIFICATION entry | A, B (item 1 only for validate/TV) | bench |
| D | driver: summary text, raw 0xBC stats, `ber()`, CSV/JSON; optional poll in `stream` | — | PIX-DT295 repo |
| E | `tools/isdbs-level-sweep.sh` (+ `.py` table/fit) | D optional (parses text without it) | isdb-rs `tools/` |
| F | sweep run(s), VERIFICATION section | E, a `iio_attr` live-write check | bench |
| G | isdbs-core packet slot time (shared with item 1) | — | isdbs-core |
| H | isdbt-mux `Schedule` refactor (ISDB-T byte-identical) | — | isdbt-mux |
| I | satellite schedule + `with_schedule`/`next_period` | G, H; item 2 for BS-shaped SI | isdbt-mux |
| J | `tx/loopback --system isdbs --mux-config`, docs | I | isdb-cli, docs |
| K | optional: run-time attenuation in `isdb tx` | — | isdb-radio, isdb-cli |

Status 2026-10-31: D done (PIX-DT295 `d4ad6dc`: `status --json`, raw C/N
in the summary, `ber()`; no poll inside `stream`), E done
(`tools/isdbs-level-sweep.sh`, `.py`, `-test.sh`; it parses the driver's
JSON lines, not the human format); F waits for TX1 to be reconnected
([isdbs-next.md](../isdbs-next.md) item 4).

## 8. Open questions

- Absolute TX1 level at 1049.48 MHz for this waveform (PA on the PlutoSky R2 or not); needs a
  power meter or spectrum analyser.
- TC90532 C/N scale: raw 0x0B66–0x0B7A at att 45 is outside the tc90522 polynomial; the sweep
  gives raw vs. relative level, not absolute C/N.
- Does the TC90532 re-lock by itself after a loss (hold vs. acquisition thresholds)?
- Is the threshold inside the linear att range with a 30 dB pad, or is the +20 dB pass
  needed?
- 0xEB/0xF0 meaning (INFERRED from tc90522); 0xBA meaning (0x63 here, 0x58/0x5A off-air).
- `iio_attr` writes during a symbols stream: harmless as expected? (one-step bench check).
- Does the DT295 pass ISDB-S ES PIDs with `--raw-config` as it does for ISDB-T (§2.3)?
- ISDB-S PCR arrival model (constant one-superframe deinterleave delay): a SPEC-NOTES entry
  against B20/B21.
- Multi-TS in-process muxing (several TOMLs) wanted, or one TS per run enough for the bench?
