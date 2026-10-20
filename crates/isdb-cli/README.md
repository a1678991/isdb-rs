# isdb — ISDB-T streamer and TS tools

`isdb` drives the spec crates of this workspace from the command line:
transport streams → ARIB STD-B31 baseband → IQ file or radio, and back,
plus the service multiplexer and TS analysis tools, and receive-only raw
IQ capture and survey of satellite IF (ISDB-S, ISDB-S3; no demodulator
yet).

```text
cargo build --release -p isdb-cli      # → target/release/isdb
cargo build --release -p isdb-cli --no-default-features   # without libhackrf and libiio
```

Radios are selected with `--radio NAME[:key=value,...]` and reached
through `isdb-radio`; this crate contains no device code. Two cargo
features (both default on) forward to the backends of `isdb-radio`:
`hackrf` links the system libhackrf (≥ 2022.09.1) through `isdb-hackrf`,
`pluto` (ADALM-Pluto family, e.g. a PlutoSky R2 over Ethernet) the system
libiio 0.x through `isdb-iio`. Built without one, every file-based
command works and `--radio` with that backend (also with `--dry-run`)
fails with a message naming the feature.

> **Legal and safety notice** (also printed by `isdb tx --help`):
> radiating on broadcast frequencies requires a licence (in Japan the
> Radio Act, elsewhere local law). `isdb tx --radio` is for shielded or
> conducted (coax + attenuator) lab setups only and radiates only with
> `--rf-enable`. `isdb` does not descramble (no B-CAS / CAS support) and
> refuses scrambled input; you are responsible for the rights to any
> content you transmit or relay.

Position in the stack: the only binary crate, on top of every library
crate. It calls `isdb-iq` (recordings, SigMF, survey), `isdbt-phy` (`Transmitter`, `PipelinedReceiver`, `iq`),
`isdbt-core`/`isdbt-fec`/`isdbt-ofdm` (parameters, multiplex pattern,
demodulator configuration), `isdbt-mux` (`Multiplexer`), `isdb-analyze`
(`isdb ts`) and `isdb-radio` (`IqSink` / `IqSource`, the backend
registry); `isdb-ts`/`isdb-si`/`isdb-text`, `isdb-hackrf` and `isdb-iio` are reached
only through `isdbt-mux`/`isdb-analyze`/`isdb-radio` (not direct
dependencies). It adds I/O, threading, pacing, the `--rf-enable` gate and
reporting; no spec logic and no device code lives here (no SPEC-NOTES of
its own).

Transmission parameters (`tx`, `loopback`, `mux`) default to the Japanese
terrestrial norm: `--mode 3`, `--gi 8`, `--partial true`, layer A =
`1,qpsk,2/3,2` (one-seg, 64 TSP/frame), layer B = `12,64qam,3/4,2`
(2592 TSP/frame), layer C `none`. Layer specs are `SEGMENTS,MOD,RATE,TI`
(`--layer-a/--layer-b/--layer-c`, `none` for unused) with MOD
`dqpsk|qpsk|16qam|64qam`, RATE `1/2|2/3|3/4|5/6|7/8` and TI the TMCC
time-interleave index 0..3. `isdb <cmd> --help` is the authoritative option
list; the sections below explain behaviour. Task-oriented guides are in the
workspace [`docs/`](https://github.com/a1678991/isdb-rs/blob/main/docs/getting-started.md): getting started,
[transmitting](https://github.com/a1678991/isdb-rs/blob/main/docs/transmitting.md) (bench set-up, levels, channel
plan, receiver compatibility notes), [receiving](https://github.com/a1678991/isdb-rs/blob/main/docs/receiving.md),
[analysis](https://github.com/a1678991/isdb-rs/blob/main/docs/analysis.md) and the multiplexer
[configuration reference](https://github.com/a1678991/isdb-rs/blob/main/docs/mux-config.md).

## Commands

### `isdb tx` — TS → ISDB-T → sink

Inputs (one of):

* `--a-ts/--b-ts/--c-ts PATH` per-layer TS (`-` = stdin; FIFOs fine). A layer
  without input carries null packets.
* `--mux-ts PATH --a-pids 0x0000,0x1fc8,… [--c-pids …]` one TS split by PID
  (relay of a station mux): listed PIDs go to A/C, every other PID to B,
  input null packets are dropped. Input order is preserved: when the next
  packet's layer is full for the current frame, the frame is closed and the
  other layers are padded. With `--feed file`/`loop` the input is **paced
  by its own PCR** (H.222.0 §2.4.2.2: every packet's arrival time is
  interpolated between the PCRs of the first PCR PID, wraps unwrapped,
  discontinuities and loop rewinds bridged): OFDM frame *k* carries the
  packets due before its end and null padding, so a file goes on air at
  its recorded rate, not at layer capacity. A regular file is probed first
  (up to 30 s of PCR / 128 MB): the per-layer mean and 1-s peak rates are
  printed against the layer capacities (ARIB STD-B31 Table 3-3, from
  `isdbt-core`), and a mean above a layer's capacity is **refused** with
  the layer layouts that fit (same mode; other code rates, a single
  13-segment layer, shorter guard intervals) unless `--allow-overrate`
  (then it runs behind its schedule — slow motion — and every status line
  shows `OVER CAPACITY` and the lag). An input without PCR is not paced.
  For a recorded *program* of another network, use `--relay FILE` (or
  `--mux-config` with a `ts-program` source) instead (see "Rebroadcast a
  recording" below): relaying a foreign TS as is keeps that network's
  NIT/SDT, which a terrestrial receiver will not register.
* `--mux-config FILE.toml` the isdbt-mux service multiplexer run in-process
  (`Multiplexer::next_frame()` straight into the modulator, same
  `--mode/--gi/--partial/--layer-*` parameters), so a synthetic service goes
  on air without files or pipes. Never ends by itself (unless every
  `ts-program` relay has `at_eof = "end"`).
* `--relay FILE [--program N | --service-name S] …` a recorded program
  rebroadcast as a terrestrial service, the multiplexer configuration
  built in memory (see "Rebroadcast a recording"); `--auto-layout` adapts
  the layer to its rate. Never ends by itself with `--at-eof loop`
  (the default).
* `--synthetic` deterministic test packets (PID 0x0100/0x0200/0x0300 for
  A/B/C); never ends by itself.

Feed policy `--feed`:

* `file` (default) — read ahead, end at EOF;
* `loop` — rewind at EOF (continuity counters jump);
* `live` — never block the modulator: whatever has arrived is sent, the rest
  of the frame is H.222.0 null packets (constant bit rate), the null-fill
  ratio is reported per status line; ends when the writer closes. With
  `--iq` the frames are paced to real time (also `--realtime`).

Sinks: `--iq PATH [--iq-format cf32|cs8]` (`-` = stdout) or
`--radio SPEC`:

| flag | default | meaning |
|---|---|---|
| `--radio NAME[:key=value,...]` | — | radio backend and its device arguments, e.g. `hackrf`, `hackrf:serial=0000abcd`, `pluto:uri=ip:192.168.2.1`; an unknown name lists the compiled-in backends and their cargo features |
| `--channel N` / `--freq HZ` | — | UHF ch 13..62 → 473 + 6(N−13) + 1/7 MHz; the centre must lie in the 470–710 MHz TV band (ch 13–52) |
| `--freq-unrestricted` | off | allow a centre outside 470–710 MHz (shielded lab only); the backend's own range still applies (HackRF 1 MHz–6 GHz, Pluto TX 47 MHz–6 GHz) |
| `--freq-corr-ppm P` | 0 | divide tuning and sample clock by 1 + P·1e-6 |
| `--gain NAME=VALUE` | backend's | repeatable; names, ranges and defaults are the backend's (an unknown name lists them). HackRF TX: `txvga=0..47` dB (0), `amp=on\|off` (RF amplifier ≈ +11 dB, off); Pluto TX: `att=0..89.75` dB attenuation in 0.25 dB steps (89.75) |
| `--backoff-db DB` | 10 | OFDM RMS below integer full scale (radios with integer samples, e.g. HackRF cs8, Pluto cs12, and cs8 files); clipped components counted |
| `--rf-enable` | — | **required to radiate**. `--radio` needs `--rf-enable` or `--dry-run` (otherwise the command line is rejected); with `--dry-run` it prints the backend's settings and exits with an error ("not transmitting") before opening the device |
| `--seconds S` | 0 | bound on air time incl. lead-in/flush; `0` = unbounded: until the input ends (`file` at EOF, `live` when the writer closes, `--relay --at-eof end`) or Ctrl-C / SIGTERM — `--synthetic`, `--mux-config`, `--relay` and `--feed loop` never end. The start-up line says `duration: unbounded (…); stop with Ctrl-C`. An endless input into an `--iq` regular file is refused without `--seconds` (the file would fill the disk; `--seconds 0` forces it); stdout, FIFOs and devices such as `/dev/null` need none |
| `--bb-filter-hz HZ` | 6 MHz | analog (baseband filter) bandwidth request; the backend picks a supported bandwidth (HackRF: the largest MAX2837 setting ≤ the request; Pluto: the AD936x RF bandwidth, 0.2–40 MHz TX / 56 MHz RX) |
| `--queue-frames`, `--prefill-frames` | 4 / 2 | frames produced ahead of the sink; frames converted before the radio starts |
| `--status-secs` | 5 | status line interval (0 = final summary only) |
| `--allow-overrate` | off | send a `--mux-ts` file whose PCR rate exceeds a layer's capacity (otherwise refused with layouts that fit) |
| `--iq-in PATH`, `--iq-in-format cf32\|cs8` | — / cf32 | replay a baseband file instead of modulating TS input: parameters and frame start found by the receiver from its TMCC, scaled to unit RMS, no lead-in or flush added; `--feed loop` repeats it |
| `--seek-step S` | 10 | seconds per ←/→ playback key |
| `--no-keys` | off | do not read playback keys from the terminal |

**Playback keys** (`--relay` / `ts-program` relays of `--mux-config`, and
`--iq-in`; when stdin is a terminal, put in non-canonical mode without
echo and restored at exit; Ctrl-C still stops): space pauses / resumes,
←/→ seek by `--seek-step`, ↑/↓ step the play rate through 0.25, 0.5, 1,
1.5, 2, 4, 8, 16× (relays only). The relay engine is
`isdbt_mux::playback` (`Multiplexer::pause`, `resume`, `seek`,
`set_rate`); what each key does on air is described in
[docs/transmitting.md](https://github.com/a1678991/isdb-rs/blob/main/docs/transmitting.md#playback-keys).

Status lines (every `--status-secs`): air time, frames, null fill per layer,
input counters (`--mux-ts`: dropped nulls, frames closed by a full layer,
the PCR-measured input rate per layer against its capacity, the PCR lag;
`--mux-config`: late PES, errors, one status per `ts-program` relay),
**producer speed** (`producer 4.1× real time (input 0.3 + modulate 54 ms/frame)`:
the on-air time of the frames produced in the interval divided by the time
spent assembling and modulating them; waiting on the queue is not
counted), and the radio's own counters (HackRF: USB buffers and
underruns; Pluto: IIO buffers and underruns). A producer below
1× with a real-time sink prints a warning with the per-stage times.

The backend's settings (`Backend::describe_tx`, which also validates the
device arguments, gains, tuning range and `--backoff-db`) are printed before the device is
opened. SIGINT/SIGTERM stop immediately (queued samples are discarded); a
normal end drains the queue. With HackRF every exit path stops streaming,
turns the amp off and prints `device stopped streaming: yes` (error if
not); with a Pluto it stops streaming, sets the maximum attenuation and
powers the TX LO down; underruns (USB buffers sent as zeros) are reported in the status
lines and at the end.

On-air sequence: `Transmitter::preroll` (frames modulated and discarded, so
the first emitted frame already carries null packets instead of the delay
lines' start-up fill: a receiver locked from the start sees no undecodable
fill, and the FEC encoder's zero fill cannot produce clipping peaks in the
8-bit DAC), `--lead-frames` null frames (default 2 = `Transmitter::lead_frames`:
a receiver locking on the first or second frame still outputs every input
packet), the data, then `Transmitter::flush_frames` null frames (`--no-flush`
to omit) that push the last input packets through the interleavers and the
receiver look-ahead.

### `isdb rx` — IQ / radio → TS

* Source: `--iq PATH [--iq-format cf32|cs8]` (`-` = stdin, 512/63 MHz) or
  `--radio SPEC` with `--channel/--freq`, `--freq-corr-ppm`,
  `--bb-filter-hz` and repeatable `--gain NAME=VALUE` (HackRF RX:
  `lna=0..40` in 8 dB steps (16), `vga=0..62` in 2 dB steps (20),
  `amp=on|off` (off); Pluto RX: `agc=manual|slow_attack|fast_attack|hybrid`
  (slow_attack), `gain=dB` (manual gain)). `--seconds` bounds the signal time (default `0` =
  unbounded: the whole file for `--iq`, until Ctrl-C / SIGTERM for
  `--radio`).
* Outputs (valid frames only): `--out PATH` the §3.2.2 reproduced single TS,
  `--a-out/--b-out/--c-out` per-layer packets, `--strip-nulls`.
* Status lines every `--status-secs` (1) seconds of signal: lock state,
  mode/GI from TMCC, valid frames, MER per layer, CFO, RS
  clean/corrected/uncorrectable per layer. `--json` prints a summary on stdout.

### `isdb loopback` — TX → software channel → RX

Inputs and feed policy as `tx` (synthetic if none). `--frames` data frames
(default: until EOF for `--feed file` inputs, 6 otherwise). Impairments:
`--snr-db` (AWGN, `--seed`), `--cfo-carriers` (fractional carrier spacings),
`--sample-offset N` (N > 0 samples of silence first, N < 0 drop the first |N|
samples). The first valid frame with input data is anchored to the
transmitted frame with identical content; every later frame is compared
packet by packet per layer and as a reproduced TS. Prints PASS/FAIL counts
and lost input packets per layer; exits non-zero on FAIL (`--no-flush`
makes the last frames count as lost). `--json` for a machine-readable verdict.

### `isdb mux` — service multiplexer

TOML description (network, services, events, SI tables, experiment
overrides) + media files → per-layer TS paced to the OFDM frame (exactly
`tsp_per_frame` packets per layer per frame). Takes the same `--mode/--gi/
--partial/--layer-*` options as `tx`. Length: `--frames N` or `--seconds S`
of stream time (default 10 s; media loop). Outputs `--out-a/--out-b/--out-c`
and `--out-ts` (`-` = stdout).

```text
isdb mux --print-config-example > bench.toml          # annotated example (= examples/bench.toml)
isdb mux -c bench.toml --frames 40 --out-a a.ts --out-b b.ts --out-ts full.ts --report
isdb mux --relay FILE --auto-layout --seconds 20 --out-ts relay.ts   # a recording, no TOML
```

`isdb mux` also takes `--relay FILE` with the options of "Rebroadcast a
recording" (`--channel` for the NIT, default 44) and `--auto-layout`.

Receiver-facing signalling values are typed options whose names are the
B10 table entries (numbers are rejected with an error listing the names):

| TOML (per `[[service]]` / `[[service.stream]]` / `[network]`) | Descriptor | Table |
|---|---|---|
| `video_decode_control = { mode = "derived" \| "explicit" \| "off", still_picture, sequence_end_code, video_encode_format = "1080i", transfer_characteristics = "bt709" \| "pq" \| "hlg" \| "undefined" }` | 0xC8 (PMT ES) | B10 Tables 6-59/6-60/6-60-1 |
| `component_type = "derived"` \| `"1080i-16:9-without-pan-vectors"` … (video) \| `{ audio_mode = "2/0", dialog_control, for_disabilities }` (audio) | 0x50 / 0xC4 (EIT) | B10 Tables 6-5 / 6-43 |
| `copy_control = { recording_control = "copy-free" \| "provider-defined" \| "copy-one-generation" \| "copy-never" \| "source" (ts-program services: the source program's value), copy_control_type = "serial-output-encoded", aps_control_data = "copy-free", maximum_bitrate_mbps, component_control, in_pmt, in_sdt, in_eit }` | 0xC1 | B10 Table 6-39, Annex F Tables F-2/F-4 |
| `remote_control_key_id = 1..=12` | 0xCD (NIT) | B10 §6.2.42 |
| `audio_quality_indicator = "derived"` \| `"mode-1"` \| `"mode-2"` \| `"mode-3"` (aac-adts) | 0xC4 quality_indicator (EIT) | B10 Table 6-44, B32 Part 2 Attachment Ch. 3 Table 1 (`derived`: < 32 kHz → Mode 3, ≥ 256 kbit/s per stereo pair → Mode 1, ≥ 144 → Mode 2, else Mode 3; 192 kbit/s stereo gives Mode 2) |

`crates/isdbt-mux/examples/bench.toml` uses the values live broadcasters
send; `bench-minimal.toml` the derived / copy-free variants. Both reference
the placeholder media paths `media/video.m2v` / `media/audio.m2a` (relative
to the TOML; point them at your own files, no media is committed). A raw descriptor
(`descriptors`, `add_descriptors`) with the tag of an active typed one in the
same loop is rejected at load (set the typed one `off`). Every key:
[`docs/mux-config.md`](https://github.com/a1678991/isdb-rs/blob/main/docs/mux-config.md). Background and the
receiver bisection behind the copy-control default:
[`../isdbt-mux/SPEC-NOTES.md`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdbt-mux/SPEC-NOTES.md).

AAC input is checked against ARIB STD-B32 Part 2 §5.2 (`adts_compliance`
per `aac-adts` stream, overridden for all by `--adts-compliance`):
`conform` (default) sends a file whose ADTS headers satisfy every rule
byte-identically and refuses anything else, listing each rule with its
clause, the reason and what the input needs. Only the headers are checked
and nothing is rewritten: crc_check cannot be inserted into
protection_absent '1' input, and adts_buffer_fullness 0x7FF / ID '0' of
protected input is not rewritten because that would invalidate crc_check
(both need a raw_data_block walk with the ISO/IEC 13818-7 tables, not
included). Use an encoder that writes the ADTS crc_check and a
constant-rate adts_buffer_fullness itself (MPEG-2 AAC LC, 48 kHz, ≤ 5.1,
CBR). For a 0x7FF input the refusal also prints the constant-rate
fullness model. `warn` sends the file unchanged and prints each violation;
`pass-through` sends it unchanged unchecked (e.g. to reproduce an old
non-conforming stream on a receiver). Audio without crc_check and with
fullness 0x7FF — e.g. FFmpeg's ADTS output — therefore needs
`--adts-compliance warn` (or `adts_compliance = "warn"` in the TOML, which
`loopback` and `tx --mux-config` also read):

```text
isdb mux -c bench.toml --seconds 20 --out-ts raw.ts --adts-compliance warn  # file as is
```

`--out-ts` is the single multiplex-frame TS a 13-segment receiver
reproduces (nulls in the slots of no layer); `--report` measures section
intervals, PCR, timestamps and the T-STD. For transmission, prefer
`isdb tx --mux-config` (no intermediate files).

### `isdb ts` — TS analysis (`isdb-analyze`)

Common options: `--json`, `--max-packets N`, `--bitrate BPS` (timing when
the stream has no PCR). Input packets may be 188/192/204 bytes (autodetected).

* `inspect FILE` — packets, PIDs, CC, PCR, programs/PES, sections, decoded SI
  (`--decode` adds the field tree of every section).
* `validate FILE` — spec conformance findings with clause citations
  (`--show`, `--fail-on` = `info|warning|error|never`; default exits non-zero
  on errors). Bench expectations: `--expect-tsid/-onid/-nid/-services/
  -freq-mhz/-guard/-mode/-area/-remote-key/-ts-name/-network-name`. Also
  warns (`si.copy_control`) on a digital copy control descriptor with
  copy_control_type '00', which a Sony BDZ-series recorder refuses. AAC PIDs
  (stream_type 0x0F): the ADTS frames are reassembled from the PES and
  header-checked against ARIB STD-B32 Part 2 §5.2 (`aac.protection_absent`,
  `aac.profile`, `aac.sampling_frequency_index`,
  `aac.channel_configuration`, `aac.adts_buffer_fullness`,
  `aac.number_of_raw_data_blocks_in_frame`, `aac.id`: errors) with frame
  counts and the quoted text; the raw_data_block (crc_check value, element
  rules) is not checked. `aac.quality_indicator` compares the EIT/PMT
  audio component descriptor's quality_indicator with the measured rate
  (Table 1 contradictions are warnings); `aac.summary` (info) gives
  profile, rate, frames with crc_check and fullness range. (A live
  terrestrial capture recorded during development gave no AAC finding and
  a consistent '10'.)
* `diff A B` — table/field-level comparison of two captures plus PID
  inventory (e.g. ours vs a live station; `--all` lists every EIT schedule
  section).
* `edit IN OUT` — experiment transforms producing a valid TS, applied in the
  order `--null-pid`, `--remove PID:TID[:EXT[:SN]]`, `--replace …=FILE`,
  `--set nit.version=24` (typed field mutation, see `isdb_analyze::mutate`),
  `--insert …=FILE@MS` (`--insert-offset-ms`); `--null-slots
  never|needed|always` controls use of null packets for rewritten sections.
* `extract FILE` — sections to a hex fixture (`pid tid hex` per line;
  `--pid`, `--tid`, `--all-versions`, `-o`).

### `isdb capture` — radio → raw IQ recording (receive only)

Captures one contiguous block of raw samples from a radio at any rate it
supports (not only 512/63 MHz) and writes it as cs16 (little-endian I/Q)
with [SigMF](https://sigmf.org) metadata next to it: `NAME.sigmf-meta` for
`-o NAME.sigmf-data`, else the file name with `.sigmf-meta` appended. It
never transmits and has no `--rf-enable`.

```sh
isdb capture --radio pluto:uri=ip:192.0.2.30 --freq bs1 --rate 57.72M -o bs1.sigmf-data
isdb capture --radio pluto:uri=ip:192.0.2.30,recorder=maia --freq nd2 --rate 61.44M \
    -o nd2.sigmf-data --survey
```

- `--freq` is a frequency (`1049480000`, `1049.48M`) or a channel name:
  `chN` (UHF 13..62), `bsN` (BS, odd 1..23) or `ndN` (110° CS, even
  2..24), the satellite ones at their IF = RF − 10.678 GHz (BS-n IF =
  1049.48 + 19.18·(n − 1) MHz, ND-n IF = 1613 + 20·(n − 2) MHz).
- `--rate` in samples per second (`57.72M` is 2 samples per ISDB-S symbol,
  `61.44M` is the AD936x maximum); the rate the device runs at is read
  back. `--bw` defaults to 0.8 × the rate, `--gain` takes the backend's
  gains (Pluto: `agc=…`, `gain=dB`).
- `--samples` defaults to the largest contiguous capture of the backend
  (`isdb_radio::Backend::capture_limit`) and more is refused. Pluto:
  16 515 072 samples through libiio (63 MiB, 282 ms at 57.72 Msps),
  33 554 432 with `recorder=maia` (Tezuka's maia-sdr recorder, 581 ms).
  Longer recordings take `--segments N`: N captures one after another into
  the same file, each its own SigMF capture segment, the 2nd and later
  marked `isdb:gap_before` (samples between them are lost).
- The metadata records the sample rate and LO read back, the RF
  bandwidth, the gain (AGC mode and the gain at the end), the device
  identity, the capture method, the host time and the channel name
  (`isdb:` extension fields, `isdb_iq::sigmf`). Do not put station names
  in `--description` of recordings you share.
- Existing outputs are kept unless `--overwrite`; a failed capture
  removes its partial outputs; `--dry-run` prints the settings without
  opening the radio; `--survey` runs `isdb iq survey` on the result.

### `isdb iq survey` — what a single-carrier recording contains

`isdb iq survey FILE…` reads a recording (SigMF metadata, or a raw file
with `--format cs8|cs16|cs16/2048|cf32 --rate SPS`) and prints, for 4 Mi
samples of its first SigMF segment (`--samples`, `--segment`): power,
band centre and widths, the symbol-rate line of |z|² with the known
system it matches (ISDB-S 28.860 Mbaud, ISDB-S3 33.7561 Mbaud; at
2 samples per symbol the line and its alias fs − R coincide, so the sign
of the ppm deviation is ambiguous), the z², z⁴, z⁸ carrier lines (and the
π/2-BPSK z² lines at ±R/2) with the carrier offset each gives, and the
strongest one. `--json` prints one object per file. The analysis is
`isdb_iq::survey`.

### `isdb completions` — shell completions

`isdb completions SHELL` prints a completion script to stdout for `bash`
or `zsh` (also `fish`, `elvish`, `powershell`; generated by
`clap_complete`). It completes subcommands, option names and the fixed
values of options such as `--feed`, `--iq-format` or `ts validate
--fail-on`; every other option value falls back to file-name completion.
Nothing is completed from the environment (radio backends, PIDs,
service names).

```sh
# bash: loaded on demand by bash-completion …
mkdir -p ~/.local/share/bash-completion/completions
isdb completions bash > ~/.local/share/bash-completion/completions/isdb
# … or, without bash-completion, in ~/.bashrc
source <(isdb completions bash)

# zsh: a file named _isdb in a directory on $fpath …
mkdir -p ~/.zfunc
isdb completions zsh > ~/.zfunc/_isdb
# … and in ~/.zshrc, before any other compinit:
#   fpath=(~/.zfunc $fpath); autoload -Uz compinit && compinit
```

The script is generated from the installed binary's command line, so
regenerate it after upgrading `isdb`; a new shell picks it up.

## Examples

Bench configuration (ch13, HackRF TX VGA 20 dB, amp off, coax + attenuator
only):

```text
# check the settings without radiating
isdb tx --radio hackrf --channel 13 --gain txvga=20 --b-ts service.ts --a-ts oneseg.ts --dry-run
# radiate until Ctrl-C (the files loop)
isdb tx --radio hackrf --channel 13 --gain txvga=20 --rf-enable --b-ts service.ts --a-ts oneseg.ts --feed loop
# relay a live mux from a FIFO, at most 10 minutes
isdb tx --radio hackrf --channel 13 --gain txvga=20 --rf-enable --seconds 600 \
    --mux-ts /tmp/live.fifo --feed live --a-pids 0x0000,0x0010,0x0011,0x0012,0x0014,0x1fc8,0x1fc9

# file round trip
isdb tx --a-ts a.ts --b-ts b.ts --iq x.cf32
isdb rx --iq x.cf32 --out mux.ts --a-out a.out.ts --b-out b.out.ts --strip-nulls --json

# hackrf_transfer-compatible cs8 file
isdb tx --b-ts b.ts --iq x.cs8 --iq-format cs8 --backoff-db 10
# endless inputs into a file need a bound
isdb tx --synthetic --iq syn.cf32 --seconds 5

# receive with a (second) HackRF
isdb rx --radio hackrf --channel 13 --gain lna=16 --gain vga=20 --seconds 30 --out rx.ts

# software loopback through 22 dB SNR, 2.3 carriers CFO, mid-frame start
isdb loopback --frames 8 --snr-db 22 --cfo-carriers 2.3 --sample-offset -700000
```

## End-to-end bench recipe

HackRF on channel 13, TX VGA 20 dB, amp off, **coax + attenuator only**, into
the receiver under test; a capture of a live station (e.g. from a
PC tuner card) as reference.

```text
# 1. service description: copy the example, set [network] channels = [13]
#    (relative media `source` paths resolve against the TOML's directory)
isdb mux --print-config-example > bench.toml   # or crates/isdbt-mux/examples/bench.toml
isdb mux -c bench.toml --frames 40 --out-ts full.ts --report   # optional: check the mux
isdb ts validate full.ts

# 2. software loopback of exactly what will be radiated
isdb loopback --mux-config bench.toml --frames 8 --snr-db 25

# 3. check the RF settings (no radiation), then transmit until Ctrl-C
isdb tx --mux-config bench.toml --radio hackrf --channel 13 --gain txvga=20 --dry-run
isdb tx --mux-config bench.toml --radio hackrf --channel 13 --gain txvga=20 --rf-enable
#    (amp stays off unless --gain amp=on; --seconds S to bound the transmission)

# 4. receive with a second HackRF (or the tuner), then compare
isdb rx --radio hackrf:serial=<RX> --channel 13 --seconds 30 --out ours.ts --json
isdb ts validate ours.ts
isdb ts diff ours.ts live-capture.ts
```

## Rebroadcast a recording

A recorded program (unscrambled, transport_scrambling_control '00'; one
you have the rights to relay) as a terrestrial service on UHF ch44,
remote key 9, in one line:

```text
isdb tx --relay FILE --auto-layout --radio hackrf --channel 44 --gain txvga=20 --rf-enable
```

It runs until Ctrl-C (the recording loops; `--seconds S` to bound,
`--at-eof end` to stop after one pass).

`--relay FILE` (on `tx`, `loopback` and `mux`) builds the configuration of
`crates/isdbt-mux/examples/relay-ts.toml` in memory (`kind = "ts-program"`:
the program's PES go out unchanged at the recording's own PCR pace with
PTS/DTS rebased onto this multiplex; NIT/SDT/EIT/PMT/CAT/TOT/BIT are
terrestrial ones: service 0x0400, PMT 0x01F0, PCR 0x01FF, network/TS
0x79EE (a placeholder), the NIT naming the transmit channel). The
recording's conditional-access signalling is kept by default (`--ca
keep`): its PMT conditional_access_descriptors, the ECM PIDs they name
and its CAT's CA_descriptors with the EMM PIDs present in the recording
are relayed, CA_PIDs following the PID mapping; the payload stays
unscrambled, so receivers see CA signalling with a clear payload.
`--ca drop` relays none of it. Nothing is written by hand: the program is the recording's only
one (`--program N` or `--service-name S` for multi-service recordings; a
missing or ambiguous choice lists the programs with SDT names and
streams), and every elementary stream is classified from the PMT and
renumbered by role — video → 0x0111, main audio → 0x0112 (further audio
0x0113 …), captions → 0x0130, superimpose → 0x0138, data → 0x0140 (rules
and sources: `isdbt-mux/SPEC-NOTES.md` "Program selection, stream roles and
the PID plan"). At start the probe report lists the source program, each
source PID → role (with the rule that decided it) → output PID with its
rate, the relayed ECM / EMM PIDs, the dropped PIDs with the reason (SI,
PMT, source PCR, …), the source's copy control (every
digital_copy_control_descriptor and content_availability_descriptor found
in its PMT program and ES loops, SDT and EIT p/f) and the measured mean /
peak rates and layer usage. One info line says what the relay sends,
e.g. `source copy control: copy-one-generation (PMT); relayed as:
copy-free (explicit)`.

| option (`--relay` only) | default | meaning |
|---|---|---|
| `--program N` / `--service-name S` | the only program | source program_number (hex or decimal) / SDT name (exact, else unique substring; full-width ASCII folded: `TV12` finds `ＴＶ１２テスト`) |
| `--channel N` | 44 | UHF channel in the NIT (`tx`: its `--channel`, or the channel of `--freq`) |
| `--remote-key K` | 9 | remote_control_key_id 1..12 |
| `--area-code C` | 0x5A5 | area_code (12 bits, B10 Annex D Table D-2); set the receiver's area |
| `--name S` | source SDT name, else ベンチ | network / TS / service / provider / broadcaster name |
| `--copy copy-free\|copy-one-generation\|copy-never` | the source program's digital_recording_control_data (PMT program loop, else relayed ES loops, else SDT, else EIT p/f; copy-free only if the source signals none) | digital copy control (copy_control_type '01'). Without `--copy` the relay keeps the recording's value and its per-component (ES loop) descriptors; an explicit value is used as given (ES-loop descriptors dropped). The source's value is printed either way |
| `--ca keep\|drop` | keep | the recording's CA descriptors, ECM and CAT/EMM: relayed (payload unscrambled) or dropped |
| `--at-eof loop\|end` | loop | at the end of the recording |
| `--eit-from-source [BOOL]` | true | EIT p/f event name / text / genre from the recording (the TOML key `eit_from_source` itself defaults to false; `--relay` sets it) |
| `--clock-start JST` | system clock | fixed stream clock (reproducible output) |
| `--print-config` | — | print the resolved TOML (header: the transmission options, after `--auto-layout`) and exit; save, edit, run with `--mux-config` / `-c` |

`--auto-layout` (with `--relay` or `--mux-config`): when the rate the
relay needs over the whole recording (every packet in time for its
decoding; the file is read once at start) + PCR/PSI/SI exceeds its layer,
use the first suggested
layout that only changes that layer (the most robust modulation / code
rate that fits, configured GI first; partial reception and the one-seg
layer A kept) and print the choice, instead of refusing with the list of
layouts. For example, a recording of about 18.5 Mbit/s is refused on the
default B (12 seg 64QAM 3/4 = 16.85 Mbit/s); `--auto-layout` picks B 12
seg 64QAM 7/8 (19.66 Mbit/s), and a single 13-segment 64QAM 7/8 layer
(`--partial false --layer-a 13,64qam,7/8,2 --layer-b none`, 21.30 Mbit/s)
leaves more spare.

```text
# check (no radiation): the probe report, the layer usage, the TS itself
isdb mux --relay FILE --auto-layout --seconds 60 --out-ts check.ts
isdb ts validate check.ts
ffmpeg -v error -i check.ts -f null -     # only the cut ends may complain
isdb loopback --relay FILE --auto-layout --frames 8 --snr-db 25
# keep and tweak the configuration
isdb mux --relay FILE --auto-layout --print-config > relay.toml
isdb tx --mux-config relay.toml --layer-b 12,64qam,7/8,2 \
        --radio hackrf --channel 44 --gain txvga=20 --rf-enable
```

(Runs until Ctrl-C; the recording loops.) The status line shows
per relay the lag behind the PCR schedule, late PES, the source's peak
rates and the loop period. PES that leave after their decoding time —
receivers drop them, audio first — print a `mux warning` (at most once a
minute per relay).

An RF loopback needs two radios: `isdb tx --radio hackrf:serial=A …` and
`isdb rx --radio hackrf:serial=B …`, connected through attenuators
([docs/transmitting.md](https://github.com/a1678991/isdb-rs/blob/main/docs/transmitting.md#bench-set-up)).
A Pluto can also loop back digitally without radiating:
`isdb tx --radio pluto:uri=…,loopback=on … --rf-enable` keeps the TX LO
powered down and feeds the samples into its own RX path, where
`isdb rx --radio pluto:uri=…` receives them (over Ethernet the RX side
drops samples while both directions stream; see the `isdb-radio`
README).

## Radio backends

`--radio NAME[:key=value,...]` names a backend of the
[`isdb-radio`](https://github.com/a1678991/isdb-rs/blob/main/crates/isdb-radio/README.md)
registry and its device arguments; `--gain NAME=VALUE` are its gains.
`isdb` computes only what is device-independent — the centre frequency
(`--channel`/`--freq`, the 470–710 MHz check, `--freq-corr-ppm`), the
512/63 MHz sample rate and the `--bb-filter-hz` request — into an
`isdb_radio::RadioConfig`, puts `--backoff-db` and `--prefill-frames` into
`isdb_radio::TxOptions` (`rx`: a 1 s receive queue in
`isdb_radio::RxOptions`), and passes the arguments and gains through as
strings for the backend to validate. The `--rf-enable` /
`--dry-run` gate applies to every backend. The compiled-in backends, their
device arguments and gains are listed in the `isdb-radio` README: `hackrf`
and `pluto` (cargo features of the same names, default on).

Adding a device touches only `isdb-radio` (the `pluto` backend is an
example of one added this way):

1. Implement `isdb_radio::Backend` in
   `crates/isdb-radio/src/backends/<name>.rs`. `describe_tx` /
   `describe_rx` validate the device arguments, gains, tuning range and
   sample rate without hardware and return the settings text (what
   `--dry-run` prints); `open_tx` / `open_rx` open the device and return a
   `Box<dyn IqSink>` / `Box<dyn IqSource>`. A device fed with buffers of
   integer samples implements `isdb_radio::RadioTx` (its `IntFormat`, e.g.
   `Cs8` or `Cs12`) and returns an `isdb_radio::QueuedSink`, which quantises with
   `TxOptions::backoff_db` and provides the prefill, back-pressure, stall
   detection, drain and underrun counting; `open_rx` converts
   `RxOptions::queue_seconds` to its own buffer count.
2. Put it behind its own cargo feature of `isdb-radio` (the driver crate
   as an optional dependency) and register it in `backends/mod.rs`: the
   `#[cfg(feature = "<name>")]` module, an entry in `COMPILED`, and
   `("<name>", "<feature>")` in `KNOWN`, so a build without it names the
   feature. Add its row to the README's backend table.
3. To build it into `isdb`, forward the feature in
   `crates/isdb-cli/Cargo.toml` (`<name> = ["isdb-radio/<name>"]`), as
   `hackrf` is.

No CLI source changes: `--radio <name>:key=value`, `--gain`, the dry run,
status lines and summaries reach the new backend through the traits.

## Architecture

```text
tx:  reader thread(s) ─packets (bounded, ~2 s)─► producer thread
       (FrameInput: per-layer / PID split (PCR-paced for files) /
        in-process isdbt-mux (its ts-program relays have their own
        reader thread) / synthetic; feed policy)
     producer: Transmitter (pre-roll, lead, data, finish + flush);
       playback keys (keys thread) applied to the input between frames
       --iq-in: playback thread (file frames, null frames when paused)
       ─frames (bounded, --queue-frames 4)─► main thread: dyn IqSink
     IqSink: FileSink (cf32/cs8) | isdb-radio backend (Backend::open_tx),
       e.g. hackrf: cs8 quantiser → libhackrf TX callback;
       pluto: cs12 quantiser → isdb-iio TX thread (iio_buffer_push)
       (starts after --prefill-frames 2; back-pressure paces the producer)
rx:  dyn IqSource (file | Backend::open_rx, e.g. HackRF RX callback,
       Pluto isdb-iio RX thread)
       ─► PipelinedReceiver
       (demod thread ─► FEC-decode thread) ─► outputs + status
loopback: producer ─► Channel ─► PipelinedReceiver ─► Verifier
```

Throughput (Mode 3, GI 1/8, default layers, file I/O, measured on a
current 16-core desktop CPU): `tx --synthetic --iq` of 20 s of signal took 4.9 s (≈ 4×
real time); `rx --iq` of that file took 14.0 s (≈ 1.4× real time; the
demodulator is the bottleneck).

**Build profile matters for the radio.** On the same machine,
`tx --mux-ts <a 19.1 Mbit/s recording> --iq /dev/null` runs at 4.1× real
time in a release build (input 0.3 ms + modulation 54 ms per 231 ms frame)
but at **0.18× in an unoptimised build**. Under a simulated real-time
HackRF (cs8 on stdout consumed at 512/63 MS/s in 256 KiB buffers) the
unoptimised build underran 73 % of the USB buffers ("2.8 s on air, 12
frames (10 data)"), the same with `--mux-config` (so not an input
problem), and the release build had 0 underruns. The workspace `Cargo.toml` therefore builds the
signal-processing crates (`isdbt-core`, `isdbt-fec`, `isdbt-ofdm`,
`isdbt-phy`) and all dependencies with
`opt-level = 3` in the dev profile too: `cargo run -p isdb-cli -- tx …`
now runs at 3.2× real time (0 underruns in the same simulation). Prefer
`cargo build --release -p isdb-cli` for transmissions anyway. The
benchmark `mux_ts_producer_throughput` (ignored, `--ignored` to run)
asserts ≥ 2× real time for a 19.5 Mbit/s `--mux-ts` file.

## Verification

`cargo test -p isdb-cli` (no radio needed; the real-time streaming path is
tested through the `isdb-radio` `QueuedSink` that the radio backends
transmit with, over the in-memory `mock::MockRadio`):

| Test | What it proves |
|---|---|
| `tests/cli.rs` `tx_then_rx_reproduces_every_layer_byte_exact` | `isdb tx --iq` → `isdb rx --iq` returns every input packet of every layer byte-exact |
| `tests/cli.rs` `mux_ts_is_split_by_pid` | `--mux-ts` routing by `--a-pids`, order preserved |
| `tests/cli.rs` `mux_ts_file_is_paced_by_its_pcr` | an 8 Mbit/s PCR-timed file into the 16.85 Mbit/s layer B: ≈ 50 % null fill, `tx` → `rx` returns every data packet in order |
| `tests/cli.rs` `mux_ts_over_capacity_is_refused_with_layouts_that_fit` | a 19.1 Mbit/s file is refused with the computed capacities and `--layer-b 12,64qam,7/8,2` / the 13-segment layout; the suggested layout and `--allow-overrate` run |
| `tests/cli.rs` `mux_ts_producer_throughput` (ignored) | producer ≥ 2× real time for a 19.5 Mbit/s `--mux-ts` file (regression benchmark against HackRF underruns) |
| `tests/cli.rs` `relay_recording` | opt-in real data (`ISDB_RS_RELAY_TS` names a recording): the first 60 s through `examples/relay-ts.toml` (placeholder `source` replaced) with `--auto-layout` → video on 0x0111, 0 late PES, T-STD ok, `ts validate` without errors, ffmpeg decodes it (at most the AAC resync at the cut-in), `loopback` PASS; without `--auto-layout` the default layer B either carries it or is refused with the layouts that fit. Prints `SKIP` and passes when the variable is unset or the file is missing |
| `tests/cli.rs` `relay_without_toml` | `--relay` on a synthetic recording of another network (≈ 8.6 Mbit/s, CA + ECM, copy control, captions in the PMT): refused on B 12 seg 16QAM 1/2 with the layouts that fit; `--auto-layout` picks B 12 seg 16QAM 2/3 and prints it; program / roles / PIDs (0x0140→0x0111 video, 0x0141→0x0112 main audio, 0x0145→0x0130 captions), the relayed ECM, the CA notice and the source copy-control line in the report; `ts validate` clean; `--print-config` TOML (header with the chosen `--layer-b`) run with `mux -c` gives a byte-identical TS; every option (including `--ca drop`) lands in the printed TOML; `tx` takes the NIT channel from `--channel` / `--freq`; an unknown `--service-name` lists the programs; `loopback --relay --auto-layout` PASS |
| `tests/cli.rs` `relay_recording_without_toml` | opt-in real data (`ISDB_RS_RELAY_TS`): `mux --relay <recording> --auto-layout --seconds 20` names the program, relays the video to 0x0111 with 0 late PES, `ts validate` 0 errors; `loopback --relay … --auto-layout` PASS. Prints `SKIP` and passes without the variable |
| `tests/cli.rs` `live_feed_with_stalling_input_stays_cbr_and_lossless` | `--feed live` pads with nulls at constant rate and loses no input |
| `tests/cli.rs` `loopback_passes_with_impairments`, `loopback_with_file_inputs_and_detects_loss` | loopback verdicts: PASS under impairments, FAIL with lost packets counted |
| `tests/cli.rs` `radio_tx_refuses_without_rf_enable` | `--radio hackrf --dry-run` prints the backend's settings and refuses; no `--rf-enable` ⇒ rejected |
| `tests/cli.rs` `radio_rejects_unknown_backends_gains_and_ranges` | an unknown backend lists the compiled-in ones with their features; unknown or out-of-range `--gain` values (TX and RX) and unknown device arguments are rejected before a device is opened |
| `tests/cli.rs` `pluto_dry_run_validates_without_a_device` | `--radio pluto:…` dry runs print the settings (attenuation, default maximum; loopback; cs12 level) and reject out-of-range attenuation, HackRF gain names, a third channel, a 50 MHz bandwidth, `loopback` on RX and manual gain with AGC, without connecting |
| `tests/cli.rs` `capture_dry_run_validates_without_a_device` | `capture --dry-run` with `--freq bs1` / `nd24` prints the tuning (BS-1 IF 1049.48 MHz, ND24 IF 2053 MHz), the method (libiio buffer / maia-sdr at the `ip:` host) and the limit (16 515 072 / 33 554 432 samples), writes nothing; refuses more samples than the limit, `buffer` (a streaming argument), `bs2`, a 60 MHz bandwidth and a backend that cannot capture |
| `tests/cli.rs` `iq_survey_finds_rate_carrier_and_system` | `iq survey` on a synthetic QPSK file at 28.86 Mbaud, 61.44 Msps, −115 kHz: a raw file needs `--format`/`--rate`; the JSON names ISDB-S and gives the rate and the offset within one FFT bin; with SigMF metadata the format, rate, centre and channel come from it |
| `tests/cli.rs` `capture_from_a_pluto` | opt-in hardware, receive only (`ISDB_RS_PLUTO_URI`, optionally `ISDB_RS_PLUTO_CAPTURE_FREQ`, default `bs1`): two 256 Ki-sample segments at 61.44 Msps; data size, SigMF datatype, full scale 2048, rate read back within 10 ppm, the second segment's start and gap marker, the capture method; an existing output is not overwritten. Prints `SKIP` without the variable |
| `tests/cli.rs` `radio_without_backends_names_the_feature` | `--no-default-features` build: `tx`/`rx --radio hackrf` and `--radio pluto` name their features |
| `tests/cli.rs` `scrambled_input_is_refused` | a TS with one scrambled packet (transport_scrambling_control ≠ '00') is refused: `isdb` neither descrambles nor transmits scrambled content |
| `tests/cli.rs` `completions_cover_every_subcommand` | `completions bash` defines `_isdb` and dispatches every subcommand; `completions zsh` starts with `#compdef isdb` and lists every subcommand; both complete `--radio`; an unknown shell is a usage error |
| `tests/cli.rs` `endless_input_into_iq_file_is_refused_without_seconds` | `tx --synthetic --iq FILE` without `--seconds` is refused before the file is created; with `--seconds` it runs |
| `tests/cli.rs` `mux_config_and_ts_tools_smoke`, `loopback_with_in_process_multiplexer` | `isdb mux` / `ts inspect` and `loopback --mux-config` on `bench.toml` with media streams replaced by `declared` streams; the media variant runs only when `ISDB_RS_MEDIA_DIR` names a directory with `video.m2v` and `audio.m2a` (prints `SKIP` otherwise; [docs/test-data.md](https://github.com/a1678991/isdb-rs/blob/main/docs/test-data.md#making-a-media-directory) shows how to make one) |
| `tests/cli.rs` `media_aac_gate_matches_b32_part2_report` | `isdb mux` on `bench.toml` with your own `audio.m2a`, expectations taken from its ADTS headers: the default `conform` succeeds exactly when no error-severity B32 Part 2 rule is violated (refusal says "crc_check cannot be inserted" for protection_absent '1'); with `--adts-compliance warn` `isdb ts validate` reports each violated per-frame rule as an `aac.*` error and the quality_indicator Table 1 derives from the file's rate (opt-in: `ISDB_RS_MEDIA_DIR` with `video.m2v` and `audio.m2a`; prints `SKIP` otherwise) |
| `src/*` unit tests | `--relay` numbers (hex/decimal), NIT channel from `--freq`, service layer from the layout; PCR pacing of the PID split (frames carry only the packets due; over capacity: everything sent, lag measured), layer-spec parsing and defaults, UHF channel centres and ppm correction, BS / ND satellite IF tables, channel names and `--freq` / `--rate` / `--samples` syntax, `capture` arguments → `RadioConfig` and its SigMF metadata (segments, gap marker, read-back values), the band check, the `RadioConfig` built from the tuning / `--radio` arguments / `--gain` list, cs8 file bytes, AWGN/CFO channel, verifier anchoring, producer lead/data/flush sequence and stop, file/loop/live sources, PID split, streaming into memory and through the queued radio sink over the mock radio (prefill and drain; cs8 bytes equal to the quantised samples; stop aborts), `clap` definition consistency, `--seconds` default 0 for every `tx` input and for `rx`, the `--iq` bound check (regular file refused; stdout, FIFO, `/dev/null` accepted), which `--mux-config` / `--relay` configurations end by themselves |

Workspace overview: [`../../README.md`](https://github.com/a1678991/isdb-rs/blob/main/README.md); architecture and
the frame/latency contract: [`docs/architecture.md`](https://github.com/a1678991/isdb-rs/blob/main/docs/architecture.md);
optional real-data tests: [`docs/test-data.md`](https://github.com/a1678991/isdb-rs/blob/main/docs/test-data.md).
