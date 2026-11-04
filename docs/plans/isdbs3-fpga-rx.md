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
(`/path/to/captures/2026-10-26-fpga/bs7-fpga-40s-drift.sym`, 16APSK 93/120
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

(Version 2, 2026-11-04, adds a fourth block type, the sums block: a
decoded frame's training sums after its status block; "Sums in the
stream" in the results.)

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
   before its updates ends the codeword), a fixed iteration limit (a
   run-time register, default 16). The result of a decoded codeword is a
   function of its LLRs, the code, the schedule and the limit, as on the
   CPU; load decides only whether it is decoded. **Amended 2026-11-02:**
   the queue is not provisioned for all 121 codewords at the limit (that
   would cap the limit at 8 at S = 34 / 200 MHz, 1 338 failures of 18 336
   near threshold against 3 at 16) but for the frame average: K = 3
   L/H banks, because a codeword is complete only at its slot's end, so
   with K banks one codeword may take about (K − 1) slots (K = 2: 9
   passes, K = 3: about 19) while the frame budget is about 10 passes
   per codeword against a measured mean of 3.1; a codeword begun with all
   banks held is dropped, counted (`ldpc_overrun`) and reported in order
   as a slot not decoded (fpga/README.md "The LDPC decoder"). Not a
   symbol queue before the demapper (it would break the one-symbol-per-
   clock demapper), not limit 8. The TMCC stage stages its codeword
   across the frame and loads it as a burst after the frame's end (an
   interleaved TMCC load would hold a bank all frame and need K = 4).
   Cost: 76.5 BRAM36 at S = 34 with all rates, from the receive-only
   image's budget.
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

| Stage | LUT | BRAM36 | DSP48E1 | Fit (2026-11-02, Yosys; target LUT / BRAM36 / DSP) |
|---|---|---|---|---|
| sync, carrier loop, slot phase | 3 000 | 9 | 8 | 3 783 + 1 439 / 5.5 + 4 / 4 + 12 (3 500 / 5.5 / 6; — / 4 / —) |
| equaliser FIR + training sums | 2 500 | 1 | 44 (22 at 2×) | 2 322 / 3 / 15 (2 500 / 3 / 18) |
| TMCC soft bits, demapper, de-interleave, constellation and noise accumulators | 3 500 | 2 | 16–64 | 183 + 4 909 / 8 + 2.5 / 21 (4 500 / 3 / 22 for the demapper) |
| LDPC decoder (budget; **measured 2026-11-02: 14 034 LUT, 76.5 BRAM36 at S = 34, K = 3, all rates**, `fpga/README.md`) | 14 000 | 50 → 77 | 0 | 14 034 / 76.5 / 0 |
| TMCC staging buffer (31 680 × 8, in the TMCC stage, so the decoder keeps its 3 banks for slots) | — | 7 | — | (in the TMCC row: 8) |
| dispersal, BCH, packer, registers | 3 000 | 4 | 0 | 5 941 / 2 / 0 (3 500 / — / 0); + the result FIFO 2 BRAM36 |
| **total** | **26 000** | **66 → 100** | **68–116** | chain 19 122 / 25 / 52 (one nominal ROM); + bridge, FIFOs, registers: `SlotsRx` less the front end 20 873 / 27.5 / 52 (16 000 / 26 / 60) |

Against 29.6k LUT / 72 BRAM36 / 62 DSP free in the deployed image, or
35.5k / 107 / 80 in a receive-only image. **Decision (2026-11-02): the
chain goes into a receive-only image** (no `SymbolsTx`, no `maia_sdr`;
the stock image stays on the card for them): the decoder's three
codeword banks (17 BRAM36 each; two banks cap the usable iteration
limit at 8, which fails 1 338 of 18 336 near-threshold codewords where
16 fails 3) and the TMCC staging take the BRAM to about 100 of 107, and
the DSP budget wants the same image anyway, with the demapper at a 2×
or 4× clock, or the front end's `k` products moved to the fabric. The
remaining stage estimates (piece 4) decide the DSP side. Timing: the PlutoSky R2 image has +0.100 ns on the
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
  FEATURES allows (`ChainInit`; the CLI passes taps only, always with
  INIT bit 0 since #28: those `isdbs3_phy::chain::probe::probe_taps`
  solves from the acquisition's IQ probe, if its sums pass the slow
  loop's checks, else the process's last good set, else the identity
  (2026-11-04, "Build 9 residuals"); the nominal points and the default
  n0; what an image refuses is warned); INIT; CTRL START, then RUN | SYMBOLS | SLOTS;
  STATUS must read run, symbols and slots; a failed step sends CTRL 0.
  **Slow loop** (`SlowLoop`, `spawn_slow_loop`): a timer thread with its
  own libiio context (the stream's context stays on the capture thread)
  polls every 200 ms until the first solve, then every 2 s (on an image
  of `Slots` format 2 with FEATURES bit 7 SUMS_STREAM it reads no register:
  the consumer hands it each sums block, `ChainHealth::sums`, and the
  status blocks give it the frame, layout, lock and carrier; the first
  block wakes it at once: "Sums in the stream" in the results; otherwise,
  as built first): SUMS_FRAME,
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
  radio's open / slow-loop / counters and `start_taps` (where the taps
  at START came from). Not built: `isdb slots --radio
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
3. The checks of issue #30 that catch the board-found defect classes
   before hardware (fpga/README.md): value coverage of the vectors
   (item 1, `tools/value_coverage.py`, in `fpga:test`: a field constant
   across the corpus is a field no simulation checks, as build 7's n0);
   the invariant monitors in every simulation and the pipeline-delay
   mutation run (item 2, `chain/monitors.py`, `tools/pipeline_mutation.py`);
   the chaos runs of random stops, STARTs, losses and tap writes against
   the model restarted at the same points (item 3, `chain/chaos.py`,
   opt-in `FPGA_CHAIN_CHAOS`).
4. The timed host–FPGA model (item 4, `tools/hostfpga_model.py`, in
   `fpga:test`; fpga/README.md "A timed host–FPGA model"): the register
   map, the frame period and the measured iiod cost against the host's
   sequences, for any change to a register's semantics, the stream's
   blocks or the slow loop: every multi-word value read is one snapshot,
   polled live values are single words, no host sequence waits for ever,
   and the time to the first taps in frames. It reproduces build 6's torn
   reads, build 7's frame 26 and #27 / #29's foreign words.
5. Yosys estimate per stage against the budget; Vivado build; the final
   the recordings (opt-in, as the front end's). Part of it: the formal
   checks of the small control blocks (`mise run fpga:formal`: the LDPC
   bank allocator, the packer's sequence numbers, the frame boundaries;
   fpga/README.md "Formal checks").
3. Yosys estimate per stage against the budget; Vivado build; the final
   checkpoint's timing (`timing.tcl`) and utilisation (`utilization.tcl`).
6. On the board (receive only, nothing transmitted): the ID word; a
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
   VERIFICATION.md. **Status (2026-11-03): with the fit's HDL timing
   remedies (`7f70e26`) the image meets every constraint (WNS +0.045,
   WHS +0.011 ns; 82 % LUT, 123.5 BRAM36, 176 DSP), runs on the bench
   board as `isdbs3_rx_top.bin` and decodes BS7 through the Slots path
   (909 frames, 0 losses, 0.4 cores of host CPU); the equaliser's slow
   loop never wrote taps (the training sums move every frame, a read of
   308 registers takes about 15 frames), so the chain ran on identity
   taps: 268 LDPC failures of 87 177 slots, mean 4.63 iterations, where
   the CPU model with taps has 1 and 2.08. Build 7 (`d4042bb`,
   SUMS_CAPTURE) meets timing without repair and the taps arrive (first
   write 0.9 s after open, 12 in 30 s): 0 failures with good taps, but one
   solve of twelve made every codeword fail for 81 frames (7 808 failed),
   and n0 stays at its default on the board. Build 8 (`682032e`: n0
   learned, the status's points, TAPS_FROM, the tap gate) on the board:
   1 LDPC failure in 30 s, 2.15 mean iterations, MER 14.9 dB, 0 losses,
   0.41 cores, like the CPU model on the same signal. Results below,
   "Piece 6".**

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

### Fit: the chain next to the front end and the decoder (2026-11-02)

The receive-only image must hold the fixed part (≈ 17 700 LUT, 33 BRAM36,
140 DSP: `axi_ad9361`, the front end, DMAs and clocking), the decoder
(S = 34, K = 3: 14 043 / 76.5 / 0) and the chain: the chain's stages 1–9
had to come to ≤ 16 000 LUT, ≤ 26 BRAM36, ≤ 60 DSP48E1 from 29 744 / 38 /
160 (fpga/README.md, "The ISDB-S3 chain": every table and the clocking).

**Clocking.** A 4× of the symbol clock (`fast`): `axi_ad9361` l_clk itself
(160.0 MHz; the symbol clock is l_clk / 4, so the ratio and the phase hold
by construction), clock-enable phasing (the phase from a toggle of the
symbol clock, no FIFO), the block's l_clk paths constrained at their real
6.25 ns (`fpga/tezuka/isdb_slots.xdc`: ADI times rx_clk at 4 ns). The
decoder keeps 200 MHz from an MMCM on clk_fpga_0 behind the async bridge:
the queue model with the chain's pace (`ldpcqueue.run(finish_hz=40e6)`)
overruns 9 of 363 codewords at 160 MHz, 0 at 200 MHz, and 32APSK's
163 M LLR/s exceeds one LLR per 160 MHz cycle. The chain is valid at
40 Msps only. **Amended 2026-11-04:** the bridge's serialiser took
n + 1 decoder cycles for a beat of n values, so 32APSK with the TMCC
burst (four values a beat on the 7.4 M free cycles/s) needed 232.6 M of
the 200 M cycles/s and its input FIFO overflowed (chain-14-apsk32); a
beat now takes n cycles, back to back: 192.6 M (16APSK 160 M), and a
symbol clock's beat never needs more than its 5 decoder cycles
(`ldpcqueue.port_cycles`; fpga/README.md "Integration"). At 32APSK
109/120 the queue keeps K = 3 at the limit 14 (0 overruns of 363).

**Every stage stayed bit-exact** (the whole existing vector suite, the
whole-chain sets): the arithmetic is the model's; what changed is when
and on what it is computed.

| Block | LUT | BRAM36 | DSP48E1 | How |
|---|---|---|---|---|
| demapper | 14 348 → 4 919 | 7 → 2.5 | 95 → 21 | metrics in the 4× clock, 8 points per fast cycle; per-phase minima and running minima (the max-log minimum is order-free); K products time-shared; point sets and pilot accumulators in LUT RAM; one compact nominal ROM |
| equaliser + FrameCtl | 2 682 → 2 322 | 7.5 → 3 | 50 → 15 | 44 products on 11 DSPs in the 4× clock; the compact ROM; sign ROM in LUTs |
| slot phase | 1 122 → 1 439 | 8 → 4 | 8 → 12 | the ring holds the NCO's 6-bit input (three per 36-bit word), the NCO re-applied after it; Delta and Gamma on one bit-serial multiplier |
| finish / BCH | 7 301 → 5 941 | 2 → 2 | 0 | one Realign shared; the BCH roots flipped in the buffers; the locator in LUT RAM banks with rotating roles (+ the state a late TMCC result needs); the BCH in the 4× clock with 4-bit syndrome steps; one syndrome read and squarer; the packer's one CRC network |
| stage 1 | 4 194 → 3 783 | 5.5 → 5.5 | 7 → 4 | FSync / SSync trees shared (C = P ± Q); the lock rule by shifts; one multiplier for the FSync sign |
| TMCC soft bits | 134 → 183 | 8 → 8 | 0 | four values per burst beat (a fix: the burst held a decoder bank 15.6 slots, the queue model assumed 3) |

**The device is at the edge on LUTs**: fixed ≈ 17 700 + decoder 14 043 +
`SlotsRx` less the front end 20 873 = 52 616 LUT, 98.9 % of 53 200 (from
53 337, 100.3 %); 137 BRAM36 (97.9 %; the result FIFO's 2 included), 192
DSP (87 %). 94 % needs about 2 600 LUT more: the finish stage (5 941
against its 3 500: the BCH unit 2 276, its glue, the status block's word
mux, the layout, the packer, the Realign and the TMCC extraction, two
PRBS and root fixers), the demapper (4 909 against 4 500), stage 1
(3 783 against 3 500). The decoder at S = 22 is not a way out: 9 647 LUT
but 92 BRAM36 at K = 3 (152.5 of 140 in the device) and at K = 2 its
queue overruns at every limit (`fpga/tools/queue_sweep.py`); at K = 3 it
would also need the limit at 10 or less (9 overruns of 363 at 16, 200 MHz,
the passes before the decoder's pipelining).

**A corrected slot no longer holds the decoder.** The BCH (Chien over n,
3.1 slots at 93/120) held the result stream and a decoder bank; it now
runs in the 4× clock (`FastBch`, 8 694 symbol clocks of Chien) behind a
4 096 × 18 result FIFO (2 RAMB36; sized so the finish stage keeps up with
every 16APSK slot corrected, the FIFO then holding at most the next
result, 2 179 entries). chain-10-bch (eight consecutive slots with 1-12
bit errors injected into the decoder's words) is bit-exact with the real
decoder, 0 overruns, the FIFO's peak 1 426 entries.

**Integration.** The real decoder in the chain (`ldpc.LdpcDecoder` S = 34,
K = 3 behind the bridge, its own 5× domain, the input at the front end's
rate; Verilator, fpga/README.md "Simulation backends"):
**bit-exact** on chain-7-hdl (chain-6's input decoded by the model with the
decoder's settings) and on the loss and fade sets: every block of the
model's frames, 0 decoder overruns, no queue overflow. It found two HDL
defects, both fixed: the demapper tagged a frame's last codeword with the
next frame's counter (the finish stage then reset the slot PRBS, the BCH's
Chien search held the results 35 000 cycles and the decoder overran), and
an input loss deadlocked stage 1. Result
order: the decoder's commit order puts frame N's TMCC (and its trailing
dummies and status) after frame N + 1's first slot blocks; `Finish` keeps
their contents the model's, nothing is reordered, the host (`isdb-cli`
`slotrx.rs`) releases frames by frame counter within one frame (a unit
test: the FPGA order decodes the same as the model's); the HDL's bound is
the TMCC burst plus its decode, the next frame's first 26 slots at most
(4 at the front end's rate), measured 3. Loss and sync loss
(chain-8-loss, chain-9-fade, decoded with the decoder's settings): the
chain restarts or loses and regains sync and decodes frame 3 after the
event, bit-exact with the stub and with the real decoder. A frame the chain
drops after its first slots were decoded leaves those slot blocks without
its tmcc and status (the model sends nothing of it): the host discards
them (`orphan_slots`). `SlotsRx` takes the host's register map (three registers
added: MAX_ITER, AGC, TMCC_OVERRUNS; the first image offers the host's
taps and the sums, not a constellation or n0 at START, lag 2 only) and
`mise run fpga:slots-verilog` writes its Verilog.

**Vivado, build 4 (2026-11-03, the image piece, of 5c8b9eb).** It fits:
45 275 LUT (85 %), 119 BRAM36, 176 DSP; the chain 18 346 LUT (Vivado
about 7 % under Yosys). Setup failed (WNS −3.257 ns, 7 429 endpoints;
hold met); the chain's share and the remedies (each value unchanged,
every bit-exact test passing; `fpga/tools/vivado_paths.py` reads the
reports):

| Path (clock) | Slack, endpoints | Remedy |
|---|---|---|
| BCH: locator read → GF(2¹⁶) multiplier → locator write, 10 LUT levels (l_clk) | −3.257 ns, 812 | Berlekamp-Massey's product steps two fast cycles: operands registered, then the product used; the syndrome read registered for the steps that use it whole |
| symbol-clock toggle → `Phase4`'s combinational phase → BCH / demapper fast logic | −1.563 / −1.132 ns, 466 / 217 | the phase a fast register (a counter kept in step by the toggle registered twice in l_clk), plus the phase one fast cycle late |
| equaliser: the tap switch's 32-bit compare and selects → DSP A / B inputs (crossing), the phase → B inputs (l_clk) | −2.863 / −1.379 ns, 494 / 5 | the chosen taps and the line through fast registers, the 22 operands registered before the products (two fast cycles later, still inside the symbol clock) |
| demapper: the K product → rounding add → saturation → LLR (l_clk) | −1.201 ns, 110 | the rounding's half added with the products, so the last stage is a shift and the saturation |
| FastBch's inputs from the finish stage (crossing) | (in the 466) | through fast registers, the beat's chunks on the late phase |

The BCH's budget per corrected slot: 8 993 symbol clocks after the word
(8 855 before; only Berlekamp-Massey's steps doubled, the Chien search
is one position per fast cycle as before), so the result FIFO's account
holds (at most one result waiting at 16APSK with every slot corrected);
chain-10-bch with the real decoder: bit-exact, 0 overruns, the FIFO's
peak 1 426 entries of 4 096. Yosys: `SlotsRx` without the decoder
26 803 LUT (+51), 27 380 FF (+785), BRAM and DSP unchanged. The decoder's
own 200 MHz paths (`ldpc.py`) are the decoder's piece; `LdpcBridge`'s
input FIFO (188 endpoints at 200 MHz, −1.0 ns: Amaranth's AsyncFIFO, the
synchronised write pointer → empty compare → read address → 64-deep LUT
RAM, 211 bits wide → read register) is left for the next build: a
block-RAM FIFO or a registered read address would remove it (the
decoder's timing commit did: its FIFOs in block RAM).

**The iteration limit: 14 (2026-11-03).** With the decoder pipelined for
200 MHz (93/120: 6 125 cycles per pass, from 5 420) the queue at the
chain's pace (`ldpcqueue.run(..., finish_hz=40e6)`, the measured
distribution, its long codewords at the limit; `tools/queue_sweep.py`)
gives, of 363 codewords at 200 MHz:

| K | limit | overruns | with two adjacent long pairs | BRAM36 (Vivado 119 + the bridge's FIFOs in block RAM) |
|---|---|---|---|---|
| 3 | 16 | 9 | 15 | ≈ 123.5 |
| 3 | **14** | **0** | 6 | ≈ 123.5 |
| 4 | 16 | 0 | 6 | ≈ 140.5: over the device |

(K = 3 at limits 12 and 10: 0 as well; at 160 MHz, limit 14: 9.) So K = 3
and the default limit 14: `ldpc.DEFAULT_MAX_ITER`, the chain's MAX_ITER
reset, the model's `chain::ldpc::HDL_MAX_ITER` (`FixedLdpc::hdl`) and the
host's `control::chain::MAX_ITER_RESET` agree; the host may write
another limit (MAX_ITER), and K = 4 stays a build parameter (`banks`).
The sensitivity cost is small: at 13.0 dB the layered decoder fails 3 of
18 336 codewords at limit 16 and 47 at 12 (the piece-1 table), 14 between.
With it the regenerated decoder-settings sets (chain-7-hdl, -8-loss, -9-fade, -10-bch; no codeword of them needs more than 4 iterations) are bit-exact with the real decoder, and the FPGA suite and cargo test pass.

**Build 7's board defects (2026-11-03).** Four, all reproduced or
explained off the board:

- *n0 never learned* (52 556 and K 81 722 in every status block): the
  demapper cleared its noise sum at every frame start, also in the 12
  cycles between a decoded frame's last symbol and its noise division;
  on a gapless stream the next frame starts a cycle after the last
  symbol, so the division always saw a count of 0. No HDL set showed it:
  the stage-6 test's next frame had no layout, and every whole-chain set
  ends before a status block carries a learned n0 (all their status
  blocks read 52 556, the model's too). Reproduced on sets made from
  build 7's own Symbols capture (`chainvectors` on `before.sym`, the
  first 0.2 s: frames 3 and 4 read 52 556 against the model's 33 540 and
  32 891); fixed (the clear waits for the drain), K in time (`late` 0);
  `DemapTest.test_next_frame_starts_at_once` covers it.
  `fpga/tools/frame_events.py` prints the demapper's frame events.
- *The status block's learned points shifted* (found behind the first):
  the finish stage took each status word in the cycle the point RAM's
  address moved, so each point's re was the previous point's; the words
  now leave two cycles after `next`; `StatusPointsTest` covers it. With
  both fixes the whole chain on build 7's symbols (5 frames, 631 175
  words) is word-exact with the model.
- *`taps_from` one write behind*: `SlotsRx` armed the equaliser with the
  previous TAPS_FROM value; fixed, `SlotsRxRegisterTest` covers it.
- *One bad solve in twelve* (frame 250, centre 18 691 against 17 325):
  no capture-side cause found. The sums are of the equaliser's input
  (the taps in use do not enter them), each frame's bank is cleared at
  its start and latched whole at its end (6 720 symbols, as every other
  solve), the hold keeps the read bank out of the MAC unit's and the
  clear's way, and each of the 308 words re-reads the held entry. The
  host now logs every set of sums it solves (frame, count, a digest, R
  and p at the centre, the taps) and gates the taps it writes (isdb-radio
  `chain::Gate`: the normal equations' residual and the centre tap's
  magnitude, 5 % each, a lasting change taken at its second solve); on
  build 7's sequence it rejects frame 250's solve and writes the rest.

### Piece 5: the host path against the host receiver (2026-11-02)

`tools/slots-vs-symbols.sh /path/to/captures/2026-10-26-fpga/bs7-fpga-40s-drift.sym`
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

### Results, piece 3/5 on a live capture (2026-11-02)

The model and the host path on a fresh signal, not the 2026-10-26
recording they were developed against. Receive only (nothing
transmitted, nothing changed on the board): PlutoSky R2 at
`ip:<PLUTO_IP>`, `isdb_txrx2_top.bin` (`--probe`: "the symbol front end
(format 4, 32 taps)"; no image has the chain yet), release build of
`eb28673`, this host (32 threads).

**Capture.** `isdb symbols --radio pluto:uri=ip:<PLUTO_IP> --freq BS7
--system isdb-s3 --offload auto --seconds 30 --out bs7-live.sym`,
14:04:51 JST: BS7 locked at the first try (carrier −193.2 kHz, trim
−0.166 symbol, acquisition sync Es/N0 14.27 dB; AGC slow_attack, 32 dB
at the end). 762 231 858 words (1.52 GB), 50.8 MB/s, 0 DMA overflows;
the reader finds 247 236 blocks, **0 losses** (0 blocks missing; 2 439
words skipped at the join, the stream starting mid-block). Signal: sync
Es/N0 14.2 dB (17.7 dB equalised), main MER 15.4 dB, residual carrier
drift −0 … −175 Hz (−155 Hz at the end) as the host tracks it. TMCC
16APSK 93/120 × 120, three TLV streams B070 / B071 / B072 of 40 slots
(32 data + 8 dummy each), as on the recording.

| Run | Decoded by | Frames (TMCC ok) | Slots | LDPC / BCH failed | Mean iterations | MER | TLV packets B070 / B071 / B072 | Wall | User CPU | Max RSS |
|---|---|---|---|---|---|---|---|---|---|---|
| A `--symbols` | host receiver | 907 (907) | 87 072 | 0 / 0 | 2.055 | 15.36 dB (main) | 88 795 / 90 486 / 90 398 | 17.8 s | 286.5 s | 3.9 GB |
| B `--symbols --chain fixed --slots-out` | the chain's CPU model, through `Slots` | 907 (907; 2 undecoded) | 86 880 | 0 / 0 | 2.066 | 15.06 dB (n0) | 88 592 / 90 288 / 90 206 | 152.0 s | 164.2 s | 67 MB |
| C `--slots` (B's words, 380 MB) | recorded `Slots` words | 907 (907; 2 undecoded) | 86 880 | 0 / 0 | 2.066 | 15.06 dB | 88 592 / 90 288 / 90 206 | 2.4 s | 2.4 s | 43 MB |

- **A against B** (`tools/tlv-packet-diff.py`): every packet of B is a
  packet of A at the same place (0 only in B, 0 replaced); A has 203 /
  198 / 192 more, in **one run per stream at 0 %, exactly 275 264
  bytes** (2 frames × 32 data slots × 4 301 bytes): the two frames the
  chain does not decode after its start (layout lag 2; 192 slots =
  87 072 − 86 880). `cmp -i 275264:0` confirms B's three files equal A's
  from byte 275 264 to the end. With no loss there is no restart, so one
  run instead of the recording's three.
- **B against C: identical** (`cmp` on all three files; same frames,
  slots, iterations, packets): the `Slots` words written and read back
  decode as in process.
- **`chaincheck`** (`cargo run --release -p isdbs3-phy --example
  chaincheck -- bs7-live.sym`): frames 907 / 907, all matched (0 only
  one side), the chain 0 TMCC failures, 2 not decoded, 0 restarts;
  **86 880 slots compared, 86 880 byte-identical, 0 different**; LDPC /
  BCH failures 0 / 0 on both; mean iterations 2.066 (chain) vs 2.055
  (reference). `Slots`: 110 414 blocks, 190 003 369 words, 12.67 MB/s of
  signal, 0 losses. Carrier: the chain's register −8.5 … −185.2 Hz,
  −148.7 Hz at the end, against the reference's tracked −0 … −175.2 Hz
  (−155 Hz at the end, from the CLI run); n0 32 702 (Q.20, MER
  15.06 dB), taps last solved from frame 902 (centre [17 330, −255]),
  sync locked at offset 2 781 (peak/mean 4.3, 0 stray, 0 missed
  markers). The model's stages 1–6 and 8: 4.705 s per second of signal
  on one thread (LDPC 3.41 s on all cores).
- **No model defect found**: no slot where both decoded differs. The
  differences between A and B are only the lag's two frames; the
  chain's iteration mean is 0.011 above the float receiver's (0.014 on
  the recording), and its n0-based MER reads 0.3 dB below the host's main
  MER (a different estimator, not a decoding difference).
- **Host CPU behind the chain** (C): 2.4 s for 30 s of signal,
  0.08 cores, as on the recording; A takes 286.5 s (9.6 cores), B 4.83 s
  per second of signal (the bench model, not a receive path).

Not covered: a loss or a lost lock on live signal (none occurred), the
drift range (−185 Hz here, against −989 Hz on the recording), and the
chain on the board (pieces 4 and 6).

### Piece 6: the receive-only image (2026-11-02 / 03)

#### Builds 1–4 (fit `5c8b9eb`): fit, missed timing

`fpga/tezuka/build.sh WORK isdbs3-rx` (Vivado 2026.1, `xc7z020clg484-2`;
`apply.sh --isdbs3-rx`, fpga/README.md "Building the PlutoSky R2
bitstream"): `SlotsRx` from the verified fit (`5c8b9eb`, HDL unchanged) on
the RX path, without maia-sdr, `SymbolsTx`, the DVB-S2 transmitter and the
TX FIR, and with the AD9361 core's TX datapath disabled
(`DAC_DATAPATH_DISABLE`: `axi_ad9361` 7 603 → 2 647 LUT, 28 → 12 DSP). The
DAC DMA, unpacker and FIFO stay (the device tree probes the DMA; 546 LUT,
2 BRAM36); the cs12 packers stay.

**Resources: fit, with margin.** The first build stopped before
placement at 146.5 BRAM36 of 140: Vivado put the 64-deep memories of the
block in block RAM where the Yosys estimates count LUT RAM (the decoder's
64 × 714 input FIFO alone ten RAMB36). `ram_style.py` marks every memory
of depth ≤ 64 distributed (the RTL's behaviour unchanged). Then, the
second build's final checkpoint: **45 506 LUT (85.5 %), 43 109 FF, 119 BRAM36 (85 %), 176 DSP
(80 %)**; `isdb_slots` 39 026 LUT (3 666 as LUT RAM), 32 531 FF, 112.5
BRAM36, 164 DSP (front end 6 102 / 1 / 112, chain 18 346 / 31.5 / 52,
decoder 12 819 / 79.5 / 0, bridge 661 / 0 / 0, control 1 027). Vivado's
LUT count is 7 % under the Yosys sum (52 616 before the drops).

**Timing: not met** (the ADI flow stops before the bitstream:
"Timing Constraints NOT met"). Second build's final checkpoint
(`system_top_postroute_physopt.dcp`): WNS −5.934 ns, 10 600 failing
endpoints of 150 558, WHS +0.050. By clock and block (`failing.tcl`):

| Launch → capture | Block | Failing endpoints | Worst setup | Cause |
|---|---|---|---|---|
| symbol clock → l_clk (6.25 ns rule) | chain `back` (equaliser, demapper), `finish` | 2 794 | −5.934 ns | **clock skew**: `util_ad9361_divclk` (BUFR ÷ 4 + BUFGMUX) reaches its registers 4.2 ns after l_clk's BUFG; the fit's phase-aligned 4× clock is aligned logically, not physically |
| l_clk → l_clk (6.25 ns) | `finish`: the BCH unit (`FastBch`: Berlekamp–Massey's syndrome read, GF(2¹⁶) multiply and locator write in one cycle; 10 LUT levels, 9–10 ns) | 790 | −3.834 ns | logic depth |
| l_clk → l_clk | chain `back` | 182 | −1.623 ns | logic depth |
| decoder 200 MHz | `ldpc` (the TMCC fill's lo/hi bounds from `fill_base` into 34 BRAM enables: 4 CARRY4 + 5 LUT, 7.2 ns; `layer_info` loads) | 6 123 | −2.708 ns | logic depth: the decoder closes at about 125–130 MHz |
| decoder 200 MHz | bridge FIFOs' decoder side | 310 | −1.355 ns | logic depth |
| rx_clk at ADI's 4 ns | `axi_ad9361` PN monitors, data format, TX channels | ≈ 300 | −2.654 ns | placement pressure (the deployed image: +0.542) |
| symbol clock at ADI's 16 ns | `finish` (`al`: 19 levels, 16.3 ns) | 0 | +0.194 (−0.454 at placement) | met by a hair; the chain runs only at 40 Msps: 25 ns (below) |

Remedies tried, each once:

- **Directives** (`reimpl.tcl` on the optimised checkpoint: place
  `ExtraTimingOpt`, phys_opt / route / post-route phys_opt
  `AggressiveExplore`): WNS −5.815, the decoder −2.846, the fast domain
  −4.009: nothing material.
- **XDC**: the chain's and the bridge's own symbol-clock paths at the real
  25 ns (the 6.25 ns l_clk rule's argument), false paths for the
  asynchronous reset assertions into the reset synchronisers (a bridge
  FIFO's `rst_cdc`, −3.634 ns at 1 ns of requirement).
- **Clock skew glue** (`isdb_slots_top.v`): fast_clk through a BUFR in
  BYPASS and a global buffer, like the symbol clock. On the third build's
  placement (`skew_check.tcl`): skew −0.49 ns instead of −3.99, the
  symbol → fast crossings' worst setup −1.69 ns (a DSP A input of the
  equaliser, 3.1 ns of DSP setup) instead of −5.93. The third build used a
  BUFGMUX_CTRL with its select tied, through which Vivado does not
  propagate the clock (the fast registers unclocked); the fourth uses a
  BUFG. **Fourth build, final checkpoint: WNS −3.257 ns, 7 429 failing
  endpoints** (45 275 LUT, 43 030 FF, 119 BRAM36, 176 DSP; WHS +0.016):
  the BCH unit −3.257 (812 endpoints), the decoder −3.016 (5 063; the
  bridge −1.005, 228), the symbol → l_clk crossings −2.863 (1 177: the
  equaliser's symbol-clock registers into DSP A inputs, 3.1 ns of DSP
  setup), the chain's `back` in l_clk −1.379 (115), ADI's own `rx_clk`
  paths −0.525 (about 25); the symbol clock (+0.663) and the l_clk →
  symbol clock crossings (+0.305) met.
- `use_dsp` hints: not applicable (the failing paths are GF(2) logic,
  comparators and DSP inputs, not adders in fabric); pipeline registers
  absorbed by existing enables: none found on the failing paths (the BCH
  unit's FSM and the decoder's fill and layer control are single-cycle
  loops).

**What the HDL needs before an image** (not done here: the fit's
arithmetic is out of this piece's scope): the BCH unit's
Berlekamp–Massey step split over two fast cycles (or the syndrome mux
registered); the decoder's TMCC fill bounds (`fill_lo`, `fill_hi`)
computed a cycle ahead (they depend on `fill_base` only) and its
`layer_info` load registered, or the decoder at about 125 MHz, which the
queue model (`ldpcqueue`) would have to re-check (9 overruns of 363 at
160 MHz with limit 16 on the measured distribution); the equaliser's
symbol-clock → DSP feed registered on the fast side; the clocking glue
above. Then the ADI paths' placement pressure has to be seen again.

Builds 1–4 wrote no bitstream (the flow refuses on failed timing) and
the board was not touched.

#### Builds 5–6 (fit `7f70e26`, the HDL remedies): timing met

The fit registered the chain's 160 MHz and crossing paths and pipelined
the decoder (its bridge FIFOs in block RAM, +4.5 BRAM36; default
iteration limit 14, K = 3), bit-exact on every set. `ram_style.py` now
leaves a memory's own `ram_style` alone (Yosys writes the attribute
lines above a declaration's `src`; the first version would have overridden
the bridge FIFOs' `block`). Build 5's placement failed only on the
decoder crossing's 5 ns rule, which named cells: with the FIFO memories
in block RAM (clocked by both clocks) it caught same-clock paths into
and out of them (181 endpoints, −1.551 ns); the rule now names the two
clocks (`isdb_slots.xdc` rule 4). Build 6, final checkpoint: setup met
everywhere (WNS +0.021 ns) and one hold violation outside the block,
Tezuka's RGMII receive IDDR into the fabric (−0.433 ns; the Vivado
2025+ router, as upstream's `hold_fix.tcl` says). `holdfix.tcl` on that
checkpoint (phys_opt `ExploreWithHoldFix`, the failing nets rerouted,
twice): **every constraint met, WNS +0.045 ns, WHS +0.011 ns, 0 failing
endpoints of 161 991 (setup) / 161 821 (hold)**; it wrote the bitstream
and the XSA.

| Clock | WNS (ns) | WHS (ns) |
|---|---|---|
| `rx_clk` (l_clk; the block's fast domain at 6.25 ns, ADI's paths at 4 ns) | +0.045 | +0.011 |
| `c0` (the decoder's 200 MHz) | +0.194 | +0.042 |
| `clk_div_sel_0_s` (the symbol clock) | +1.625 | +0.044 |
| symbol clock → l_clk / l_clk → symbol clock | +0.255 / +0.593 | +0.051 / +0.050 |
| symbol clock ↔ decoder (5 ns, datapath) | +3.237 / +3.108 | |
| `clk_div_sel_1_s` | +0.519 | +0.088 |
| `clk_fpga_0` / `clk_fpga_1` | +1.143 / +1.537 | +0.048 / +0.121 |
| `RGMII_rxc` | +0.100 | +0.192 |

Utilisation (build 6): 43 817 LUT (82.4 %; 3 918 as LUT RAM), 49 845 FF,
123.5 BRAM36 (88 %), 176 DSP (80 %); `isdb_slots` 37 354 LUT, 39 331 FF,
117 BRAM36, 164 DSP (front end 6 098 / 1 / 112, chain 18 185 / 31.5 /
52, decoder 11 693 / 79.5 / 0, bridge 270 / 4.5 / 0, control 1 029).
Artefacts: `isdbs3_rx_top.bin` 2 746 304 bytes, sha256
`41997abe5f455fb8117ccda14d2c414ead397d39d9aac2508b9ced0bf54995ac`; the XSA
(from `holdfix.tcl`) `19003701d60c91c99eecfe2f2e956f732b3827cf7a4a74a7e87fdae063dcb61a`.

#### On the board (2026-11-03, receive only)

Deployed with `tools/pluto-image.sh deploy` beside the other images
(`/boot/isdbs3_rx_top.bin`; the previous `uEnv.txt`, `bitstream_image=
isdb_txrx2_top.bin`, saved as `uEnv.txt.prev-txrx2`). First boot:
"Tuning TX FAILED" (the known 2-in-3), `cf-ad9361-lpc` bound on the
second rebind (`pluto-image.sh recover`). ID word 0x15DB4120 (front end
format 4, 32 taps, `Slots` version 1). `tools/isdbs3-board-check.sh`, BS7,
30 s each:

| Run | Decoded by | Frames (TMCC ok) | Slots | LDPC / BCH failed | Mean iterations | MER | TLV packets B070 / B071 / B072 | Losses | Host CPU | Max RSS |
|---|---|---|---|---|---|---|---|---|---|---|
| FPGA chain (`isdb rx --offload auto`, 03:34) | the FPGA receive chain (`Slots`) | 909 (909; 2 undecoded) | 87 177 | **268** / 0 | **4.63** | 13.0 dB (n0) | 91 183 / 91 431 / 90 587 (lost slots 133 / 65 / 70) | 0 (190.8 M words, 0 blocks missing) | **10.2 s user + 2.1 s sys for 30 s (0.41 cores, the IQ acquisition included)**, wall 34.4 s | 6.2 GB |
| `--slots` of its recorded words | recorded `Slots` | 909 | 87 177 | 268 / 0 | 4.63 | | identical to the live run (every packet) | | | |
| host receiver on the Symbols capture just before (03:33) | host | 907 (907) | 87 072 | 0 / 0 | 2.061 | 15.4 dB main | 91 721 / 91 339 / 91 121 | 0 | 286.1 s user (9.5 cores) | 3.9 GB |
| CPU model (`--chain fixed`) on the same capture | the chain's model | 907 (2 undecoded) | 86 880 | 1 / 0 | 2.077 | | every packet a packet of the host's, the 2 lag frames missing | | | |

- The open line: "FPGA receive chain: Slots format 1, features 0x220e03
  (LDPC limit 14, step 34)"; `--offload auto` took the slots path. The
  chain locked (offset 6 542, peak/mean 4.5, 0 missed markers), tracked
  the carrier from +38.9 to −762.8 Hz in 30 s (−753 Hz at the end; the
  drift was faster than on 2026-11-02), TMCC 16APSK 93/120 × 120 every
  frame, 0 restarts, 0 discontinuities. The TLV streams parse cleanly
  (`isdb tlv inspect`: 1 271 tables on B070, 0 CRC errors; NIT, SDT,
  EIT, TOT as on the host's), their lost slots being exactly the 268
  LDPC failures.
- **The equaliser ran on identity taps all along**: "the equaliser's
  slow loop wrote taps 0 time(s) in 46 ticks". A debug run (8 s,
  `RUST_LOG=isdb::chain=debug`) shows why: SUMS_FRAME reads a new frame
  each tick (6 720 symbols, as designed) and has moved 14–15 frames on
  by the time the 308 sum registers are read through iiod, so every read
  is torn and retried, forever. The FPGA latches the sums every frame
  (33 ms); one read of the set takes about 0.5 s. With identity taps the
  chain's n0 gives MER 13.0 dB (the model with solved taps, on the
  2026-11-02 capture: 15.06 dB), mean 4.63 LDPC iterations and 268
  failures at limit 14, where the model (with taps) has 1. Not changed
  here (HDL and host are another piece's): the HDL could hold the latched
  sums until the host has read them (a request / release register), or
  the host could load taps solved from its acquisition IQ at open
  (`ChainInit`, which the CLI does not pass yet).
- The ADC core reported 27 DMA overflows during the Slots run (the first
  after 5.2 M words) and 1 at the end of the 'before' Symbols capture,
  while the `Slots` sequence numbers show 0 blocks missing and the CRC
  0 errors: the flag does not correspond to lost words here; cause not
  found.
- Packets across runs: the host's 'before' streams against the FPGA's
  (a minute apart: different content) have 1 027 / 358 / 358 packets
  equal and aligned (the repeated SI), everything else different, as
  expected for different windows; the like-for-like check is the model
  against the host on the same capture (above) and the FPGA run against
  its recorded words (identical).
- The host's 6.2 GB RSS on the radio path (41–67 MB replaying `--slots`)
  is the capture side's buffering; not investigated.

**Rollback** (the board stays on `isdbs3_rx_top.bin`, now build 7):
`tools/pluto-image.sh rollback uEnv.txt.prev-txrx2` then
`tools/pluto-image.sh reboot` (and `recover` if `cf-ad9361-lpc` is
missing) boots `isdb_txrx2_top.bin` again, with the transmit block;
`rollback uEnv.txt.prev-rx6` boots build 6 (`isdbs3_rx6_top.bin`).

#### Build 7 (fit `d4042bb`: SUMS_CAPTURE) on the board (2026-11-03)

The fit's taps fix: a SUMS_CAPTURE register (0x422) holds the training
sums of a host-armed frame end, FEATURES bit 4, the slow loop reading a
held frame once. Build 7 met every constraint in the project flow itself
(no `holdfix.tcl` needed): **WNS +0.067 ns, WHS +0.029 ns, 0 failing
endpoints**; `rx_clk` +0.067 / +0.051, decoder `c0` +0.235 / +0.048, symbol
clock +0.578 / +0.029, symbol → l_clk +0.264, l_clk → symbol +0.578,
symbol ↔ decoder +3.327 / +3.201, `clk_fpga_0` +1.345, `RGMII_rxc` +0.100 /
+0.133. 43 731 LUT (82.2 %), 49 812 FF, 123.5 BRAM36, 176 DSP; `isdb_slots`
37 253 LUT, 117 BRAM36, 164 DSP. `isdbs3_rx_top.bin` 2 833 440 bytes,
sha256 `717f7e34adaf5cadd906aa70bd2a8b482a3b5ceb6c328f2f52299ceed28e6a2a`
(XSA `1b1abd0ff308df3245fecb94b3971c94ee66351ea345a7254c3aec19df31558e`).
On the card: build 6 kept as `isdbs3_rx6_top.bin` (`uEnv.txt.prev-rx6`
selects it), build 7 as `isdbs3_rx_top.bin`; the boot bound
`cf-ad9361-lpc` at once (no TX tune failure); ID word 0x15DB4120.

`tools/isdbs3-board-check.sh`, BS7, 30 s (log lines stamped; the
recorded words replayed):

| Run | Frames | Slots | LDPC / BCH failed | Mean iterations | n0 / MER | TLV packets B070 / B071 / B072 (lost slots) | Losses | Host CPU | Max RSS |
|---|---|---|---|---|---|---|---|---|---|
| FPGA chain (`--offload auto`) | 909 (2 undecoded) | 87 187 | **7 808** / 0 | 2.38 | 52 556 (13.0 dB) all run | 83 483 / 38 448 / 83 199 (2 597 / 2 617 / 2 594) | 0 | 9.97 s user + 2.33 s sys, wall 34.4 s | **6.28 GB** |
| `--slots` replay of its words | 909 | 87 187 | 7 808 / 0 | 2.38 | | identical | 0 | 2.28 s user, 2.29 s wall | **39 MB** |
| host receiver, Symbols capture just before | 907 | 87 072 | 1 / 0 | 2.078 | 15.4 dB main | 91 686 / 42 190 / 91 662 | 0 | 294 s user | 3.9 GB |
| CPU model (`--chain fixed`) on that capture | 907 (2 undecoded) | 86 880 | 2 / 0 | 2.094 | 32 000–33 700 (≈ 15 dB) | every packet a packet of the host's | 0 | | |

- **The taps now arrive**: the open line reads "features 0x220e13
  (LDPC limit 14, step 34)" (bit 4 SUMS_CAPTURE); the slow loop wrote
  taps 12 times in 24 ticks, 0 torn reads, 0 singular; the **first write
  0.90 s after the chain's open** (solved from frame 5; the chain's first
  frame is 2), then about every 2.7 s (81 frames). The taps were in
  force from frame 26 on (`examples/slotsstatus` on the recorded words).
- **With good taps the chain decodes like the model**: frames 26–269 and
  379–909, 0 failed codewords, mean 2.0–2.2 iterations per frame.
- **One solve of the twelve was bad**: the host's "solved from frame 250"
  (centre tap [18 691, −285]; the eleven others [17 325 … 17 848,
  −190 … −271]) was in force from frame 270 to 350, and **every
  codeword of those 81 frames failed** (96 / 96, 10.4 mean iterations,
  limit 14; with the switching frames 270 and 351, 7 800 of the 7 808 failures; the other 8: 7 in frames 2–25, before the first taps, 1 in frame 378), and 2 597 / 2 617 / 2 594 lost
  slots in the TLV streams (the 81 frames × 32 slots per stream). The next
  solve (frame 331's) brought the decoding back within a frame. The
  model's 30 solves on the capture just before are all within
  [17 315 … 17 379]. Cause not found: the host does not log the sums it
  solves from, and SUMS_COUNT read 6 720 as for every capture.
- **n0 is never learned on the board**: the status blocks give n0 =
  52 556, the reset default (`N0_DEFAULT`), and LLR scale 81 722 in every
  frame of both board runs, while `learned_from` advances every frame (the
  constellation is learned); the model on the same signal learns n0
  ≈ 32 500–33 700 (≈ 15 dB) from its fourth frame. The decoder then sees
  LLRs scaled about 0.62× the model's; the iteration means are close
  (2.07–2.2 vs 2.09) on this signal, but the FPGA is not the model here.
- **The taps' frame label lags one write**: the status's `taps_from`
  shows the previous TAPS_FROM written (the first set, solved from frame
  5, reads 0; the set from 87 reads 5, …), consistent with `SlotsRx`
  arming the equaliser with `tap_from` / `tap_at` in the same cycle as it
  registers the new value (`top.py`, REG_TAPS_FROM). The taps themselves
  were applied (their values change in the status about 20 frames after
  each write).
- **The 6.2 GB RSS is the radio path's**: the same words replayed with
  `--slots` take 39 MB and 2.3 s of CPU.
- ADC overflow flag: 8 during the Slots run, no block missing by sequence
  number, as in run 1.
- The 'before' Symbols capture decodes on the host with 1 failure in
  87 072 slots (MER 15.4 dB): the signal is as on 2026-11-02.

#### Build 8 (fit `682032e`: n0 learned, the status's points, TAPS_FROM, the tap gate) (2026-11-03)

Built and met: the project flow's final checkpoint had setup met (WNS
+0.034 ns) and the RGMII receive input's hold at −0.046 ns (one
endpoint, the same class as build 6); `holdfix.tcl` repaired it in one
round: **WNS +0.034 ns, WHS +0.033 ns, 0 failing endpoints of 161 734 /
161 564**. Per clock (WNS / WHS, ns): `rx_clk` +0.034 / +0.044 (a
demapper K product), decoder `c0` +0.218 / +0.043, symbol clock +0.554 /
+0.044, symbol → l_clk +0.034 / +0.050, l_clk → symbol +0.554 / +0.057,
symbol ↔ decoder +3.570 / +3.489, `clk_fpga_0` +1.174 / +0.036,
`RGMII_rxc` +0.100 / +0.033. 43 704 LUT (82.2 %), 49 745 FF, 123.5
BRAM36, 176 DSP; `isdb_slots` 37 244 LUT, 117 BRAM36, 164 DSP.
`isdbs3_rx_top.bin` 2 822 976 bytes, sha256
`f2cb47d12b6d6732497b04f57d8635b82a78e50c90122244460190b2131ff1b4` (XSA
from `holdfix.tcl`
`2c6028ecf3024875cf0c0d29068dd28298bdb52305b5de93fd6c7c20826d4b56`), in
`/path/to/sdr/isdb-work/out/isdbs3-rx-build8/`.

The bench board was unreachable for a while after this host's restart
(04:41–13:15 JST); powered up again it had booted build 7 from the card.

#### Build 8 on the board (2026-11-03, 14:23 JST, receive only)

Card: build 7 kept as `isdbs3_rx7_top.bin` (`uEnv.txt.prev-rx7` selects
it), build 8 as `isdbs3_rx_top.bin` (selected), build 6
`isdbs3_rx6_top.bin` (`prev-rx6`), `isdb_txrx2_top.bin` (`prev-txrx2`).
Clean boot (`cf-ad9361-lpc` at once), ID word 0x15DB4120, open line
"features 0x220e13 (LDPC limit 14, step 34)".
`RUST_LOG='info,isdb::chain=debug' tools/isdbs3-board-check.sh`, BS7, 30
s; the captures on disk in `/path/to/sdr/isdbs3-bench/build8/` (not
committed).

| Run | Frames | Slots | LDPC / BCH failed | Mean iterations | MER | TLV packets B070 / B071 / B072 (lost slots) | Losses | Host CPU | Max RSS |
|---|---|---|---|---|---|---|---|---|---|
| FPGA chain (`--offload auto`) | 909 (2 undecoded) | 87 234 | **1** / 0 | **2.15** | **14.9 dB** (n0) | 88 399 / 91 518 / 90 846 (0 / 0 / 1) | 0 | 10.28 s user + 1.93 s sys for 30 s (0.41 cores), wall 34.4 s | 6.28 GB |
| its words replayed (`--slots`) | 909 | 87 234 | 1 / 0 | 2.15 | 14.9 dB | identical | 0 | 2.41 s | 39 MB |
| host receiver, the Symbols capture after | 907 | 87 072 | 1 / 0 | 2.113 | 15.2 dB main | 90 222 / 91 741 / 89 305 | 0 | 291.6 s user (9.7 cores) | |
| CPU model on the same capture | 907 (2 undecoded) | 86 880 | 1 / 0 | 2.129 | | every packet one of the host's (the 2 lag frames apart) | 0 | | |

The fix agent's checks (`examples/slotsstatus` on the recorded words):

- **n0 learned**: frame 4 on (the frame after the first decoded one,
  frame 3) n0 is the measured value: 46 000–47 400 while the identity
  taps are in force (frames 4–26), 33 000–34 000 once the first taps are
  (from frame 28), K = 2³²/n0 throughout (e.g. 33 650 → 127 636). Frames
  30–909: n0 mean 33 705 (30 892 … 39 389), K 109 040 … 139 032; the
  model on the 'after' capture: mean 33 554 (31 186 … 35 417).
- **Learned 16APSK points plausible**: frame 911, 4 inner points at
  radius 445–473 on the diagonals and 12 outer at 1 025–1 156 (Q.10),
  ring ratio ≈ 2.3.
- **`taps_from` = the solve frame**: the first set (solved from frame 6)
  reads 6 from frame 26; every later change carries the host's frame
  (86, 168, 249, 330, 411, 492, 573, 655, 763, 844).
- **Slow loop**: 11 solves written in 24 ticks, the first 0.87 s after
  the Slots open (from frame 6); **0 rejected by the gate**; one capture
  did not hold ("the held training sums moved from frame 0 to 736 while
  they were read"; recaptured, the next solve from frame 763). R[5][5]
  6.46–6.53 × 10⁹ and p[5] re 6.58–6.61 × 10⁹ in all eleven, digests all
  distinct.
- **The last solve (frame 844) passed the gate but is off in its outer
  taps**: centre [17 150, −267] (−0.9 % from the previous set), but tap
  3 −893 where the other ten have +300 … +406, tap 4 −1 546 (others
  −1 716 … −1 866), tap 0 301 (others 167 … 217). Its R[5][5] and p[5]
  are like the others'. In force from frame 865: n0 rose from ≈ 33 700
  to 36 300–39 400 and the mean iterations from 2.07 to 2.3–2.5 for the
  last 45 frames, with no failure. The gate looks at the centre tap and
  the residual only; a check of the whole tap vector against the
  previous set would have held this one. (The carrier drifted fast on
  this run, +208 → +2 384 Hz in 30 s, the board warming after its power
  cycle; the chain tracked it.)
- LDPC failures 1 in 30 s (the model's figure on the capture after: 1),
  0 losses (0 blocks missing), BCH 0. The ADC overflow flag rose 13
  times, nothing lost by sequence number, as before.
- The 'before' Symbols capture mis-acquired (carrier −528 kHz, Es/N0
  6.4 dB in the 0.1 s survey: the first capture after the reboot); the
  host could not decode it, so the comparisons use the 'after' capture.
- The radio path's RSS is 6.28 GB again against 39 MB for the replay.

#### Build 8 issues (#27, #28, #29; HDL side, 2026-11-04)

A second user ran build 8 (built locally from `2ce1696`, timing met) on
their own PlutoSky R2 (BS-14, receive only) and reported three issues
with precise evidence. The host side (verified register reads, taps at
open, recovery from a no-layout state, buffer sizing) is a separate
change; this is the HDL's.

- **#28, the decoder's banks leaked across stop/START (high).**
  Confirmed in the code and in simulation: the `ldpc` domain is reset only
  by the MMCM's lock, START restarts only the chain side, and a slot
  codeword whose input stops (the host's stop, a lost sync, a dropped
  frame) never gets its last beat: the decoder kept the bank allocated
  at its `begin`, and the next `begin` on the context allocated another
  and overwrote `ctx_bank`, so the old bank was never committed or freed.
  With K = 3 that is the board's 0 → 2 → 50 of 96 dropped slots per frame
  → nothing (TMCC dropped too: no layout, STATUS 0x3) over consecutive
  runs. On the old HDL the regression below shows it: after its STARTs
  1, 2, 2 and 2 banks held (with two held, the last one is the TMCC
  codeword's while frame 2's first codeword begins, so that one is
  dropped rather than leaked), and in the last pass slots 0, 1, 2, 3, 6,
  8, 11, 13, 16, … of frame 2 dropped: the issue's "50 of 96" pattern,
  slot for slot. Fix (`ldpc.py` "Abandoned codewords", `back.py`,
  `ldpc_if.py`): `abort` gives up a context's open codeword and frees its
  bank; the back end sends it as an OP_ABORT beat on context 0 after every
  frame drop and restart, once the demapper's pipeline has emptied, in
  order through the bridge's input FIFO (no new clock crossing for it);
  and a `begin` on a context that still holds an uncommitted bank takes
  that bank again, so no bank can stay held even without the abort.
  **Committed codewords are never discarded**: they decode and leave in
  order (the result stream, the tag queue and `Finish` stay in step; a
  stop lets them drain, the chain's output running on unselected), so
  after a stop and START every bank is free, no context open, the
  bridge's FIFOs, tag queue and the result FIFO empty, without a
  bitstream reload. Tests: `test_abort_frees_an_abandoned_bank`,
  `test_a_begin_takes_back_its_contexts_abandoned_bank` (decoder, toy
  code), `BridgeTest` (the abort beat), and the issue's recipe on the
  chain with the real decoder, `test_stop_and_start_free_the_decoder`
  (`stages.run_chain_restarts`, `tools/restart_cycles.py`): chain-7-hdl's
  input to inside frame 2's first codeword, 200 000 symbol clocks of no
  input, START (CTRL START's effect on the chain: a loss of 0 symbols,
  CARRIER and the taps loaded, the sequence from 0), four times, then the
  whole input: after each START no bank held and every queue empty
  (`aborted` 1, 2, 3, 4: one bank freed per START), and the last pass
  decodes every slot of frame 2 as the
  model does (bits and BCH results; 7 of 96 slots take one iteration
  more or less: a restart keeps the NCO's phase, as the model's
  `Chain::loss` does, so the replay's symbols round differently; with the
  phase zeroed at START in a scratch copy every block equals the model's,
  iterations included). A first version of the test idled after START
  instead of before: then frame 1's TMCC decoded after the restart and its
  layout stayed queued for the replay's first frame (the known restart
  epoch, README "What is left"), so the replay learned n0 a frame early.
  On the board a stop precedes START by far more than the drain.
- **#28, decoder health visible (medium).** DECODER (0x439: overruns
  15-0, queue overflow 16, banks held 23-20, K 27-24) and RECLAIMED
  (0x43A: banks freed by the abort 15-0, taken back by a begin 31-16), a
  snapshot from the decoder's clock through a toggle handshake in the
  bridge (`set_max_delay -datapath_only 5.0` already covers it), FEATURES
  bit 5. A register only: the status block is the model's format
  (`slots::Status`), and a field there would change the model, the
  vectors and the host's parser.
- **#28, a stream without frames (low).** Already so: without a layout
  the finish stage still sends, per whole frame, its TMCC block (604
  words) and status block (79 words, + 4 · 2^m per learned modulation m):
  683 to 807 words per 33.05 ms frame, at least **20 668 words/s** (a
  1 Mi-word buffer takes 50.7 s: longer than the host's refill timeout,
  #28's "cannot fill a DMA buffer"); nothing while
  the sync is unlocked or no frame ends whole. chain-6/7's frames 0 and 1
  (no layout yet) are compared word for word in the existing tests. No
  periodic block was added: the host sizes its buffers from this floor.
- **#28, no-layout lock-in.** Kept: without a layout no sum opens, so
  nothing latches and an armed capture waits. Visible now in one read:
  SUMS_CAPTURE bit 2 NO_LAYOUT (= STATUS bit 2 DECODING clear; DECODING
  is now also cleared at a restart, since the restart drops the layout
  queue). Taps at START confirmed (`eq.py` `tap_now`): with INIT bit 0
  the taps last written to TAPS go in use at once (TAPS_FROM NONE, an
  armed set dropped); without it the taps in use, and an armed
  TAPS_FROM set, stay as they are.
- **#27/#29, the corrupted sum reads.** Their cause is not the HDL: other
  clients of `cf-ad9361-lpc`'s `direct_reg_access` (the host's per-buffer
  ADC overflow poll on another libiio context; Tezuka's
  `api_controller.sh` polling 0x80000088 every 3–4 s) move the cached
  register address between the host's address write and its read (#29's
  E1–E3). The host verifies its reads (host change). The HDL's latent
  window is closed anyway: a sum READ used to set the readback to 0 and
  wait for the MAC unit to be idle (up to about 4 400 symbol clocks); now
  the read takes the accumulator RAM's read port for one cycle (a MAC due
  then waits a cycle; it writes only the other bank), `rd_ack` 3 symbol
  clocks after the request, the readback keeping the previous READ's
  value until then; SUMS_CAPTURE bits 15-8 SEQ count the latches, equal
  before and after a 308-word set when no latch moved in between;
  FEATURES bit 6. Tests: `EqTest.test_sums_read_while_the_mac_runs` (the
  308 words read while the MAC unit sums a slot: the held frame's, every
  read 3 cycles), `SlotsRxRegisterTest` (no 0 between).
- **Fit**: Yosys, `SlotsRx` without the decoder 26 395 → 26 487 LUT (+92,
  0.35 %), 27 119 → 27 245 FF, 32.5 BRAM36 and 164 DSP48E1 unchanged; the
  decoder (S = 34, K = 3) 12 510 → 12 538 LUT, 17 397 → 17 433 FF, 77
  BRAM36, 0 DSP. Not built (build 9 is the image's).

**Follow-up (not this round): the sums in the `Slots` stream.** A block
kind 4 per latch: the frame (2 words), the count (1) and the 77 complex
48-bit sums (77 × 2 × 3 = 462 words), 465 payload words, 476 with the
header and the CRC-32C trailer the packer already computes: at most one
per decoded frame, 14 400 words/s, 0.23 % of a decoded frame's 210 000
words. The HDL: a reader in `Finish` after the frame's status block taking the latched
bank through the same one-cycle steal of the accumulator's read port (77
entries × 6 words, the word select a 6:1 mux of 16 bits), its FSM states
and counters: about 150–250 LUT and 100 FF, no block RAM or DSP; the
capture's hold and the host's 308 reads (1.3–1.5 s each, exposed to any
other `direct_reg_access` client) go. It is a `Slots` format change: the
model (`isdbs3_phy::chain` emits the block, so the bit-exact tests keep
it), the vectors, `isdb_frontend::slots` (version 2, the reader) and the
host's slow loop, about 150 lines each side.

#### Build 9 on the board (2026-11-04, receive only)

The image of `fpga/isdbs3-rx-issues-27-28-29` (HDL at 291756f; host
binary from 291756f for the first runs and from 91c360a, the clean-end
fix, for the repeat of #28's recipe). Built in the project flow with
every constraint met, no `holdfix.tcl` needed: **WNS +0.044 ns, WHS
+0.050 ns, 0 failing endpoints of 162 123**; `rx_clk` +0.044 / +0.051,
decoder `c0` **+0.240** / +0.050 (the allocation path's extra LUT level
did not cost the 200 MHz), symbol clock +0.416 / +0.053, symbol → l_clk
+0.151, l_clk → symbol +0.416, symbol ↔ decoder +2.833 / +2.134,
`clk_fpga_0` +1.388, `RGMII_rxc` +0.100 / +0.062. 43 730 LUT (82.2 %),
49 859 FF, 123.5 BRAM36, 176 DSP; `isdb_slots` 37 266 LUT (decoder 11 572,
chain 18 189, bridge 308), 117 BRAM36, 164 DSP. `isdbs3_rx_top.bin`
2 788 864 bytes, sha256
`447223e684eee748484b5e8f5072625f5e5ab862a33916ef200dea2157109dc3` (XSA
`33a53cd5b4d711ca7228ee20f7e648644bb1f3487f01b85cc353e8259f5c1feb`). On the
card: build 8 as `isdbs3_rx8_top.bin` (`uEnv.txt.prev-rx8`), build 9 as
`isdbs3_rx_top.bin`; the first boot needed two rebinds ("Tuning TX
FAILED"); ID word 0x15DB4120. Every run opened with "features 0x220e73
(LDPC limit 14, step 34); taps identity (loaded)" (bits 4 SUMS_CAPTURE,
5 DECODER_HEALTH, 6 SUMS_SEQ). Runs with `tools/isdbs3-issue-checks.sh`
(captures in `/path/to/sdr/isdbs3-bench/build9/`, not committed).

**#28, stop/START (6 × 8 s back to back, then 60 s, no reboot; twice).**
Every run completed with exit status 0 and decoded; the dropped slots
(not converged with 0 iterations) per run:

| Series | run 1 | 2 | 3 | 4 | 5 | 6 | 60 s after | at most per frame |
|---|---|---|---|---|---|---|---|---|
| host 291756f, 02:13 | 1 | 2 | 0 | 3 | 3 | 0 | 0 | 1 |
| host 91c360a, 02:34 | 4 | 3 | 3 | 4 | 1 | 2 | 1 | 2 |

The issue's 0 → 2 → 50 → dead progression is gone: nothing accumulates
over 12 consecutive stop/START cycles. Every drop falls in a run's first
34 frames, before its first solved taps are in force (identity taps,
4.1–4.8 iterations per codeword, the decoder at its busiest), and the
not-converged slots there too (7–13 frames per 8 s run before the taps,
0–1 after): load, not a leak. At each open the decoder reads **1 of 3
banks held** ("the LDPC decoder still holds banks before START …; START
frees them": a JSON-logged run, `held 1, banks 3, overruns 30`), so a
stop still leaves its open codeword's bank, and START's abort frees it:
the runs after show no held bank's effect. TMCC decoded every frame,
STATUS locked / framed / decoding throughout, 0 losses in the second
series; the 60 s runs: 4 and 2 not converged (0 and 1 dropped), 2.30
mean iterations, MER ≈ 15.0 dB.

The first series lost blocks twice (runs 4 and 5: 10 704 and 15 329
blocks, 3.4 and 4.9 s of signal, early in the run), and the 600 s run
below once (14 615 blocks at 45 s): the board's `dmesg` shows its
Ethernet link down and up three times in that period ("macb … Link is
Down", "unable to generate target frequency: 125000000 Hz", "Link is Up
- 1Gbps/Full"), and no further flap and no loss in the 20 min of runs
after. A link fault of the board (or its switch port), not the chain;
the Slots sequence numbers counted every lost block.

**#27 / #29, with `api_controller.sh` running (600 s, `RUST_LOG=
isdb::chain=debug`).** `tools/pluto-autorun.sh` had stopped it at boot;
started again for this run (`setsid /root/api_controller.sh`), running
throughout (two processes). Result: 1 731 413 valid slots, **8 not
converged** (2 dropped), per minute of signal 4 0 1 0 2 0 1 0 0 0
(3 of the 8 before the first taps); 200 tap sets written in 400 ticks,
**0 solves rejected, 0 captures rejected, 0 reverts, 0 recoveries, 0
torn, 0 unsettled, 1 reread** (1 of 65 201 register reads read again
after a foreign value: the race was there and caught); no "capture did
not hold"; n0 33 100–34 500 at every 2-minute mark (K 124 380–129 687),
MER 15.04 dB at the end, mean iterations 2.11; 70.9 s user + 5.4 s sys
for 600 s (0.13 cores), RSS 6.0 GB; exit 0. One loss (the link flap
above, 14 615 blocks, 120 frames).

**Then `api_controller.sh` stopped, 300 s.** 871 488 valid slots, **2 not
converged** (0 dropped; per minute 1 0 0 0 1), 102 solves, **0
rereads**, every other counter 0, 0 losses, MER 15.09 dB, 2.11 mean
iterations, exit 0. (The `pkill -f '[a]pi_controller'` on the board also
ended the ssh session it ran in, whose command line contained the
pattern; the board was fine.)

Rollback: `tools/pluto-image.sh rollback uEnv.txt.prev-rx8` (build 8),
`prev-rx7`, `prev-rx6` or `prev-txrx2`, then `reboot` and `recover`.

#### Build 9 residuals, host side (2026-11-04)

Two host defects of the build 9 runs, fixed without the board
(measurements: VERIFICATION.md "Build 9 residuals, host side").

- **6.0–6.3 GB max RSS on the Slots path** (every run, 30 s or 600 s:
  a fixed peak, not growth). It is the acquisition, before the chain
  opens: `s3sync::best_trim` ran its 13 trim candidates on threads, each
  collecting its front end's whole `Symbol` output over 0.15 s at
  40 Msps (5 M × 56 bytes) and measuring it (40 bytes per symbol more)
  all at once; `pick_carrier` the same with 8 candidates on 0.05 s. Now
  the front ends keep only `zk` and measure one at a time
  (`s3sync::measure_one`): the acquisition's peak 6.1 GB → about
  0.6 GB, results bit for bit the same, time unchanged. The stream
  itself never held more than its 17 MiB of queues.
- **0–4 dropped slots per 8 s run, all before the first solved taps.**
  Options:
  1. *The taps from the acquisition's probe* (built): the CLI's probe is
     0.2 s of IQ at the radio's rate (8 M samples at 40 Msps, about six
     frames). `chain::probe::probe_taps` runs it through the front end's
     model with the parameters the FPGA's front end gets (bit-exact with
     it) and the chain's model (TMCC decoded, main signal not); the
     first training sums are the third frame's after the frame sync
     (layout lag 2), within 0.13–0.17 s of the probe, solved with
     `solve_sums` as the slow loop solves the FPGA's. 0.3 s at the open.
     The sums are taken at the equaliser's input, so the same signal
     gives the same sums on the FPGA up to noise and drift: on the
     synthetic echo the probe's taps are within 0.03 of every later
     solve and lose ≤ 0.03 dB of training MER against them. Correct by
     construction, no state on disk, and right after a channel change
     (the process's last good set is not).
  2. *A cache of the last good set across processes*: needs a key (URI,
     channel, FEATURES), an age limit and a file; a stale set is caught
     only by the gate (two solves) or the revert (10 frames). Not built:
     (1) costs 0.3 s and is always fresh.
  3. *The slow loop sooner*: its first capture is already armed at the
     first tick; the first sums need frame 2 (the layout), the 308
     register reads take ≈ 15 frames, and taps apply two frames after
     their sums, so frames 0–≈ 25 stay on the start taps whatever the
     loop does (the sums in the `Slots` stream, the follow-up above,
     would save the reads only).

  Order at the open: the probe's taps when its sums pass the slow loop's
  `SumsCheck` and its solve the gate's residual bound, else the
  process's last good set, else the identity (warned with the reason);
  loaded with INIT bit 0 as before, and the slow loop's good set. The
  acquisition's line names them; `chain.start_taps` in `--json`.

  On the board (2026-11-04, VERIFICATION.md "Build 9 residuals, host
  side"): 0 not-converged slots in each of 3 × 8 s and 30 s, 1.9–2.1 mean
  iterations from the first decoded frame, max RSS 0.68–0.71 GB. What was
  expected, and seen: the acquisition line ends "equaliser start
  taps solved from the acquisition's probe (its frame 2, 6720 training
  symbols: …) in ≈ 0.3 s", the open line "taps from the host";
  `chain.start_taps.source` "probe"; the status blocks' `taps` the
  probe's from frame 0 with `taps_from` none until the slow loop's first
  write, which stays solved from frame ≈ 5–6 and in force from frame
  ≈ 33–34 (≈ 26 with build 7's single reads: the verified reads, six iiod
  requests a word instead of four, cost 8 frames, as the timed host–FPGA
  model gives; build 10's register path read +33); that first solve taken
  by the gate against the probe's
  set (`solves_rejected` 0, `reverts` 0); the mean iterations of frames
  0–25 ≈ 2.1–2.3 as after it (4.1–4.8 on the identity) and no dropped
  slot in the first 34 frames; max RSS ≈ 0.6–0.9 GB, all at the open.

#### Chaos-found defects (HDL, 2026-11-04)

The chaos runs and the invariant monitors (fpga/README.md "Chaos runs",
"Invariant monitors") found these on the build 9 HDL; each is fixed with
a test that fails without the fix.

- **The `Slots` sequence across a START during a block.** `Packer`
  restarted the sequence only between blocks, and START is one cycle, so
  a START while a block left was lost for the sequence (8 of 80 chaos
  runs). The packer latches it: the block leaving finishes whole with
  its old number, the next has 0 (the model's new writer). The host's
  reader takes any sequence number, so the stream stayed parseable; a
  block cut at START would only have been skipped.
- **The status block's sync statistics after a restart just after a
  slot head.** The sync then locks with its heads 4 - 29 positions
  before its period's end, so a period end decides a few symbols after
  each head; stage 3 took the status snapshot when it queued the head's
  slot (about 80 cycles later), the period end's, and every later status
  block carried the next period's peak and sum. Stage 3 keeps the first
  snapshot after the head's record, the model's state after the head's
  push.
- **The status counters counted orphan slots.** The finish stage counted
  every slot it decoded, also those of a frame a restart dropped after
  some of its codewords were committed (the host discards those blocks),
  while the model counts whole frames (`chain::frame::Counters`). The
  model's documentation decides, and the HDL's change is small: a frame's
  slots are counted aside and added at its TMCC result (two 36-bit
  per-frame counts replace the 160 bits of counters held for a late TMCC
  result), so the HDL counts as the model; the chaos harness's
  known-divergence branch is gone.
- **A stop left the open codeword's bank held until START** (build 9:
  "1 of 3 banks held" at every open). Deselected, `SlotsRx` writes no
  symbol into its FIFO, so on the board a stop is the chain's input
  stopping while the front end runs on, as in the benches (no input
  records). `SlotsChain` now counts symbol clocks without an input record
  and after `STOP_AFTER` (131 072, 3.3 ms; the input's own gaps are a
  clock or two, and what the delivered input completes drains in about
  45 000) restarts as at a loss of 0 symbols: the open frame and its
  loading codeword are dropped and the back end's abort frees the bank,
  committed codewords leave as always. The model needs no change: a stop
  is invisible to it, and the START that follows is the same restart.
  An 18-bit counter; the stub bench (`ChainBench`, whose LDPC
  results come as one long run of records) has it off.

Stage 3 also no longer carries the snapshot with its slot queue: it
writes a frame's snapshot (`o_snap`) when the head of the frame's slot 1
gets its snapshot (the previous frame ends at the FIR after slot 0's
first symbols, so later), dropping its two 210-bit snapshot registers.
Fit of the four (Yosys, against `4eb0602`): `SlotsRx` without the decoder
26 487 → 26 461 LUT, 27 245 → 26 970 FF; `Finish` 5 832 → 5 809 LUT,
2 960 → 2 873 FF; block RAM and DSP unchanged.

**Build 10 on the board** (2026-11-04, VERIFICATION.md "Build 10 on the
board"): every constraint met after `holdfix.tcl` (WNS +0.041 ns, WHS
+0.046 ns; 43 792 LUT, 123.5 BRAM36, 176 DSP); 3 × 8 s back to back, 30 s
and an 8 s debug run on BS7: no bank held at any open (build 9 read
"held 1" at every open after the first), 0 dropped slots, 0–1 not
converged per run, the counters counting whole frames. The card keeps
build 9 as `isdbs3_rx_top.bin` (`uEnv.txt.prev-rx9`) and runs build 10 as
`isdbs3_rx10_top.bin`.

#### Sums in the stream (`Slots` format 2; 2026-11-04, simulation only)

The follow-up of "Build 8 issues", built on all sides; no image yet.

- **Format** (`isdb_frontend::slots`): version 2 = version 1 + a sums
  block (type 4) right after a decoded frame's status block: the frame
  summed (2 words, the header's frame), the count (1), then the 77
  complex sums, R's 66 upper-triangle entries in register order (`SUMS_R
  + 4i`) and p's 11, each re then im as three words (48-bit two's
  complement, low word first): 465 payload words, 476 with the header
  and CRC-32C trailer, about 14 400 words/s. Every block of a stream
  carries its writer's version; the slot, tmcc and status blocks are word
  for word version 1's. The host's `Reader` reads both versions; an
  intact block of a type its header's version does not define is
  `Event::Unknown` (skipped, counted, not a loss: a loss would close the
  TLV assemblers for an intact block). A version-1 host refuses a
  version-2 stream header by header and the image at open (VERSION and
  the ID word read 2), rather than run without the sums.
- **Model** (`isdbs3_phy::chain::out::blocks`): the block for every frame
  whose sums the equaliser latched (a decoded frame), after its status
  block; `eq::solve_block` solves it through `SumsBlock::to_training`,
  the registers' view, so both host paths share one conversion. The
  vectors carry it in every whole-chain set.
- **HDL** (`fpga/isdb_fpga/chain/sums.py`, `SumsReader`, hooked into
  `Finish` after the status block; `eq.py`'s second readback request):
  the latched bank an entry at a time through the same one-cycle steal of
  the accumulator RAM's read port as the register READ, the six words of
  an entry by a 6:1 select; VERSION 2, FEATURES bit 7 SUMS_STREAM, the ID
  word's bits 11-8 2; SUMS_CAPTURE and the registers kept. Bit-exact
  with the model on every set (chain-6 with the stub, chain-7 to chain-10
  with the real decoder, the chaos runs); three monitors (its word count,
  that it follows its frame's status block, that the latch holds still
  while it is read). Yosys: `SlotsRx` without the decoder 26 487 →
  26 641 LUT (+154), 27 245 → 27 415 FF (+170), BRAM and DSP unchanged
  (the estimate above: 150–250 LUT, 100 FF; the FF are 96 for the entry's
  register). fpga/README.md "Sums in the stream".
- **Host** (`isdb_radio::chain`, `isdb-cli` `slotrx.rs`): with VERSION 2
  and SUMS_STREAM the consumer hands each sums block to the slow loop
  (`ChainHealth::sums`) and the loop reads no register at all: the
  status blocks give it the frame, the layout, the lock and the carrier
  register (`SumsSource::Stream`; `sums_source` in the open line and in
  `--json`); the checks, the gate, the writes (11 taps, TAPS_FROM), the
  revert and the recovery are unchanged, `words_read`, `rereads` and
  `torn_reads` read 0. A format-1 image (build 9: VERSION 1, FEATURES
  0x220e73) keeps the register path exactly as before (tested through the
  mock register backend and recorded words of both formats). A stalled
  stream (found by the timed host–FPGA model): another client's
  SUMS_CAPTURE write latches one frame and holds the latch until the
  next write or a restart, so no sums block leaves while the status
  blocks still show decoded frames; the consumer counts the decoded
  frames since the last sums block, and at 90 (3 s) the slow loop warns,
  counts it (`sums_stalls`) and takes the sums from the registers (the
  register path, a capture armed and the 308 verified reads) until blocks
  come again at the free latch's rate (three between two ticks: a
  capture lets out one), then goes back to the stream. A strobe alone is
  no remedy: each SUMS_CAPTURE write holds again.

**On the board (build 11, 2026-11-04; VERIFICATION.md "Build 11 on the
board"):** the slow loop's first taps in force from frame 4 (+3 to +4
frames; +33 on the register path), 0 register words read, 183 sums blocks
per 8 s, 0 dropped slots; the same host on the format-1 build 10 takes
the register path as before. Build 12 (PR #40 as merged, the capture
hold released at a restart): the same figures, every constraint met in
the project flow (WNS +0.099, WHS +0.034 ns); the board runs it.

**When the first solved taps are in force.** Register path, measured
(build 7, and build 9 with the probe's taps: "Build 9 residuals"): the
first capture armed at the first tick latches frame ≈ 5, its 308 reads
take ≈ 15 frames, so the first write lands ≈ 0.9 s after the open and the
taps are in force from frame ≈ 26–28 (build 7's single reads; with build
9's verified reads, six iiod requests a word instead of four, the timed
model gives 1.16 s and frame 34, and the board's "Build 9 residuals" run
showed +33–34). Stream path, expected (not
measured: no image): the first sums block is frame 2's (the first frame
with a layout at lag 2), sent after frame 2's status block, about 4–5
slots into frame 3 (the TMCC burst and decode); the 64 Ki-word DMA buffer
holding it fills in ≈ 11 ms at the ≈ 6.4 M words/s of decoded frames,
plus ≈ 1 ms of network; the loop's thread wakes for it within 5 ms
(`SlowLoop::sums_waiting`); the 25 command words of the taps and TAPS_FROM
take ≈ 10 ms (≈ 0.4 ms per iiod access, from the 308-read set's ≈ 1 230
accesses in ≈ 0.5 s). TAPS_FROM 2 is then written ≈ 28 ms into frame 3,
inside its 33 ms, so the taps are in force from frame 4 (from inside frame
4 when a slower write lands after its start): ≈ 22 frames (0.73 s) sooner
than the register path's frame 26. With the probe's taps loaded at open
(already within 0.03 of a solve, "Build 9 residuals") the decoding of
frames 0–25 was already near the solved taps', so the gain is mostly the
time and the 308 exposed reads, not dropped slots. To check on the board:
the status blocks' `taps_from` (`examples/slotsstatus` on `--slots-out`
words) should read 2 from frame 4 or 5. The timed host–FPGA model
(fpga/README.md) measures it: frame 2's sums block at the consumer 10 ms
after the frame's end, TAPS_FROM 2 written 0.171 s after the open, in
force from frame 4 (4 or 5 on the reporter's slower network).
