# Receiving

`isdb rx` demodulates an ISDB-T signal from an IQ file or a radio
(`--radio`; HackRF One or an ADALM-Pluto / PlutoSky R2) and
writes the transport stream it carries. It needs no configuration: the
mode, guard interval and layer parameters are detected from the signal
(guard-interval correlation and the TMCC). It also receives ISDB-S (BS
and 110° CS satellite transponders) to one TS per TS_ID
([below](#isdb-s-bs-and-110-cs)).

## Sources

```sh
isdb rx --iq capture.cf32 --out out.ts                  # float32 I/Q (GNU Radio "complex")
isdb rx --iq capture.cs8 --iq-format cs8 --out out.ts   # signed 8-bit I/Q (hackrf_transfer)
some-producer | isdb rx --iq - --out out.ts              # stdin
isdb rx --radio hackrf --channel 27 --seconds 30 --out out.ts # live, from a HackRF
isdb rx --radio pluto:uri=ip:192.0.2.30 --channel 27 --out out.ts # a Pluto over Ethernet
```

IQ files must be complex baseband centred on the ISDB-T signal at
512/63 MHz ≈ 8.126984 MS/s. `isdb tx --iq` writes exactly that; so does
`hackrf_transfer -r FILE -f <f_c in Hz> -s 8126984` (the integer rate is
0.02 ppm off, well within what the receiver tracks). The centre frequency
of channel N is 473 + 6·(N − 13) + 1/7 MHz ([transmitting.md](transmitting.md#channel-plan)).

Receiving is not transmitting, but the bench rules still apply when the
signal comes from your own transmitter: keep a HackRF's RX input well
below its −5 dBm damage limit (attenuators, splitter; an AD936x's limit is
in the same range), and see
[transmitting.md](transmitting.md#bench-set-up). For off-air captures, see
[test-data.md](test-data.md#capturing-your-own-data).

| Option | Default | Meaning |
|---|---|---|
| `--iq PATH` | — | read baseband from a file (`-` = stdin) |
| `--iq-format cf32\|cs8` | cf32 | file format |
| `--radio NAME[:key=value,...]` | — | receive with a radio backend and its device arguments (`hackrf`, `hackrf:serial=0000abcd`, `pluto:uri=ip:192.168.2.1`); needs `--channel` or `--freq`. An unknown name lists the compiled-in backends |
| `--channel T` / `--freq HZ` | — | terrestrial channel T13–T52 (a bare number N means TN) or centre frequency in Hz, within 470–710 MHz. A satellite channel (BSn, NDn) selects ISDB-S (below) |
| `--freq-unrestricted` | off | allow a centre outside 470–710 MHz (channels 53–62, other bands); the backend's own range still applies |
| `--gain NAME=VALUE` | backend's | radio gain, repeatable; names, ranges and defaults are the backend's (an unknown name lists them). HackRF: `lna=0..40` dB in 8 dB steps (16), `vga=0..62` dB baseband VGA in 2 dB steps (20), `amp=on\|off` RF amplifier (off). Pluto: `agc=manual\|slow_attack\|fast_attack\|hybrid` (slow_attack), `gain=dB` manual gain (implies `agc=manual`; the range depends on the LO and is checked at open) |
| `--freq-corr-ppm P` | 0 | reference-clock correction (tuning and sample clock) |
| `--bb-filter-hz HZ` | 6 MHz | analog (baseband filter) bandwidth request; the backend picks a supported bandwidth |
| `--seconds S` | 0 | bound on signal time; `0` = the whole file, or until Ctrl-C / SIGTERM with `--radio` |
| `--status-secs S` | 1 | status line interval in seconds of signal (0 = none) |

## Outputs

Only frames the receiver marks *valid* are written (see below), so the
start of a capture — before lock and while the deinterleavers fill — is
not output.

| Option | Writes |
|---|---|
| `--out PATH` | the single TS a 13-segment receiver reproduces (ARIB STD-B31 §3.2.2): all layers in multiplex-frame slot order, with null packets in the slots of no layer |
| `--a-out/--b-out/--c-out PATH` | the packets of one layer |
| `--strip-nulls` | drop null packets (PID 0x1FFF) from every output |
| `--json` | a summary on stdout at the end |

Packets whose RS word could not be corrected are written with
transport_error_indicator = 1 (H.222.0), so downstream tools can see them.
`isdb rx` does not descramble: services that a broadcaster scrambles stay
scrambled in the output, while PSI/SI and unscrambled services are
readable. The output can be fed straight to `isdb ts` ([analysis.md](analysis.md)).

## ISDB-S (BS and 110° CS)

`--system isdbs` (or `auto`, the default, on a BSn / NDn channel or a
SigMF recording whose survey finds the ISDB-S symbol rate) receives an
ISDB-S transponder: frame sync and TMCC, then the main signal of every
superframe (TC8PSK, QPSK or BPSK, Viterbi, de-interleaving, energy
dispersal, RS(204,188)). A transponder carries up to 8 transport streams,
told apart by the TS_ID the TMCC gives each slot, so the output is per
TS_ID instead of per layer.

```sh
isdb rx --iq bs3.sigmf-meta --out 'bs3-{ts_id}.ts'          # one file per TS: bs3-4030.ts, …
isdb rx --iq nd2.sigmf-meta --ts-id 0x6020 --out nd2.ts      # one TS
isdb rx --radio pluto:uri=ip:192.0.2.30 --channel BS3 \
        --seconds 30 --out 'bs3-{ts_id}.ts'                   # continuous, FPGA front end
isdb rx --radio pluto:uri=ip:192.0.2.30,recorder=maia,bits=8 --channel BS3 \
        --offload cpu --seconds 10 --out 'bs3-{ts_id}.ts'     # blocks of IQ, CPU front end
isdb rx --symbols bs3.sym --out 'bs3-{ts_id}.ts'             # an FPGA words file (isdb symbols --out)
```

| Option | Default | Meaning |
|---|---|---|
| `--system auto\|isdbt\|isdbs\|isdbs3` | auto | the transmission system |
| `--out PATH` | — | with `{ts_id}` in the path: one file per TS_ID (4 hex digits); without it, select one TS with `--ts-id` |
| `--ts-id ID` | every TS | receive only this TS_ID (repeatable; `0x4010`, `4010h` or decimal) |
| `--iq REC` | — | a SigMF recording (data file, `.sigmf-meta` or base name): its metadata gives the rate and format; `isdb capture` writes them |
| `--rate SPS` / `--bw HZ` | 40M / 56M | radio sample rate and analog bandwidth (≥ 1.35 × 28.86 Mbaud; a narrower filter costs MER) |
| `--block-seconds S` | the radio's longest single capture | radio: seconds per contiguous capture |
| `--carrier-offset HZ` | from the survey | carrier offset of the signal |
| `--offload auto\|fpga\|cpu` | auto | radio: the radio's FPGA symbol front end when it offers one (continuous reception), else contiguous IQ captures with the CPU front end; `fpga` fails without one, `cpu` forces the captures |
| `--symbols FILE` | — | decode a `Symbols` words file (`isdb symbols --out`) as one continuous stream; the system comes from its `.json` sidecar |
| `--ldpc fixed\|scalar\|float` | fixed | ISDB-S3 LDPC decoder: fixed-point with 16 SIMD lanes; the same one codeword at a time (its bit-exact reference); the floating-point reference |
| `--json` | — | summary with per-TS counters (packets, RS clean / corrected / uncorrectable) and the stage times |

**Continuous reception (FPGA front end).** When the radio offers the
symbol front end in its FPGA (a PlutoSky R2 with the `isdb_top.bin`
bitstream, `fpga/README.md`), `isdb rx` acquires on a 0.2 s IQ probe
(the carrier tried from every survey candidate, the trim measured;
another probe if the lock is weak), configures the front end, and then
decodes the symbols it streams: 43–51 MB/s over Gigabit Ethernet, within
what the link carries. The words are parsed on their own thread (so the
radio never waits for the receiver), checked block by block (CRC-32C,
sequence numbers), and decoded in chunks of 24 frames (ISDB-S3) or 24
superframes (ISDB-S), cut on their own boundaries so the TS / TLV
streams run on without a gap. A loss in the words (a DMA overflow on the
device), or a lost lock, is a discontinuity in the output: reported as
`[rx] loss at symbol …` and counted in the summary's `[rx] stream:` line
and `stream` in `--json`. 30 s of BS1 gave three gapless TSs (2 716
superframes, every packet RS-clean, 0 continuity errors); 30 s of BS7
gave its three TLV streams with one reported loss (VERIFICATION.md
"Continuous reception through the FPGA front end"). `--symbols FILE`
decodes the same words recorded by `isdb symbols --out`.

**Blocks of IQ (CPU front end).** Without that front end, or with
`--offload cpu`, `isdb rx` takes one contiguous capture after another and
decodes each on its own: over Gigabit Ethernet a radio cannot stream the
≥ 39 Msps ISDB-S needs without losses (VERIFICATION.md "8-bit captures
and streaming over Gigabit Ethernet"), so the TS has a gap at every block
boundary, which the summary states. The longest single capture is about
1.7 s with a PlutoSky's maia-sdr recorder in 8 bits at 40 Msps.

Capturing, the front end and the receiver run at once, and later blocks
reuse the first block's carrier and trim after checking them on the
block's first 0.05 s (re-acquiring if they no longer hold). From a
PlutoSky over libiio a block of 0.83 s arrives every 2 s and the decoder
keeps up with it; from a recording, up to three blocks' front ends run
at a time. One contiguous block is limited by its front end, a single
sequential stream (about 0.75× real time at 40 Msps on a 32-thread
host). The summary's `[rx] time:` line (and `time` in `--json`) gives
the wall time against the signal time and the time of each stage
(VERIFICATION.md "Satellite receive speed"). Continuous reception needs
a link that carries the samples, or the FPGA symbol front end.

Slots whose relative TS ID points at an unused TS_ID entry (FFFFh) carry
no TS and are not written. Uncorrectable packets keep
transport_error_indicator = 1, as for ISDB-T. Satellite services are
mostly scrambled: their payloads stay scrambled, while PAT, PMT and SI
are readable (`isdb ts inspect`; `validate` does not parse scrambled
payloads).

## ISDB-S3 (advanced BS and wide-band CS)

`--system isdbs3` (or `auto` on a SigMF recording whose survey finds the
ISDB-S3 symbol rate) receives an ISDB-S3 transponder (ARIB STD-B44): slot
and frame sync on the π/2-BPSK sync words, a least-squares equaliser
trained on each frame's sync and pilot symbols, the TMCC (shortened LDPC,
BCH), then every slot's codeword (demapping against the constellation
the pilots show, bit de-interleaving, LDPC, energy dispersal, BCH). The
slots of each relative stream join into a TLV stream; the output is one
file of TLV packets (each with its 4-byte header) per TLV stream id.

```sh
isdb rx --iq bs7-61-bw56.sigmf-meta --out 'bs7-{tlv_id}.tlv'   # bs7-b070.tlv, …
isdb rx --iq bs11.sigmf-meta --ts-id 0xb0b0 --out b0b0.tlv     # one stream
isdb rx --radio pluto:uri=ip:192.0.2.30 --channel BS7 \
        --system isdbs3 --seconds 30 --out 'bs7-{tlv_id}.tlv'  # continuous, FPGA front end
isdb rx --symbols bs7.sym --out 'bs7-{tlv_id}.tlv'             # an FPGA words file
isdb iq s3tmcc bs7-61-bw56.sigmf-meta                           # the TMCC of every frame
isdb tlv inspect bs7-b070.tlv                                   # what a TLV stream carries
```

The options are those of ISDB-S above, with `{tlv_id}` (or `{ts_id}`) in
`--out` and `--ts-id` selecting TLV stream ids; `--strip-nulls` and the
layer outputs do not apply. `auto` cannot tell ISDB-S3 from ISDB-S on a
BSn channel from a radio before capturing, so give `--system isdbs3`
there. Record ISDB-S3 with the full 56 MHz analog bandwidth: the
default 0.8 × rate filter cuts into the 34.8 MHz band.

`isdb tlv inspect` lists the TLV packets by type (IPv6, header-compressed
IP, control, null), the MMTP packet ids, the MMT-SI messages and tables
(PA message with MPT and PLT, MH-EIT, MH-SDT, MH-TOT, MH-AIT, ECM, EMM,
…) and the share of MPU payloads whose MMTP header carries scramble
information. The scrambled payloads stay scrambled (no descrambling). The
same modes as ISDB-S apply from a radio: continuous with the FPGA front
end, or blocks of IQ with the CPU front end (VERIFICATION.md "ISDB-S3
receiver" and "Continuous reception through the FPGA front end").

## Status lines

```text
isdb rx: test.cf32 (Cf32); until end of input
[rx]     1.0 s LOCKED    (no frame yet)
[rx]     2.1 s LOCKED    M3 GI1/8 | frames 1/4 valid | MER A 118.6 B 125.1 dB | CFO +0.000 car | RS clean/corr/uncorr A 64/0/0 B 2592/0/0
[rx]     3.0 s LOCKED    M3 GI1/8 | frames 5/5 valid | MER A 118.6 B 125.1 dB | CFO +0.000 car | RS clean/corr/uncorr A 320/0/0 B 12960/0/0
[rx] done: 3.01 s of signal, 12 frames (9 valid); Mode 3, GI 1/8, partial reception; A 1seg QPSK 2/3 TI2 (64 TSP/frame); B 12seg 64QAM 3/4 TI2 (2592 TSP/frame)
[rx]   layer A: 576 packets; RS clean 576 corrected 0 (0 bytes) uncorrectable 0; MER mean 118.6 dB
[rx]   layer B: 23328 packets; RS clean 23328 corrected 0 (0 bytes) uncorrectable 0; MER mean 125.1 dB
```

| Field | Meaning |
|---|---|
| time | seconds of *signal* processed (not wall-clock time) |
| `searching` / `acquiring` / `LOCKED` | receiver state: looking for an ISDB-T signal; mode known, synchronising (frame sync from the TMCC); frame-locked |
| `M3 GI1/8` | mode and guard interval detected from the signal |
| `frames v/n valid` | OFDM frames in this interval, and how many were valid: the TMCC was decoded and every deinterleaver of the active layers was primed. After lock, the first frames are not valid because time interleaving delays data by up to several frames |
| `MER A … B … dB` | modulation error ratio per layer, from the equalised carriers. A noise-free cf32 file gives ≈ 120 dB (float precision), a cs8 file at the default back-off ≈ 41 dB (8-bit quantisation); in the software channel's white noise the default 64QAM 3/4 layer still decodes without packet loss at 18 dB SNR, with RS corrections (`isdb loopback --snr-db` to try) |
| `CFO … car` | carrier frequency offset in carrier spacings (Mode 1: 3.968 kHz, Mode 2: 1.984 kHz, Mode 3: 0.992 kHz). On a bench, a steady CFO is mostly the radio's clock error: CFO × spacing / f_c gives the ppm for `--freq-corr-ppm` |
| `RS clean/corr/uncorr` | per layer, packets whose RS(204,188) word was error-free, corrected (up to 8 bytes), or uncorrectable |
| `TMCC parity failed n` | frames whose TMCC failed the difference-set parity check (shown only when non-zero) |
| `overruns` | with `--radio`: buffers (HackRF USB transfers, Pluto IIO buffers) dropped because the demodulator fell behind |

The final summary repeats the totals; `--json` prints them as one object:
`frames`, `valid_frames`, `tmcc_parity_failed`, `params` (mode, guard
interval, partial reception, per-layer segments, modulation, code rate,
time-interleave index, TSPs per frame), `cfo_carriers`,
`timing_drift_samples_per_symbol`, per-layer `packets`, `rs_clean`,
`rs_corrected_packets`, `rs_corrected_bytes`, `rs_uncorrectable`,
`mer_db_mean`, `mer_db_min`, and the number of packets written per output.

## Performance

The receiver runs as a pipeline of two threads (demodulation, then FEC
decoding). On a current 16-core desktop CPU it processes a Mode 3 signal at about 1.4×
real time, the demodulator being the bottleneck. Live reception with
`--radio` therefore needs a fast CPU and a release build; if it falls
behind, the status line counts `overruns`. Recording to a file
(`hackrf_transfer` or `isdb tx`-generated files) and decoding offline has
no such limit.

## Software loopback

`isdb loopback` runs `tx` and `rx` in one process through a software
channel and checks every packet; it is the quickest way to see what the
receiver does under impairments:

```sh
isdb loopback --synthetic --frames 8 --snr-db 22 --cfo-carriers 2.3 --sample-offset -700000
```

`--snr-db` adds white Gaussian noise (`--seed` for another noise
realisation), `--cfo-carriers` a frequency offset in carrier spacings, and
`--sample-offset N` starts the receiver N samples late (N < 0) or after N
samples of silence (N > 0). The first valid frame is anchored to the
transmitted frame with the same content and every later frame is compared
packet by packet per layer and as a reproduced TS. It prints PASS/FAIL
counts and lost packets per layer and exits non-zero on failure; `--json`
gives a machine-readable verdict. Inputs are the same as for `isdb tx`
(`--synthetic` is the default when none is given).
