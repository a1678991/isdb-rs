# Plan: the ISDB-S3 receiver in the FPGA (2026-11-01)

Goal: cut the host CPU that ISDB-S3 reception needs once the symbol
front end already runs in the PlutoSky R2's FPGA, so that `isdb rx
--system isdbs3` can run on a small host (a few cores, an SBC, the
Zynq's own ARM cores one day) instead of a 16-core desktop. Everything
under "Measured" was run on this host on 2026-11-01; everything under
"Design" is the plan. Conventions as in [isdb-s.md §7](isdb-s.md): the CPU
fixed-point model first, frozen, with vectors; the HDL reproduces it bit
for bit in simulation before any Vivado run; nothing is deployed to the
board without the hardware-in-the-loop checks.

## Measured

### What the host still does for ISDB-S3 after the front end

`isdb rx --symbols` on the 40 s FPGA symbol recording of BS7
(`~/isdb-captures/2026-10-26-fpga/bs7-fpga-40s-drift.sym`, 16APSK 93/120
on all 120 slots, 3 TLV streams, 1 207 frames, 0 LDPC / BCH failures,
mean 2.08 LDPC iterations), release build, x86-64 baseline:

| Host | CPU time for 40 s of signal | Wall | Keeps up? |
|---|---|---|---|
| 32 threads (Ryzen 9 7950X, 16 cores) | 386 s user (≈ 9.7 cores busy) | 25.1 s (1.59×) | yes |
| pinned to 8 physical cores | 250 s | 41.2 s (0.97×) | **no** |
| pinned to 4 physical cores | 220 s | 64.0 s (0.63×) | no |

(`GNU time` on `isdb rx --symbols … --system isdbs3 --json`, `taskset`
on one hardware thread per core. The CPU-time drop with fewer cores is
the per-chunk thread spawning and the LDPC lanes' better cache use.)

Where it goes, one 24-frame chunk (0.826 s of signal, the streamed
path's unit) on **one core** (`isdbs3-phy` `examples/streamprof`,
`taskset -c 0`):

| Stage | One core | 32 threads (wall) | Notes |
|---|---|---|---|
| demap + de-interleave + LDPC + BCH + dispersal (`decode_frames`) | 3.30 s | 0.23 s | of which LDPC ≈ 40 % (`rxparts`: 1.76 s with LDPC against 1.06 s at 0 iterations on 12 frames), the rest the `f64` max-log demapper and the `Vec<f32>` → `i8` path |
| per-frame preparation: TMCC LLRs + LDPC, main symbols derotated by the slot phase, pilots | 0.61 s | 0.04 s | `phase_at` per symbol: one `arg`, one `from_polar`, a complex divide |
| equaliser apply (11 complex taps, `f64`) | 0.27 s | 0.07 s | |
| derotation, two slot syncs, noise estimate, train | 0.16 s | 0.13 s | |
| **sum** | **4.35 s = 0.19× real time** | 0.49 s | |

So the LDPC is about a third of the host's work; the demapper, the
per-symbol phase correction and the equaliser are the other two thirds.
Moving only the LDPC would not help even if it were possible (it is
not: the LDPC's input is on the host, the FPGA is in the radio), and a
soft-bit boundary out of the radio is heavier than the symbol one
(isdb-s.md §7.1: 63–105 MB/s against 51 MB/s). The useful boundary is
**after the whole chain: slot data**, ≈ 12.5 MB/s for a 16APSK 93/120
transponder, 18.3 MB/s at most (32APSK 109/120, 120 valid slots). The
host then only assembles TLV packets and parses MMT, which is a fraction
of one core.

### Capacity of the deployed image

`fpga/tezuka/utilization.tcl` on the final checkpoint of the deployed
image `isdb_txrx2_top.bin` (both blocks, no DVB-S2 transmitter, no TX
FIR; Vivado 2026.1, `xc7z020clg484-2`):

| Block | LUT | FF | BRAM36 (RAMB18 as ½) | DSP48E1 |
|---|---|---|---|---|
| whole image | 23 557 (44 %) | 30 211 (28 %) | 68 (49 %) | 158 (72 %) |
| `axi_ad9361` (ADI, needed) | 7 603 | 12 263 | 0 | 28 |
| `isdb_symbols` (the RX front end) | 5 824 | 5 519 | 25 | 112 |
| `maia_sdr` (spectrometer 2 750 LUT / 14 BRAM36, DDC, recorder) | 3 412 | 3 509 | 29 | 18 |
| `isdb_symbols_tx` (the ISDB-S transmitter) | 2 420 | 3 105 | 6 | 0 |
| DMAs, interconnects, cs12 packers, upack, clocking | ≈ 4 300 | ≈ 5 800 | 8 | 0 |
| **free** | **29 643** | **76 189** | **72** | **62** |

A receive-only ISDB-S3 image can also drop `isdb_symbols_tx` and
`maia_sdr` (Tezuka's web spectrum and recorder; `isdb` never uses them;
the stock image stays on the card for them): **35 500 LUT, 107
BRAM36, 80 DSP** free. The front end's 112 DSP48E1 stay (its 64 `k`
products could move to the fabric or to a 2× clock if the DSP budget
becomes the limit, a separate change).

### The codes, for the decoder's sizing

`tools/ldpc-layer-conflicts.py` (the B44 address tables rebuilt as
`isdb_fec::ldpc::Code::new` does):

| Rate | q (layers) | P (checks) | edges | largest check degree | row entries congruent mod q ("conflicting pairs") |
|---|---|---|---|---|---|
| 41/120 | 79 | 29 546 | 147 729 | 5 | 4 |
| 61/120 (also the TMCC's shortened code) | 59 | 22 066 | 154 461 | 7 | 6 |
| 93/120 (the lab transponders) | 27 | 10 098 | 161 567 | 16 | 25 |
| 109/120 | 11 | 4 114 | 131 647 | 32 | 26 |

Every code has 374 information groups; the check c = (x + j·q) mod P
of entry x and bit j of a group, so the checks with one residue mod q
form a layer in which each group touches each check at most once, bar
the conflicting pairs. Throughput needed: 120 slots × 30.26 frames/s =
**3 631 codewords/s** at most (all 120 slots valid: 32APSK; 2 905 for
16APSK's 96), plus 30 TMCC codewords/s; mean 2.08 iterations on the lab
signal at 13.5 dB sync Es/N0 (16.5 dB equalised), 50 the CPU's limit,
with no failure recovered past about 10 (VERIFICATION.md "ISDB-S3 …
fixed-point": 5 failures of 9 216 at 50, 100 and 200 iterations).

## Design

### Boundary: `Slots`, format version 1

What leaves the radio instead of `Symbols`: a stream of 16-bit words in
blocks with the `Symbols` framing (sync `0x1ACF 0xFC1D`, a header with
a type and sequence number, the CRC-32C trailer seeded with the
sequence number: `isdb_frontend::format`), of three block types:

- **slot**: frame counter (32 bits), slot index (0–119), the slot's
  mode (modulation, rate), LDPC iterations and converged flag, BCH
  result (corrected bits, or failed), then the payload: the 176 header
  bits and the data bits of Table 3.2-1 (dispersal removed, BCH
  corrected), least significant bit first as `Symbols` packs, padded to
  whole words. Dummy slots and slots whose LDPC did not converge are
  sent too (flagged; the host counts them as today's `data: None`).
- **tmcc**: once per frame, the 9 422 TMCC information bits as received
  (after dispersal and BCH), with the BCH result and the frame counter
  of the first frame the layout was applied to.
- **status**: once per frame: sync detector statistics (the per-frame
  correlation peak and its off-peak mean, the FSync/SSync parity, the
  frame-head slot), the residual-carrier register (Hz as the loop's
  fixed-point word), the AGC gain, the equaliser's state (taps in use
  and the training accumulators' frame counter), the noise estimate
  `n0`, the learned constellation in use, and the counters (frames,
  slots, LDPC failures, BCH failures, iteration sum).

Host: `isdb_frontend::slots` (or a new small crate) reads the stream
(`Reader` with sequence gaps reported as losses, as `Symbols`),
`isdb rx --system isdbs3 --radio … --offload auto` takes it when the
radio offers the block, `isdb rx --slots FILE` decodes a recorded stream,
and the TLV assembly, MMT-SI and outputs are today's (`rxs3`). The
whole chain's registers go through the existing command protocol
(`control.py`, `isdb_frontend::control`) on `up_adc_gpio_out` /
`up_adc_gpio_in`, the ID word extended with the chain's version.

### The chain in the FPGA, stage by stage

All of it streams at one symbol per clock from the front end's
quantiser (the 6-bit `k` output, or wider internally: the chain reads the
unquantised `zk` 16-bit value where that is free, which the model
states), in the 40 MHz sample clock domain except the LDPC decoder,
which runs in a faster domain (`clk_fpga_0`, 100 MHz, or a 200 MHz
MMCM output) behind FIFOs. The CPU model of every stage is fixed point,
written first in Rust, and the only reference.

1. **Slot sync.** Correlation of each symbol position with FSync and
   SSync (24 symbols at angles π/4 + k·π/2: sums of ±I ±Q, no
   multiplier), magnitude by max(|re|, |im|) + ½·min (the model's
   choice, not `hypot`), a per-position accumulator over the 9 296
   positions of a slot (BRAM: 9 296 × 16 bits) with leak, the peak
   position = the slot offset, the FSync/SSync parity from the
   alternation, and the frame head from the sign minority as
   `sync::synced` does: the slot head record (phase as the 24-symbol
   correlation, amplitude as its magnitude). Lock / loss flags in
   status. A loss restarts the search; the host sees it as a gap in the
   frame counter.
2. **Carrier loop.** The residual carrier from the phase step between
   consecutive SSync slots (two slots apart, no sign ambiguity; ±908 Hz
   unambiguous, as `sync::residual_hz`), integrated into an NCO that
   rotates the stream (CORDIC or a 1 024-entry sine table, 12-bit
   phase increment per symbol), replacing the host's per-chunk tracking.
   Bandwidth: the live drift was −22 Hz/s; a loop that corrects once per
   frame (33 ms) with gain ½ follows it with < 1 Hz of lag.
3. **Slot phase and amplitude.** Each symbol divided by the linear
   interpolation of its slot head's phase and the next head's (`phase_at`):
   the next head is 9 296 symbols ahead, so the stream is delayed one
   slot (9 296 × 2 × 12 bits = 223 kbit, 7 RAMB36, or 6-bit components in
   3.5). The division by the unit phasor is a complex multiply by its
   conjugate; the amplitude normalisation a reciprocal from a table and
   one multiply. 4–6 DSP48E1.
4. **Equaliser.** 11 complex taps, Q2.14, applied as a FIR to the phase-
   corrected stream (44 real multiplies per symbol: 44 DSP48E1 at 40 MHz,
   or 22 at 80 MHz with the taps' symmetry not assumed; this is the
   largest DSP user and the first candidate for a 2× clock). **Training
   stays a slow loop:** the block accumulates the normal equations
   R = Σ x xᴴ (11 × 11 Hermitian, 66 complex sums) and p = Σ x d* over
   the 6 720 training symbols of a frame (24 sync + 32 pilots per slot,
   with the pilot PRBS signs and the slot's phase removed, as
   `eq::training`), time-multiplexed on a handful of DSPs (56 training
   symbols × 77 complex MACs per 9 296-symbol slot); the host reads the
   sums (through the register file, ≈ 300 words), solves them in `f64`
   (`eq::solve`) and writes the 11 taps, once at lock and then every few
   seconds. The taps in use are in the status block, so a recorded `Slots`
   stream and the model agree. Before the first taps arrive the block
   runs with the identity tap (the model does too).
5. **TMCC.** The 264 TMCC symbols per slot demapped as π/2-BPSK soft
   bits (`tmcc::bpsk_soft`, in the model's fixed point), quantised to
   i8 and written to a 9 614-position buffer in the LDPC input; the
   frame's TMCC codeword goes through the shared LDPC decoder as the
   shortened 61/120 code (the 1 870 + 11 330 known zeros as +127 LLRs,
   which the decoder's address generator can supply instead of storing);
   then the TMCC dispersal and BCH (step 8), `TmccInfo` parsing in the
   FPGA only as far as the layout needs: the eight modes (modulation,
   rate, slots) and `allocation_ok`; the rest of the 9 422 bits go to
   the host in the tmcc block.
   **When a TMCC applies.** B44 §3.11: "the TMCC information transmits
   the switching information two frames prior to actual switching; the
   minimum update interval of the TMCC information is one frame". The
   FPGA decodes frame N's TMCC while frame N streams past, so the layout
   it can apply to frame N + 1 at the earliest is frame N's; the model
   does the same (the layout in force for a frame is the one decoded
   from the latest complete earlier frame) and the CPU `rx::receive`,
   which uses the frame's own TMCC, is left as it is (identical on a
   constant allocation, which is all the lab has seen). The first frame
   after lock has no layout: it is reported (status) and not decoded, as
   the host's streamed path drops the frame it cuts on. A SPEC-NOTES entry
   records the reading and that a switch has never been observed.
6. **Demapper.** Max-log LLRs against the slot's constellation: with
   d² = |y|² − 2 Re(y p̄) + |p|², the |y|² term cancels in m₁ − m₀, so
   the metric per point is |p|² − 2(yᵢpᵢ + y_q p_q): two multiplies per
   point, 32 points at most, min over the label's bit sets, difference,
   times the noise scale, rounded to i8 (the model's `quantise` with
   `input_scale` 1 folded into the scale), de-interleaved on the way
   into the LDPC input buffer (the §3.7 column write is an address
   pattern: bit b of symbol s goes to codeword position
   b · rows + s, forward or backward by Table 3.7-2). At 40 Msym/s and
   32 points this is 64 multiplies per symbol: 64 DSP48E1 at the symbol
   clock, or 16 at 160 MHz; alternatively a ring-first search (the APSK
   points sit on 1, 2 or 3 rings) in the model, which the HDL then
   reproduces exactly. **The constellation** is the pilot-learned one as
   today: per label, the sum of the frame's pilot symbols of that label
   (3 840 pilots per frame, 32 accumulators) divided by their count at
   the frame's end (one divider, 32 divisions per frame), in force from
   the next frame; before the first frame, the nominal points of §3.8.
   **The noise** `n0` likewise from the previous frame: the mean of the
   minimum metric over every 16th symbol (`rx::noise_of`), its
   reciprocal to the scale by one division per frame.
7. **LDPC decoder.** Layered offset min-sum in i8, the arithmetic of
   `isdb_fec::ldpc::fixed` per check (saturating t = L − r, sign parity,
   min1 / min2 with the first edge on ties, magnitude minus offset
   floored at 0, L = t + r saturating), with a **schedule** that the
   model and the HDL share (next section). Early stop when the hard
   decisions satisfy every check (checked on the fly: a layer's checks
   report unsatisfied; an iteration whose every layer was satisfied
   before its updates ends the codeword), a fixed iteration limit. The
   codeword queue (slots in arrival order, the TMCC codeword at the
   frame's end) is provisioned for the worst case, all 120 codewords at
   the iteration limit, so the result of a codeword never depends on
   load: it is a function of its LLRs, the code, the schedule and the
   limit, as on the CPU.
8. **Dispersal and BCH.** The slot dispersal PRBS runs across the
   frame's valid slots in order (`SlotDispersal`), then the (k − 6)-bit
   BCH word (header + data + 192 parity): syndromes S₁ … S₂₄ computed
   bit-serially as the word streams (24 GF(2¹⁶) multipliers by constants:
   LUTs), Berlekamp–Massey (24 steps of up to 12 GF multiplies, a small
   sequential unit), Chien search over the word's positions (one GF
   multiply by a constant per position per coefficient, bit-serial over
   the word's second pass out of its buffer). One codeword every 275 µs
   at most; the word's second pass is the slot block leaving the FPGA.
   The TMCC's BCH is the same unit (9 614 bits).
9. **Packer.** The `Slots` blocks, the CRC-32C network reused from the
   front end's packer, into the ADC DMA in place of the `Symbols`
   words (the mux `SymbolsRx` already has, one more selection).

### The LDPC schedule contract (model = HDL)

`isdb_fec::ldpc::fixed` gets a `Schedule`:

- `Schedule::Sequential`: checks 0 … P − 1 one at a time, each reading
  the a-posteriori values the previous check wrote: today's decoder
  exactly (kept as the CPU's fast path; the lane tests stay).
- `Schedule::Layered { step: usize }`: the P checks ordered **layer by
  layer** (layer l = the checks c with c mod q = l, l = 0 … q − 1, each
  layer's checks in increasing c), each layer cut into **steps of
  `step` consecutive checks** (the last step of a layer shorter).
  **Amended 2026-11-01 (delta write-back; the first text, "the later
  check's value wins", is in the Results below as the reason):**
  - Within a step every check reads L and r as they were at the step's
    start and computes t_e = sat(L_v − r_e,old) and r_e,new as before;
    r is written per edge.
  - At the step's end, for each variable v with edges e₁, …, e_m in the
    step (m ≥ 2 only for a conflicting pair; edges in check order):
    L_v ← sat( t_e₁ + r_e₁,new + Σ_{i=2..m} (r_eᵢ,new − r_eᵢ,old) ), the
    sum taken in a wide integer (i16 is enough: |terms| ≤ 127 each,
    m ≤ 3) and saturated to i8 once at the end. With m = 1 this is
    exactly sat(t + r_new), so single-edge variables are bit-identical
    to the sequential kernel.
  - Flooding inside a step, layered between steps. The stopping rule
    and the result are otherwise the sequential decoder's. With
    `step` = 1 this is a sequential decoder in layer order.

**HDL constraint (2026-11-01):** the L memory has exactly S lanes with a group's 374 positions in 374 / S words, so **S must divide 374: 11, 17, 22, 34 or 187**; the CPU model runs any step. The vector sets are therefore at steps 17, 34 and 187 (34 the design point if 187 does not fit the LUT budget).

The HDL implements `Layered { step: S }` with S check-node units that
read before any of the step writes (a pipeline with a step barrier or
forwarding, the designer's choice as long as the values are the
contract's), and the step's L writes as the delta rule (a variable's
later edges in the step add their message change to the first edge's
t + r_new: a read-modify-write of L, or the pair's terms merged before
the write). S divides 374 or not; the model does not care. The schedule
is part of the vectors and of the ID word's feature bits, so a host can
tell which decoder it is talking to. Expected: the layered schedule
converges in about the same iterations as the sequential one (both are
serial-C schedules) and is measured on the recordings before the HDL
is written (`examples/ldpcfixed`, with the failures at iteration limits
5, 8, 10, 12, 20, 50, so the limit can be chosen with evidence).

Throughput with S units at clock f, cycles per step ≈ (largest check
degree in the layer + pipeline depth ≈ 6), steps per iteration ≈ P / S:

| S | f | 93/120 cycles per iteration (P = 10 098, degree 16) | iterations per frame budget (33.05 ms) | per codeword, 120 codewords |
|---|---|---|---|---|
| 34 | 100 MHz | 297 steps × 22 ≈ 6 500 | 508 | 4.2 |
| 68 | 100 MHz | 149 × 22 ≈ 3 300 | 1 000 | 8.3 |
| 68 | 200 MHz | 3 300 | 2 000 | 16.7 |
| 187 | 100 MHz | 54 × 22 ≈ 1 200 | 2 750 | 22.9 |

(41/120 has 2.9× the checks but degree 5: about 1.4× the cycles.) The
target is a fixed limit of 10–12 iterations with the worst case met,
so S = 68 at 200 MHz, or S = 128–187 at 100 MHz; the Yosys estimate of
one check-node unit decides. Memory per codeword: L 44 880 × 8 bits
(10 RAMB36 as one 8-bit-wide bank per 374-group would be 120 BRAMs: the
layout is instead S banks addressed by group and bit; the designer
chooses), messages compressed per check (min1, min2, index, parity:
19 bits, plus one sign per edge): 709 kbit for 41/120 (20 RAMB36),
353 kbit for 93/120. Double-buffered input (the demapper writes the
next codeword while one decodes): 20 RAMB36 more. **Budget for the
decoder: ≤ 14 000 LUT, ≤ 50 BRAM36, 0 DSP.**

### Budget for the whole chain

| Stage | LUT | BRAM36 | DSP48E1 |
|---|---|---|---|
| sync, carrier loop, slot phase | 3 000 | 9 | 8 |
| equaliser FIR + training sums | 2 500 | 1 | 44 (22 at 2×) |
| TMCC soft bits, demapper, de-interleave, constellation and noise accumulators | 3 500 | 2 | 16–64 |
| LDPC decoder | 14 000 | 50 | 0 |
| dispersal, BCH, packer, registers | 3 000 | 4 | 0 |
| **total** | **26 000** | **66** | **68–116** |

Against 29.6k LUT / 72 BRAM36 / 62 DSP free in the deployed image, or
35.5k / 107 / 80 in a receive-only image. The DSP budget forces either
the receive-only image (no `SymbolsTx`, no `maia_sdr`) with the
demapper at a 2× or 4× clock, or moving the front end's `k` products
to the fabric. The decision is taken on the Yosys estimates of the
stages, not now. Timing: the PlutoSky R2 image has +0.100 ns on the
RGMII input whatever we add; the blocks' own clocks had > 0.8 ns.

### Host side

As built (piece 5, 2026-11-02; nothing run on hardware: no image has the
chain yet):

- **Discovery.** Bits 11–8 of the front end's ID word (0 on every front
  end so far) carry the chain's `Slots` format version
  (`isdb_frontend::control::chain::slots_version`):
  `isdb_radio::Offload::from_rx_id` gives `Offload::Symbols` and, with a
  version there, `Offload::RxSlots { version, taps, slots_version }`. The
  Pluto's `offloads` is still the one `gpio_in` read.
- **Registers** (`isdb_frontend::control::chain`, one file; **every
  address is a placeholder** for pieces 4 and 6 to fix): the chain sits
  in the front end's command address space at 0x400–0x6FF.

  | Address | Register | Host |
  |---|---|---|
  | 0x400 | VERSION (ro): `Slots` format version | checked at open |
  | 0x401 | FEATURES (ro): bit 0 TAPS (host may write taps), 1 SUMS (training sums readable), 2 CONSTELLATION, 3 N0 (may load at START); bits 15–8 LDPC iteration limit, 27–16 LDPC step | gates the open and the slow loop |
  | 0x402 | CARRIER: initial value before START; reads the register in use (2⁻³² turn / symbol, signed) | written at open (0), read each slow-loop tick (informational) |
  | 0x403 | LAYOUT_LAG (1 or 2) | written at open (2) |
  | 0x404 | INIT: bit 0 taps, 1 constellation, 2 n0: what START loads from the host | written at open |
  | 0x405 | STATUS (ro): bit 0 locked, 1 framed, 2 decoding | read each tick (logged) |
  | 0x406 / 0x407 | FRAME / SEQ (ro): last frame counter, next block sequence | — |
  | 0x408 | N0 (Q.20): initial value; reads n0 in force | written at open when the host has one |
  | 0x409 / 0x40A | LLR_SCALE / LEARNED_FROM (ro) | — |
  | 0x40B | CONST_MASK: modulations whose CONST points are loaded | written with the points |
  | 0x410 + k | TAPS, k < 11, re \| im << 16 (Q2.14): the next taps; reads the taps in use | slow loop, and at open with INIT bit 0 |
  | 0x41B | TAPS_FROM: written after TAPS, commits them (loaded at the next frame start); reads the frame the taps in use came from (`0xFFFF_FFFF`: none) | slow loop |
  | 0x420 / 0x421 | SUMS_FRAME / SUMS_COUNT (ro): the latched training sums' frame (`0xFFFF_FFFF`: none) and symbols | slow loop |
  | 0x430 … 0x437 | COUNTERS (ro): frames, undecoded, TMCC failed, slots, LDPC failed, BCH failed, iterations low, high | read at the end of a capture |
  | 0x500 + 4i | SUMS_R, i < 66: re low 32, re high 16 sign-extended, im low, im high | slow loop (264 reads) |
  | 0x610 + 4i | SUMS_P, i < 11, likewise | slow loop (44 reads) |
  | 0x640 + 32m + t | CONST: point t of modulation m (π/2-BPSK … 32APSK), re \| im << 16 (Q.10); reads the points in force | at open when the host has them |

  CTRL bit 3 (`control::ctrl::SLOTS`, with SYMBOLS) sends the chain's
  `Slots` words to the DMA instead of the symbols; the front end's
  STATUS then reads bit 2 (`chain::STATUS_SLOTS`) as well.
- **`isdb-radio`** (`isdb_radio::chain`, device-neutral over a
  `RegisterAccess` trait; tested against a model of the command decoding
  and these registers): **open** = CTRL 0; VERSION and FEATURES read
  (another `Slots` format is `RadioError::Chain`); the front end's
  configuration as for symbols with KP read back; CARRIER and
  LAYOUT_LAG; the host's taps / constellation / n0 when it has them and
  FEATURES allows (`ChainInit`; the CLI passes none, so the chain starts
  on identity taps, the nominal points and the default n0, and what an
  image refuses is warned); INIT; CTRL START, then RUN | SYMBOLS | SLOTS;
  STATUS must read run, symbols and slots; a failed step sends CTRL 0.
  **Slow loop** (`SlowLoop`, `spawn_slow_loop`): a timer thread with its
  own libiio context (the stream's context stays on the capture thread)
  polls every 200 ms until the first solve, then every 2 s: SUMS_FRAME,
  SUMS_COUNT, CARRIER, TAPS_FROM and STATUS; for a frame not yet
  solved, the 308 sum registers, SUMS_FRAME again (a moved latch is a
  torn read, retried at the next poll), `isdbs3_phy::chain::eq::
  solve_training` (the model's `f64` solve and Q2.14 rounding), the 11
  taps, TAPS_FROM. A failed access ends the loop (warned, the taps in use
  stay). **`Backend::capture_slots`** (the Pluto's): the same `voltage0`
  16-bit DMA path as `capture_symbols`; at the end the counters and CTRL
  0. The carrier register is read and reported only: the FPGA tracks
  the carrier itself, the host's front-end NCO stays at the
  acquisition's offset.
- **`isdb-cli`** (`slotrx.rs`): one `Consumer` takes `Slots` reader
  events to today's TLV assemblers and outputs (`rxs3::push_slot`): slot
  blocks of a frame are assembled once its tmcc (or status) block
  arrives, the stream ids from the TMCC whose `applies_from` covers the
  frame; the assemblers close at a loss and wherever the decoded frames
  are not consecutive. Three callers: `isdb rx --slots FILE` (a recorded
  words file, `system` ISDB-S3 implied); `isdb rx --symbols FILE --chain
  fixed [--slots-out FILE]` (the model in process, its frames through
  `Writer` and `Reader`); `isdb rx --radio … --system isdbs3 --offload
  auto|fpga [--slots-out FILE]` (`auto` prefers slots over symbols over
  the CPU; `fpga` needs the front end and takes slots when offered).
  The JSON summary gains `decoded_by` and `chain`: the block counts,
  losses, undecoded and missing frames, discontinuities, the carrier
  register and its range, and the last status block (n0 and MER, taps
  and `taps_from`, sync statistics, the chain's counters), plus the
  radio's open / slow-loop / counters. Not built: `isdb slots --radio
  --out` (recording is `isdb rx … --slots-out` instead).

### Verification ladder

1. Model tests: each stage's fixed-point model against the floating-
   point receiver on the recordings (sync offsets equal, phases within
   the quantisation, equaliser taps within 1 LSB when solved from the
   fixed-point sums, LDPC failures and iterations per schedule, every
   slot's bytes equal where both converge); `prop_` tests that any
   chunking of the stream gives the same words; the layered decoder's
   lane equivalence as today.
2. HDL simulation (`mise run fpga:test`) bit-exact on the vectors of
   every stage and of the chain: synthetic (the models' generators) and
   the recordings (opt-in, as the front end's).
3. Yosys estimate per stage against the budget; Vivado build; the final
   checkpoint's timing (`timing.tcl`) and utilisation (`utilization.tcl`).
4. On the board (receive only, nothing transmitted): the ID word; a
   `Slots` capture against the model run on a simultaneous `Symbols`
   capture of the same signal is impossible (one DMA), so: a `Slots`
   capture's slot bytes against `isdb rx --symbols` of a `Symbols` capture
   taken just before, compared per TLV packet (the streams are
   continuous and the same content repeats in the SI tables); then the
   TLV streams of a 60 s run through `isdb tlv inspect` and the
   `isdb-mmt` recordings test; the host's CPU time per second of signal,
   which is the figure this plan is for.

## Work split (each piece its own worktree, one owner, contract above)

1. **`isdb-fec`: the layered schedule.** `Schedule` in `ldpc::fixed`,
   the sequential behaviour unchanged bit for bit, the layered one as
   contracted, `decode_all` lane equivalence for both; the shortened
   TMCC code through it; `examples/ldpcfixed` (or a new example) prints
   failures and mean iterations per schedule and per iteration limit on
   the recordings; a vectors writer (`isdb-fec` example or `isdbs3-phy`
   `examples/ldpcvectors`): JSON with the code (rate), schedule, limit,
   offset, input LLRs, and per codeword the expected bits, iterations,
   convergence, and the hard decisions after every iteration (for
   debugging a mismatch), in `fpga/build/ldpc-*.json`; `mise run
   fpga:vectors` writes them. **Status: done (2026-11-01), with the
   contract amended to delta write-back; results below. Recommended:
   step 187 at 100 MHz, limit 20 (step 374 if the units fit).**
2. **`fpga`: the LDPC decoder HDL.** `isdb_fpga/ldpc.py`: the code
   tables as ROM (the 11 rates' entries: 1 015 entries × 15 bits, the
   layer order computed at elaboration), S check-node units, the
   compressed message memory, the L memory, the input double buffer
   with a write port for the demapper's (position, i8) stream and the
   known-zero fill for the shortened TMCC code, the output as hard bits
   of the first k positions in order, the schedule of the contract,
   early stop and the fixed limit; simulated against piece 1's vectors
   (a Python stand-in model of the contract first, as `txmodel.py` was
   for the transmitter); `synth/estimate.py` for it; a table of S and
   clock against the estimate. **Status: in progress (2026-11-01).**
3. **`isdbs3-phy`: the streaming fixed-point chain model** (stages
   1–6, 8 and the `Slots` format): a `chain` module with one struct per
   stage, each a push-style fixed-point state machine with the HDL's
   word widths stated in its docs, the one-frame-lag rules above, the
   host-side equaliser solve, the `Slots` writer and reader, a vectors
   writer per stage and for the chain, `isdb rx --symbols --chain fixed`
   (or an example first) decoding the FPGA recordings through it with
   the slot bytes compared to `rx::receive`'s; the SPEC-NOTES entry on
   TMCC timing. **Status: model, format, checks and vectors done
   (2026-11-02, "Results" below); `isdb rx --symbols --chain fixed` not
   done.**
4. **`fpga`: stages 1–6 and 8–9 HDL** against piece 3's vectors, one
   block per stage, then the chain top (`SlotsRx`) sharing the front
   end's control and packer; Yosys estimates against the budget.
5. **Host path** (`isdb-radio`, `isdb-cli`): the slots offload, the slow
   loops, the CLI, the JSON, docs (receiving.md, fpga/README.md,
   isdb-frontend README). **Status: built and tested off the board
   (2026-11-02, "Host side" above and the results below), with the
   chain's register map as placeholders; `isdb rx --symbols --chain
   fixed` done.**
6. **Image**: `apply.sh` mode `isdbs3-rx` (receive only), build, timing,
   deploy on a card copy, the board checks of the ladder's step 4,
   VERIFICATION.md.

Pieces 1–3 are independent and run in parallel; 4 needs 3's vectors
(and can start on the sync stage from 3's first commit); 5 needs 3's
format; 6 needs all.

## Results

### Piece 1: the layered schedule on the recordings (2026-11-01)

`isdb_fec::ldpc::fixed::Schedule` with the amended contract (delta
write-back). `Sequential` is today's decoder bit for bit: `ldpcspeed`
on BS7, one thread pinned to one core, 4 690–4 840 cw/s before the
schedule existed and 4 761–4 842 with the delta rule. `isdbs3-phy`
`examples/ldpcsched` decodes every codeword of the six recordings (BS7,
BS11, BS17 at 61.44 Msps, 2026-10-20; the same at 40 Msps from the live
path, 2026-10-25), quantised as `Ldpc::FIXED` (input scale 1, offset
1): 18 336 codewords of 16APSK 93/120. A decode's path does not depend
on the limit, so every limit is read off one run at 50 (checked against
a direct run at 30).

**As received, nothing fails.** At any limit from 5 to 50 and any
schedule, 0 of 18 336, all converged within 4 iterations. Mean
iterations: sequential 2.072, layered 1 2.052, 34 2.053, 68 2.053, 187
2.056, 374 2.060. (The demapper's 1/N0 is 14.8–15.6 dB on these.
VERIFICATION.md's 5 of 9 216 failures on the live recordings were
measured on 2026-10-25, before the carrier fixes of 2026-10-26; today's
chain gives cleaner codewords and these recordings no longer test the
decoder.)

**Near threshold** (`ldpcsched --noise V`: complex Gaussian noise of
variance V added to the main symbols before demapping, N0 measured on
the noisy symbols as the receiver does; same 18 336 codewords). Failed
codewords per limit, and mean iterations at limit 12, **with the delta
rule**:

| 1/N0 | Schedule | 5 | 8 | 10 | 12 | 16 | 20 | 30 | 50 | mean at 12 |
|---|---|---|---|---|---|---|---|---|---|---|
| ≈ 13.0 dB (V 0.03) | sequential | 14 174 | 1 405 | 210 | 42 | 4 | 2 | 1 | 1 | 6.48 |
| | layered 1 | 13 706 | 1 324 | 206 | 41 | 4 | 2 | 1 | 1 | 6.42 |
| | layered 34 | 13 738 | 1 338 | 208 | 47 | 3 | 2 | 1 | 1 | 6.43 |
| | layered 68 | 13 878 | 1 388 | 210 | 44 | 3 | 2 | 1 | 1 | 6.45 |
| | layered 187 | 14 109 | 1 482 | 226 | 47 | 4 | 2 | 1 | 1 | 6.49 |
| | layered 374 | 14 412 | 1 579 | 233 | 49 | 5 | 2 | 1 | 1 | 6.54 |
| ≈ 12.8 dB (V 0.035) | sequential | 18 303 | 9 739 | 5 210 | 3 076 | 1 229 | 669 | 346 | 242 | 9.10 |
| | layered 1 | 18 300 | 9 538 | 5 136 | 3 044 | 1 209 | 681 | 336 | 240 | 9.05 |
| | layered 34 | 18 300 | 9 572 | 5 160 | 3 049 | 1 221 | 688 | 341 | 242 | 9.06 |
| | layered 68 | 18 303 | 9 696 | 5 195 | 3 098 | 1 244 | 682 | 338 | 247 | 9.08 |
| | layered 187 | 18 314 | 9 873 | 5 341 | 3 166 | 1 271 | 691 | 345 | 244 | 9.13 |
| | layered 374 | 18 313 | 10 125 | 5 462 | 3 250 | 1 307 | 710 | 349 | 241 | 9.19 |

The waterfall is steep: 0 failures at 15 dB, 2 at 13.0 dB with limit 20,
most at 12.6 dB. With delta write-back **every step from 1 to 374
decodes as well as the sequential decoder**: the same failures at
limits 20–50 (2, 1, 1 at 13.0 dB; 669–710, 336–349, 240–247 at 12.8 dB)
and within a few per cent at 10–16, where a wider step costs at most
0.1 iteration on average.

**Final recommendation: step 187 at 100 MHz with a limit of 20** (22.9
iterations of budget per codeword at 120 codewords, so 20 is met with
margin; 2 failures of 18 336 at 13.0 dB, the sequential decoder's 2),
**or step 374 if the check-node units fit** (a whole layer per step,
twice the throughput: the same 2 failures at limit 20). Below 20 the
cost is gradual (limit 16: 4 failures at step 187, sequential 4; limit
12: 47 against 42).

**Why the contract was amended: the first rule.** The first text of
the contract had the later check's sat(t + r) overwrite the earlier
one's for a variable two checks of a step share. Measured on the same
codewords (`ldpcsched` before the amendment), failed codewords:

| 1/N0 | Schedule | 5 | 8 | 10 | 12 | 15 | 20 | 30 | 50 | mean at 12 |
|---|---|---|---|---|---|---|---|---|---|---|
| ≈ 13.0 dB (V 0.03) | sequential | 14 174 | 1 405 | 210 | 42 | 8 | 2 | 1 | 1 | 6.48 |
| | layered 1 | 13 706 | 1 324 | 206 | 41 | 9 | 2 | 1 | 1 | 6.42 |
| | layered 34 | 14 127 | 1 548 | 244 | 52 | 11 | 2 | 2 | 1 | 6.51 |
| | layered 68 | 15 068 | 2 127 | 376 | 102 | 20 | 3 | 2 | 1 | 6.73 |
| | layered 187 | 16 353 | 3 483 | 911 | 297 | 94 | 33 | 16 | 11 | 7.16 |
| | layered 374 | 17 384 | 5 456 | 2 347 | 1 053 | 469 | 246 | 169 | 143 | 7.82 |
| ≈ 12.8 dB (V 0.035) | sequential | 18 303 | 9 739 | 5 210 | 3 076 | 1 482 | 669 | 346 | 242 | 9.10 |
| | layered 1 | 18 300 | 9 538 | 5 136 | 3 044 | 1 474 | 681 | 336 | 240 | 9.05 |
| | layered 34 | 18 312 | 10 065 | 5 551 | 3 382 | 1 754 | 825 | 433 | 314 | 9.18 |
| | layered 68 | 18 325 | 11 329 | 6 374 | 4 063 | 2 248 | 1 147 | 636 | 451 | 9.47 |
| | layered 187 | 18 334 | 13 567 | 8 481 | 5 925 | 4 034 | 2 692 | 1 927 | 1 663 | 10.05 |
| | layered 374 | 18 335 | 16 066 | 11 821 | 8 927 | 6 817 | 5 528 | 4 817 | 4 581 | 10.79 |
| ≈ 12.6 dB (V 0.04) | sequential | 18 336 | 17 991 | 15 802 | 12 939 | 9 912 | 7 476 | 5 941 | 5 150 | 11.56 |
| | layered 68 | 18 336 | 18 190 | 16 806 | 14 396 | 11 483 | 8 948 | 7 243 | 6 514 | 11.72 |
| | layered 374 | 18 336 | 18 335 | 18 249 | 17 877 | 17 096 | 16 083 | 15 281 | 14 929 | 11.98 |

With "later wins", layered steps up to 34 were as good as sequential, 68
cost a little, and 187 and 374 **showed an error floor** (11 and 143
failures at limit 50 at 13.0 dB where sequential has 1; 1 663 and 4 581
against 242 at 12.8 dB). `examples/ldpcconflict` (3 000 of the live
codewords, scalar decoders; the delta rule checked equal to the
library's decoder on every codeword) compares the rules on the same
reads and steps, and with steps cut wherever a check would share a
variable with an earlier one of its step (no conflicting pair inside a
step). Failed at limit 12 / 50:

| V | Rule | step 34 | step 68 | step 187 | step 374 |
|---|---|---|---|---|---|
| 0.03 | later wins (first contract) | | 41 / 0 | 106 / 4 | 365 / 54 |
| | delta (amended contract) | | 16 / 0 | 17 / 0 | 18 / 0 |
| | split steps, later wins | | 14 / 0 | 14 / 0 | 14 / 0 |
| 0.035 | later wins (first contract) | 1 104 / 107 | 1 303 / 154 | 1 811 / 554 | 2 426 / 1 441 |
| | delta (amended contract) | | 1 025 / 87 | 1 035 / 88 | 1 065 / 85 |
| | split steps, later wins | 997 / 87 | 997 / 87 | 997 / 87 | 997 / 87 |

Under "later wins" the other check's update to a shared variable is
lost, every iteration, at 25 pairs × up to 374 positions per layer of
93/120. The delta rule restores it; in hardware it is a read-modify-write
of L with an adder (or the two writes of a pair merged before the RAM),
not more memory. Measured first was the variant sat(L_v + Σ over all the
variable's edges of (r_new − r_old)); the contract's formula starts from
the first edge's t = sat(L_v − r_old), and the two differ only when
that t saturates. On these 3 000 codewords at V 0.03 and 0.035, steps
68, 187 and 374, they gave identical counts (the table's delta rows are
the contract's formula). (Rates other than 93/120 were not measured on
recordings: the lab transponders carry only 93/120; 109/120 has 26
conflicting pairs in 11 layers, 105/120 39 in 15, with up to three
entries of one row in one layer in 81/120, 89/120 and 105/120, so
m ≤ 3.)

### Piece 1: vectors (2026-11-01)

`mise run fpga:vectors` writes `fpga/build/ldpc-tables.json` (21 kB)
and nine sets, 0.45–1.3 MB each (`fpga/README.md` "LDPC vectors";
format in `isdbs3-phy` `examples/ldpcvectors.rs`), plus two from a
recording with `ISDB_RS_ISDBS3_TLV_IQ`, all under the amended contract.
`tools/ldpc-vectors-check.py`, an independent pure-Python decoder built
from the tables file, reproduces all eleven sets (68 codewords, every
bit, iteration count, unsatisfied count and trace), and rejects the
vectors written under the first rule.

## Tools added for this plan

- `tools/ldpc-layer-conflicts.py`: the layer structure of the eleven
  codes (q, P, edges, largest layer degree, conflicting pairs and their
  shift differences).
- `isdbs3-phy` `examples/ldpcsched` (failures, mean iterations and
  histogram per schedule and limit, `--noise` to move the operating
  point), `examples/ldpcconflict` (the conflict rules compared),
  `examples/ldpcvectors` (the FPGA vectors), and
  `tools/ldpc-vectors-check.py` (the vectors against an independent
  Python decoder).
- `fpga/tezuka/utilization.tcl`: whole-design utilisation by hierarchy
  of a routed checkpoint (the table under "Capacity").
- `tools/slots-vs-symbols.sh` (piece 5): `isdb rx --symbols` against
  `--symbols --chain fixed --slots-out` and `--slots` of the words
  written, then `tools/tlv-packet-diff.py` (TLV packets of two runs
  aligned per stream id, the differing runs located).
- The measurements above are reproducible with the existing
  `isdbs3-phy` examples `streamprof` and `rxparts` under `taskset`, and
  `isdb rx --symbols` under `/usr/bin/time -v`.

## Results

### Piece 3: the fixed-point chain model (2026-11-02)

`isdbs3_phy::chain` (module docs: every width and rule),
`isdb_frontend::slots` (format version 1), examples `chaincheck`,
`chainsync`, `chainstages`, `chainiq`, `chainprof`, `chainvectors`.
Decisions the HDL and the integrator need:

- **Layout lag 2, not 1.** Frame N's TMCC codeword is complete at its
  last symbol; frame N + 1's first main symbol leaves the equaliser 56
  symbols later, so lag 1 needs the TMCC LDPC + BCH done in 56 symbols or
  a slot of buffering. B44 §3.11 ("switching information two frames
  prior") reads as the TMCC of frame N describing frame N + 2, which
  gives the FPGA a whole frame. Lag 1 stays selectable
  (`ChainOptions::layout_lag`); the SPEC-NOTES entry "tmcc-timing" is
  pending real-world check (no switch observed).
- **Equaliser after the slot correction** (stage 3 → 4, as this plan
  orders them), on Q.10 symbols; the float receiver equalises before.
  Taps solved from frame N are in use from frame N + 2; the host may
  write taps at open.
- **TMCC soft-bit scale** 45 per unit amplitude (`>> 5` of the Q.10
  projection), known zeros +127.
- **Demapper** = the float demapper's scale exactly (n0 in Q.20, K =
  round(2³² / n0)), so the main LDPC sees the same `i8` LLRs as today.
- Lock 4 periods, loss 8 periods, frame marker from a ±8 sign counter
  with |μ| ≥ 2, two missed markers lose the frame; a loss or lost sync
  restarts everything but the carrier register, taps, constellation and
  n0.

| Stage | Agreement with the float receiver (BS7 FPGA recording unless stated) |
|---|---|
| 1 slot sync | same slot offset and SSync parity as `s3sync::measure` on six IQ recordings (BS7/11/17 at 61.44 and 40 Msps); every head and frame head among `sync::synchronise`'s (1 796 heads, 15 frame heads on the FPGA recording) |
| 2 carrier loop | followed the recording's −38 … −989 Hz drift (the host's tracking −0 … −989 Hz); synthetic −22 Hz/s and −2 kHz/s followed with the predicted lag (2·D·33 ms) |
| 3 slot phase | head carrier phase within 3.4 mrad (1.7 rms), amplitude within 0.13 % |
| 4 equaliser | integer sums exact; taps rounded within 0.68 LSB of Q2.14; within 6.3 LSB (2.3 rms) of `eq::train`, normalised |
| 5 TMCC | soft-bit signs 100 % equal; decoded TMCC identical (85 / 85 frames) |
| 6 demapper | 99.996 % of LLRs equal, max difference 1 |
| whole chain | 1 207 frames; 115 296 of 115 296 slots byte-identical with `rx::receive` (through the Slots words); 0 LDPC / 0 BCH failures on both; 2.095 vs 2.081 mean iterations; 6 frames undecoded = 2 per (re)start (lag 2) |

`Slots` words for 16APSK 93/120: 12.2 MB/s of signal (the plan's 12.5).
CPU: the model's stages 1–6 and 8 take 4.9 s per second of signal on
one thread (LDPC apart, 0.12 s per s on all cores): a bit-level
reference, not a host fast path. Vectors: `chain-{1-sync,2-phase,3-eq,
4-tmcc,5-demap,6-chain}.json` (synthetic, `mise run fpga:vectors`) and
`chain-rec-*.json` from the FPGA recording (opt-in).

Left: `isdb rx --symbols FILE --chain fixed` (the CLI path; `chaincheck`
does the comparison meanwhile); the model's speed; the HDL (piece 4).

### Piece 5: the host path against the host receiver (2026-11-02)

`tools/slots-vs-symbols.sh ~/isdb-captures/2026-10-26-fpga/bs7-fpga-40s-drift.sym`
(release build, this host, 32 threads):

| Run | Decoded by | Frames | Slots | LDPC / BCH failed | Mean iterations | TLV packets B070 / B071 / B072 | Wall | User CPU | Max RSS |
|---|---|---|---|---|---|---|---|---|---|
| A `--symbols` | host receiver | 1 207 | 115 872 | 0 / 0 | 2.081 | 121 919 / 121 101 / 115 301 | 34.5 s | 380.6 s | 3.9 GB |
| B `--symbols --chain fixed --slots-out` | the chain's CPU model, through `Slots` | 1 207 (6 undecoded) | 115 296 | 0 / 0 | 2.095 | 121 330 / 120 490 / 114 708 | 208.8 s | 225.2 s | 62 MB |
| C `--slots` (B's words, 504 MB) | recorded `Slots` words | 1 207 (6 undecoded) | 115 296 | 0 / 0 | 2.095 | 121 330 / 120 490 / 114 708 | 3.2 s | 3.2 s | 41 MB |

`tools/tlv-packet-diff.py`, per stream id (packets aligned on their bytes):

- **A against B: every packet of B is a packet of A at the same place**
  (121 330 + 120 490 + 114 708 equal, 0 packets only in B, 0 replaced).
  A has 589 / 611 / 593 more, in **3 runs per stream, each exactly
  275 264 bytes**, at 0 %, 25.4 % and 50.8 % of the stream: the start,
  and the two losses in the recording's `Symbols` words (86 and 32
  blocks missing at 10.16 s and 20.36 s), where the model restarts. 275
  264 bytes = 2 frames × 32 slots of the stream × 4 301 data bytes of
  93/120: the 2 frames the chain does not decode after each (re)start
  (layout lag 2; 6 frames, 576 slots = 115 872 − 115 296). Both sides
  are discontinuous at the two losses; B's TLV assemblers restart 2
  frames later than A's, and resynchronise on the next head-TLV
  indication with nothing in between differing. **All differences are
  the chain's undecoded frames.**
- **B against C: identical** (every packet, every byte): the `Slots`
  words of the model as written and read back from the file give the
  same TLV streams as the in-process path, so `--slots` (and a radio's
  chain through the same consumer) decodes what the model decodes.

The model's 1 207 frames are numbered 0 … 1 209 (3 numbers skipped at
the two restarts: the frames the losses cut); its carrier register
followed −38 … −1 006 Hz and ended at −758 Hz (the host receiver's
tracked drift −0 … −989 Hz, −767 Hz at the end); n0 at the end 35 583
(MER 14.69 dB; the host's mean main MER 14.6 dB); taps solved every 30
frames, the last from frame 1 202.

**Host CPU.** C is what a host does behind the FPGA's chain: **3.2 s of
CPU for 40 s of signal, 0.08 cores** (Slots parsing, CRC, TLV assembly,
the per-stream statistics and the file writes), against 380 s (9.5
cores) for A. B's 4.96 s per second of signal is the CPU model of the
FPGA's arithmetic (one thread for stages 1–6 and 8, the LDPC on all
cores), a bench check and not a way to receive. The host side has not
run against a radio: no image has the chain (pieces 4 and 6).
