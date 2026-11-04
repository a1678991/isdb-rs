# isdb-frontend

Single-carrier receive front end for ISDB-S and ISDB-S3: IQ samples at
the radio's rate in, symbols at one sample per symbol out (the `Iq` →
`Symbols` stage of the receive chain). The stage exists twice with the
same output bit for bit: here, on the CPU, and in the FPGA of a PlutoSky
R2 ([`fpga/`](../../fpga/README.md)). The radio sends symbols instead
of samples when its link cannot carry the samples. This crate is the
reference: the fixed-point model is the specification the FPGA follows,
and it writes the test vectors the FPGA must reproduce. The crate also
holds the stage's inverse for the transmit direction, the pulse shaper
([below](#transmit-shaper)), and the bit-exact model of the FPGA's
transmit interpolator with its code format and registers
([below](#transmit-interpolator-the-fpga-model)): the host sends one 4-bit
code per symbol and the FPGA shapes them, because 40 Msps of IQ does not
fit the board's link.

Per input sample (the `fixed` module documents every step; format
version 4: version 3's words, the loop of an FPGA that takes one sample
per clock, [below](#format-4-one-sample-per-clock)):

1. **Mixer**: 32-bit NCO, 1024-entry cosine table, removes the carrier
   offset the host measured (`isdb_iq::survey`).
2. **Three polyphase filter banks** over the same delay line: the matched
   filter `h` (root-raised cosine under a Kaiser window), its derivative
   `g`, and the output filter `k` (the matched filter evaluated a trim
   later inside the same window, [below](#the-timing-trim)); 64 phases of
   32 taps, 18-bit coefficients. `h` and `g` are non-zero on their centre
   8 taps only (format 4).
3. **Timing recovery**: non-data-aided maximum-likelihood detector
   `Re{z·conj(dz)}` on `h` and `g`, which needs no carrier lock. It feeds
   a PI loop on the symbol period (Q8.24 samples) that applies each
   symbol's error 24 symbols later (the latency the FPGA pipeline is
   allowed).
4. **AGC** on the output symbols `zk` (gain Q6.10, same delay).
5. **Output**: n-bit I and Q (2..8) of `zk`, framed into the `Symbols`
   words.

The front end does not recover the carrier phase: its symbols rotate
slowly with the residual offset. Carrier recovery runs on the host after
the boundary, where the frame structure (TMCC, bursts, pilots) is
available.

## Design choices (measured 2026-10-20)

All on the lab's satellite IF (PlutoSky R2, Tezuka v0.3.24, 8-bit
captures at 40 Msps with 56 MHz analog bandwidth; details in
[VERIFICATION.md](../../VERIFICATION.md#symbol-front-end-iq-to-symbols-on-the-cpu-and-in-the-fpga)).

| Question | Measurement | Choice |
|---|---|---|
| Why not stream IQ? | iiod streams 8-bit IQ loss-free for 5 min up to 36 Msps (71.9 MB/s) with Tezuka's services stopped, and drops at 80 MB/s; a raw TCP send from the board manages 59.8 MB/s (one stream) to 69.5 MB/s (two, 89 % CPU) | IQ for ISDB-S needs ≥ 38.96 Msps ((1 + α)·R), ≥ 78 MB/s in 8 bits: above the ceiling. At 36 Msps it fits only by aliasing the roll-off (−0.9 dB MER) and with no margin. Send symbols instead |
| Sample rate | BS1 8PSK MER 14.9 dB at 40 Msps and 56 MHz bandwidth, against 15.2 dB at 57.72 / 61.44 Msps; 13.9 dB with 40 MHz and 13.0 dB with 32 MHz analog bandwidth | 40 Msps (≥ (1 + α)·R for both systems), analog bandwidth 56 MHz |
| Taps | ISDB-S MER at 8 / 12 / 16 / 24 / 32 taps: 13.75 / 14.33 / 14.65 / 14.86 / 14.94 dB. ISDB-S3 (2026-10-25, trimmed, sync-symbol Es/N0 on BS7): 32 / 48 / 64 taps 14.96 / 15.16 / 15.15 dB | 32 (64 DSP48E1 in the FPGA with the output bank) |
| Timing loop bandwidth | ISDB-S locks at 2·10⁻⁴ R. ISDB-S3 (α = 0.03: the detector's slope shrinks with the roll-off) wanders at 2·10⁻⁴ R with 32 taps (−265 ppm) and locks at 2·10⁻⁵…10⁻⁶ R (+0.68 ppm, the board's clock offset, as on ISDB-S) | 2·10⁻⁴ R for ISDB-S, 5·10⁻⁶ R for ISDB-S3 (`Design::for_system`) |
| Fixed point | Fixed-point and floating-point outputs within 0.03 dB (ring MER, 8PSK MER) on every recording | the model as specified |
| Output width | MER change by the n-bit quantiser (no clipping): ISDB-S 4 / 5 / 6 bits −1.1 / −0.32 / −0.07 dB. ISDB-S3, measured after the trim on BS7 / 11 / 17 (2026-10-25): 16APSK ring MER 4 / 5 / 6 / 7 bits −0.6 / −0.16…−0.19 / −0.04 / −0.01 dB, sync-symbol Es/N0 −1.0 / −0.27 / −0.07 / −0.02 dB; nothing reaches full scale. (The earlier "5 bits" for ISDB-S3 was measured through the untrimmed filter, whose 9 dB hid the quantiser's cost) | 6 bits for both: ISDB-S 43.4 MB/s, ISDB-S3 51.0 MB/s, 62 % and 73 % of the measured 70 MB/s ceiling |
| Timing trim | The loop locks 0.15–0.18 T off the best instant on ISDB-S3, 0.05–0.10 T on ISDB-S ([below](#the-timing-trim)) | output bank `k` with a trim the host measures (format 2) |
| Loop delay, detector taps (2026-10-26) | ISDB-S3 trimmed sync-symbol Es/N0 on BS7 / 11 / 17: loop delay 6 → 24 symbols 14.65 / 14.67 / 14.58 → 14.69 / 14.71 / 14.58 dB; detector banks on 32 / 24 / 16 / 12 / 8 centre taps 14.69 / 14.95 / 14.94 / 14.93 / 14.89 dB (BS7), the same pattern on BS11 and BS17 (`examples/tedspan.rs`). Running the detector on every 2nd or 4th symbol only (fewer multipliers by time-sharing) breaks ISDB-S3: 1.5–8 dB with cycle slips (`examples/loopdiag.rs`) | format 4: loop delay 24, detector on the centre 8 taps, on every symbol ([below](#format-4-one-sample-per-clock)) |

## Format 4: one sample per clock

The PlutoSky R2 clocks the FPGA's sample path with
`util_ad9361_divclk` = l_clk / 4: measured on the board, `adc_clk_count`
reads 160 MHz (l_clk), r1_mode is 0 and Tezuka's plutoskyr2 build is LVDS
(divider 4), so the block runs at the sample rate, 40 MHz at 40 Msps, and
sees one sample per clock (`util_wfifo` hands them over in bursts of
eight). The format-3 HDL assumed four clocks per sample. A full-rate
pipeline needs about 15 clocks from a decision to its loop terms, and
symbols can be due on consecutive clocks, so format 4 sets the loop delay
to 24 symbols (≥ 24 clocks). The output bank `k` keeps all 32 taps (64
multipliers); the detector banks `h` and `g` are non-zero on their centre
8 taps (32 multipliers instead of 128), which `Params::validate` enforces
and the FPGA relies on. Both choices cost nothing measurable on the
recordings (table above); the receivers' results are unchanged or better
(ISDB-S acceptance: 38 TSs, 0 uncorrectable; ISDB-S3 LDPC/BCH-clean,
trimmed sync Es/N0 14.79 / 14.81 / 14.69 dB through the CLI). A device
reporting format 2 or 3 is refused (`MIN_ARITH_VERSION` = 4).

## The timing trim

Measured on BS7, BS11 and BS17 (ISDB-S3, 16APSK) at 61.44 Msps and 56 MHz
analog bandwidth, 2026-10-25, with the sync-symbol Es/N0 of
[`s3sync`](src/s3sync.rs) (24 known π/2-BPSK symbols per 9 296-symbol
modulation slot):

| Where the SNR goes | Measurement (example) | Es/N0 |
|---|---|---|
| In-band SNR from the spectrum (2 MHz gap floor between transponders, approximate) | `isdb-iq` band power | ≈ 17 dB |
| Data-aided timing per slot, 128-tap filter | `s3budget` `slot` | 15.4 / 14.7 / 14.7 dB |
| Same filter, the loop's instant | `s3budget` `slot` | 9.5 / 9.6 / 9.0 dB |
| The loop's timing error | `s3budget` `slot` | −0.153 / −0.165 / −0.181 T, ±0.02 T rms, steady over 0.55 s and across sample rates (40 / 61.44 Msps), formats and analog bandwidths (32 / 49 / 56 MHz) |
| Filter length at ideal timing: 32 / 64 / 128 taps | `s3budget` `fir` | 14.39 / 14.73 / 14.80 dB (BS7) |
| Neighbouring transponders (brick-wall channel filter ahead) | `s3budget` `ideal`, `pre` | no change |
| Loop bandwidth 10⁻⁶ … 2·10⁻⁵ R, jitter 0.004–0.03 T | `s3budget` `loop` | within 0.1 dB |
| Fixed point against floating point | `s3budget` `fixed`, `float` | within 0.3 dB |

The detector is unbiased on clean synthetic signals, random QPSK and
ISDB-S3-structured streams with 8PSK or 16APSK data alike (lock offset
within ±0.05 T, `examples/tedbias.rs`), so the offset comes from the
received pulse, not from the detector or the arithmetic. Which part of
the chain distorts it (satellite transponder, cable feed or the radio's
analog filter) is not settled; it is the same on all three ISDB-S3
transponders and has the same sign, smaller in symbols, on ISDB-S
(−0.05 … −0.10 T on 21 transponders, where α = 0.35 makes the pulse
less sensitive).

Format 2 therefore evaluates the output a fixed **trim** away from where
the loop locks: the output bank `k` is the matched filter at τ + trim
inside the same window, while the loop keeps `h` and `g`. The trim
cannot be applied to the instant where `z` and `dz` are computed: the
loop would follow it. The host measures it before a run on known
symbols the front end cannot see: `s3sync::best_trim` (ISDB-S3 sync
words) and `isdbs_phy::frontend::measure_trim` (ISDB-S bursts), on
0.1–0.15 s of IQ. The optimum is flat within ±0.03 T (under 0.5 dB).
`best_trim` runs its candidates' front ends on threads of their own but
measures one at a time, each front end keeping only its symbols' `zk`
(`s3sync::measure_one`): 13 candidates on 0.15 s at 40 Msps each holding
their whole output took 6.1 GB of resident memory at once, now about
0.6 GB with the same result bit for bit.

Result over whole recordings, 32 taps, 8-bit output (`isdb iq s3sync`):

| | Trim | Untrimmed | Trimmed |
|---|---|---|---|
| BS7 | −0.168 T | 9.31 dB | 14.65 dB |
| BS11 | −0.165 T | 9.46 dB | 14.67 dB |
| BS17 | −0.180 T | 8.83 dB | 14.58 dB |

The rest of the gap to the per-slot ideal (0.7–1.2 dB) and to the
spectrum's estimate is intersymbol interference a fixed filter cannot
remove: an equaliser on the host, after the boundary, where the pilots
(STD-B44 Annex A) are available. On ISDB-S the trim raises the burst
MER by 0.1–2.3 dB (median 1.3 dB) on all 21 transponders; their TMCC
and TS stay RS-clean.

## The fast implementation

`fast::FastFrontend` computes exactly the reference's integers, only
differently: the newest `taps` mixer outputs kept contiguous (no modulo
per tap), the three banks of a phase fused, and the dot products as
16×16-bit products summed in `i32` on coefficients split as
`hi·2⁹ + lo` (both `i16`), which LLVM vectorises (`pmaddwd` at the
x86-64 baseline, `smlal` on aarch64). The sums fit `i32` while the last
`taps` inputs stay within 13 bits (|x| ≤ 4 095; the radios deliver 12):
a mixer output is then at most 8 190 in magnitude and a phase's
coefficient magnitudes sum to at most 2¹⁷ − 1, so each partial sum stays
below 64 × 512 × 8 190 < 2²⁸. While a larger input is in the delay line
an `i64` kernel on the unsplit coefficients is used, so the equality
holds for every `i16` input. The padded tap count is a const generic
chosen once per `process` call.

### Instruction sets (AVX2, AVX-512)

`simd::Isa` chooses at run time which build of the 16-bit dot products
the block methods (`process`, `process_each`) use: the baseline (what
LLVM vectorises for the build's target, SSE2 on x86-64), AVX2, or
AVX-512 (`vpmaddwd` written with `std::arch` inside `#[target_feature]`
functions). The loop around them (`advance`: mixer and delay line;
`finish`: loop update and decision) is the same generic code inlined into
each. Every instruction set gives the same integers.

- **Default:** AVX2 when the CPU has it, else the baseline. AVX-512 is
  slower on a Zen 4 (512-bit integer operations run as two halves) and is
  used only on request.
- **Override:** `ISDB_SIMD=baseline|avx2|avx512` (a request the CPU or
  build cannot run falls back to the widest available below it), or
  `FastFrontend::with_isa`. `push` always runs the baseline.
- **AVX-512 needs Rust ≥ 1.89** (stable AVX-512 intrinsics and target
  features). `build.rs` probes the compiler and sets
  `cfg(isdb_frontend_avx512)`; at the workspace's 1.88 minimum the
  AVX-512 code is left out and a request for it runs AVX2.
- **The one `unsafe`.** Calling a `#[target_feature]` function from code
  that lacks the feature is an `unsafe` call by the language rules (the
  run-time check is not something the compiler can verify), on stable
  and nightly alike. The crate does it in exactly one place,
  `simd::run`, immediately after `is_x86_feature_detected!` has found
  every feature of the loop it calls, with `#[allow(unsafe_code)]` on
  that item and a `SAFETY` comment. Inside a `#[target_feature]` loop the
  kernels (same features) are called without `unsafe`. The workspace's
  `unsafe_code = "deny"` is unchanged. A safe dispatch crate (`pulp`) was
  measured and rejected (plan §0.11: its kernels came out scalar).
- **Helpers as macros.** The kernels' load / fold / sum steps are macros
  that expand inside each kernel: as `#[target_feature]` functions (which
  cannot be `#[inline(always)]`) LLVM left them out of line in one loop,
  which then ran 2.8× slower than the baseline.

One core, 32 taps, 40 Msps (`examples/feisa.rs`, best of 3, load ≈ 3.5;
the reference takes 2.2 s for the ISDB-S3 block):

| | ISDB-S3, 0.826 s block | ISDB-S, 0.419 s block |
|---|---|---|
| baseline `process` / `process_each` | 0.94 / 0.78 s (1.06×) | 0.40 / 0.34 s (1.25×) |
| **AVX2** | 0.73 / **0.69 s (1.19×)** | 0.31 / **0.30 s (1.40×)** |
| AVX-512 | 0.79 / 0.77 s (1.07×) | 0.34 / 0.33 s (1.28×) |

The receivers call `process_each`. The timing loop is sequential, so
this is the speed of one contiguous stream.

Equality: `tests/fast.rs`, for **every instruction set the host runs**
and both block methods: property tests over designs, widths, trims,
rates, amplitudes to full-range `i16` and any chunking; every padded
width 8 … 64 (all kernel tails); opt-in recordings through
`ISDB_RS_ISDBS3_IQ` / `ISDB_RS_ISDBS_IQ`. A ±1 in one lane of either AVX
kernel fails 3–4 of these tests. The FPGA vectors stay made with the
reference. Tools: `examples/fespeed.rs`, `examples/feisa.rs` (per
instruction set), `examples/feprof.rs` / `examples/feeach.rs` for
`valgrind --tool=callgrind`.

## Transmit shaper

`shape::Shaper` turns symbols (one sample per symbol) into IQ at any
sample rate ≥ (1 + α)·R, so the samples per symbol may be fractional
(1.386 for ISDB-S at 40 Msps), root-raised-cosine shaped with the
matched filter's own pulse (`tables::rrc`), so the cascade is ISI-free.
Unit-modulus symbols give an output RMS of 1. The module documents the
signal model and the arithmetic.

- **Signal:** sample n = G·Σₖ aₖ·p(n/sps − span − k), p the RRC cut at
  ±span symbols (default 16): symbol k peaks at sample (k + span)·sps.
  The sample position is a 63-bit fixed-point fraction. It never drifts,
  any chunking of `push` gives the same samples bit for bit, and
  `samples_for(n)` = ⌈n·2⁶³ / step⌉ counts them exactly (an ISDB-S
  superframe at 40 Msps: 442 810 or 442 811 samples). `finish` appends
  the tail (2·span − 1 symbols' worth) and starts a new stream.
- **Bank:** 1 024 phases of 2·span taps, nearest phase, `f32` with
  8-lane sums in a fixed order (`f64` sums measure the same to 0.01 dB).
- **Window: none.** A window tapers the pulse, so the pair is no longer
  Nyquist: at α = 0.35 and span 16 the cascade's ISI bound is 58.8 dB
  truncated, 47.9 dB under the receiver's Kaiser β = 5, which buys only
  far sidelobes (−85 against −52 dB) a loopback or cabled bench does not
  need (`examples/shapewin.rs`). ISDB-S3's α = 0.03 needs a span of 32
  or 64 for 45 or 57 dB (`with_span`).
- **Measured** (`tests/shape.rs`): through an ideal matched filter at the
  symbol instants, MER 57.6 / 59.1 / 58.8 dB at 40 / 57.72 / 61.44 Msps;
  RMS within 10⁻⁵ of 1; through `fast::FastFrontend` (ISDB-S at 40 Msps,
  trim 0) 8PSK MER 36.2 dB at the 6-bit output (41.2 dB before the
  quantiser) over every output symbol, the receiver's outputs over the
  `finish` tail included; 36.98 / 44.54 dB in the steady state
  (`examples/txloop.rs`). One core shapes 67–73 Msps, 1.2–1.7× real time at those
  rates (`examples/shapespeed.rs`).

## Transmit interpolator (the FPGA model)

`shape::fixed::Interpolator` is the bit-exact model of the FPGA's
transmit block (`SymbolsTx` in [`fpga/`](../../fpga/README.md); plan
[`docs/plans/isdbs-fpga-tx.md`](../../docs/plans/isdbs-fpga-tx.md)): 4-bit
symbol codes ([below](#the-tx-boundary-symbol-codes)) in, 12-bit IQ at the
DAC rate out, one sample per clock. Its arithmetic is
interpolator.md §4's, with the conventions pinned in the module
documentation; the HDL reproduces it on the vectors of
`vectors::tx_document` (`examples/txvectors.rs`).

- **Signal:** the `Shaper`'s signal model one symbol later (the first code
  enters at the first carry of the phase accumulator, not before sample
  0), on a grid of P = 512 phases of 2S = 24 taps (span 12), rows at
  (q + ½)/P picked by truncation, the pulse `tables::rrc` cut at ±12
  symbols without a window and scaled by the shaper's gain and the
  backoff: unit-power symbols give RMS |y| = D = 2048·10^(−backoff/20) LSB
  (647.6 at the default 10 dB).
- **No multipliers:** every ISDB-S point is e^{jkπ/4}, so a tap adds ±v
  or 0 to each component, v from bank `a` (even k) or `b` = a/√2 (odd k),
  both 18-bit and rounded from the same `f64`. Backoff and gain live in
  the banks (`shift` 7 at 10 dB, max |a| = 90 825); the sums round,
  shift and saturate to 12 bits; the DAC word is out << 4.
- **Phase:** Q0.24, `step` = round(2²⁴·R/fs): 0xB8B439 at 40 Msps
  (−0.028 ppm), 0x800000 at 57.72, 0x784000 at 61.44 Msps (exact).
- **Symmetry:** `a[q][t] == a[P−1−q][2S−1−t]` exactly (the banks are
  built from one half and mirrored); `TxParams::validate` refuses a bank
  without it, because the FPGA stores one copy of each tap pair.
- **Underrun:** a word request the DMA cannot serve enters eight zero
  symbols (silence), counted in MISSED; FIRST_MISS latches CONSUMED at the
  first, and a sticky flag is set. `Interpolator::stall(n)` injects such
  stalls before the next word, as the vectors do.
- **Measured** (`tests/txfixed.rs`, ISDB-S at 40 Msps, 10 dB):
  - through an ideal `f64` matched filter at the realised symbol
    instants, 20 000 random 8PSK: MER 54.38 dB (interpolator.md: 54.38);
  - against the f32 `Shaper` at the same span: SNR 56.8 dB;
  - through `fast::FastFrontend` (the receivers' design, trim 0), the
    digital loopback: 8PSK MER 36.96 dB at the 6-bit output, 44.41 dB
    before the quantiser, in the steady state; the f32 shaper (span 16)
    reads 36.98 / 44.54 dB the same way (`examples/txloop.rs`). Measured
    over every output symbol up to the stream's end, `metrics::psk_mer`
    also counts the receiver's outputs over the zero tail (2·span − 1
    symbols): that is the 36.74 / 43.35 dB of interpolator.md §6 and the
    shaper's 36.19 / 41.24 dB, and it is why span 12 read 0.5 dB better
    than span 16 there (a shorter tail), not the pulse (in the steady
    state the model reads 36.98 / 44.45 dB at span 16);
  - peak component 998 LSB over 200 000 symbols; the bound, the worst
    row's Σ|a| / 2^shift, is 1 042 LSB: no clipping above 4.2 dB backoff.
- **Same as the study:** the banks, step and samples equal
  `examples/txinterp.rs`'s "mux floor½ P 512 span 12 Q0.24" bit for bit,
  given its stream one leading zero symbol (`txloop study`).

## The TX register map

`control::tx`: the receive block's command words and `Encoder` on the
DAC core's general purpose pair (`cf-ad9361-dds-core-lpc`, `gpio_out`
0x800000BC, `gpio_in` 0x800000B8; bit 0 of `gpio_out`, the ×8 TX FIR's
enable in Tezuka, kept as it was).

| Address | Register |
|---|---|
| 0x00 | STEP (u32, Q0.24) |
| 0x01 | SHIFT |
| 0x02 | STATUS, read only: bit 0 run, bit 1 selected, bit 2 underrun (sticky until START) |
| 0x03 | CONSUMED: codes entered from delivered words since START, zero codes included |
| 0x04 | MISSED: zero symbols entered for stalled requests since START |
| 0x05 | FIRST_MISS: CONSUMED at the first stalled request (0 before one) |
| 0x06 | SAMPLES: outputs since START (low 32 bits) |
| 0x4000 + q·32 + t | bank A, row q, tap t < 2S (WRITE_S) |
| 0x8000 + q·32 + t | bank B, likewise |

CTRL: 1 run, 2 START (one pulse: phase, line, counters and unpack
register reset), 4 SELECT (the block drives the DAC data path instead of
the DMA's IQ). **ID word** 0xB5D1_1918: magic 0xB5D1 in bits 31–16, the
format version (1) in 15–12, log2 P (9) in 11–8, the taps (24) in 7–0;
`TxIdentity::supports` accepts only the model's version and geometry.
`Encoder::configure_tx` writes STEP, SHIFT, then banks A and B row by row,
taps 0..S only (the stored half; the block gets tap t ≥ S of row q from
tap 2S − 1 − t of row P − 1 − q): 13 315 command words, 12 290 of them
writes, about 8.3 s at the 0.62 ms per register write measured through
iiod (`fpga/README.md`), so the bitstream carries the default bank as its
initial contents.

**The host's sequence** (`isdb-radio`'s Pluto backend): configure, read
STEP back, CTRL START|SELECT (the block owns the DAC and the DMA, sends
zeros and requests nothing; STATUS must read "selected"), start the TX
DMA, and once half the device's kernel queue is filled (8 × 73 ms), CTRL RUN|SELECT
(STATUS 3): the
block counts every word it requests before the first arrives as missed,
and before SELECT the pass-through sends the DMA's words to the DAC as
samples. CTRL 0 after the stream stops, before the attenuation and the
LO are made safe.

## Spec scope

The front end implements no broadcast specification: the symbol rates
and roll-offs come from `isdb_iq::survey::KNOWN_SYSTEMS`, which quotes
them from the transmission standards, and the arithmetic and the
`Symbols` format are this project's own definitions (format version 3).
The transmit shaper's pulse is the square-root raised cosine that ITU-R
BO.1408-1 Annex 1 §9 (MIC Ordinance No. 87 annexed table 47) prescribes,
the same function as the matched filter's.
[`s3sync`](src/s3sync.rs) reads ARIB STD-B44 v2.1 Fig. 3.3-1 (slot
layout, FSync / !FSync / SSync) and §3.8 (π/2-shift BPSK) to measure
SNR; it decodes nothing, so the crate has no SPEC-NOTES. One reading
there is an observation only: on the lab's signals one odd slot in 60
(once per 120-slot frame) has the opposite sign to the other odd slots;
which of FSync and !FSync that is depends on the bit order and
reference phase assumed, and a TMCC decoder will settle it.

## The `Symbols` boundary, versions 1 to 3

A stream of 16-bit little-endian words in blocks. Header and payload are
the same in every version; version 2 changed the arithmetic (the output
bank), version 3 only the trailer. A `Packer` writes version 3, a
`Reader` takes all three, even mixed in one stream:

- **Header** (9 words): sync words `0x1ACF 0xFC1D`; format word (version,
  bits − 1, log2 of the block length); 32-bit sequence number; gain;
  32-bit period; check word.
- **Payload**: L = 4096 symbols, `I | Q << bits`, packed least
  significant bit first.
- **Trailer, version 3**: two words, low first: the standard CRC-32C
  (CRC-32/ISCSI, check value `0xE3069283`) of the 32-bit sequence
  number as 4 little-endian bytes followed by the payload as
  little-endian words, so any CRC-32C library checks a block.
- **Trailer, versions 1 and 2**: a CRC-16 (0x1021) of the payload,
  seeded with the low 16 bits of the sequence number.

Why CRC-32C: at these payload lengths (49 152 bits for 6 bits × 4 096
symbols, up to 524 288) the Ethernet CRC-32 guarantees Hamming distance
3 beyond 91 607 bits, CRC-32C distance 4 (Castagnoli 1993, Koopman
2002; cited, not re-derived). Why the whole sequence number as seed: a
splice of a later block onto a header then fails for any of 2³²
sequence differences.

**How the `Reader` accepts a block.** Version 3: on the CRC-32C alone. A
damaged block gets through only on a 2⁻³² coincidence at a valid header
position; at one loss per second that is once in about 136 years.
Versions 1 and 2: the CRC-16 *and* a framing check (the next block's
header starts right after the trailer, or the stream ends there). The
CRC-16 alone let a splice through: a property-test seed glued an intact
header to later payload with a matching word in the trailer position
(`examples/formatcut.rs`); the framing check rejects it, at the price of
the intact block just before each loss (counted as lost, never
delivered wrong) and a delivery lag of one block. Version 3 has neither
cost: a v3 file with a 1 000-word cut reads with exactly the 2 touched
blocks lost. Sequence numbers count the missing blocks, and the blocks
around a loss stay in order. The `format` module has the details.

**Speed.** The CRC-32C is slicing-by-8 in safe Rust: 2.8 GB/s on one
core (the old bitwise CRC-16: 173 MB/s; the 6-bit ISDB-S3 link: 50.6
MB/s), and the `Reader` takes a clean version-3 stream at 558 MB/s
(version 2: 137 MB/s) (`examples/crcspeed.rs`). The FPGA updates the
CRC one 16-bit word per clock.

## The TX boundary: symbol codes

What a host sends the FPGA's transmit block instead of IQ (`txformat`,
format version 1): one 4-bit code per symbol.

| Code | Symbol |
|---|---|
| 0 | the zero symbol (silence) |
| 1–7 | reserved: the zero symbol |
| 8 + k | the point e^{jkπ/4}, k the angle index of annexed table 46 |

Every ISDB-S symbol is one of those eight points (header and burst BPSK,
BPSK, QPSK and TC8PSK main signal), so the stream is lossless.
`LabelCodes::TABLE_46` maps the sources to codes: TC8PSK label
000 → k 0, 001 → 1, 011 → 2, 010 → 3, 100 → 4, 101 → 5, 111 → 6,
110 → 7; BPSK bit 0 → k 0, 1 → k 4; QPSK (P1, P0) → its table-46 label
(00 → 001, 10 → 010, 11 → 101, 01 → 110). A conjugated constellation is
another 8-entry table (`conjugated`), nothing in the block.

**DMA words:** the DAC DMA sample is two little-endian `i16` words,
`voltage0` (I) then `voltage1` (Q), both channels enabled, and carries
eight codes, the first in time in I bits 3..0, then I 7..4, 11..8, 15..12,
then Q likewise: as one `u32`, `Q << 16 | I` with code j in bits
4j + 3..4j. 28.86 Mbaud / 8 × 4 B = 14.43 MB/s, against a measured host →
board ceiling of 64 MB/s. Zero padding (libiio's, the DMA's on underflow)
is silence. There is no header or sequence number: the DMA is lossless
and in order, and its only failure, an underrun, inserts zero symbols
the block counts (MISSED, FIRST_MISS) rather than losing or reordering
any. `CodePacker` carries a partial word across calls; an ISDB-S frame
(39 936 symbols) is a whole number of words.

## Position in the stack

- Depends on: `isdb-iq` (the known systems), `num-complex`, `serde_json`,
  `thiserror`.
- Used by:
  - `isdb-radio`: `Offload::Symbols`, `Backend::capture_symbols`, and
    the Pluto's discovery and configuration through `control`;
  - `isdb-cli`: `isdb symbols`, `isdb iq symbols`; `shape::Shaper` in
    `isdb tx` / `isdb loopback --system isdbs`;
  - `fpga/`: the test vectors (receive and transmit);
  - `isdbs-phy` and `isdbs3-phy`, the ISDB-S / ISDB-S3 receivers,
    through `fast::FastFrontend`; `isdbs-phy`'s transmitter makes its
    codes with `txformat::LabelCodes`.

## Key types and entry points

| Item | Role |
|---|---|
| `params::Design`, `Design::for_system`, `Params::design` | physical settings (rates, roll-off, taps, loop bandwidth, bits, trim) to the integer parameters (the FPGA's registers and banks) |
| `fixed::Frontend` (`push`, `process`) → `Symbol` | the bit-exact model; `Symbol` carries every internal the FPGA test compares |
| `fast::FastFrontend` (`push`, `process`, `process_each`) | the same `Symbol`s, 2–2.4× faster: what the receivers run (below) |
| `float::FloatFrontend` | the floating-point reference |
| `format::Packer`, `format::Reader` → `Event::{Block, Loss, Truncated}` | the `Symbols` words and their reader |
| `control::{Encoder, Identity, reg}` | the FPGA's command words (through the AD9361 core's `up_adc_gpio_out` / `up_adc_gpio_in`), register map and ID word |
| `metrics::{ring_stats, psk_mer, sqnr_db}` | MER without frame sync, SQNR |
| `s3sync::{measure, measure_at, measure_one, best_trim, derotated_data}` | ISDB-S3 slot sync (the search on every core, bit for bit as one thread) and sync-symbol Es/N0, or the same at a known offset, or of a front end's output in bounded memory; the trim search; data symbols for ring statistics |
| `vectors::document` | the JSON test vectors for `fpga/` |
| `shape::Shaper` (`new`, `with_span`, `push`, `finish`, `samples_for`) | the transmit pulse shaper: symbols to IQ at any rate ≥ (1 + α)·R, output RMS 1 |
| `shape::fixed::{TxDesign, TxParams}`, `TxParams::design`, `validate` | the transmit interpolator's design (rates, roll-off, span, phases, backoff) to its registers (step, shift, banks `a`, `b`) |
| `shape::fixed::Interpolator` (`push_words`, `push_codes`, `stall`, `output`, `process`, counters) → `TxSample` | the bit-exact model of the FPGA's transmit block; `TxSample` carries every internal the FPGA test compares |
| `txformat::{LabelCodes, CodePacker, word, codes, pack, unpack}` | the TX boundary: symbol codes and their DMA words |
| `control::tx::{TxIdentity, reg, ctrl, status}`, `Encoder::configure_tx` | the transmit block's register map, ID word and configuration |
| `vectors::tx_document` | the JSON test vectors of the transmit block |

Diagnostics are `tracing` events, never printed; the crate installs no
subscriber. `warn`: a loss in a `Symbols` stream (`format::Reader`:
`blocks` missing, `words` skipped). `debug`: the front end built (taps,
bits, nominal period, kernels), a stream joined mid-block or ending
inside one, the ISDB-S3 slot sync measured and the trim chosen. The
sample and symbol loops emit nothing: the timing loop and AGC have no
lock state to report, and the fixed-point arithmetic and FPGA test
vectors are unchanged.

## Tests

- **Unit, spec and property tests:**
  - header layout (versions 1 to 3), CRC-16 and CRC-32C check values,
    the slicing CRC-32C against a bitwise one on every short length, the
    version-3 trailer as a CRC of its little-endian byte string, a mixed
    v1/v2/v3 stream, command word layout, which device versions the host
    drives (2 and 3);
  - the loss property for versions 1 and 2 (with the framing exception)
    and for version 3 (every untouched block delivered), the two cuts
    the property test found replayed on both, and fault injection: with
    the CRC skipped the version-3 property test fails, and a grid of
    payload cuts is caught 148 of 208 times;
  - the output bank without trim equals `h`, and a trim changes only
    the output, not the loop's first symbols;
  - the slot sync measures the SNR of a synthetic stream within 1 dB;
  - `prop_pack_unpack_round_trip`;
  - `prop_any_loss_is_reported_and_the_rest_kept`: any cut anywhere,
    every untouched block delivered;
  - mixer and timing checks.
- **The transmit shaper** (`tests/shape.rs`, unit tests in `shape`):
  - `prop_chunking_is_invisible`: any cut of the stream, the same
    samples bit for bit, tail and the next stream included;
  - the impulse response is the delayed pulse; the gain is the inverse
    RMS of the pulse; the bank's rows and phase rounding;
  - ISI-free through an ideal matched filter at 1.35 to 3.47 samples
    per symbol (≥ 40 dB; 57.6–59.1 dB measured);
  - output RMS 1, the tail and the reset, `samples_for` against `push`,
    refused rates, roll-offs and spans;
  - through `fast::FastFrontend` at 40 Msps: 8PSK MER ≥ 25 dB.
- **The transmit interpolator** (`tests/txfixed.rs`, unit tests in
  `shape::fixed`, `txformat`, `control`):
  - equal to a plain reading of the specification on every field of
    every sample, with stalls, every kind of code, clipping and a dry
    queue, at three designs;
  - the impulse response is the pulse one symbol late; ISI-free through
    an ideal matched filter (≥ 40 dB, within 0.5 dB of 54.4: 54.38);
    within 40 dB of the f32 shaper (56.8 dB); through
    `fast::FastFrontend` ≥ 36.0 dB at the 6-bit output (36.96 dB);
  - `prop_chunking_is_invisible`; a stall of n requests is exactly 8·n
    zero symbols with CONSUMED, MISSED, FIRST_MISS and the flag right; a
    dry queue decays to silence; START resets;
  - the banks' symmetry, an asymmetric bank (one LSB in `a`, in `b`, in
    the far tap of a pair) refused, coefficient and register ranges;
  - the steps 0xB8B439 / 0x800000 / 0x784000, shift 7, the ID word
    0xB5D1_1918, `configure_tx` decoded back by a model of the command
    protocol; `prop_pack_unpack_round_trip`, table 46's codes.
- **Opt-in recordings** (`tests/captures.rs`, `ISDB_RS_ISDBS_IQ`,
  `ISDB_RS_ISDBS3_IQ`):
  - timing locked within 5 ppm of the survey;
  - fixed point within 0.1 dB of floating point;
  - ISDB-S 8PSK MER above 10 dB;
  - the words read back to the same symbols;
  - ISDB-S3: the trim measured on 0.1 s raises the sync-symbol Es/N0 by
    more than 3 dB and above 13 dB, and 6 bits lose less than 0.15 dB
    of it.
- **The FPGA side:** `mise run fpga:test` writes vectors with
  `examples/vectors.rs` and simulates the HDL against them;
  `examples/txvectors.rs` writes the transmit block's (`fpga/build/tx-*.json`:
  continuous, random stalls, one long stall, another bank loaded through
  the port; `isdbs-phy`'s `examples/txvectors.rs` adds real ISDB-S
  codes).
- **Analysis:** `examples/analyze.rs` prints timing lock, MER and the
  output-width table for a recording; `examples/s3budget.rs` is the
  loss budget above, `examples/s3trim.rs` the trim end to end with the
  output-width study, `examples/tedbias.rs` the detector on synthetic
  signals; `isdbs-phy`'s `examples/trimscan.rs` the trim on ISDB-S;
  `examples/shapewin.rs` the transmit pulse's window study,
  `examples/shapespeed.rs` the shaper's speed, `examples/txloop.rs` the
  interpolator's digital loopback (steady state against the tail) and its
  equality with the study's arithmetic, `examples/txinterp.rs` the
  interpolator study.
