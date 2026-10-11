# Getting started

This walk-through builds the `isdb` command-line tool, runs the ISDB-T
modulator and demodulator in a software loopback, generates a transport
stream with the multiplexer, and analyses it. Nothing here needs a radio or
any captured data. For transmitting with a HackRF, continue with
[transmitting.md](transmitting.md) — and read its safety section first.

Terms such as TS, PSI/SI, TMCC, layer and partial reception are explained
in the [glossary](glossary.md).

## Prerequisites

- **Rust ≥ 1.88** (edition 2024), e.g. from [rustup](https://rustup.rs).
- **libhackrf** 2022.09.1 or later, the HackRF host library, for the
  `hackrf` cargo feature of `isdb-cli` (on by default). The default build
  links it dynamically, so it is needed even when no radio is attached:

  | System | Package |
  |---|---|
  | Arch Linux | `hackrf` |
  | Debian / Ubuntu | `libhackrf-dev` |
  | Fedora | `hackrf-devel` |

  The library crates (`isdb-ts`, `isdb-si`, `isdbt-phy`, …) do not need it;
  only `isdb-hackrf` and the default `isdb-cli` build do. Without it, build
  with `--no-default-features`: everything on this page works, and only
  `--hackrf` is unavailable.
- Optional: `ffmpeg` / `ffprobe` to make test media and to check the
  multiplexer's output.

The project is developed and tested on Linux. Other platforms have not been
tried.

## Build

```sh
git clone https://github.com/a1678991/isdb-rs && cd isdb-rs
cargo build --release -p isdb-cli          # → target/release/isdb
export PATH="$PWD/target/release:$PATH"
isdb --help
```

Alternatively `cargo install --path crates/isdb-cli --locked` puts `isdb`
into `~/.cargo/bin`. Always use an optimised build for anything real time:
the modulator needs it to keep up with a radio.

`isdb <command> --help` is the authoritative option list. The commands are:

| Command | What it does |
|---|---|
| `isdb tx` | transport streams → ISDB-T baseband → IQ file or HackRF |
| `isdb rx` | IQ file or HackRF → ISDB-T demodulation → transport stream |
| `isdb loopback` | `tx` → software channel (noise, CFO, timing offset) → `rx`, comparing every packet |
| `isdb mux` | service multiplexer: TOML description (+ media) → transport stream |
| `isdb ts` | transport-stream analysis: `inspect`, `validate`, `diff`, `edit`, `extract` |

## First loopback

```sh
isdb loopback --synthetic
```

```text
isdb loopback: Mode 3, GI 1/8, partial reception; A 1seg QPSK 2/3 TI2 (64 TSP/frame); B 12seg 64QAM 3/4 TI2 (2592 TSP/frame)
  input: synthetic test packets (PID 0x0100/0x0200/0x0300 for A/B/C)
  channel: SNR ∞, CFO +0 carriers, sample offset 0; lead 2, flush 4 frames
[loopback] sent 12 frames (6 data); received 11 frames (8 valid, 2 before anchor); TS frames exact 6/6
[loopback]   layer A: PASS — 384 packets byte-exact (384 input), 0 FAIL, 0 input packets lost; RS corrected 0 uncorrectable 0; MER 118.6 dB
[loopback]   layer B: PASS — 15552 packets byte-exact (15552 input), 0 FAIL, 0 input packets lost; RS corrected 0 uncorrectable 0; MER 125.1 dB
[loopback] PASS
```

What happened:

- The transmission parameters are the defaults, which match Japanese
  terrestrial broadcasts: Mode 3 (8k FFT), guard interval 1/8, partial
  reception, layer A = 1 segment QPSK 2/3 (the one-seg service), layer B =
  12 segments 64QAM 3/4, time-interleave index 2. Per OFDM frame (≈ 231 ms)
  layer A carries 64 transport packets and layer B 2592.
- Six frames of deterministic packets were modulated, preceded by two null
  "lead" frames and followed by four "flush" frames that push the last
  packets through the interleavers ([architecture.md](architecture.md#frame-and-latency-contract)).
- The receiver locked, decoded TMCC to learn the parameters, and returned
  every packet byte-exact. The MER is ≈ 120 dB because the channel is
  noise-free float arithmetic.
- `isdb loopback` exits non-zero on any failure, so it can be used in
  scripts (`--json` prints a machine-readable verdict).

Add impairments, or change the transmission parameters (the same options
exist on `tx` and `mux`):

```sh
# 22 dB SNR, 2.3 carrier spacings of frequency offset, receiver starts mid-frame
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3 --sample-offset -700000

# Mode 2, GI 1/16, three layers, no partial reception
isdb loopback --synthetic --mode 2 --gi 16 --partial false \
    --layer-a 3,qpsk,1/2,2 --layer-b 5,16qam,2/3,2 --layer-c 5,64qam,5/6,1
```

A layer is `SEGMENTS,MODULATION,CODE_RATE,TI` with modulation
`dqpsk|qpsk|16qam|64qam`, code rate `1/2|2/3|3/4|5/6|7/8` and TI the TMCC
time-interleave index 0–3 (the interleave length it means depends on the
mode, ARIB STD-B31 Table 3-2). Segments must add up to 13, and with
partial reception layer A must be exactly one segment.

## Generate a test multiplex

`isdb mux` turns a TOML description of a network and its services into a
transport stream paced to the OFDM frame. The shipped example
(`isdb mux --print-config-example`, the same as
[`crates/isdbt-mux/examples/bench.toml`](../crates/isdbt-mux/examples/bench.toml))
needs an MPEG-2 video and an AAC file, so start with a configuration whose
only stream is *declared* in the PMT without any media:

```sh
cat > test.toml <<'EOF'
[network]
network_id = 0x79EE            # placeholder: pick an ID unused where you are
name = "テスト"
area_code = 0x5A5
remote_control_key_id = 9
channels = [27]

[transport]
transport_stream_id = 0x79EE

[clock]
source = "fixed"
start = "2026-01-01T12:00:00"

[[service]]
service_id = 0x0400
name = "テストHD"
provider = "テスト"
layer = "B"
pmt_pid = 0x01F0
pcr_pid = 0x01FF
copy_control = { recording_control = "copy-free" }

  [[service.stream]]
  kind = "declared"
  pid = 0x0111
  stream_type = 0x02
  component_tag = 0x00

  [[service.event]]
  event_id = 0x0001
  start_offset_s = -600
  duration = "01:00:00"
  name = "試験放送"
EOF
isdb mux -c test.toml --seconds 5 --out-ts test.ts --report
```

The multiplexer generates PAT, CAT, PMT, NIT, SDT, EIT present/following,
TOT and BIT at the repetition rates of ARIB STD-B10, a PCR four times per
frame, and null packets for the rest. `--report` measures the result
(section intervals, PCR spacing, timestamps, continuity) independently of
the generator. Every key of the file is described in
[mux-config.md](mux-config.md).

Outputs:

- `--out-ts FILE` — the single TS a 13-segment receiver reproduces: all
  layers interleaved in the slot order of ARIB STD-B31 §3.2, with null
  packets in the slots of no layer (≈ 29.96 Mbit/s in Mode 3, GI 1/8). This
  is the file to analyse or to play.
- `--out-a/--out-b/--out-c FILE` — the packets of each layer, exactly
  `tsp_per_frame` per OFDM frame, as `isdb tx --a-ts/--b-ts/--c-ts` takes
  them.
- `--frames N` or `--seconds S` sets the length (default 10 s).

## Inspect and validate

```sh
isdb ts inspect test.ts
```

prints the packet count and duration, a PID table (rate, continuity and PCR
counts), PCR statistics, the programs with their PMT descriptors, a
decoded summary of every SI table (network name, delivery-system
parameters, remote-control key, service names, events) and per-section
repetition intervals. `--decode` adds the full field tree of every section,
`--json` gives the same as JSON.

```sh
isdb ts validate test.ts
```

```text
== validate test.ts (5.089 s) ==
WARNING [pid.missing] H.222.0 §2.4.4 ("Every program shall be fully defined within the Transport Stream itself") — program 0x0400: elementary PID 0x0111 (stream_type 0x02) listed in PMT has no packets

-- B10 Part 2 Table 5-2 transmission frequency (operational criterion) --

0 errors, 1 warnings, 2 info
```

Every finding names the clause it checks. The one warning is expected: the
declared video stream has no packets. Validation exits non-zero when there
are errors (`--fail-on` changes the threshold, `--show info` prints
everything). [analysis.md](analysis.md) covers `validate` options,
`diff`, `edit` and `extract`.

## Through the PHY and back, in files

```sh
isdb tx --mux-config test.toml --iq test.cf32 --seconds 3
isdb rx --iq test.cf32 --out rx.ts --json
isdb ts diff test.ts rx.ts
```

- `isdb tx --mux-config` runs the multiplexer in-process and modulates its
  output; `--iq` writes interleaved little-endian float32 I/Q at
  512/63 MHz ≈ 8.127 MS/s (GNU Radio "complex" format). That is about
  65 MB per second of signal; `--iq-format cs8` (signed 8-bit, the
  `hackrf_transfer` format) is 16 MB/s. An input that never ends (a
  multiplexer, `--synthetic`, `--feed loop`) needs `--seconds` when writing
  to a regular file.
- `isdb rx --iq` demodulates the file, prints a status line per second of
  signal and writes the reproduced TS of the valid frames
  ([receiving.md](receiving.md)).
- `isdb ts diff` compares the two streams table by table; every section
  should be `identical`. The received file is shorter: frames before lock
  and the interleaver start-up are not output.

The same with per-layer files:

```sh
isdb mux -c test.toml --frames 10 --out-a a.ts --out-b b.ts
isdb tx --a-ts a.ts --b-ts b.ts --iq x.cf32
isdb rx --iq x.cf32 --a-out a.out.ts --b-out b.out.ts --strip-nulls
```

## Using your own media

The example configuration expects `media/video.m2v` (an MPEG-2 video
elementary stream) and `media/audio.m2a` (AAC in ADTS frames) next to the
TOML file. For a quick test, FFmpeg can make both:

```sh
mkdir -p media
ffmpeg -f lavfi -i testsrc2=size=1440x1080:rate=30000/1001 -t 10 -vf setfield=tff \
    -c:v mpeg2video -b:v 12M -maxrate 12M -bufsize 7M -g 15 -bf 2 \
    -flags +ilme+ildct -aspect 16:9 -f mpeg2video media/video.m2v
ffmpeg -f lavfi -i sine=frequency=1000:sample_rate=48000 -ac 2 -t 10 \
    -c:a aac -b:a 192k -f adts media/audio.m2a
isdb mux --print-config-example > bench.toml
isdb mux -c bench.toml --seconds 10 --out-ts bench.ts --report --adts-compliance warn
```

`--adts-compliance warn` is needed here. ARIB STD-B32 Part 2 §5.2 requires
ADTS with crc_check (protection_absent '0'), MPEG-2 ID and a constant-rate
adts_buffer_fullness; FFmpeg's ADTS has none of these, so the default
policy (`conform`) refuses the file and lists every violated rule with its
clause. `warn` sends it unchanged and reports the violations, and
`isdb ts validate bench.ts` will flag them as `aac.*` errors. The
`--adts-compliance` override exists only on `isdb mux`; for `isdb loopback`
and `isdb tx --mux-config`, set `adts_compliance = "warn"` on the
`aac-adts` stream in the TOML instead. Use an encoder that writes
conforming ADTS for anything beyond a smoke test. The video
needs no such step: the multiplexer packetises it as it is (vbv_delay is
set to 0xFFFF as B32 Part 1 requires).

## Next steps

- [transmitting.md](transmitting.md) — bench set-up, `isdb tx --hackrf`,
  relaying a recording, receiver compatibility notes.
- [receiving.md](receiving.md) — `isdb rx` in detail.
- [mux-config.md](mux-config.md) — every multiplexer configuration key.
- [analysis.md](analysis.md) — `isdb ts` in detail.
- [architecture.md](architecture.md) — how the crates fit together.
- [test-data.md](test-data.md) — running the optional tests on your own
  captures and the gr-isdbt oracle.
