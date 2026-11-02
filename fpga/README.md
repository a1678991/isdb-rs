# fpga — FPGA implementations of receive and transmit stages

Amaranth HDL for the stages of the receive and transmit chains that a
radio's FPGA can run instead of the host CPU (plan: the run-time choice
between CPU and FPGA implementations of a stage, like a SIMD kernel). Each stage has
a Rust reference model in the workspace that it must match **bit for
bit**. The model writes test vectors, and the HDL simulation here
reproduces them.

The first stage is the single-carrier front end of
[`isdb-frontend`](../crates/isdb-frontend/README.md) (IQ → `Symbols`,
format version 4) for ISDB-S and ISDB-S3 on the PlutoSky R2
(XC7Z020). It turns 8-bit IQ at 40 Msps (80 MB/s, more than the board's
Gigabit Ethernet path sustains) into 6-bit symbols at 43 MB/s (ISDB-S)
and 51 MB/s (ISDB-S3). The output bank `k` carries the host's timing
trim ([`isdb-frontend` README, "The timing
trim"](../crates/isdb-frontend/README.md#the-timing-trim)): the output
and the AGC come from `k`, the timing detector from `h` and `g`. The
block trailer is a CRC-32C (two words) that the packer updates one
16-bit payload word per clock: `crc32c_word` is a one-level XOR network
per register bit (11 to 22 inputs, 560 in all), derived from the
polynomial by symbolic simulation of the bit-serial register, not typed
in.

The second is the ISDB-S transmitter's pulse shaper (`SymbolsTx`,
[below](#the-transmit-block-symbolstx)): one 4-bit code per symbol in
from the TX DMA (14.4 MB/s), 12-bit I/Q at 40 Msps out to the DAC, in the
place of Tezuka's TX FIR interpolator.

**Clock.** The block runs on `util_ad9361_adc_fifo/dout_clk` =
`util_ad9361_divclk` = `axi_ad9361` l_clk / 4 (maia-hdl
`projects/common/xilinx_ad9361.tcl`: `SEL_0_DIV 4` with `lvds`, which
Tezuka's plutoskyr2 build sets; `/ 2` only when the AD9361 core's ADC
and DAC are both in r1_mode). Measured on the board (read-only, 2026-10-26):
`cf-ad9361-lpc` register 0x80000054 (`adc_clk_count`) = 0x1999A, i.e.
l_clk = 104 858 × 100 MHz / 2¹⁶ = 160.0 MHz; register 0x80000044
(control) bit 2 (`r1_mode`) = 0 on the ADC core (on the DAC core r1_mode
is register 0x80000048 bit 5, and 0x44 bit 2 is `ext_sync_disarm`; the
DAC's r1_mode reads 0 too, 2026-10-29, VERIFICATION.md "ISDB-S
transmitter in the FPGA: board checks"); `rx_path_rates` RXSAMP
40 MHz. So **the block clock is the sample rate, 40 MHz, one sample per
clock**, and `util_wfifo` hands samples over in bursts of eight on
consecutive clocks. (The format-3 design assumed four clocks per sample:
"160 MHz" was l_clk, upstream of the divider.) Format 4 is the design
for that rate: a fully pipelined core (a new symbol may start every
clock), the model's loop delay 24 symbols (≥ 24 clocks; the pipeline
needs 15), the output bank `k` on all 32 taps and the detector banks
`h`, `g` on their centre 8 (the model requires the rest zero), measured
on the recordings to cost nothing ([`isdb-frontend` README, "Format
4"](../crates/isdb-frontend/README.md#format-4-one-sample-per-clock)).

## Layout

| Path | Content |
|---|---|
| `isdb_fpga/frontend.py` | `SymbolFrontend`: mixer, polyphase filters (output bank `k` on every tap, detector banks `h`, `g` on the centre 8; one 64-entry coefficient ROM per tap and bank), adder trees, timing loop, AGC, quantiser, packer; one sample per clock, fully pipelined. The cycle-by-cycle schedule is in its docstring |
| `isdb_fpga/control.py` | `Control`: the command decoder and register file on `up_adc_gpio_out`, the ID word and readback on `up_adc_gpio_in` (`isdb_frontend::control`) |
| `isdb_fpga/top.py` | `SymbolsRx` and `SymbolsTx`: the blocks for the bitstream (core + control + output selection), and where they connect in Tezuka's block design |
| `isdb_fpga/tx.py` | `SymbolTx`: the transmit datapath (accumulator, code line, one RAMB18 per tap pair, multiplier-free terms, adder trees, rounding and saturation), the DMA unpacker and the counters; `TxControl` on `up_dac_gpio_out` / `up_dac_gpio_in`. The schedule is in its docstring |
| `isdb_fpga/txmodel.py` | a Python model of the transmit block's contract and the TX vectors writer (`python -m isdb_fpga.txmodel OUT.json [--preset isdbs\|stress]`), the stand-in for the Rust model's vectors |
| `isdb_fpga/sim.py` | simulation against a vectors file; `python -m isdb_fpga.sim VECTORS.json [--pattern every\|wfifo\|jitter] [--load-port] [--via-gpio]`; a TX vectors file: `[--pattern every\|rfifo\|jitter] [--latency N] [--load-port] [--via-gpio]` |
| `tests/test_isdb_fpga.py` | unit tests (the CRC-32C network against a bit-serial register and the standard check value) and the vector tests of both blocks |
| `synth/estimate.py` | Yosys `synth_xilinx` resource estimate (of `SymbolsTx` for a TX vectors file) |
| `synth/verilog.py` | Verilog of `SymbolsRx` or `SymbolsTx` (through the venv's Yosys) |
| `synth/tx_parts.py` | the transmit block's estimate split: the whole block, the core, the core with a constant SHIFT |
| `isdb_fpga/ldpc.py` | `LdpcDecoder`: the ISDB-S3 LDPC decoder ([below](#the-ldpc-decoder-ldpcdecoder)); its schedule, memory layout and pipeline in the docstring |
| `isdb_fpga/ldpcsched.py` | the decoder's static schedule computed at elaboration (layer address generation and its derivation, chain thresholds, hazard gaps) and `lane_decode`, the design's own scheme in Python |
| `isdb_fpga/ldpcmodel.py` | a Python model of the LDPC contract, the IRA encoder and the vectors writer (`python -m isdb_fpga.ldpcmodel --all build`; `--check VECTORS…`) |
| `isdb_fpga/ldpcqueue.py` | a model of the decoder's codeword queue over frames (K banks, the overrun rule, the TMCC staged or interleaved) |
| `isdb_fpga/simldpc.py` | the decoder against LDPC vectors files (`python -m isdb_fpga.simldpc VECTORS… [--max N] [--fill-known] [--all-rates]`) |
| `synth/ldpc_estimate.py`, `synth/ldpc_cycles.py`, `synth/ldpc_memories.py` | the decoder's Yosys estimate per S, its cycles per iteration per rate and S, and how Yosys maps each memory shape |
| `isdb_fpga/chain/` | the ISDB-S3 chain after the front end (stages 1–6, 8, 9; [below](#the-isdb-s3-chain-slotsrx)) |
| `synth/chain_estimate.py` | Yosys estimates of the chain's blocks |
| `tests/test_chain.py` | the chain's stage and chain tests against `chain-*.json` |

## Running

The Python environment is a uv project pinned in `pyproject.toml` /
`uv.lock` (Amaranth 0.5.10, YoWASP Yosys 0.70, which runs as WebAssembly
in user space). Nothing here needs a vendor toolchain:

```sh
mise run fpga:test         # vectors from the models, then the HDL simulation of both blocks
mise run fpga:estimate     # resource estimate of the 32-tap block
mise run fpga:verilog      # fpga/build/isdb_symbols_rx.v for a vendor flow
mise run fpga:tx-estimate  # resource estimate of the transmit block
mise run fpga:tx-verilog   # fpga/build/isdb_symbols_tx.v (the default bank as BRAM init)
mise run fpga:chain-estimate  # resource estimates of the ISDB-S3 chain's blocks
```

(`fpga:tx-vectors`, which `fpga:test` runs, writes the Python model's
two TX sets; the Rust model's TX vectors, in the same directory, are
simulated too. The tasks set `UV_DEFAULT_INDEX` to PyPI, the index
`uv.lock` records, unless it is already set.)

Results (format 4, 2026-10-26):

- **Simulation:** bit-exact on every vector set, with every symbol's
  internals (`y`, `dy`, `yk`, `z`, `dz`, `zk`, `q`, the loop errors) and
  every output word (trailers included) compared:
  - synthetic ISDB-S at 16 taps and ISDB-S3 at 32 taps, and both with a
    trim (`k` ≠ `h`), the default `mise run fpga:test` set;
  - with a sample on **every clock** (the most the core must take), with
    `util_wfifo`'s bursts of eight, and with irregular spacing plus the
    coefficients loaded through the port; with the whole configuration
    sent as the host's `gpio_out` command words;
  - recordings (opt-in vectors written from the captures with
    `cargo run -p isdb-frontend --example vectors`): 30 000 samples of the
    live 40 Msps BS7 recording (ISDB-S3, trim −0.174, 25 317 symbols)
    and of BS1 at 61.44 Msps (ISDB-S, 14 092 symbols), every-clock and
    burst patterns;
  - one LSB off in `k`, or in a centre tap of `g`, fails the comparison.
- **Estimate** of `SymbolsRx` (Yosys, before place and route):

  | Format, taps | DSP48E1 | LUT | FF | Block RAM |
  |---|---|---|---|---|
  | 4, 32 (one sample per clock) | 112 | 4.5k (4 487) | 5.4k (5 426) | 1 RAMB18 |
  | 3, 32 (four clocks per sample) | 64 | 4.2k | 4.6k | 1 RAMB18 + 8 RAMB36 |

  112 = 64 (`k`, I and Q) + 32 (`h`, `g` on 8 taps) + 16 (mixer, gain,
  detector and power products, loop gains). Tezuka's PlutoSky R2 build
  uses 106 of the xc7z020's 220: 28 `axi_ad9361`, 18 maia-sdr, 40 the
  DVB-S2 transmitter (encoder 2, RRC interpolator 38) and 20 the TX FIR
  interpolator. The isdb image leaves out the DVB-S2 transmitter
  (`apply.sh`), which reception does not use: 66 + 112 = 178 of 220
  (158 with the transmit block in the TX FIR's place, below).

## Chain vectors

The ISDB-S3 chain after the front end (docs/plans/isdbs3-fpga-rx.md,
stages 1–6 and 8; the HDL: [below](#the-isdb-s3-chain-slotsrx)) has its model in
`isdbs3_phy::chain`, whose module documentation is the specification
(every word width, rounding, threshold and lag rule), and its vectors
from `cargo run --release -p isdbs3-phy --example chainvectors --
fpga/build` (part of `mise run fpga:vectors`): one JSON file per stage,
`chain-1-sync` (NCO, slot sync, carrier loop: the input symbols, the
NCO output of every symbol, the correlations and accumulator of the
first 30 000 positions, every period's statistics, every head record,
every carrier update), `chain-2-phase` (slot phase and amplitude: two
slots between three heads, φ, gain and y per symbol), `chain-3-eq`
(the FIR's input and output over a frame with non-identity taps, the
training references, the 48-bit sums and the host's taps for them),
`chain-4-tmcc` (the TMCC symbols, soft bits, word positions, the decoded
bits and the layout), `chain-5-demap` (points, scale, symbols, LLRs in
label and codeword order, the permutation, the learned points and n0
for the next frame) and `chain-6-chain` (the whole chain's input symbols
and the expected `Slots` words, `isdb_frontend::slots`). Complex
sequences are flat `[re, im, …]` arrays; each file carries the chain's
constants (`"chain"`: shifts, gains, thresholds, the CORDIC table, the
sync words, the pilot signs, the cosine table) and its `"source"`. The
synthetic sets are 5 B44 frames (16APSK 93/120, −400 Hz drifting
−22 Hz/s, Es/N0 14 dB, 6-bit input); with a `Symbols` file as second
argument (and seconds as third, default 0.2) the same sets come from a
recording as `chain-rec-*.json` (opt-in, as the front end's). The
example's documentation gives every field. Sizes: 1–33 MB per file.

## The ISDB-S3 chain (`SlotsRx`)

The HDL of stages 1–6 and 8–9 of
[docs/plans/isdbs3-fpga-rx.md](../docs/plans/isdbs3-fpga-rx.md), one
module per stage in `isdb_fpga/chain/`, each reproducing the fixed-point
model `isdbs3_phy::chain` (its module docs are the specification; each
module's docstring states its contract by reference and its schedule):

| Module | Stage | Content |
|---|---|---|
| `nco.py` | 2a, 2b | the rotation by the carrier register (the front end's cosine table, round_shift 11), the loop update after 60 SSync phase steps (a bit-serial product by 246 412) |
| `sync.py` | 1 | `SlotSync`: FSync/SSync correlators as add/subtract trees on a = r_i + r_q, b = r_q − r_i, max + ½min, the 9 296-entry leaky accumulator in RAM, period peak and sum, lock/loss, parity, FSync sign (bit-serial), the frame-head counter and flywheel, the frame counter (a sequential divider), the head record (16-iteration CORDIC); `SyncFront` = NCO + `SlotSync`, the input held around each decision window, the status snapshot |
| `phase.py` | 3 | the slot ring (9 512 entries), per head the gain by a divider and the steps by bit-serial products, the interpolation as running sums, the conjugate rotation and the gain product, Q.10 |
| `eq.py`, `frame.py` | 4 | the 11-tap FIR (44 products), the taps (at once, or armed for a frame), the training sums time-multiplexed on one complex MAC into a two-bank accumulator RAM with a readback port; `FrameCtl`: the frame bookkeeping and the layout queue at the FIR's centre |
| `tmcc.py` | 5 | soft bits, the 31 680 × 8 staging buffer and its burst into the decoder (context 1, the shared port's free cycles); `TmccResult` (dispersal, BCH, the information bits); `TmccLayout` (`allocation_ok`, the 120 slot entries for frame + 2) |
| `demap.py` | 6 | the max-log metric against two 32-point register sets, the min trees, × K, the de-interleaved positions, the codeword cursor, the pilot sums and learned points, n0 and K at a frame's end |
| `back.py` | 4–6 | the equaliser with FrameCtl, the routing record through the FIR pipeline, TMCC and demapper on its output, the decoder port's arbiter |
| `ldpc_if.py` | (7) | the decoder interface (the chain's port), `LdpcBridge` to `isdb_fpga/ldpc.py`'s ports (async FIFOs, the serialiser, the tags in commit order), and how the stub works |
| `finish.py`, `gf.py` | 8 | the BCH unit (syndromes 16 bits per clock, Berlekamp–Massey on one GF(2¹⁶) multiplier, Chien one position per clock, the roots flipped as the Rust decoder does), the slot PRBS, `Finish` (the result stream into slot, TMCC and status blocks in frame order); `gf.py`: the field and a Python port of `isdb_fec::bch::decode` |
| `packer.py`, `slots.py` | 9 | the `Slots` block framing with the front end's CRC-32C network, the status block's words; `slots.py`: the format in Python (reader, CRC) |
| `top.py` | | `SlotsChain` (the stages wired, the status snapshot), `SlotsRx` (the sketch of the bitstream block: front end → FIFO → chain → bridge → decoder, the register file, the output selection) |
| `sim.py`, `stages.py`, `simx.py`, `harness.cc`, `bchsim.py` | | simulation against the vectors (`python -m isdb_fpga.chain.sim VECTORS.json [--backend pysim\|cxxrtl] [--symbols N]`) |

**Clocks.** Everything runs at one symbol per clock in the 40 MHz symbol
clock (`util_ad9361_divclk`) but the LDPC decoder (`ldpc` domain, 200 MHz),
which `LdpcBridge` reaches through two async FIFOs (512 beats in, 64
words out) and a tag FIFO; no other signal crosses. Assumed faster clocks:
none in these blocks. The demapper's 64 products are at the symbol clock
(the plan's 16 at 4× is not done), the FIR's 44 likewise (22 at 2× not
done), and the BCH's Chien search runs at one position per clock, enough
only while few slots need it (a frame of 96 slots all with errors would
need 3.9 M cycles: 4× the clock or 4 positions per clock for the worst
case; on the lab signal no slot has needed BCH after a converged LDPC).
Stage 1 holds the input for about 0.7 % of the clocks (its decisions) and
stage 3's ring when its reader stalls; `SlotsRx` puts a 1 024-symbol FIFO
in front, the front end delivering 84 % of the clocks.

**Simulation** (`tests/test_chain.py`, part of `mise run fpga:test`): two
backends behind one stream API (`simx.py`): Amaranth's Python simulator
(the default; about 6 500 cycles/s on the stage-1 block, 2 000 on the
back end) and an optional compiled one, CXXRTL from the venv's Yosys and
the system C++ compiler (about 1.1 M cycles/s; built and cached in
`build/cxxrtl/` on first use, 5 s to a few minutes per block;
`simx.available()` decides, the tests that need it skip without it;
`FPGA_SIM=cxxrtl` makes it the default of `python -m isdb_fpga.chain.sim`).
Measured on this host, 2026-11-02, the synthetic sets (`chainvectors`, 5
B44 frames of 16APSK 93/120, −400 Hz drifting, 14 dB):

| Stage | Vectors | Compared | Python simulator (default test) | Compiled |
|---|---|---|---|---|
| 1 + 2 | chain-1-sync | r of every symbol and φ every 4 096, the correlations and accumulator of the first 30 000 positions, every period (241), every head (236), the carrier update | 120 000 symbols (lock, parity, 8 heads with FSync signs), 18 s | the whole set, 2 241 336 symbols (the frame head, the carrier update), 2 s |
| 3 | chain-2-phase | φ, gain, y, r of 18 592 symbols | the whole set, 3 s | 0.1 s |
| 4 | chain-3-eq (+ chain-4's layout) | z of the frame's 1 115 520 symbols, the 6 720 training references, the 308 sum words, count and frame | 18 592 symbols (2 slots' z, references and MACs), 13 s | the whole frame, 4 s |
| 5 | chain-4-tmcc | 31 680 soft bits, the 31 682 burst beats (port granted on half the cycles), 9 422 information bits, the result, 120 layout entries and their commit | the whole set, 40 s | 2 s; also with 7 bit errors corrected |
| 6 (4–6) | chain-3 → chain-5 (+ chain-4) | all 96 codewords' 1 077 120 beats (count, flags, tags), both codewords chain-5 carries in full (every position and LLR, the de-interleaved words), 16 learned points, n0 and K for the next frame, the TMCC soft bits | 12 000 symbols (the first codeword whole), 7 s | the whole frame, 9 s |
| 8 (BCH) | gf.py (the Rust decoder's port) | 32 random words of 9 614 and 34 776 bits with 0–20 errors: result, count, roots | 2 words | 32 words |
| bridge | — | the 40 MHz port to the 200 MHz decoder ports and back, two clocks | the test | — |
| 1–9 | chain-6-chain (+ chain-3's taps) | every `Slots` word: 5 587 896 symbols in, 211 281 words out (3 frames: TMCC and status blocks; frame 2 decoded, 120 slot blocks), the LDPC stub answering each frame after its TMCC codeword is committed | — | 2 260 000 symbols (frame 0's blocks, 683 words) by default, about a minute (after the first build of the chain's library, about 3 minutes); the whole set (`FPGA_CHAIN_FULL=1`) 171 s, about 38 000 cycles/s |

**Estimates** (`mise run fpga:chain-estimate`: Yosys `synth_xilinx`,
before place and route; 6-bit input) against the plan's budget:

| Block | LUT | FF | BRAM36 | DSP48E1 | Plan row: LUT / BRAM36 / DSP |
|---|---|---|---|---|---|
| `SyncFront` (NCO, slot sync, carrier loop) | 4 197 | 4 626 | 5.5 | 7 | sync + carrier + phase: 3 000 / 9 / 8 |
| `SlotPhase` | 1 122 | 2 116 | 8 | 8 | (same row) |
| `Equaliser` + `FrameCtl` | 2 685 | 4 546 | 7.5 | 50 | equaliser: 2 500 / 1 / 44 (22 at 2×) |
| `TmccSoft` | 134 | 65 | 8 | 0 | TMCC + demapper + accumulators: 3 500 / 2 / 16–64 |
| `Demap` | 14 324 | 11 409 | 7 | 95 | (same row) |
| `Finish` (`TmccResult`, BCH, PRBS, packer, status) | 7 282 | 3 337 | 2 | 0 | finish + packer + registers: 3 000 / 4 / 0 |
| **sum** | **29 744** | **26 099** | **38** | **160** | 12 000 / 16 / 68–116 |

(The whole `SlotsChain` in one run, `chain_estimate.py chain`, does not
finish: abc9 in the 32-bit WebAssembly Yosys runs out of its 4 GB.)

Over the budget, and where it goes: the demapper's ten 32-bit min trees
(about 9 000 LUT) and the per-symbol choice between its two point sets,
its 64 + 2 + 10 products (the five × K products are 31 × 25 bits, two DSP
each; the position products `col · rows` are by small variables Yosys maps
to DSP); the stage-3 ring as 16 RAMB18 (9 512 × 22 bits; storing the 12-bit
input and rotating on the read side would halve it); the TMCC staging
buffer (7 RAMB36, the coordinator's design); the nominal-point ROMs (4 096
× 32 bits, twice: 2 560 × 24 would do); the BCH unit's 24 XOR networks and
GF multiplier and the status block's word muxes. The budget's DSP total
assumed the demapper at 4× and the FIR at 2×.

**What is left** (2026-11-02): the result reorder in `Finish` (the
decoder's commit order puts frame N's TMCC after frame N + 1's first slot
results; the model's block order needs those held, about 3 slots of
results, or a format that lets a frame's TMCC and status follow later:
the integrator's call); the restart epoch for a layout committed after a
restart (the model's insert happens before it); the chain run through
`LdpcBridge` with the real decoder (`isdb_fpga/ldpc.py`); `SlotsRx` as a whole in
simulation (its register file is simulated through `gpio_out`:
`SlotsRxRegisterTest`; the CARRIER register has no write port yet); a loss in
the input stream (`i_loss`) and a sync loss in simulation (the synthetic
sets have neither); the 2×/4× clocks for the FIR and the demapper, and
the LUT reductions above; the recording sets (`chain-rec-*`, opt-in)
were not generated here.

## The transmit block (`SymbolsTx`)

The last stage of the ISDB-S transmitter (plan
[isdbs-fpga-tx.md](../docs/plans/isdbs-fpga-tx.md), step 5): the host
keeps the coder (`isdbs_phy::tx`) and sends one code per symbol; the
block maps the codes to the eight points e^{jkπ/4} that every ISDB-S
symbol is on and interpolates them with the root-raised-cosine pulse to
the DAC rate (40 Msps at 28.86 Mbaud, 1.386 samples per symbol). The
model it must match bit for bit is `isdb_frontend::shape::fixed`
(Rust); `isdb_fpga/txmodel.py` states the same contract in Python and
writes vectors of the same shape, which the HDL was built and checked
against before the Rust vectors existed.

- **Codes.** 4 bits per symbol: 0 = the zero symbol, 8 + k = the point
  e^{jkπ/4}, 1..7 reserved (zero). One DMA sample (the I and Q words
  `util_upack2` delivers per `fifo_rd_en`) carries 8: codes 0..3 in the
  I word (code 0 in bits 3..0, first in time), 4..7 in the Q word.
- **Datapath** (`tx.py` docstring: the schedule, D0..D9). Q0.24 phase
  accumulator (`step = round(2²⁴·R/fs)`), a line of 2S = 24 codes,
  P = 512 rows at (q + ½)/P picked by q = frac[23:15]. Per tap one ROM
  value, bank a for even k and b = a/√2 for odd k, and a sign or zero per
  component: no multiplier. The bank is exactly symmetric
  (a[q][t] = a[P−1−q][2S−1−t]), so one true-dual-port RAMB18 per tap
  pair holds a and b for both taps (port A at {k_t & 1, q}, also the load
  port; port B at {k_u & 1, ~q}). A term is v, ~v (= −v − 1) or 0 and
  the count of negated terms is one more leaf of the 25 → 1 adder tree
  (exact, and no carry chain per tap); `round_shift` by the run-time
  SHIFT, saturation to 12 bits, `dac_data[15:4]`. No forced wrap at
  START: the line is all zero, frac 0, sample 0 is 0. Latency 9 enabled
  clocks, constant; nothing feeds back.
- **Clock and rate.** The DAC unpacker's clock, `util_ad9361_divclk`
  (the RX block's too: the sample rate, 40 MHz). One output sample per
  request strobe (`util_rfifo`'s `din_valid_0`, bursts of eight that
  average the sample rate): the whole pipeline is clock-enabled by it,
  as Tezuka's FIR runs on `dac_valid` in this place. While selected the
  block holds the rfifo's `din_valid_in` high (as txfir.tcl's
  `dac_fifo_valid_or`), so the rfifo writes on every clock; since the
  din clock is the DAC rate, its request is high on every clock in steady
  state and the two agree (a skipped strobe would repeat a sample, as it
  would with Tezuka's FIR).
- **DMA words.** An 8-code unpack register and one staging word: the
  block keeps two words held or requested (`fifo_rd_en`, one pulse per
  word) and takes the answers in order at any fixed latency (one clock
  at the upack's default PIPELINE_STAGES 0; tested at 1 and 3). A word
  lasts at least 8 samples, so the symbol cadence never waits on the
  DMA. While running, an answer with `fifo_rd_underflow` instead of
  `fifo_rd_valid` is a word of 8 zero symbols. Each symbol counts as it
  enters: a missing word's in MISSED (FIRST_MISS latches CONSUMED at the
  first, the sticky underrun bit is set; FIRST_MISS reads 0 before any
  miss), a delivered word's in CONSUMED. Counting at entry makes the
  counters a function of the sample count, independent of the prefetch.
  CONSUMED, FIRST_MISS and SAMPLES are 64 bits (MISSED 32).
- **Prefetch while selected.** Requests start 4 clocks after SELECT
  (an answer to a pass-through read is never taken for the block's
  own), not after RUN: selected and not running, the block polls the DMA,
  drops the underflow answers (they store and count nothing but PRE_UNF)
  and keeps up to two words. STATUS bit 3 PRIMED says a delivered word is
  held; CTRL RUN|SELECT then starts the datapath on words in hand, so the
  start misses nothing. A DMA that has no word yet when RUN comes is
  still counted (the answers after RUN are missing words, as before).
  CTRL 0 (deselect) drops the held words and ignores the answers to
  requests under way, so a new stream never plays the last one's words.
  Samples start once the first word (or, running, a miss) is in. START
  drops the rest of a word whose codes have begun to enter and keeps the
  words the model has not asked for yet (the prefetched ones), as the
  model's START leaves the DMA queue as it is. After START the output is
  0 until the first new sample arrives; while not running it is 0. The
  model sees the prefetch as `Interpolator::prefetched(k)`: the stalls on
  the k words held at RUN were met before it.

| Port | Connects to |
|---|---|
| `req` | `axi_ad9361_dac_fifo/din_valid_0` (the rfifo's request) |
| `rd_en` | `util_ad9361_dac_upack/fifo_rd_en` (`req` itself when not selected) |
| `rd_data0`, `rd_data1`, `rd_valid`, `rd_underflow` | `util_ad9361_dac_upack/fifo_rd_data_0`, `_1`, `fifo_rd_valid`, `fifo_rd_underflow` |
| `pass_data0`, `pass_data1`, `pass_valid` | the same unpacker nets (today's path, passed through) |
| `out_data0`, `out_data1`, `out_valid` | `axi_ad9361_dac_fifo/din_data_0`, `_1`, `din_valid_in_0` and `_1` (valid high while selected) |
| `gpio_out` | `axi_ad9361/up_dac_gpio_out` (bit 0 not used: txfir's `active` in the stock image, also written by the kernel's interpolation attribute) |
| `gpio_in` | `axi_ad9361/up_dac_gpio_in` (unconnected in Tezuka v0.3.24: reads 0, VERIFICATION.md "board checks") |
| clock, reset | `util_ad9361_divclk/clk_out`, `util_ad9361_divclk_reset/peripheral_reset` (the unpacker's) |

Channels 2 and 3 (TX2), the channel enables and `din_unf` (the DAC
core's underflow flag, 0x80000088 bit 0: it now also reports the
block's missed words) stay as `xilinx_ad9361.tcl` connects them;
`fpga/tezuka/isdb_symbols_tx.tcl` has the exact connections. The host
enables `voltage0` and `voltage1` of `cf-ad9361-dds-core-lpc` (both
words of a sample are codes; with `voltage0` alone Q0 would come from
the DDS).

**Control** (`TxControl`): the RX block's command protocol (`control.py`:
two-register synchroniser, toggle bit 31, ops NOP 0 / ADDR 1 / HI 2 /
WRITE 3 / READ 4 / CTRL 5 / WRITE_S 6, argument in bits 27..1, bit 0
untouched) on the DAC core's GPIO pair, `cf-ad9361-dds-core-lpc` debug
addresses 0x800000BC (`gpio_out`) and 0x800000B8 (`gpio_in`). The ID word
after any command but READ, and after reset, is 0xB5D11918: magic
0xB5D1, version 1, log2 P = 9, 24 taps; a bitstream without the block
reads 0.

| Address | Register |
|---|---|
| 0x00 | STEP (24 bits kept) |
| 0x01 | SHIFT (5 bits) |
| 0x02 | STATUS, read only: bit 0 run, bit 1 selected, bit 2 underrun (sticky until START), bit 3 primed (a delivered DMA word held) |
| 0x03 / 0x05 / 0x06 | CONSUMED / FIRST_MISS / SAMPLES (outputs since START), read only, 64 bits: a READ shows the low 32 and latches the high 32 in HI |
| 0x04 | MISSED, read only, 32 bits |
| 0x07 | FEATURES, read only: 7 = bit 0 prefetch (PRIMED, PRE_UNF), bit 1 BANK_TAG, bit 2 64-bit counters (HI); the image before them reads 0 here |
| 0x08 | BANK_TAG: the bitstream's value is the default bank's tag (`tx.bank_tag`: CRC-32C of the near halves as `Encoder::configure_tx` writes them, a then b, i32 LE; 0 ↦ 1); any bank write sets 0; WRITE sets it; no reset (the memories have none either) |
| 0x09 | HI, read only: the high 32 bits latched by the last READ of CONSUMED, FIRST_MISS or SAMPLES (LO then HI is one snapshot) |
| 0x0A | PRE_UNF, read only: underflow answers dropped before RUN since START (32 bits, wraps; a diagnostic of the start) |
| 0x4000 + q·32 + t | bank a, row q, tap t < 2S (a far tap t ≥ S writes its mirror, so either half loads the bank) |
| 0x8000 + q·32 + t | bank b, likewise |
| CTRL argument | bit 0 RUN, bit 1 START (one pulse: frac, the line, the counters, the unpack register), bit 2 SELECT (the block drives the rfifo and owns the upack's read; it polls the DMA from here on) |

Other addresses read 0. The ID word stays 0xB5D11918 (the arithmetic and
the code format are unchanged); a host tells the images apart by
FEATURES.

The default bank (ISDB-S, 40 Msps, 10 dB backoff) is the memories'
initial content (`mise run fpga:tx-verilog` takes it from the Rust
model's `tx-1-continuous.json` and refuses another design). The deployed
image (2026-10-29) has none of the features above: the host writes both
banks on every open (`Encoder::configure_tx`: STEP, SHIFT, the near half
of each row, 13 315 commands, 3.9 s through iiod on the bench), since
the memories keep the last bank loaded until the FPGA is reconfigured.
The Python model's encoder (`txmodel.py`) ends its list with CTRL START,
CTRL RUN|SELECT; the host instead sends CTRL START|SELECT (selected, not
running: that image requests nothing and sends zeros), pushes half its
kernel queue, then CTRL RUN|SELECT. That image's first two requests go
out 4 clocks after RUN; on the board one or both find no word (8–16
symbols in MISSED at FIRST_MISS 0, in the lead-in; VERIFICATION.md "on
the board"). CTRL 0 makes the block transparent again.

The image with the features (built from this source; FEATURES 7): the
host reads FEATURES and BANK_TAG at open and writes STEP and SHIFT alone
when BANK_TAG already reads the bank's tag (the default bank at
power-up, or the bank an earlier open loaded and tagged), else the whole
configuration and then the tag (an interrupted load leaves 0, so the
next open reloads); after CTRL START|SELECT the block polls the DMA, and
the host sends CTRL RUN|SELECT once half its queue is pushed and STATUS
reads PRIMED: MISSED 0 at the start. The old host's sequence (no PRIMED
read, a fixed wait) gets the same: the words are in the block before
RUN. CONSUMED and FIRST_MISS come with their HI words; on the old image
the host extends the 32-bit values itself (they wrap every 148.8 s at
28.86 Mbaud, SAMPLES every 107.4 s at 40 Msps).

Results (2026-10-29; the features 2026-10-31), on the Rust model's
vectors (`mise run fpga:vectors`: `tx-1-continuous` … `tx-4-bank` and
`tx-6-prefetch`, from `isdb-frontend`'s `txvectors`, and `tx-5-isdbs`,
superframe codes from `isdbs-phy`'s transmitter; 1 442 to 5 578
samples, 24 stalls in `tx-2-stalls`, one long one in `tx-3-long-stall`,
61.44 Msps at 3 dB with 12 clipped samples and a bank that is not the
default in `tx-4-bank`, stalls on words 0, 1 and 60 with `"prefetch":
2` in `tx-6-prefetch`, where only the last is seen) and on the Python
model's two (`txvec-py-1-isdbs`, ISDB-S with 5 stalls of 1 to 6
requests, and `txvec-py-2-stress`, a symbol on all samples but one,
SHIFT 2 below the bank's so that 70 % of the samples saturate, a stall
at the first word, the DMA dry before the end and the run ending inside
a missed word):

- **Simulation:** bit-exact on all eight, every sample's frac, q, line,
  carry and entered code, its sums before rounding, its output, and the
  four counters: with a request on every clock, with `util_rfifo`'s
  bursts of eight (and pauses), with random strobes and a 3-clock upack;
  with the vectors' whole command list (STEP, SHIFT, both banks' near
  halves: 13 317 words from `Encoder::new(0)`) sent on `gpio_out` into
  empty memories, the ID word and the readback of STEP, SHIFT, STATUS and
  the counters on `gpio_in`; with the bank loaded through the port (every
  tap, and the near half only); with START while running (each set once,
  the first at 12 consecutive restart points; against the Python model
  with the same restart). One LSB off
  in a bank-a entry, in a bank-b entry, or in a far tap written through
  the port fails the comparison, and so does one stalled request more
  than the vectors say. The Python model reproduces the Rust model's
  banks, samples, counters, command words and bank tags exactly.
- **The features** (`TxPrefetchTest`, `TxRegisterTest`): PRIMED within
  ARM + latency + 2 clocks of SELECT; 300 clocks of a dry DMA before RUN
  drop their polls and miss nothing; a DMA later than RUN is counted as
  the model with that many stalls before word 0; CTRL 0 mid-stream then
  START|SELECT plays the DMA's next words, none of the held ones; the
  deployed host's sequence (START|SELECT, a fixed wait, RUN|SELECT) with
  the DMA silent for 12 clocks right after RUN (where the old image's
  first requests fall) misses nothing and PRE_UNF counts the dropped
  polls; FEATURES 7; BANK_TAG initial value (= the Rust model's tag),
  cleared by a bank write, set by a WRITE, no reset; CONSUMED and
  SAMPLES carried past 2³² and HI latched by the counter's READ, kept
  over other reads. `mise run fpga:test`: 33 tests (24 before) in
  ≈ 4 min.
- **Estimate** of `SymbolsTx` (Yosys `synth_xilinx`, before place and
  route): **0 DSP48E1, 2 793 LUT, 3 110 FF, 12 RAMB18E1**, 349 CARRY4,
  no LUTRAM, with the features (without them, the deployed image's
  source: 2 713 LUT, 2 904 FF, 324 CARRY4; so +80 LUT, +206 FF: the
  64-bit counters, HI, BANK_TAG, PRE_UNF and the prefetch's control).
  The datapath alone (the throwaway sketch,
  `synth/tx_sketch.py mux 512 12`) was 1 962 LUT / 2 266 FF; the rest is
  the run-time SHIFT's barrel shifter, the control and readback, the
  unpacker, the counters and the clock enable (`synth/tx_parts.py`
  splits it). Device: 5 % of the xc7z020's LUTs.

## LDPC vectors

The ISDB-S3 LDPC decoder (in progress,
[plan](../docs/plans/isdbs3-fpga-rx.md) "LDPC decoder" and "The LDPC
schedule contract") is checked against the fixed-point model
`isdb_fec::ldpc::fixed` with its `Schedule::Layered { step }`, whose
step writes are the delta write-back of the amended contract (a
variable with edges e₁ … e_m in a step: sat(t₁ + r₁,new + Σᵢ₌₂
(rᵢ,new − rᵢ,old)) in i16, saturated once; m ≤ 3 in the B44 codes).
`mise run fpga:vectors` writes, from `isdbs3-phy`'s
`examples/ldpcvectors` (format version 1 in its module docs):

- `ldpc-tables.json`: the eleven address tables of ARIB STD-B44 v2.1
  §3.4 (2) (`{"rate": 41, "rows": [[…], …]}` …, one row per 374-bit
  group), so the HDL elaborates its address ROM and layer order from the
  same source as the model;
- `ldpc-1-93-seq` (sequential schedule, limit 50), `ldpc-2-93-layered34`
  and `ldpc-3-93-layered187` (limit 12), `ldpc-4-41-layered34` and
  `ldpc-5-109-layered34` (the extreme rates), `ldpc-6-tmcc-layered34`
  (the shortened 61/120 TMCC code: `"shortened": {"info_start": 1870,
  "info_len": 9614}`, the known zeros carried as +127),
  `ldpc-7-93-stress-layered34` (a word valid at iteration 0, all-zero
  input, ±127 with strong flips, −128 inputs, failing words),
  `ldpc-8-93-trace` (2 codewords with the hard decisions after every
  iteration, `trace_hex`, to locate a mismatch), `ldpc-9-93-iter0`
  (`max_iter` 0); with `ISDB_RS_ISDBS3_TLV_IQ` set, also
  `ldpc-10-93-rec-layered34` and `ldpc-11-tmcc-rec-layered34` (real
| `ldpc-12-93-layered17`, `ldpc-13-tmcc-layered17` | 93/120 and the TMCC code, layered step 17 (374 = 2 · 11 · 17), limit 12, with traces: small steps for the Python simulation |
  demapped codewords of its first recording). 4 to 8 codewords per file,
  0.5–1.3 MB each.

Each codeword carries its n input LLRs (i8) and the expected n hard
decisions (`bits_hex`: bit i at byte i / 8, bit i % 8, lowercase hex),
iterations, convergence and the unsatisfied-check count. Every set is
decoded with 16 lanes and with one, the same. `tools/ldpc-vectors-check.py
TABLES VECTORS…` is an independent pure-Python reading of the contract,
built from the tables file alone, that reproduces every set (2026-11-01:
all eleven, 68 codewords, traces included) — a starting point for the
HDL's Python stand-in model. The front end's vector test skips
`ldpc-*.json`.

## The LDPC decoder (`LdpcDecoder`)

The ISDB-S3 inner decoder of the receive chain
([plan](../docs/plans/isdbs3-fpga-rx.md) §7 and "The LDPC schedule
contract"): layered offset min-sum in i8, `Schedule::Layered { step: S }`
with the delta write-back, S check-node units, early stop and a fixed
limit, for all eleven B44 rates (the address tables as ROM, built at
elaboration from `ldpc-tables.json` or `ldpc_tables.rs`) and the
shortened TMCC code. `isdb_fpga/ldpcmodel.py` states the contract in
Python (and reproduces every Rust set, traces included);
`ldpcsched.lane_decode` is this design's own scheme in Python, checked
against the contract; `ldpc.py`'s docstring has the pipeline cycle by
cycle.

- **S must divide 374** (11, 17, 22, 34, 187): bit j of a 374-bit group
  is in lane j mod S, word j div S, so a step's reads of one table entry
  are every lane once, at one of two words, rotated by the entry's shift
  mod S. 187 is the largest the chain handling allows (a pair at shift
  difference E shares a bit in one step in one direction only when S ≤
  187).
- **Layer address generation** (derivation in `ldpcsched`'s docstring,
  verified edge for edge against the code's checks for every rate and
  S = 11, 17, 34, 187): check i of layer l (c = l + i·q) has, for each
  table entry x of row g with x ≡ l (mod q), bit j = (i − x div q) mod 374
  of group g (since c = (x + j q) mod P ⇔ i = (x div q + j) mod 374), plus
  p_c and p_{c−1}. The parity bits are stored permuted: p_{l + i q} is
  bit i of parity group G + l, which makes the staircase quasi-cyclic
  (p_c: group G + l at shift 0; p_{c−1}: group G + l − 1 at shift 0, or
  in layer 0 group G + q − 1 at shift 1, check 0 having none). The output
  stream follows the storage (the information bits in order, then the
  parity group by group); `simldpc` un-permutes it to the vectors' order.
- **Edge order** inside a check is free (min1/min2 and the parity depend
  on the multiset of t; a tie gives every edge the same magnitude), so a
  layer's entries are ordered by group, the groups the previous layer
  wrote last (fewer hazard stalls), and a group's entries consecutive.
- **Delta write-back**: each lane keeps the unsaturated sums it wrote
  for the last two entries of the step; an edge whose bit an earlier
  check of the step shares contributes r_new − r_old, else t + r_new,
  and adds onto the partner's sum when it shares the bit; the memory
  gets sat8 of the sum. Per-entry thresholds on the unit index from a
  small chain ROM (428 chain entries over the eleven rates) select the
  modes. Chains of three (81, 89, 105/120) are covered; their check-order
  precedence can be cyclic (93/120 at S = 187), which is why the lane
  sums in write order.
- **Syndrome**: every stored bit also keeps H, the hard decision of the
  previous iteration (written with each write: the sign of L at the bit's
  first read in the pass, else H). Pass p's check-node XOR of the old
  decisions is the syndrome of iteration p − 1, so a codeword converging
  after i iterations runs i + 1 passes and its result is H.
- **Shortened (TMCC)**: with `st_short`, the decoder writes +127 into the
  13 200 known-zero positions itself (S per cycle, 1 320 cycles at S = 17)
  and only the 31 680 sent LLRs are loaded; the tests check the file's
  values there are +127 and run both ways.
- **The codeword queue and the iteration limit.** The limit is a
  run-time input per codeword (`st_max_iter`; the chain's register
  resets to 16). K L/H banks (a build parameter, **default 3**): a
  codeword holds a bank from its `begin` until its hard decisions have
  left; the engine decodes the committed codewords in commit order and
  the results leave in that order. The input never stalls; `load_ready`
  (a bank is free) is the back-pressure, and a codeword whose `begin`
  comes while all K banks are held is dropped: `overruns` counts it and
  its place in the result sequence still gets a record (res_overrun 1,
  converged 0, iterations 0, no data words), exactly like a slot whose
  LDPC failed, so the dispersal and the host's slot count stay in step.
  **Why K = 3:** a codeword is complete only at its slot's end (the
  interleaver spreads it over the slot, T = 33.05 ms / 121 ≈ 54.6k
  cycles at 200 MHz), so with K banks one codeword may take about
  (K − 1)·T less the output (2.8k cycles) and any queue wait: K = 2
  gives 9 passes at 93/120, S = 34 (8 iterations, the worst-case figure
  again), K = 3 about 19 (a limit of 16 for an isolated long codeword).
  The frame-average budget is 6.61 M cycles / (121 × 5 420) ≈ 10.1
  passes per codeword against a measured mean of 3.1 (2.1 iterations
  + 1): a codeword may run to the limit as long as the frame's total
  stays inside the budget. Not a symbol-level queue before the demapper
  (it would need a demapper faster than one symbol per clock), not
  limit 8 (1 338 failures of 18 336 near threshold against 3 at 16).
  **The TMCC codeword** is staged by the TMCC stage across the frame and
  loaded as a burst after the frame's end, on context 1 (`in_ctx`), its
  writes sharing the single input port with the slot LLRs cycle by cycle
  (the port carries one write per cycle; the slot LLRs use ≈ 0.81 of the
  cycles at 200 MHz, so the 31 680-write burst takes ≈ 167k cycles, about
  three slots); the default K = 3 assumes that. Interleaved TMCC LLRs
  would hold a bank for the whole frame and need K = 4.
  `isdb_fpga/ldpcqueue.py` models the queue over frames (tests
  `LdpcQueueModelTest`, S = 34, 200 MHz, 93/120, limit 16):

  | frames (×3) | K = 2 | K = 3 | K = 4 |
  |---|---|---|---|
  | measured distribution (mean 2.1, three isolated codewords at 16), TMCC staged | 13 overruns of 363 | **0** | 0 |
  | the same with two adjacent pairs at 16, TMCC staged | 16 | 6 | 0 |
  | the measured distribution, TMCC interleaved | 68 | 7 | 0 |
  | all 120 slots at the limit (×2), TMCC staged | 104 of 242 | 96 | 95 |

  (K = 2 overruns even without long codewords: 4 in 3 frames, the TMCC
  burst holding a bank across three slots.) The HDL tests check the
  overrun record in order (K = 2, a codeword begun while both banks are
  held), two contexts open at once with the results in commit order, and
  the limit as a run-time input (toy codewords at 16 against the model).
- **The contract points this design relies on** (agreed with the Rust
  model's owner): the edge order inside a check is free (ties give every
  edge the same magnitude); the output stream is in storage order, so its
  first k bits are the information bits in natural order and the parity
  follows layer by layer; a shortened codeword's known zeros are written
  as +127 by the decoder itself.
- **Memory**: K banks of S lanes, each 120 × Wp words of 9 bits (L and H,
  Wp = W rounded up to a power of two); the check records (m1, m2 after
  the offset, the min1 index: 19 bits) one word of 19 S bits per step; the
  edge signs one word of S bits per (step, entry); a 64-deep FIFO between
  the read and write phases (LUTRAM); the ROMs (entries, chains, gaps).
- **Clock**: designed for 200 MHz (each stage at most one carry chain
  plus a few LUT levels; the rotators log2 S levels of 2:1 muxes). Not
  yet timed by Vivado.

**Cycles per iteration** (`synth/ldpc_cycles.py`: the static schedule's
steady state, the hazard gaps included; the read and write phases of
consecutive steps overlap, a step taking d cycles, d the layer degree)
and the iterations per codeword if all 121 codewords of a frame (120
slots + the TMCC) ran equally long, one pass more than the iterations
(the syndrome of the last one): the frame-average budget, not a limit
(the queue above lets single codewords run longer):

| S | rate | d | cycles per pass | iterations per codeword, 100 MHz | 200 MHz |
|---|---|---|---|---|---|
| 17 | 41/120 | 5 | 8 702 | 2.1 | 5.3 |
| 17 | 93/120 | 16 | 10 199 | 1.7 | 4.4 |
| 17 | 109/120 | 32 | 8 438 | 2.2 | 5.5 |
| 22 | 41/120 | 5 | 6 727 | 3.1 | 7.1 |
| 22 | 93/120 | 16 | 8 007 | 2.4 | 5.8 |
| 34 | 41/120 | 5 | 4 357 | 5.3 | 11.5 |
| 34 | 61/120 (TMCC) | 7 | 4 842 | 4.6 | 10.3 |
| 34 | 93/120 | 16 | 5 420 | 4.0 | **9.1** |
| 34 | 109/120 | 32 | 4 315 | 5.3 | 11.7 |
| 187 | 41/120 | 5 | 1 291 | 20.2 | 41.3 |
| 187 | 93/120 | 16 | 1 150 | 22.8 | 46.5 |

(The plan's estimate, P/S steps × (d + 6), counted the pipeline per
step; here only the hazard gaps add to d per step: 84 of 297 steps at
93/120, S = 34, wait, 668 cycles in all.)

**Estimate** (`mise run fpga:ldpc-estimate`, Yosys `synth_xilinx`, all
eleven rates, before place and route), against the plan's budget of
14 000 LUT, 50 BRAM36, 0 DSP:

| S | LUT | FF | BRAM36 (RAMB36E1 + RAMB18E1/2) | LUTRAM cells | DSP48E1 | CARRY4 |
|---|---|---|---|---|---|---|
| 11 | 5 280 | 3 724 | 70 (64 + 12/2) | 90 | 0 | 462 |
| 17 | 7 036 | 5 006 | 59.5 (59 + 1/2) | 131 | 0 | 632 |
| 22 | 8 899 | 6 103 | 70 (64 + 12/2) | 166 | 0 | 776 |
| 34 | 12 976 | 8 663 | 59.5 (25 + 69/2) | 250 | 0 | 1 108 |
| 187 | 72 250 | 44 288 | 58 (3 + 110/2) | 5 806 | 0 | 5 401 |

(These rows are two banks without the queue, 2026-11-02 morning.) With
the queue, S = 34, by bank count K and the rates built in (the record
and sign memories are sized by the largest rate built: `codes` /
`--rates`):

| S = 34 | rates | K | LUT | FF | BRAM36 (RAMB36E1 + RAMB18E1/2) |
|---|---|---|---|---|---|
| all eleven | 41 … 109 | 3 | 14 034 | 9 202 | 76.5 (25 + 103/2) |
| all eleven | 41 … 109 | 2 | 13 082 | 9 167 | 59.5 (25 + 69/2) |
| 93/120 and the TMCC's 61/120 | 61, 93 | 3 | 13 415 | 8 964 | 74.5 (23 + 103/2) |
| 93/120 only (no TMCC) | 93 | 3 | 13 492 | 8 930 | 65 (14 + 102/2) |
| 93/120 only (no TMCC) | 93 | 2 | 12 590 | 8 895 | 48 (14 + 68/2) |

0 DSP48E1 throughout. **The decision (2026-11-02): S = 34, K = 3, all
eleven rates, 76.5 BRAM36**, from the receive-only image's budget (107
BRAM36 free without `SymbolsTx` and `maia_sdr`), 14 034 LUT (the plan's
14 000). Sizing the record and sign memories by the rates built saves
little because the TMCC's 61/120 has 649 steps per pass, nearly 41/120's
869; only a 93/120-only build (no TMCC) reaches 65 (K = 3) or 48 (K = 2).
S = 187 would be 5× the LUT budget (the units and the 187-lane
rotators), so it is out.

Block RAM by memory at S = 34 (`synth/ldpc_memories.py`): the L/H
banks K × 34 RAMB18 (17 BRAM36 per bank), the check records (646 × 869) 18, the edge
signs (34 × 4 785) 5, the ROMs 2.5. The data needs 15.2 + 4.4 + 2 × 4.4
BRAM36 for records, signs and L; the records' and signs' depth is set by
41/120 (869 steps, 4 785 edges per unit) while every other rate needs at
most 4 114 / 374 × 11 = 649 steps.

**Status (2026-11-02)**: bit-exact in simulation on every toy set
(steps 11, 17, 22, 34, 187: pairs, triples, chains wrapping round the
group, the shortened code with the known zeros filled by the decoder,
stress inputs, the limit) and on the Rust sets' 93/120 and TMCC codewords
at step 17; ≈ 70 s of simulation per 93/120 codeword at 4 iterations
(≈ 14 s per pass at S = 17), so the default `fpga:test` set is two
93/120 codewords and one TMCC codeword; `FPGA_LDPC_ALL=1` runs every
codeword of every layered set. With the queue (K banks, 2026-11-02):
the same sets bit-exact through K = 3, plus the overrun record in order
(K = 2), two load contexts open at once, and the limit as a run-time
input; the Rust set `ldpc-2-93-layered34` (3 codewords) bit-exact at
81–105 s per codeword (run by the integrator).

## Integration

`SymbolsRx` goes between Tezuka's sample path and the ADC packer
(maia-hdl `projects/common/cs12_cs8.tcl`, Tezuka's fork F5OEO/maia-sdr,
branch `refactor`):

| Port | Connects to |
|---|---|
| `adc_valid`, `adc_i`, `adc_q` | `mux_decim_i/valid_out`, `mux_decim_i/data_out`, `mux_decim_q/data_out` (12-bit samples) |
| `pass_valid`, `pass_data0`, `pass_data1` | `cs12_8mux_chan0/valid_out`, `cs12_sync_chan0/data_out0`, `data_out1` (today's path, passed through) |
| `pack_valid`, `pack_data0`, `pack_data1` | `util_ad9361_adc_pack` `fifo_wr_en`, `fifo_wr_data_0`, `fifo_wr_data_1` |
| `gpio_out` | `axi_ad9361/up_adc_gpio_out` (bit 0 stays Tezuka's: the DDC select of `maiafirtoiq.tcl` and the cs12 `zero_count` of `cs12_cs8.tcl`) |
| `gpio_in` | `axi_ad9361/up_adc_gpio_in` (unconnected in Tezuka v0.3.24) |
| clock, reset | `util_ad9361_adc_fifo/dout_clk`, `dout_rstn` inverted (the sample rate on the PlutoSky R2: "Clock" above) |

(`pass_valid` is `channels_or/Res`, the OR of both channels' valid, not
`cs12_8mux_chan0/valid_out`: that is what drives the packer's
`fifo_wr_en` in `cs12_cs8.tcl`. `fpga/tezuka/isdb_symbols.tcl` has the
exact connections.)

**Discovery and control without kernel changes.** libiio reaches both
GPIO registers through iiod's register access on `cf-ad9361-lpc`:
`gpio_in` at debug address 0x800000B8, `gpio_out` at 0x800000BC. Reading
`gpio_in` (one register read, nothing written) gives the ID word 0x15DB
`v` `0` `TT`: magic, format version, taps. A bitstream without the block
reads 0. Configuration is about 6,300 register writes for 32 taps (three
banks of 64 × 32 coefficients and the scalars; 0.62 ms each through
iiod, measured): about 3.9 s. `isdb symbols` measures the trim
on a short IQ capture first and sends it with the rest. A new trim alone
would reload only `k` (0x3000…, about 2,100 writes, 1.3 s;
`control::Encoder::output_bank`), which the running core accepts; the
host does not do that yet. With symbols
selected, the host enables only `voltage0` of `cf-ad9361-lpc` and reads
16-bit words (`isdb_radio::Backend::capture_symbols`, `isdb symbols`).

The ISDB-S3 receive chain after the front end (docs/plans/isdbs3-fpga-rx.md)
is found the same way: the host reads bits 11–8 of the ID word, 0 on
every front end so far, as the chain's `Slots` format version, and
drives it through the same command words, CTRL bit 3 (with bit 2)
selecting its `Slots` words for the DMA. The registers the host uses
(0x400–0x6FF: version, features, carrier, layout lag, start values,
taps, training sums, counters) are placeholders in
`isdb_frontend::control::chain` until the chain's HDL fixes them; the
HDL should take that file's map or change it there (the plan's "Host
side" lists it).

A dedicated IIO device per output boundary, with its format version as
an attribute (the plan's longer-term form), would need a device-tree
node and a driver. The GPIO route needs neither.

### Building the PlutoSky R2 bitstream (`tezuka/`)

```sh
fpga/tezuka/build.sh WORK stock     # the unmodified design
fpga/tezuka/build.sh WORK isdb      # with SymbolsRx and SymbolsTx
fpga/tezuka/build.sh WORK isdb-tx   # with SymbolsTx alone
fpga/tezuka/sdbin.sh WORK/out/isdb/plutoskyr2.xsa isdb_txrx_top.bin   # both blocks (the deployed image)
```

`build.sh` clones the pinned upstream revisions into WORKDIR (outside
the repository; several GB), builds maia-hdl's IP cores and the
`plutoskyr2` XSA. `apply.sh` (called in the `isdb` modes; `--tx-only`
for `isdb-tx`) regenerates `fpga/build/isdb_symbols_rx.v` and
`isdb_symbols_tx.v`, sets the `isdb_symbols` and `isdb_symbols_tx` flags
in the plutoskyr2 case of maia-hdl's `system_bd.tcl`, sources
`isdb_symbols.tcl` after `cs12_cs8.tcl` and `isdb_symbols_tx.tcl` where
`txfir.tcl` would be, drops the plutoskyr2 case's `dvb` flag (the DVB-S2
transmitter, for its DSP slices, and because its AXIS switches would sit
in the DMA path the transmit block reads) and its `txfir` flag (the ×8
TX FIR, whose place the transmit block takes), and adds
`isdb_symbols.xdc` and `isdb_symbols_tx.xdc` (below). `sdbin.sh` turns
an XSA into the file U-Boot loads from the SD card (Tezuka's
`postimage-sd.sh` conversion).

| Upstream | Revision |
|---|---|
| tezuka_fw | v0.3.24 = `a304a1f7ecba49dc4cd9d57a7935ec52504c7334` |
| F5OEO/maia-sdr | `f7092ba` (branch `refactor`): the source of the plutoskyr2 bitstream v0.3.24 ships (built 2026-10-06 with Vivado 2023.1, device `xc7z020clg484-2`) |
| adi-hdl / dvb_fpga / XilinxUnisimLibrary | `cf81ab15`, `297a5b7e` (with its own submodules), `1c8e05fd` |

Needs Vivado with the Zynq-7000 devices and a licence that covers the
xc7z020: Vivado 2026.1's free BASIC tier does. A licence file that
carries an ALVEO-tier section before the BASIC one makes Vivado take the
ALVEO tier and list no parts at all; keep only the BASIC section in
`~/.Xilinx/`. Workarounds for the upstream flow, all in `build.sh`:
`ADI_IGNORE_VERSION_CHECK=1` (adi-hdl asks for 2023.2); Yosys for
maia-hdl's Amaranth from the `amaranth-yosys` wheel (upstream uses
oss-cad-suite); the XSA target built directly (the Makefile's
`build_project` names a rule ADI's makefile lacks, and `make all` ends
in an install to the upstream author's path); dvb_fpga's own submodules
fetched recursively.

**Status (2026-10-31): the image with both blocks and the transmit
block's prefetch / BANK_TAG / 64-bit counters, `isdb_txrx2_top.bin`,
is built, meets timing and runs on the bench board**
(`/boot/uEnv.txt` `bitstream_image=isdb_txrx2_top.bin`, `uEnv.txt.prev-txrx`
and `isdb_txrx_top.bin` kept for rollback; VERIFICATION.md "ISDB-S
transmitter in the FPGA": the image, on the board, into the PIX-DT295,
image 2). The receive-only format-4 image `isdb_top.bin` (2026-10-26)
ran on the board before it (isdb-s.md §0.16) and stays on the card.
Expected before the build, from Tezuka's TX FIR figures and the Yosys
estimate: 158 DSP, ≈ 23.8k LUT, ≈ 30.0k FF, 68 BRAM36; measured: 158,
23 470, 29 994, 68. The transmit block's features (prefetch, BANK_TAG,
64-bit counters; 2026-10-31) are in this source but not yet in a built
image: the deployed one reads FEATURES 0.

| xc7z020clg484-2 | LUT | FF | BRAM36 | DSP | WNS | WHS |
|---|---|---|---|---|---|---|
| Tezuka's own build (2023.1, `doc/fpga_usage.md`) | 23 005 | 32 111 | 46 + 19 BRAM18 | 106 | — | — |
| stock, rebuilt with 2026.1 | 23 211 (44 %) | 32 304 (30 %) | 55.5 | 106 (48 %) | +0.100 ns | +0.024 ns |
| format 4, without the DVB-S2 transmitter | 21 694 (41 %) | 28 421 (27 %) | 62 | 178 (81 %) | **+0.100 ns** | **+0.022 ns** |
| of which `isdb_symbols` | 5 825 | 5 519 | — | 112 | — | — |
| **both blocks** (RX front end + `SymbolsTx`, no DVB-S2 TX, no TX FIR; 2026-10-29) | 23 470 (44 %) | 29 994 (28 %) | 68 | **158 (72 %)** | **+0.100 ns** | **+0.033 ns** |
| of which `isdb_symbols_tx` | 2 331 | 2 672 | 12 RAMB18 | 0 | — | — |

Per clock in the final checkpoint of the image with both blocks (all
met): `clk_div_sel_0_s` (the blocks' clock, constrained at 62.5 MHz from
ADI's worst-case 250 MHz `rx_clk`; it runs at 40 MHz) +2.325 ns (an
RX-block path); `clk_div_sel_1_s` (125 MHz, r1_mode only; the blocks'
paths there are false) +0.821 ns; `clk_fpga_0` +0.970 ns; maia-sdr's
`clk_out3` +0.779 ns; `rx_clk` +0.542 ns. The overall WNS +0.100 ns is
the RGMII input (`RGMII_rxc`), the same path class as the stock build
(Tezuka's PlutoSky R2 has the smallest margin of its boards). Reports:
`WORK/out/isdb/final_timing.rpt`, `final_timing_isdb_util.rpt` (from
`timing.tcl`). The RX-only build's figures were `clk_div_sel_0_s`
+1.523 ns, `clk_div_sel_1_s` +0.652, `clk_fpga_0` +0.548, `clk_out3`
+0.675, `rx_clk` +0.445 (`v4b_*.rpt`, no longer in `out/`).

| Artefact (`WORK/out/isdb/`) | Bytes | sha256 |
|---|---|---|
| `plutoskyr2.xsa` | — | `12abb5eb1047290101d54ea82e8bfb73e3f978819fa14131d6839c340a8f3368` |
| `isdb_top.bin` (from `sdbin.sh`) | 2 785 408 | `479141e02d8a151dd8475ac196b658beee7820ad9c298123191688aaa3ed1fd3` |
| `plutoskyr2.xsa`, both blocks (2026-10-29) | 1 226 851 | `4b9b4038b7b515d6eb8169c01a233ad3e137836dbee451852fae808a586bb86d` |
| `isdb_txrx_top.bin` (both blocks, from `sdbin.sh`) | 2 773 472 | `644ab085093fb0d7a1ce0922ea7fa490ae9a5f24f55a45ec332600030178a473` |
| `plutoskyr2.xsa`, both blocks, transmit block with prefetch / BANK_TAG / 64-bit counters (2026-10-31) | — | `52939814d36e907760bb9512501266fa67c590145e7d6df6a6d0351aa50e61d0` |
| `isdb_txrx2_top.bin` (the deployed image, from `sdbin.sh`) | 2 816 192 | `2ed4c1c691bf3840e2da22928fdfba0266c0ec15f6acb6f63440912026b8b86a` |

**The image with both blocks (2026-10-29)** builds with `build.sh WORK isdb`
in about 25 minutes on this host. The project flow's
`*_timing_summary_routed.rpt` says "Timing constraints are not met"
(WHS −0.122 ns on ten of Tezuka's own `axi_ad9361` TX-channel paths in
`rx_clk`): that report is taken after `route_design`, before upstream's
`hold_fix.tcl` hook re-routes and the post-route physical optimisation
restores setup. The checkpoint the bitstream is written from
(`impl_1/system_top_postroute_physopt.dcp`) meets every constraint:
`vivado -mode batch -source fpga/tezuka/timing.tcl -tclargs
CHECKPOINT.dcp` prints its WNS / WHS and the isdb blocks' placed
resources (the table above). Deployed on the bench board 2026-10-29
(VERIFICATION.md "on the board").

How it got there (each step a commit): the first format-4 build put the
block at 154 DSP48E1 (Vivado moved the adder trees and the loop's adds
into DSP post-adders), filling the device (220 / 220), and missed timing
in Tezuka's own paths (`interclk` → `cs12_8mux`, maia-sdr's DDC) from the
placement pressure; `isdb_symbols.xdc` had been read during synthesis,
before `util_clkdiv`'s generated clocks exist, so the block was still
timed in the r1_mode clock. `use_dsp = "no"` on the adders and the XDC
as implementation-only, processed late, fixed both.

`isdb_symbols.xdc` (and `isdb_symbols_tx.xdc`, the same three on the DAC
core's `up_dac_gpio_out`, `up_rdata` of `i_up_dac_common` and the
`isdb_symbols_tx` cells): `gpio_out` into the block's synchroniser is a false
path (Tezuka's and ADI's constraints declare the same register's bits 0–2
false paths); `gpio_in` into `axi_ad9361`'s `up_rdata` a datapath-only
max delay (the word is static whenever the host reads it); the block's
paths in `clk_div_sel_1_s` (l_clk / 2, only with r1_mode on both AD9361
cores, never on this board) false. The host refuses the symbol path if
r1_mode is ever set (`isdb-radio` pluto backend reads register
0x80000044 bit 2 of the ADC core and 0x80000048 bit 5 of the DAC core).

**What the image leaves out:** Tezuka's DVB-S2 transmitter (`dvb.tcl`,
40 DSP48E1), for the DSP budget, and its ×8 TX FIR interpolator
(`txfir.tcl`, 20 DSP48E1; the kernel's parent/8 `sampling_frequency` of
the DDS device then has no effect). To transmit DVB-S2, or at sample
rates below the AD9361's 2.08 Msps (what the ×8 FIR is for), boot the
stock `system_top.bin`.

**Deploying.** Never flash. The card's FAT
partition already holds several bitstreams (`system_top.bin`,
`tezuka_top.bin`); U-Boot loads `${bitstream_image}` (`fatload mmc 0 …`),
which `uEnv.txt` sets (`bitstream_image=system_top.bin` in v0.3.24).
On a **copy** of the card (keep the original as the rollback):

1. copy `isdb_txrx_top.bin` (both blocks; or `isdb_top.bin`, receive
   only) from `sdbin.sh` to the FAT partition, next to `system_top.bin`
   (a rebuilt image under a new name, e.g. `isdb_txrx2_top.bin`, beside
   the one it replaces);
2. in that partition's `uEnv.txt`, set
   `bitstream_image=isdb_txrx_top.bin` (keep the previous `uEnv.txt` as
   `uEnv.txt.prev-*`);
3. boot from SD as today (DIP switches unchanged: both at 3.3 V).

Rollback: `bitstream_image=isdb_top.bin` (receive only) or
`system_top.bin` (stock), or the original card.
`sdbin.sh` was checked against the board: on the XSA v0.3.24 ships it
reproduces the card's `system_top.bin` byte for byte (sha256
`4e9ea3825f2c9fd08906f0398fff2a652736a843503f21407793fc300c045c76`).

**Verifying the receive block after deployment** (either image):

1. `isdb symbols --radio pluto --freq BS7 --probe`
   reads the ID word 0x15DB4020 (magic, format 4, 32 taps); the stock
   bitstream reads 0.
2. `isdb symbols --radio pluto --freq BS7 --system
   isdb-s3 --offload auto --seconds 10 --out fpga.sym`: 0 loss events
   from the CRC-32C reader, the link rate at 51 MB/s (6 bits).
3. Bit-exactness on the board: capture the same signal as IQ and run
   `isdb iq symbols --system isdb-s3` with the same parameters; the words
   must be identical (the FPGA vectors are the bench version of this).
4. Rest of the chain: `isdb iq s3sync --read fpga.sym` (Es/N0 as from
   the CPU front end) and `isdb rx --system isdbs3` on the symbols.

**Verifying the transmit block** (plan §4.3, done 2026-10-29,
VERIFICATION.md "on the board"; nothing radiated: the AD9361's
data-port loopback, TX LO down, which the board checks found
sample-exact):

1. `cf-ad9361-dds-core-lpc` register 0x800000B8 reads the ID word
   0xB5D11918 (the stock bitstream and `isdb_top.bin` read 0); after
   CTRL RUN|SELECT, STATUS (0x02) reads 3 (0xB with the prefetch: bit 3
   PRIMED while a word is held); FEATURES (0x07) reads 7 on an image
   with the features, 0 on the 2026-10-29 one, and the open line of
   `isdb tx` names them ("block features: …").
2. `isdb tx --system isdbs --radio pluto:loopback=on --offload fpga
   --rf-enable` in one process and `isdb rx --system isdbs --radio pluto
   --offload fpga` in another (two libiio contexts, both at 40 Msps):
   every TS byte-exact against its input, 0 uncorrectable, 0 host
   underruns, MISSED ≤ 16 at FIRST_MISS 0 (the start-only miss; 0 once
   the prefetch is in the image: `tx_symbols_loopback_on_a_pluto`
   requires 0 when the open line names it), 0 RX losses, 8PSK MER
   ≥ 36.0 dB at the 6-bit output, the measured trim within ±0.05 T of 0.
   With BANK_TAG, a second open logs "bank: default (tag match), skipped
   13312 commands" and configures in milliseconds.
3. Bit-exactness on the board: a buffered 12-bit IQ capture in loopback
   equals the model's samples for the same codes after an offset search
   (the TX vectors are the bench version of this).
4. A deliberate starvation run: FIRST_MISS lands in the superframe the
   receiver reports lost.

## Licences

The code here is Apache-2.0 like the rest of the repository. maia-hdl
is MIT and Tezuka is GPL-3.0: neither is copied here, and both are
referenced by path and revision only. `fpga/tezuka/apply.sh` edits
maia-hdl's `system_bd.tcl` (MIT) in the build tree only; it carries no
upstream code, only short anchor strings. ADI HDL is licensed per module
(ADIBSD, BSD-1-Clause, GPL2, LGPL); dvb_fpga is CERN-OHL-W v2: none of
them is modified. A bitstream built this way combines all of these, so
distributing one (not building it for your own board) brings their
terms with it, notably CERN-OHL-W for the DVB-S2 encoder.
