# Transmitting

`isdb tx` turns transport streams into an ISDB-T signal and sends it to an
IQ file or a radio (`--radio`; HackRF One is the backend available
today). This page covers the conducted bench set-up, the
levels and channel plan, the `isdb tx` inputs and options, relaying a
recorded program, and what real receivers turned out to need.

## Safety and legal requirements

> [!WARNING]
> Radiating on UHF television frequencies requires a radio station licence
> (in Japan under the Radio Act, 電波法; elsewhere under local law).
> Unlicensed transmission is illegal and interferes with television
> reception around you. Use `isdb tx --radio` only in a **conducted**
> set-up (coax and attenuators into the receiver, nothing radiating) or in
> a **shielded enclosure**. Never connect an antenna to the radio while
> transmitting. You are responsible for the rights to any content you
> transmit or relay; `isdb` does not descramble and refuses scrambled
> input.

`isdb tx --radio` radiates only with `--rf-enable`, whatever the backend;
without `--rf-enable` or `--dry-run` the command line is rejected. With
`--dry-run` it prints the backend's settings (after validating the device
arguments, gains and tuning range) and exits with "not transmitting"
before the device is opened, so you can check every setting first. With HackRF every exit path stops streaming and switches
the RF amplifier off; a normal end prints `device stopped streaming:
yes`.

## Bench set-up

```text
HackRF One ──SMA coax── fixed attenuators ── DC block ── F/IEC adapter ── receiver antenna input
 (TX port)               (e.g. 30–60 dB)                                   (TV, recorder, USB tuner)
```

- **Attenuation.** A TV tuner is specified for roughly −75 dBm to −20 dBm
  at its input (ARIB STD-B21 §5.2.5: minimum input level −75 dBm or lower,
  maximum −20 dBm or higher). The HackRF can deliver far more than the
  maximum, so start with the RF amplifier off (`--gain amp=off`, the
  default), a low `--gain txvga=N` and plenty
  of attenuation, then raise the TX gain until the receiver's
  signal-quality indicator is comfortably good. Aim for the middle of the
  range rather than the top: overload produces errors that look like a
  weak signal.
- **DC block.** Many TV receivers and recorders can supply DC power on the
  antenna input (for mast amplifiers). Switch that off in the receiver's
  settings and put a DC block in the line to protect the HackRF. `isdb`
  itself keeps the HackRF's own antenna-port power off.
- **Shielding.** Use double-shielded coax and terminated, tight
  connectors; disconnect the receiver's normal antenna. Leakage works both
  ways: strong local broadcasts can leak into the bench and the bench can
  leak out.
- **Receiving with a second HackRF** (for `isdb rx`, [receiving.md](receiving.md)):
  a splitter after the attenuators feeds both receivers. Keep the level at
  the HackRF's RX port low — HackRF documents −5 dBm as the maximum RX
  input before damage, and its receiver works best far below that.
- **Clock accuracy.** The HackRF's reference oscillator may be several ppm
  off. ISDB-T receivers track ±20 ppm of clock error (B21 §5.2.4), but a
  large offset slows acquisition. `--freq-corr-ppm P` divides both the
  tuning frequency and the sample clock by (1 + P·10⁻⁶) so the signal is at
  the nominal values; measure P with `isdb rx` (the CFO figure) or a
  reference receiver.

## Channel plan

Japanese UHF television channels are 6 MHz wide. The ISDB-T signal is
centred 1/7 MHz above the channel centre:

  f_c(N) = 473 + 6 · (N − 13) + 1/7 MHz, for channel N = 13 … 62

| Channel | Band (MHz) | f_c (MHz) | NIT frequency code (1/7 MHz units) |
|---|---|---|---|
| 13 | 470–476 | 473.142857 | 3312 |
| 27 | 554–560 | 557.142857 | 3900 |
| 44 | 656–662 | 659.142857 | 4614 |
| 52 | 704–710 | 707.142857 | 4950 |

The NIT carries the frequency in 1/7 MHz units (ARIB STD-B10 §6.2.31),
i.e. `7·(473 + 6·(N − 13)) + 1`. Today Japanese terrestrial television
uses channels 13–52 (470–710 MHz); `--channel` maps 13–62 and
`--freq HZ` sets a centre frequency instead (the formula is not in the
ARIB specs used here; it is the Japanese channel plan). A centre outside
470–710 MHz (channels 53–62, or any other `--freq`) is refused unless
`--freq-unrestricted` is given, which leaves only the radio backend's own
range (HackRF: 1 MHz–6 GHz) for shielded lab setups.

**Choosing a channel.** Use a channel that no station receivable at your
location uses, so that leakage cannot interfere in either direction, and
set the multiplexer's `[network] channels` to the same channel: receivers
retune to the NIT frequency after a scan, so a mismatch registers the
service on a channel with no signal.

## The signal

Each OFDM frame is 204 symbols. In Mode 3 with GI 1/8 a frame lasts
231.336 ms and has 1 880 064 samples at 512/63 MHz ≈ 8.127 MS/s, the rate
used for files and for radios. The defaults are the Japanese
broadcast parameters:

| Option | Default | Meaning |
|---|---|---|
| `--mode` | 3 | 1, 2 or 3 (2k, 4k, 8k FFT) |
| `--gi` | 8 | guard interval denominator: 4, 8, 16, 32 |
| `--partial` | true | partial reception (layer A = the centre segment, one-seg) |
| `--layer-a` | `1,qpsk,2/3,2` | 64 TSP/frame, 0.42 Mbit/s |
| `--layer-b` | `12,64qam,3/4,2` | 2592 TSP/frame, 16.85 Mbit/s |
| `--layer-c` | `none` | |

A layer is `SEGMENTS,MODULATION,CODE_RATE,TI` (modulation
`dqpsk|qpsk|16qam|64qam`, code rate `1/2…7/8`, TI = TMCC time-interleave
index 0–3). The start-up line prints the TSPs and the TS rate of every
layer; the rates follow from ARIB STD-B31 Table 3-3.

The level in an integer-sample radio such as the HackRF (8-bit DAC) and
in cs8 files is set by `--backoff-db` (default
10 dB): the OFDM RMS sits that far below full scale (RMS 40 of 127), which
leaves headroom for OFDM's peaks. Clipped components are counted and
reported; a few in 10⁵ are normal at 10 dB.

## `isdb tx` inputs

Exactly one input:

| Input | Use |
|---|---|
| `--mux-config FILE.toml` | the multiplexer runs in-process ([mux-config.md](mux-config.md)); never ends by itself (unless every relay has `at_eof = "end"`) |
| `--relay FILE` | rebroadcast a recorded program as a terrestrial service, no TOML needed ([below](#relaying-a-recording)) |
| `--a-ts/--b-ts/--c-ts PATH` | per-layer transport streams (`-` = stdin, FIFOs fine), e.g. from `isdb mux --out-a/--out-b`; a layer without input carries null packets |
| `--mux-ts PATH --a-pids … [--c-pids …]` | one TS split into layers by PID: the listed PIDs go to A / C, every other PID to B, input null packets are dropped |
| `--synthetic` | deterministic test packets (PID 0x0100/0x0200/0x0300); never ends |

`--feed` sets how file and pipe inputs are consumed: `file` (default; read
ahead, end at EOF), `loop` (rewind at EOF; continuity counters jump), or
`live` (never block the modulator: what has arrived is sent, the rest of
the frame is null packets, the null-fill ratio is reported; ends when the
writer closes).

With `--mux-ts` and `--feed file|loop`, the input is **paced by its own
PCR** (H.222.0 §2.4.2.2): each OFDM frame carries the packets due before
its end, padded with nulls, so a recording goes on air at its recorded
rate. A regular file is probed first (up to 30 s of PCR) and refused, with
the layer layouts that would fit, if its mean rate exceeds a layer's
capacity; `--allow-overrate` sends it anyway (it then falls behind its own
schedule). A station multiplex split by PID keeps that station's SI — for
a program of *another* network (e.g. a satellite recording), use
`--relay`, which gives it terrestrial SI.

## `isdb tx` options

| Option | Default | Meaning |
|---|---|---|
| `--iq PATH` | — | write baseband to a file (`-` = stdout) instead of a radio |
| `--iq-format cf32\|cs8` | cf32 | float32 I/Q (GNU Radio) or signed 8-bit I/Q (`hackrf_transfer`, scaled with `--backoff-db`) |
| `--radio NAME[:key=value,...]` | — | transmit with a radio backend and its device arguments (`hackrf`, `hackrf:serial=0000abcd`); needs `--channel` or `--freq`, and `--rf-enable` or `--dry-run`. An unknown name lists the compiled-in backends and their cargo features |
| `--channel N` / `--freq HZ` | — | UHF channel 13–52, or a centre frequency in Hz, within 470–710 MHz |
| `--freq-unrestricted` | off | allow a centre outside the 470–710 MHz TV band (shielded lab setups only); the backend's own range still applies (HackRF 1 MHz–6 GHz) |
| `--rf-enable` | off | **actually radiate** (licensed or conducted/shielded use only) |
| `--dry-run` | off | print the settings and exit without opening the device |
| `--gain NAME=VALUE` | backend's | radio gain, repeatable; names, ranges and defaults are the backend's (an unknown name lists them). HackRF: `txvga=0..47` dB TX IF gain (0), `amp=on\|off` RF amplifier ≈ +11 dB (off) |
| `--backoff-db DB` | 10 | OFDM RMS below integer full scale (radios with integer samples, e.g. HackRF cs8, and cs8 files) |
| `--freq-corr-ppm P` | 0 | reference-clock correction (positive = the device clock runs fast) |
| `--bb-filter-hz HZ` | 6 000 000 | analog (baseband filter) bandwidth request; the backend picks a supported bandwidth (HackRF: the largest MAX2837 setting ≤ the request). 6 MHz passes the 5.57 MHz ISDB-T band |
| `--seconds S` | 0 | bound on air time including lead-in and flush; `0` = unbounded (until the input ends, or Ctrl-C / SIGTERM) |
| `--lead-frames N` | 2 | null frames before the data |
| `--no-flush` | off | omit the null frames that flush the interleavers at the end |
| `--realtime` | off | pace production to real time even into a file (on by default for `--feed live` with `--iq`) |
| `--queue-frames N` | 4 | frames produced ahead of the sink |
| `--prefill-frames N` | 2 | frames converted before the radio starts streaming |
| `--status-secs S` | 5 | status line interval (0 = final summary only) |
| `--allow-overrate` | off | send a `--mux-ts` file whose PCR rate exceeds a layer's capacity |

`--radio` names a backend of `isdb-radio`; the compiled-in backends with
their device arguments and gains are listed in the
[isdb-radio README](../crates/isdb-radio/README.md#backends), and adding
one (e.g. a PlutoSDR) needs no change to the CLI
([isdb-cli README](../crates/isdb-cli/README.md#radio-backends)).

An input that never ends (`--synthetic`, `--mux-config`, `--relay`,
`--feed loop`) written to a regular `--iq` file is refused unless
`--seconds` is given (the file would fill the disk); stdout, FIFOs and
devices such as `/dev/null` need no bound. Ctrl-C or SIGTERM stops
immediately; a normal end drains the queue first.

**On-air sequence.** The transmitter first modulates and discards a few
frames (pre-roll) so the first emitted frame is ordinary OFDM, sends
`--lead-frames` null frames (a receiver that locks on the first or second
frame still outputs every input packet), the data, then null frames that
push the last packets through the interleavers and the receiver's
look-ahead. Details: [architecture.md](architecture.md#frame-and-latency-contract).

## Checking before transmitting

Run the whole chain without a radio first:

```sh
isdb mux -c bench.toml --seconds 20 --out-ts check.ts --report   # optional: check the multiplex
isdb ts validate check.ts
isdb loopback --mux-config bench.toml --frames 8 --snr-db 25     # exactly what will be modulated
isdb tx --mux-config bench.toml --iq /dev/null --seconds 10      # producer speed, no radio
```

The `tx` status line reports the **producer speed** — on-air time of the
frames produced divided by the time spent making them:

```text
[tx]     3.5 s on air, 15 frames (12 data) | null fill A  94.4% B  99.7% (total A  94.4% B  99.7%) | mux late PES 0 errors 0 | producer 4.2× real time (input 0.2 + modulate 54.4 ms/frame)
```

Below 1× the radio underruns (USB buffers sent as zeros, reported in the
status lines) and `isdb tx` prints a warning with the per-stage times. An
unoptimised build runs at ≈ 0.2×; use `cargo build --release`.

Then check the RF settings, still without radiating:

```sh
isdb tx --mux-config bench.toml --radio hackrf --channel 27 --gain txvga=20 --dry-run
```

```text
hackrf TX settings (UHF ch27):
  device       HackRF (first found)
  tuning       centre 557.142857 MHz, programmed 557142857 Hz; sample rate 512000000/63 Hz = 8.126984 MHz
  bb filter    6.00 MHz requested
  TX VGA       20 dB
  RF amp       off
  backoff      10.0 dB below cs8 full scale (RMS 40.2 of 127)
Error: dry run, not transmitting: use --rf-enable instead of --dry-run to radiate with the settings above (coax/attenuator or shielded setup only)
```

## Transmitting (conducted bench only)

```sh
# the multiplexer in-process, until Ctrl-C
isdb tx --mux-config bench.toml --radio hackrf --channel 27 --gain txvga=20 --rf-enable

# per-layer files, looped, at most 10 minutes
isdb tx --radio hackrf --channel 27 --gain txvga=20 --rf-enable --seconds 600 \
    --b-ts service.ts --a-ts oneseg.ts --feed loop

# a live multiplex from a FIFO, split by PID, padded with nulls when it lags
isdb tx --radio hackrf --channel 27 --gain txvga=20 --rf-enable \
    --mux-ts /tmp/live.fifo --feed live --a-pids 0x0000,0x0010,0x0011,0x0012,0x0014,0x1fc8,0x1fc9
```

The status lines show air time, frames, null fill per layer, input
counters (for `--mux-config`: late PES and errors; for `--mux-ts`: the
PCR-measured input rate per layer and the lag behind the PCR schedule),
the producer speed and the radio's own counters (HackRF: USB buffers and
underruns).

Typical end-to-end check with a second HackRF:

```sh
isdb rx --radio hackrf:serial=<RX-SERIAL> --channel 27 --seconds 30 --out ours.ts --json
isdb ts validate ours.ts
isdb ts diff ours.ts check.ts
```

## Relaying a recording

`--relay FILE` (on `tx`, `loopback` and `mux`) rebroadcasts one program of
a recorded transport stream — for example a recording of another network
— as a terrestrial service. The recording must be unscrambled
(transport_scrambling_control '00'; scrambled input is refused), and you
must have the rights to relay it. A foreign TS relayed as is keeps that
network's NIT/SDT, which a terrestrial receiver will not register. The
relay instead:

- takes only the program's elementary streams; their PES bytes go out
  unchanged, at the recording's own PCR pace, with PTS/DTS shifted onto
  this multiplex's clock;
- generates terrestrial PSI/SI around them: service 0x0400, PMT PID
  0x01F0, PCR PID 0x01FF, network / TS id 0x79EE (a placeholder: use an ID
  not used where your receiver is), a NIT naming the transmit channel,
  SDT, EIT (event names from the recording), TOT, BIT;
- **carries the recording's conditional-access signalling** by default
  (`--ca keep`; TOML `ca = "keep"`): the PMT's conditional_access_descriptors
  (program and ES loops), the ECM PIDs they name, and the CAT's
  CA_descriptors with the EMM PIDs present in the recording, every CA_PID
  following the PID mapping. The payload stays unscrambled, so receivers
  see CA signalling with a clear payload. `--ca drop` (`ca = "drop"`)
  relays none of it;
- selects the program automatically when the recording has one
  (`--program N` or `--service-name S` otherwise; a missing or ambiguous
  choice lists the programs with their names and streams);
- classifies every stream from the PMT and renumbers it by role: video →
  0x0111, main audio → 0x0112 (more audio 0x0113, …), captions → 0x0130,
  superimpose → 0x0138, data → 0x0140;
- **keeps the recording's copy restriction** by default: the generated
  digital copy control descriptor takes the source program's recording
  control value — from its PMT program loop, else its relayed ES loops,
  else its SDT, else its EIT p/f (copy-free only if the source signals
  none) — and the relayed streams keep their per-component (ES loop)
  descriptors. `--copy` (TOML `recording_control`) sets any value
  explicitly; it is used as given. The source's value is printed as an
  info line either way, e.g. `source copy control: copy-one-generation
  (PMT); relayed as: copy-free (explicit)`, and raw descriptors or byte
  patches that set the copy control of a relayed service are reported
  with what is actually sent. Relaying content you have no rights to is
  your responsibility.

Check it without a radio, then transmit:

```sh
isdb mux --relay FILE --auto-layout --seconds 60 --out-ts check.ts   # probe report + TS
isdb ts validate check.ts
isdb loopback --relay FILE --auto-layout --frames 8 --snr-db 25
isdb tx --relay FILE --auto-layout --radio hackrf --channel 27 --gain txvga=20 --dry-run
isdb tx --relay FILE --auto-layout --radio hackrf --channel 27 --gain txvga=20 --rf-enable
```

At start the probe report lists the source program, each source PID → role
(and the rule that decided it) → output PID with its rate, the relayed
ECM / EMM PIDs, the dropped PIDs with the reason, the source's copy
control (every 0xC1 / 0xDE found in its PMT, SDT and EIT p/f), and the
rate the whole recording needs against the layer capacity. The status line shows, per relay, the lag behind the PCR
schedule, late PES, peak rates and the loop period; PES that leave after
their decoding time (receivers drop them, audio first) also print a
`mux warning`, at most once a minute per relay. The recording loops
until Ctrl-C (`--at-eof end` stops after one pass; `--seconds S` bounds
the air time).

| Option (`--relay` only) | Default | Meaning |
|---|---|---|
| `--program N` / `--service-name S` | the only program | source program_number, or SDT service name (exact, else a unique substring; full-width ASCII folded) |
| `--channel N` | `tx`: the transmit channel; `mux`/`loopback`: 44 | UHF channel named in the NIT |
| `--remote-key K` | 9 | remote_control_key_id 1–12 |
| `--area-code C` | 0x5A5 | area_code (12 bits, B10 Annex D Table D-2); set the receiver's area |
| `--name S` | the recording's SDT service name | network / TS / service / provider name |
| `--copy copy-free\|copy-one-generation\|copy-never` | the source program's value | recording control of the digital copy control descriptor; an explicit value is used as given |
| `--ca keep\|drop` | keep | the recording's CA descriptors, ECM and CAT/EMM: relayed (payload unscrambled) or dropped |
| `--at-eof loop\|end` | loop | at the end of the recording |
| `--eit-from-source [BOOL]` | true | EIT present/following names, texts, genres from the recording |
| `--clock-start JST` | system clock | fixed stream clock, for reproducible output |
| `--print-config` | — | print the generated TOML and exit (save it, edit it, run it with `--mux-config` / `-c`) |

**Capacity.** HD recordings often exceed the default layer B
(12 segments 64QAM 3/4 = 16.85 Mbit/s). The rate checked is what the whole
recording needs, not the mean of its first seconds: the file is read once
at start, and the need is the smallest constant rate that delivers every
relayed packet by the decoding time of its PES (`whole file: …` in the
probe report). The relay keeps the recording's pace, so on a layer below
that rate it falls further behind for as long as the recording stays
above it, and its PES — audio first, whose lead before decoding is the
shortest — arrive too late to be played. Such a relay is refused with a list
of layouts that fit; `--auto-layout` takes the first one that changes only
the relay's layer (the most robust code rate that carries that rate
plus PCR and SI, the configured GI first; partial reception and the one-seg
layer kept) and prints the choice. For example, a recording of about
18.5 Mbit/s is refused on 3/4 and carried on 12 segments 64QAM 7/8
(19.66 Mbit/s). A single 13-segment 64QAM 7/8 layer (`--partial false
--layer-a 13,64qam,7/8,2 --layer-b none`, 21.30 Mbit/s) leaves more room
but has no one-seg layer.

To keep and tweak the generated configuration:

```sh
isdb mux --relay FILE --auto-layout --print-config > relay.toml
isdb tx --mux-config relay.toml --layer-b 12,64qam,7/8,2 --radio hackrf --channel 27 --gain txvga=20 --rf-enable
```

`crates/isdbt-mux/examples/relay-ts.toml` is the same configuration with
comments; the `ts-program` keys are described in [mux-config.md](mux-config.md).

## Receiver compatibility notes

These were found by feeding `isdb tx` output over a conducted link to two
consumer receivers — a **MyGica S270** USB tuner and a **Sony BDZ-series**
Blu-ray recorder — in one configuration (Mode 3, GI 1/8, partial reception,
layer A 1 segment QPSK 2/3, layer B 12 segments 64QAM 3/4, time-interleave
index 2). They are requirements of those receivers, not of the
specifications, and the defaults follow them.

- **Copy control type '01'.** The recorder refuses to play a service whose
  digital copy control descriptor (0xC1) carries copy_control_type '00' in
  its user_defined bits ("Unsupported signal: This program is not
  available") and plays it with '01' (user_defined 0b0100, what live
  broadcasters send). In a one-change-at-a-time bisection, the recording
  control value (copy-free or copy-one-generation), the placement (PMT +
  SDT or PMT + EIT), the video decode control byte and the EIT component
  type made no difference. B10 only shows this layout in an informative
  annex (Annex F); the ARIB operational guidelines that may make it
  normative are not available to the project. `isdbt-mux` defaults to
  copy_control_type '01' + APS '00', and `isdb ts validate` warns
  (`si.copy_control`) on '00'. Details:
  [isdbt-mux SPEC-NOTES](../crates/isdbt-mux/SPEC-NOTES.md#digital-copy-control-descriptor-0xc1-and-its-user_defined-bits).
- **Remote-control key collisions.** Receivers file a TS under its
  remote_control_key_id (one-touch key 1–12) and list its services as
  three-digit channels key·10 + index (key 9 → 091, 092, …). A key that a
  local station already occupies made the recorder's scan merge or ignore
  the bench service. Use a key no station in your area uses; the examples
  use 9.
- **NIT channel = transmit channel.** The recorder registers services from
  the terrestrial NIT of the received TS and tunes by its frequency, so
  `[network] channels` must name the channel you transmit on (`--relay`
  does this automatically on `tx`). A relayed foreign TS with its original
  NIT is not registered.
- **Channel choice.** Pick a channel unused at your location (see
  [Channel plan](#channel-plan)).
- **USB tuner.** The MyGica S270 decoded the Rust transmitter with zero
  packet errors, and its capture validated with 0 errors. On that tuner
  `dvbv5-scan` failed to read sections for driver reasons unrelated to the
  TS (the all-PID DVR path works), so channel discovery on it is untested.
- **Open observation.** While the SDT carried running_status 4
  ("running") the recorder listed the service as off air (休止中). The SDT
  now sends 0, as the live stations do, but the recorder still showed the
  service as off air afterwards, so the cause is not isolated.

Other modes, guard intervals, DQPSK, three-layer and non-partial-reception
configurations have not yet been shown to a real receiver
([VERIFICATION.md](../VERIFICATION.md#open-questions)).
