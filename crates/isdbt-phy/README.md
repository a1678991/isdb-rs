# isdbt-phy

ISDB-T transmitter and receiver pipelines: per-layer 188-byte transport
stream packets ⇄ complex baseband at 512/63 MHz. The crate wires the
bit-domain channel coding of [`isdbt-fec`](https://github.com/a1678991/isdb-rs/tree/main/crates/isdbt-fec) and the
symbol-domain processing of [`isdbt-ofdm`](https://github.com/a1678991/isdb-rs/tree/main/crates/isdbt-ofdm) into one
frame-synchronous TX and one RX object, adds the stream-boundary rules
(pre-roll, lead-in, flush) and IQ sample-format conversion. It contains no
signal processing of its own.

## Spec scope

ARIB STD-B31 v2.2, Fig. 3-2 (transmission system block diagram), as the
composition of:

| Stage | Clause | Implemented in |
|---|---|---|
| TS re-multiplex, multiplex-frame pattern, hierarchical division, §3.2.2 TS reproduction | §3.2, §3.4 | `isdbt_fec::remux` |
| RS(204,188), energy dispersal, delay adjustment, byte interleaving, convolutional code | §3.3–3.8 | `isdbt_fec::layer` |
| Mapping … OFDM frame, IFFT/GI; synchronisation, demodulation | §3.9–3.16 | `isdbt_ofdm` |
| TMCC current/next information | §3.15.6 | `isdbt_ofdm::tmcc` |
| Null packets for empty slots and filler frames | ITU-T H.222.0 §2.4.3.3 | `isdbt_fec::remux::null_tsp` |

Packets of uncorrectable RS words are marked with
`transport_error_indicator = 1` (H.222.0 §2.4.3.3).

## Position in the stack

- Depends on: `isdbt-core` (parameters), `isdbt-fec`, `isdbt-ofdm`.
- Used by: `isdb-cli` (`isdb tx`, `isdb rx`, `isdb loopback`); `isdbt-mux`
  only as a dev-dependency (its tests feed the multiplexer's frames to a
  `Transmitter`); the stand-alone oracle crate `oracle/rust_tx3`.

## Key types and entry points

| Item | Role |
|---|---|
| `Transmitter` | one call per OFDM frame: `transmit_frame([&[Packet]; 3])` (exactly `tsps_per_frame(layer)` packets per active layer) or `transmit_multiplex_frame(&[Packet])` (one §3.2.1 multiplex frame, divided by the model-receiver pattern, null slots dropped) → `Vec<C32>` of `params.samples_per_frame()` samples |
| `Transmitter::preroll` / `lead_frames` / `finish` / `flush_frames` / `transmit_null_frame` | stream boundaries (see design notes) |
| `Transmitter::set_tmcc`, `set_gain` | override TMCC information (emergency flag, countdown); output scaling |
| `Receiver` | `push(&[C32])` with any chunking → `Vec<RxFrame>` |
| `PipelinedReceiver` | the same on two worker threads (demodulation, decoding) with bounded queues: `push(Vec<C32>)`, `try_recv()`, `finish()` |
| `FrameDecoder` | the channel-decoding half alone: `DemodFrame` (LLRs) → `RxFrame` |
| `RxFrame` | `tx_frame`, `valid`, per-layer packets and `RsStats`, the reproduced TS `ts`, TMCC, `Params`, demodulator `Metrics`, `start_sample` |
| `iq` | `cf32_from_bytes` / `cf32_to_bytes` (interleaved little-endian f32, GNU Radio complex); `cs8_from_samples` (HackRF signed 8-bit, scaled, saturating, returns the clip count) / `samples_from_cs8` |

Software loopback of one data frame (Mode 1, one 13-segment layer); every
input packet comes out of the receiver:

```rust
use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave};
use isdbt_ofdm::DemodConfig;
use isdbt_phy::{Packet, Receiver, Transmitter, null_packet};

let a = LayerConfig {
    segments: 13,
    modulation: Modulation::Qpsk,
    code_rate: CodeRate::R1_2,
    time_interleave: TimeInterleave(0),
};
let params = Params::new(Mode::M1, GuardInterval::G1_8, false, [Some(a), None, None])?;
let mut tx = Transmitter::new(params.clone());
tx.preroll()?; // discard start-up fill frames
let mut iq = Vec::new();
for _ in 0..tx.lead_frames() {
    iq.extend(tx.transmit_null_frame()?);
}
let sent: Vec<Packet> = (0..tx.tsps_per_frame(Layer::A))
    .map(|i| {
        let mut p = null_packet();
        p[1] = 0x01; // PID 0x100
        p[4..8].copy_from_slice(&(i as u32).to_be_bytes());
        p
    })
    .collect();
iq.extend(tx.transmit_frame([&sent, &[], &[]])?);
iq.extend(tx.finish()?);
for _ in 1..tx.flush_frames() {
    iq.extend(tx.transmit_null_frame()?);
}

let mut rx = Receiver::new(DemodConfig::default());
let got: Vec<Packet> = rx
    .push(&iq)?
    .into_iter()
    .filter(|f| f.valid)
    .flat_map(|f| f.layers[0].clone().unwrap())
    .filter(|p| *p != null_packet())
    .collect();
assert_eq!(got, sent);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Examples (file tools)

Built with `cargo build --release -p isdbt-phy --examples`; used by the
gr-isdbt differential test ([VERIFICATION.md](https://github.com/a1678991/isdb-rs/blob/main/VERIFICATION.md)).
Arguments are positional and unchecked (they panic on bad input).

```text
tx_file OUT.cf32 FRAMES MODE GI PARTIAL A-SPEC A.ts [B-SPEC B.ts [C-SPEC C.ts]]
    MODE 1|2|3, GI 4|8|16|32 (denominator), PARTIAL 0|1,
    layer spec "segs,mod,rate,ti" e.g. 12,64qam,3/4,2
    (mod dqpsk|qpsk|16qam|64qam; ti = TMCC time-interleave index 0..3).
    Layer TS files are read cyclically; the transmitter is pre-rolled and
    the file ends with finish() + the flush frames.
rx_file IN.cf32 OUT_PREFIX
    Writes OUT_PREFIX.{a,b,c}.ts (packets of valid frames) and
    OUT_PREFIX.mux.ts (the §3.2.2 reproduced TS); per-frame status on stderr.
```

## Design notes

- **Frame contract.** The modulator holds one frame (B31 Part 2 Fig. 3-8
  one-frame buffer): the call receiving input frame `k + 1` emits on-air
  frame `k`. The demodulator yields LLRs of data frame `m − L` with on-air
  frame `m` (`L = isdbt_ofdm::latency::latency_frames`, the largest Table
  3-12 delay of the active layers), and the FEC pair adds one frame (§3.6,
  `isdbt_fec::layer::LATENCY_FRAMES`). `RxFrame::tx_frame =
  frame_number − L − 1`, counted from the on-air frame at which the
  receiver locked (warm-up outputs are negative); `RxFrame::valid` is set
  once both the current and the previous LLR frame were valid. Details in
  [`isdbt-ofdm/src/latency.rs`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-ofdm/src/latency.rs) and the
  workspace [SPEC-NOTES](https://github.com/a1678991/isdb-rs/blob/main/SPEC-NOTES.md) cross-crate entry.
- **Stream boundaries.** `preroll()` modulates and discards
  `end_to_end_frames + 1` null frames, because the FEC encoder's
  byte-interleaver / delay-adjustment start-up fill is not
  energy-dispersed and would give > 30 dB PAPR (clipping an 8-bit DAC).
  `lead_frames()` = 2 null frames before data let a receiver started with
  the transmitter (and up to one frame of acquisition delay) output every
  packet. After the last data frame, `finish()` plus `flush_frames() − 1`
  null frames (= modulator buffer + `L` + FEC frame + one frame of
  channel-estimation look-ahead) bring every packet out of a receiver.
- **Multiplex frame.** `transmit_multiplex_frame` and `RxFrame::ts` use the
  §3.2.2 model-receiver slot pattern (`isdbt_fec::remux::MultiplexPattern`),
  so a single TS can be carried and reproduced with null packets in the
  slots of no layer.
- **Receiver reconfiguration.** `FrameDecoder` rebuilds its layer decoders
  whenever the TMCC-derived `Params` change.
- **Threading.** `PipelinedReceiver` runs the `Demodulator` and the
  `FrameDecoder` on threads `rx-demod` and `rx-decode`, connected by a
  `depth`-chunk sample queue and a 2-frame queue; the output queue is
  unbounded so a caller blocked in `push` cannot deadlock the pipeline.
  Decode errors are delivered as `Err` items through `try_recv` /
  `finish`.

## Verification

| Test | What it proves |
|---|---|
| `tests/loopback.rs` `ts_loopback_hierarchical_mode3` | bench parameters (Mode 3, GI 1/8, partial reception, A 1 seg QPSK 2/3, B 12 seg 64QAM 3/4, I = 2): 64 / 2592 TSP per frame, TX → CFO +1.7 carriers + 28 dB AWGN → RX is byte-exact per layer, no uncorrectable RS word in layer B, and the reproduced TS has the multiplex-frame length and the layer-A slot count |
| `tests/stream.rs` `lead_and_flush_deliver_every_packet` | with pre-roll, `lead_frames` and `finish` + flush, every input packet of every frame is received, in order, for Mode 1 I = 3, Mode 3 I = 2 and Mode 3 I = 0 |
| `tests/stream.rs` `preroll_removes_fill_peaks` | PAPR < 15 dB with pre-roll (the un-pre-rolled figure is printed only) |
| `tests/stream.rs` `pipelined_receiver_matches_inline` | `PipelinedReceiver` returns the same frames as `Receiver` |

The PHY as a whole was cross-decoded against gr-isdbt in both directions,
byte-exact in 8 parameter sets, using `examples/tx_file.rs` /
`examples/rx_file.rs` ([VERIFICATION.md](https://github.com/a1678991/isdb-rs/blob/main/VERIFICATION.md)). No test
needs captures or other files outside the repository.

```text
cargo test --release -p isdbt-phy
```

## Known limitations

- No `Transmitter` API for AC data: `transmit_frame` always sends AC
  stuffing (`AcFrame::stuffing`); seismic-warning / serial AC are only
  reachable through `isdbt_ofdm::Modulator` directly. TMCC switching is
  possible only by replacing the whole `TmccInfo` with `set_tmcc`.
- `Transmitter` parameters are fixed at construction; a parameter change
  needs a new transmitter (and its pre-roll).
- `RxFrame::tx_frame` is relative to the lock frame; the absolute frame
  number is not recoverable from the signal.
- Impairment behaviour (noise, CFO, sample-rate offset, multipath) is
  tested only in software loopback, not against the gr-isdbt oracle or
  off-air signals (VERIFICATION.md "Open questions").
- This crate has no SPEC-NOTES of its own; interpretations live in
  [isdbt-fec](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-fec/SPEC-NOTES.md),
  [isdbt-ofdm](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-ofdm/SPEC-NOTES.md) and the workspace index
  [SPEC-NOTES.md](https://github.com/a1678991/isdb-rs/blob/main/SPEC-NOTES.md).

See also the workspace [README](https://github.com/a1678991/isdb-rs/blob/main/README.md).
