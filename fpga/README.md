# fpga — FPGA implementations of receive and transmit stages

Amaranth HDL for the stages of the receive and transmit chains that a
radio's FPGA can run instead of the host CPU (plan: the run-time choice
between CPU and FPGA implementations of a stage, like a SIMD kernel). Each stage has
a Rust reference model in the workspace that it must match **bit for
bit**. The model writes test vectors, and the HDL simulation here
reproduces them.

For the development loop (`fpga:test-quick`, a test class, the suite, the
real decoder), the bounded speed measurement and how the simulation's cost
was measured, see [FPGA simulation
turnaround](../docs/plans/fpga-simulation-timescale.md).

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
| `isdb_fpga/simldpc.py` | the decoder against LDPC vectors files (`python -m isdb_fpga.simldpc VECTORS… [--max N] [--fill-known] [--all-rates] [--backend pysim\|cxxrtl\|verilator]`): Amaranth's Python simulator or the design compiled with Verilator or CXXRTL (`chain.simx`; the default where it can be built: Verilator where installed, else CXXRTL) |
| `synth/ldpc_estimate.py`, `synth/ldpc_cycles.py`, `synth/ldpc_memories.py`, `synth/ldpc_simspeed.py`, `synth/ldpc_latency.py` | the decoder's Yosys estimate per S, its cycles per iteration per rate and S, how Yosys maps each memory shape, the simulation speed of both backends, the measured write latency |
| `synth/sim_compare.py`, `synth/sim_speed.py`, `synth/sim_parallel_build.py`, `synth/sim_xcheck.py` | the compiled simulation backends ([below](#simulation-backends)): every test bench on two backends compared value for value (`--x-unique SEED`: Verilator's X values random), one bench's build time and speed, parallel builders of one library, the random X values in effect |
| `synth/chain_simspeed.py` | the chain with the real decoder on a short prefix or idle clocks: cycles per second on a backend, what chunks or another compiler level cost and change, bounded ([the report](../docs/plans/fpga-simulation-timescale.md)) |
| `tools/sim_hashseed.py` | the compiled libraries' keys per design and `PYTHONHASHSEED`, each seed in a process of its own |
| `isdb_fpga/chain/` | the ISDB-S3 chain after the front end (stages 1–6, 8, 9; [below](#the-isdb-s3-chain-slotsrx)) |
| `synth/chain_estimate.py` | Yosys estimates of the chain's blocks (LUT including the LUT RAM cells' LUTs) |
| `synth/cells.py`, `synth/bch_parts.py` | which cells (DSP, block RAM, LUT RAM) a block's estimate made, by name; the BCH unit's parts |
| `tools/queue_sweep.py` | the decoder queue's overruns at the chain's pace by S, clock, banks, iteration limit and slot rate (the 160 MHz and S = 22 decisions; `--rate 109` 32APSK), and the decoder cycles per second the bridge's input port needs |
| `tezuka/isdb_slots.xdc` | the constraints of `SlotsRx` (its three clocks, the GPIO paths) |
| `tests/test_chain.py` | the chain's stage and chain tests against `chain-*.json` |
| `isdb_fpga/chain/chaos.py`, `tests/test_chaos.py`, `tools/chaos_run.py`, `tools/chaos_table.py` | chaos runs of the chain with the real decoder: random stops, STARTs, losses and tap writes, health checks, the model restarted at the same points ([below](#the-isdb-s3-chain-slotsrx), "Chaos runs") |
| `tools/hostfpga_model.py`, `tests/test_hostfpga_model.py` | the timed host–FPGA model: the chain's register file and frame clock, `direct_reg_access`'s shared address, the host's slow loop and the board's other clients over iiod, standard-library Python ([below](#a-timed-hostfpga-model)) |
| `isdb_fpga/formal/`, `tests/test_formal.py`, `tools/formal.py` | formal checks with SymbiYosys of the LDPC bank allocator, the `Slots` packer's sequence numbers and the chain's frame boundaries: the harnesses, their `.sby` files, the runner ([below](#formal-checks)) |

## Running

The Python environment is a uv project pinned in `pyproject.toml` /
`uv.lock` (Amaranth 0.5.10, YoWASP Yosys 0.70, which runs as WebAssembly
in user space). Nothing here needs a vendor toolchain:

```sh
mise run fpga:test         # vectors from the models, then the HDL simulation of both blocks
mise run fpga:test-quick   # seconds, no vectors: the stream driver, the LDPC bridge, the CRC
mise run fpga:formal       # the formal checks alone (also part of fpga:test)
mise run fpga:estimate     # resource estimate of the 32-tap block
mise run fpga:verilog      # fpga/build/isdb_symbols_rx.v for a vendor flow
mise run fpga:tx-estimate  # resource estimate of the transmit block
mise run fpga:tx-verilog   # fpga/build/isdb_symbols_tx.v (the default bank as BRAM init)
mise run fpga:chain-estimate  # resource estimates of the ISDB-S3 chain's blocks
mise run fpga:slots-verilog   # fpga/build/isdb_slots_rx.v: front end + chain + LDPC decoder
```

(`fpga:tx-vectors`, which `fpga:test` runs, writes the Python model's
two TX sets; the Rust model's TX vectors, in the same directory, are
simulated too. The tasks set `UV_DEFAULT_INDEX` to PyPI, the index
`uv.lock` records, unless it is already set.)

### Simulation backends

The chain's stages and the LDPC decoder run long vector sets through
one stream API (`isdb_fpga/chain/simx.py`) with three backends:

| `FPGA_SIM` | Backend | Needs | Built |
|---|---|---|---|
| `verilator` | the design as Verilog (the venv's Yosys) Verilated, with `chain/harness.cc` (`chain/simv.py`) | Verilator ≥ 5 (`$VERILATOR`), a C++ compiler | `make -j`, cached in `build/verilator/` |
| `cxxrtl` | Yosys `write_cxxrtl`, with the same `harness.cc` | a C++ compiler (the CXXRTL headers come with yowasp-yosys) | one translation unit (`c++ -O1`), cached in `build/cxxrtl/` |
| `pysim` | Amaranth's Python simulator | — | — |

The tests' compiled paths (the chain's whole sets, the LDPC decoder's
B44 sets) use `simx.compiled_backend()`: `FPGA_SIM` if it names a compiled
backend, else Verilator where it is installed, else CXXRTL, else they skip
(the chain) or run their Python-simulator subset (the decoder,
`FPGA_SIM=pysim` too). Both compiled libraries are keyed by the design,
`harness.cc` and their build flags (Verilator's also by its version, its
design without the `src` attributes, so another checkout shares them),
never by the Python around them; one process builds a library while the
others (pytest-xdist workers) wait on its lock. On Ubuntu `apt install
verilator` (5.032 on 26.04); `tools/verilator-apt-time.sh` times that in
a container.

**The hash seed.** Amaranth collects a design's implicit clock domains in
sets, so a design with more than one has an RTLIL port order, and a
library key, per Python hash seed (`tools/sim_hashseed.py`: `RealChainBench`
three keys over seeds 0, 1, 2, a decoder alone one). `fpga:test` and
`fpga:test-quick` set `PYTHONHASHSEED=0`, which CI's `mise run fpga:test`
inherits, so the workers of a run, later runs and CI's cache share one
library per design. Set it before Python starts on a direct command
(`PYTHONHASHSEED=0 uv run --locked pytest …`): without it each process,
xdist workers included, has a seed of its own and may build its own copy of
such a design. It cannot be set from inside a running interpreter.

**`FPGA_SIM_PROGRESS=1`** reports on stderr the elaboration, the cache's
answer and a run's records, drain cycles and cycles per second, for the
compiled backends. The clock sequence is the same (`tests/test_simx.py`,
`ProgressTest`), the cost not measurable (the report); under pytest add
`-s`, which also works with xdist.

The backends give the same records, value for value: `synth/sim_compare.py`
runs every compiled test bench (the chain's stages 1–6 and BCH whole, the
chain's frame 0, every step-17/34 B44 set, the toy sets, the plans) on
CXXRTL and on Verilator and compares every value the engine returns (each
group's records, each `get`, the cycle counts) and the ports before the
first clock; `--x-unique SEED` with Verilator's X values random
(`--x-initial unique --x-assign unique`; the designs initialise every
register and memory, the only X being memory read ports before their
first read and parallel-mux defaults, none of which reaches a record).
`tests/test_simx.py` runs a bench of the harness's whole protocol
(backpressure, gaps, two groups, signed and 64-bit ports, a memory, an
output combinational from registers) on all three backends and the
chain-2 set on both compiled ones. The same holds with Ubuntu 26.04's
Verilator 5.032 (the CI's). Measured on this host (Ryzen 9 7950X, GCC
16.2, Verilator 5.054), 2026-11-02, one core (`taskset`) unless said:

| Design (`sim_compare.py` case) | Harness cycles | Verilator | CXXRTL | Verilator build: 1 job / 4 jobs | CXXRTL build |
|---|---|---|---|---|---|
| `SlotsChain` + stub, frame 0 (`chain`) | 2 504 981 | 374 000–381 000 cycles/s | 27 000 cycles/s | 43 s / 23 s | 183 s |
| `LdpcDecoder` S = 34, all rates, every step-34 set (`ldpc-34`) | 2 615 569 | 500 000–530 000 cycles/s | 31 000 cycles/s | 22 s / 11 s | 58 s |
| `LdpcDecoder` S = 187, all rates, `ldpc-3` (`ldpc-187`, loaded host) | 377 489 | 158 000 cycles/s | 1 900 cycles/s | ≈ 45 s with 8 jobs | 1 282 s |
| the TMCC stage (`tmcc`, loaded host) | 133 108 | 1.6 M cycles/s | 39 000 cycles/s | 6 s with 8 jobs | 45–53 s |

(`synth/sim_speed_all.sh`, `synth/sim_speed.py`: cycles over the time in
the engine's `run` / `tick` calls; the build alone, in an empty
directory; the cached setup, elaboration and the RTLIL included, is
about 17 s for the chain and 2 s for the decoder on both.) The small
stages (sync, phase, equaliser, demapper) run at 0.3–1.9 M cycles/s on
both, within a factor of 2.5. CXXRTL takes three steps per cycle (one
to settle the inputs, two at the edge), Verilator two: CXXRTL's step
commits the registers after evaluating the logic, so an output
combinational from registers needs the second step to read as it is
after the edge, as on the other two (`harness.cc`; until 2026-11-02 it
read one edge late: the LDPC decoder's output words reached the harness
a cycle later, with the same records).

The whole `pytest -n 4 --dist worksteal tests` on four cores (the CI
runner has four vCPUs; without the recordings' opt-in sets):

| Backend | Every model built | Cached | Tests |
|---|---|---|---|
| Verilator (the default where installed) | 296 s | 161 s | 84, S = 187 included |
| `FPGA_SIM=cxxrtl` | 479 s | 265 s | 82 (S = 187 skipped) |

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

### Invariant monitors

Properties that hold in every correct run, checked on every cycle of every
simulation independently of the vectors (`isdb_fpga/chain/monitors.py`,
issue #30 item 2). Six defects reached the board although every bit-exact
test had passed (the decoder's bank leak across stop/START, #28, fixed in
e95c563; n0 cleared a cycle before the ended frame's division, 243d672;
the learned points one word shifted, d53de60; TAPS_FROM one write behind,
5f60649; the sum readback's transient 0, a052ece); each broke one of the
properties below.

**Mechanism.** A monitored module declares its checks (`MONITORS`, name →
clock domain and the property) and adds them at the end of its
`elaborate` when it has a `Monitor`; `monitors.enable(design)` gives every
monitored module reachable from a design one. The parameter is
`monitors=True` on `SlotsChain`, `SlotsRx`, `LdpcDecoder` and
`LdpcBridge` (default False: nothing is added, the RTLIL and the bitstream
are unchanged; `synth/verilog.py --slots` writes the same Verilog as
before, byte for byte without the `src` attributes, at a fixed
`PYTHONHASHSEED`) and on the simulation benches (default True). Each
check is a 48-bit sticky register in its own clock domain holding 1 + the
cycle of its first violation (0: never), counted from reset in that
domain (`sync`: the symbol clock, the engine's cycle; `ldpc`: the
decoder's clock, 5 per symbol clock in the simulations). The registers
are ordinary signals exposed as the bench's ports (`mon_<path>_<check>`,
`monitors.Collector(bench).ports()`), so the three backends read them
alike through the engine's `get`; no harness change was needed.
Amaranth's `Assert` is not used: whether Yosys's `write_verilog` and
Verilator honour its `$check` cells is not something to rely on, and a
register is.

**Where they run.** Every bench of `stages.py` (`EqBench`, `TmccBench`,
`BackBench`, `ChainBench`, `RealChainBench`: the monitors' report is part
of each run's mismatches, after every pass of `run_chain_restarts`), every
decoder run of `simldpc.py` (both paths; a violation raises
`monitors.MonitorError`), `SlotsRxRegisterTest`, `BridgeTest` and the
`Finish` tests. Stage 1 (`SyncFront`) and stage 3 (`SlotPhase`) have no
monitors. A failure reads

    monitor ch.back.demap.noise_count (the noise count at a decoded frame's end = the noise samples of that frame (243d672: the next frame's start had cleared it)) first violated at sync cycle 1115804

(the path is the module's attribute path in the bench: `ch` the chain,
`br` the bridge, `dec` the decoder, reported in its own clock with the
symbol clock beside it).

| Module | Check | What must hold (the signals watched) |
|---|---|---|
| `LdpcDecoder` | `bank_accounting` | popcount(`alloc`) = the contexts holding a bank (`ctx_open & ~ctx_drop`) + the monitor's own count of committed codewords holding one (+1 at a `start` on a holding context, −1 when an output stream ends, `free_mask`) |
| | `held` | `held` = popcount(`alloc`) a cycle before |
| | `load_ready` | `load_ready` a cycle before = (`held` < K) |
| | `abort_closes` | the cycle after `abort`: the context closed and dropped |
| | `commit_without_begin` | no `start` on a context that is neither open nor dropped (its bank would be committed twice) |
| | `bank_leak` | no bank allocated for 32 cycles while the decoder is idle (not `busy`, no output stream, the queue empty) and no context holds one |
| | `queue_overflow` | the sticky `queue_overflow` never rises (that commit's bank is never freed) |
| `LdpcBridge` | `in_fifo_overflow`, `out_fifo_overflow`, `tag_overflow`, `tag_underflow` | no write into the full input or output AsyncFIFO or tag queue, no result record without a tag |
| `Demap` | `noise_count` | at `f_end` the noise count = the monitor's count of the noise samples (A+8, context bit 41) tagged with the frame that ended (`frame` at `f_swap`) |
| `Back` | `port_lost` | no demapper beat while the decoder port is not ready |
| `TmccSoft` | `staging_overrun`, `burst_lost` | the soft-bit writer never at or past the burst's read index; no frame end while the burst runs |
| `FrameCtl` | `frame_order` | the frames ended or dropped come in increasing counter order |
| `Equaliser` | `taps_from_source` | `taps_from` changes only to NONE or to the armed set's frame |
| `ResultFifo` | `overflow` | no write into the full FIFO outside a header's retried chunks |
| `Finish` | `status_point_settled` | a status word is taken only with the point RAM's address unchanged since the previous cycle (its registered read has landed) |
| | `frame_fifo_overflow`, `frame_fifo_underflow` | the 4-entry frame FIFO |
| `SumsReader` | `words` | a sums block's payload is 465 words: its last word is the 465th taken since its start (and no word offered outside a block) |
| | `follows_status` | a sums block is accepted by the packer only right after its frame's status block (the block before it of type 3, the same frame) |
| | `latch_steady` | the equaliser's latched sums stay the frame being sent until the block's last word (a latch moving mid-block would put two frames' sums under one CRC) |
| `Packer` | `seq_contiguous` | each block's sequence number (its header words) is the last + 1; 0 for the first block and the first block accepted in or after a `start`'s cycle |
| | `framing`, `block_kind`, `slot_index` | the output words parse as `Slots` blocks: the sync words, the header check, the payload length; header word 2; a slot block's slot index < 120 |
| `SlotsRx` | `sum_readback_hold` | from a sum register's READ until `rd_ack` the readback keeps its value |
| | `sum_read_slow` | the READ answered within 16 symbol clocks (3) |
| | `taps_from_written` | the cycle after a TAPS_FROM write the armed set's frames are the value written and value + 2 |
| | `input_fifo_overflow` | no symbol written into the full 1 024-symbol FIFO |

**Proofs** (`tests/test_monitors.py`, part of `fpga:test`): each defect is
re-introduced on a scratch copy of the module's source
(`monitors.source_patch`: the module re-executed with the edit and every
reference to its classes swapped while the design elaborates; no
fault-injection parameter in the HDL), or, for a class only a broken
neighbour can cause (a decoder answering a codeword it was never given,
a result queue that never drains), the bench drives that input; each test
also runs the unbroken case where it is cheap and expects no violation.
The board's defects: e95c563's allocation rule reverted (no reuse, `abort`
ignored) on test_isdb_fpga's abort recipe trips `abort_closes`,
`bank_accounting` and `bank_leak`; 243d672 reverted on
`DemapTest.test_next_frame_starts_at_once`'s run trips `noise_count` at
cycle 1 115 804 (and its own n0, K comparison); d53de60's pacing
(`st_pace != 1`) trips `status_point_settled`; 5f60649 reverted trips
`taps_from_written`; a052ece's readback half reverted trips
`sum_readback_hold`. The others: `held`, `load_ready`, `abort_closes`,
`commit_without_begin`, `queue_overflow`, every FIFO but `SlotsRx`'s
input FIFO and `Finish`'s frame FIFO (the same `fifo_overflow` helper),
the packer's four, `port_lost`, `staging_overrun`, `burst_lost`,
`frame_order`, `taps_from_source`, `sum_read_slow`; the sums block's
three (`SumsMonitorTest`: its last word flagged a word early, the block
sent under the next frame's counter, the latch moved mid-block by the
bench).

**Found on the HDL, fixed**: `Packer` honoured `start` only between
blocks, so a START that came while a block was being sent did not restart
the sequence numbers, as the packer's and `SlotsRx`'s documentation say
it does (blocks 0, 1, 2 instead of 0, 1, 0). The packer now latches the
`start`: the block leaving finishes whole with its old number (its header
and CRC already carry it; cutting it would only make the host's reader
skip its words) and the next block it takes has sequence 0, the model's
new `slots::Writer` at START; a block offered in the START's cycle is
taken with 0 (before, the packer did not take it and the finish stage,
which offers a block for one cycle, waited for its payload forever). The
check reads the sequence number from the output's header words
(`PackerStartTest`: a START in block 1's header, payload and trailer,
between blocks, and with an offer; the defect re-introduced trips
`seq_contiguous`). No monitor fires in any other run of the suite,
`FPGA_CHAIN_REAL=1` included (chain-3 … chain-10, the stub chain's frame
0, the restarts, every LDPC set).

**Cost** (this host, 32 threads, shared with other jobs; warm, at
`PYTHONHASHSEED=0`, below): the default `pytest -n auto` 107-110 s before
(93 tests), 115-121 s after (117 and an expected failure: the 24 new
tests). The simulation itself does not measurably slow: `chain-6-chain`'s
frame 0 (2 484 771 cycles) 10.2-10.3 s before, 9.7-9.8 s after;
`chain-7-hdl` with the real decoder (8 179 491 cycles) 115-117 s before,
112 s after (147 s once with the host loaded). With `FPGA_CHAIN_REAL=1`
490 s before, 513 s after. (The designs' RTLIL, and so the compiled
models' cache keys, depend on Python's hash seed, before and after this
change: [the hash seed](#simulation-backends).)

**The pipeline mutation run** (`tools/pipeline_mutation.py`, one-off and
opt-in, about 3 minutes with 8 jobs on Verilator): 35 mutations, each
pipeline delay of the chain's blocks and the decoder shifted by one cycle
(a delay line's length, the stage a side field is taken from, a latency
constant the schedule assumes), each on the compiled fast tests of the
stages it can affect; the unmutated run is clean. Result (2026-11-04):

| Mutation (±1 cycle) | Noticed by | Not noticed (by any field) |
|---|---|---|
| sync: FSync flag delay line | r, NCO phase, correlations, periods, heads, carrier (+1 also restarts) | |
| phase: the gain into its products; φ into the rotation table | y | |
| common: `Phase4`'s phase | z; the demapper's beats, learned points, n0/K, TMCC soft bits | |
| eq: z's delay; the valid/meta pipeline | z (the training references and sums are taken before) | |
| tmcc: the staging buffer's read-ahead (−1) | the burst | |
| demap: the LLRs' delay; the beat's context | beats (positions, LLRs), de-interleaved words | |
| demap: the minimum metric's, \|y\|²'s delay; the noise sample's context | n0/K (only that: `next_start`'s n0 comparison) | |
| back: the frame end's drain (`end_d`, 12) | | **±1** (slack: the last noise sample is in by A+8) |
| back: the abort's wait (`ABORT_WAIT`) | | **±1** (no fast test aborts; `run_chain_restarts`, opt-in, does) |
| finish: the status words' pacing | −1: `status_point_settled`, the status bench's points (not chain-6: no learned points there, d53de60's gap) | **+1** (slack: a cycle more per word) |
| ldpc: `WRITE_LATENCY` | −1: bits, iterations, convergence, unsatisfied | **+1** (slack: waits a cycle more) |
| ldpc: `DECIDE` | | **±1** (on the toy sets at step 34; the B44 sets were not run) |
| ldpc: the output read latency `OLAT` | bits | |

The gaps: `end_d`, `ABORT_WAIT`, the status pacing +1 and
`WRITE_LATENCY` +1 are slack (the design does not depend on the exact
value within a cycle); `DECIDE` −1 unnoticed on the toy sets is the one to
look at (a decision a cycle earlier than the schedule assumes changed no
result there).

### Formal checks

Bounded model checking and k-induction on three small control blocks
(issue #30, item 6), so that a property is checked on every path the
block can take, not only on the paths a vector set exercises: the #28
bank leak needed a stop/START during a codeword's load, which no vector
set had. The tools are wheels of the locked environment and run without
a vendor tool or a system Yosys: SymbiYosys (`yowasp-sby`) and smtbmc
from `yowasp-yosys`, the SMT solver from `yowasp-boolector` (YoWASP
publishes only `.dev` builds of it, hence the exact pin; it is the only
solver YoWASP publishes: there is no YoWASP Yices, Z3 or Bitwuzla, and
the `abc` and `btor` engines need binaries the wheels do not have).

```sh
mise run fpga:formal                            # every harness: bmc, cover, prove
uv run --locked python tools/formal.py banks    # one harness (in fpga/)
uv run --locked python tools/formal.py --break e95c563   # a fixed defect back: must FAIL
uv run --locked python tools/formal.py --list
```

**Mechanism.** A harness (`isdb_fpga/formal/<name>.py`) instantiates the
block unchanged, drives its inputs from free top-level ports, constrains
them with Amaranth's `Assume` only where the block's port contract
excludes a value, and states the properties with `Assert` and the
events it must reach with `Cover`; the properties compare the block
with a small spec model written from the block's documentation. Internal
state a property needs (the allocator's `alloc`) is found by name in the
block's elaborated fragment (`formal.taps`): the HDL has no formal-only
ports or parameters. Amaranth 0.5 writes the properties as `$check`
cells into the RTLIL, which sby's model (Yosys's `chformal`,
`write_smt2`) and smtbmc take as they are.
`tools/formal.py` writes the RTLIL into `build/formal/<name>/`, runs the
`.sby` file's tasks (`bmc`: every path of the depth from reset; `cover`:
each `Cover` reached; `prove`: k-induction, unbounded) and prints PASS /
FAIL per task with the failing properties' messages; on a failure, the
counter-example's VCD path and a table of its steps. The runner points
sby at the YoWASP tools (`YOSYS`, `SMTBMC`, `WITNESS`, a `boolector`
symlink first on `PATH`) and runs each tool once first under a lock: a
YoWASP tool's first run compiles its WebAssembly and says so on stderr,
which smtbmc would take for the solver's answer.

**The harnesses** (all three prove by k-induction; the helper invariants
that make them inductive are asserted too and listed in each docstring):

| Harness | Block | Properties | Assumptions | Depth (bmc, cover / prove) |
|---|---|---|---|---|
| `banks` | `LdpcDecoder`'s bank allocator (toy-a code, S = 17, NG = 6, the build's K = 3) | `alloc` is exactly the banks the contexts hold plus the committed ones not yet freed (the `bank_leak` monitor's condition, as a set); a context holds a bank exactly when the model says and it is the model's (the lowest free one at its `begin`, or the bank a `begin` on a holding context keeps); the cycle after an `abort` the context holds no bank and its bank is free; `held` = popcount(`alloc`) a cycle before; `load_ready` = a bank free, and `load_ready` a cycle before = (`held` < K) | no reset; `abort` never with `begin` or `start`; no second `start` of a committed codeword without a `begin` between; the freed bank (`free_mask`, cut: below) is zero or the oldest committed bank not yet freed (the result queue is in order) | 30 / 4 |
| `packer` | `Packer` | from a block's acceptance until it has left, `seq` is its number: 0 for the first block and for the first block accepted in or after a `start`'s cycle, else the last block's + 1; the block's header words 3 and 4 carry it; a block offered while idle is taken | no reset | 40 / 16 |
| `frame` | `FrameCtl` | the frame numbers at `o_done` / `o_dropped` strictly increase; never both; each reports the frame open the cycle before; a restart closes the open frame (reported dropped, none open after); a frame head drops the open frame and opens its own; a restart drops the queued layouts | no reset; slot number < 120, position < 9 296; frame heads in increasing frame order (the front end's counter, which START does not reset) | 30 / 4 |

The decoder is far too large for a solver as a whole, and its allocator
is not a module of its own. The `banks` harness therefore elaborates the
whole (toy) decoder and its `.sby` script cuts the one net from the rest
of the decoder into the allocator, `free_mask` (Yosys `cutpoint` after
`flatten` and `opt_clean`, so that the cell driving the net is replaced,
not an alias of it): the engine, the load pipeline and the memories then
drive no property and Yosys removes them (25 flip-flops remain), and the
freed bank is a free input under the in-order assumption, a superset of
the engine's timings (a decode takes thousands of cycles, beyond any
bounded depth from reset). The harness drives `o_ready` from a free
input: tied to 0, the engine's `free_mask` is a constant that Yosys
folds before the cut. `Cover(freed)`, unreachable without the cut,
checks that the cut took effect. Not modelled: a commit lost to a full
result queue (`queue_overflow`, its own sticky flag and monitor).

The covers (each reached within the depth): an allocation, a commit of a
held bank, an abort that frees one, a `begin` that keeps its context's
bank, a free by an output stream, a drop (every bank held), a bank
allocated again after it was freed; a block accepted with a `start` in
its cycle, a `start` in a block's header, the next block numbered 0
after it, blocks numbered 0 and 1; frames dropped by a restart, by the
next head and by an incomplete end, two reports in a row, a head with a
queued layout. `o_done` needs a frame of 1 115 520 symbols and is beyond
a bounded run; the `frame` proof covers it (the induction starts from
every state the invariants admit).

**It bites** (`--break`, `formal.BREAKS`: the fix reverted on a scratch
copy of the module through `monitors.source_patch`, as the monitors'
proofs do; `tests/test_formal.py` runs each and expects the FAIL):

- `e95c563`: the allocation rule before e95c563 (`begin` always takes a
  new bank, `abort` frees nothing): FAIL at step 2, "after abort: the
  context holds no bank and its bank is free" (and `alloc` against the
  model): `begin` on context 0 (bank 0), `abort`, and bank 0 stays
  allocated (`alloc` 1, the model's 0).
- `e95c563-begin`: its first half alone (`begin` on a context holding an
  uncommitted bank takes another): FAIL at step 2, "alloc = the banks
  held by contexts + committed not freed": two `begin`s on context 0,
  `alloc` 3 against the model's 1, the board's leak across stop/START.
- `1817079`: the packer's `start` handling before 1817079 (honoured only
  while idle, a block offered with it not taken): FAIL at step 1, "a
  block offered while idle is taken": `start` and `b_valid` together
  while idle.
- `1817079-latch`: its sequence half alone (the latch removed): FAIL at
  step 14, "a block's seq while it leaves is its number": a block (no
  payload) accepted at step 0, `start` in its header (steps 8-11), the
  next block accepted at step 13 leaves with seq 1, the model's 0.
- `restart-open` (a synthetic fault, no defect of record: the restart
  reports the frame dropped but leaves it open): FAIL at step 2, "a
  restart closes the open frame".

**Cost** (this host, warm YoWASP cache): `banks` 2.0 + 1.2 + 1.0 s,
`packer` 14.8 + 0.7 + 0.9 s, `frame` 2.0 + 0.5 + 0.5 s (bmc, cover,
prove; `packer` bmc 19 s with the host at load 34);
`tests/test_formal.py` (the three harnesses and the five breaks) 32 s on
one worker (79 s at load 34), 21-28 s with `pytest -n auto`. A cold YoWASP cache adds
the WebAssembly compilation once (about 5 s on 32 threads). It is part
of `fpga:test`, and so of CI's FPGA job.

Found: nothing on today's HDL. One limitation of the `bank_accounting`
monitor, not of the HDL: it counts a commit only on a `start` without
`begin` (`start & ~begin & holds`), but the decoder's ports allow `start`
and the next `begin` in one cycle, which commits the open codeword's bank
and allocates a new one; the monitor would then miss the committed bank
and fire. `tools/monitor_begin_start.py` shows it (`begin`, then `start`
with `begin`, on context 0 of the toy decoder: `held` 2, correct, and
`bank_accounting` violated at cycle 2). The bridge never issues the two
together, so no simulation of the chain reaches it; the `banks` harness
allows it and passes. Left as it is (a simulation-only check).

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
synthetic sets' input is a window of seven generated B44 frames (16APSK
93/120, −400 Hz drifting −22 Hz/s, Es/N0 14 dB, 6-bit input) from 16
slots before the second frame's head (the chain's frame 0, at symbol
149 736) to 3 slots after the sixth frame's end: 5 755 224 symbols,
frames 0–4 whole, 0 and 1 without a layout, 2 (D) the first decoded, 3
and 4 decoded with the n0, K and points learned on the frame before;
with a `Symbols` file as second argument (and seconds as third, default
0.2) the same sets come from a recording as `chain-rec-*.json` (opt-in,
as the front end's). The whole-chain sets beside chain-6 (written with
the FPGA decoder's settings, layered 34, limit 14, offset 1, for the
real-decoder tests): `chain-7-hdl` (chain-6's input), `chain-8-loss` and
`chain-9-fade` (6 frames of their own with a block loss and 0.3 frame of
noise), `chain-10-bch` (chain-6's input, 1–12 bit errors in eight slots'
decoder output) and `chain-11-taps` (a window of its own, the equaliser
solved by the model every second frame: the taps solved from D in use
from D + 2, frame 4's status with `taps_from` 2; the host's TAPS and
TAPS_FROM writes for them in the set's `host_writes`, which the benches
write, `stages.host_records`) and `chain-12-fail` (a window of its own
with the failure paths: in frame 1 a missed and a stray frame marker,
254 TMCC symbols weak and wrong so its TMCC word needs an iteration, and
5 errors in that word's decoder output that the TMCC BCH corrects; in
frame 2 a codeword of noise that stops at the limit, 14, unconverged, 13
errors in a converged slot's decoder output that the BCH cannot correct,
and 13 in its TMCC word's, so frame 4 has no layout; the bench flips the
same bits, `bch_errors` and `tmcc_errors`), `chain-13-modes` (six
frames of their own with four transmission modes in one TMCC: 16APSK
93/120, 8PSK 73/120, QPSK 61/120 and π/2-BPSK 41/120 with each
modulation's dummy slots; frames 2 and 3 decoded, frame 3's status with
the points learned on frame 2 for all four modulations; two AGC writes in
`host_writes`, so frames 2 and 3 report their gains) and
`chain-14-apsk32` (chain-13's modes with 32APSK 109/120 first, at Es/N0
22 dB, so frame N's TMCC burst meets frame N + 1's first 32APSK
codewords: the bridge's pace, under "Integration"; frame 3's status with
the learned 32APSK points). chain-4-tmcc also
carries chain-12's frame 1 as `noisy` (the TMCC stage test runs it with
its decoder output's errors) and chain-14's frame 2 as `modes` (a layout
of five modes). The example's documentation gives every field. Sizes:
1–37 MB per file.

## The ISDB-S3 chain (`SlotsRx`)

The HDL of stages 1–6 and 8–9 of
[docs/plans/isdbs3-fpga-rx.md](../docs/plans/isdbs3-fpga-rx.md), one
module per stage in `isdb_fpga/chain/`, each reproducing the fixed-point
model `isdbs3_phy::chain` (its module docs are the specification; each
module's docstring states its contract by reference and its schedule):

| Module | Stage | Content |
|---|---|---|
| `nco.py` | 2a, 2b | the rotation by the carrier register (the front end's cosine table, round_shift 11), the loop update after 60 SSync phase steps (a bit-serial product by 246 412) |
| `sync.py` | 1 | `SlotSync`: FSync/SSync correlators as add/subtract trees on a = r_i + r_q, b = r_q − r_i (shared: C_F = P + Q, C_S = P − Q over the 14 agreeing and 10 opposite symbols), max + ½min, the 9 296-entry leaky accumulator in RAM, period peak and sum, lock/loss, parity, FSync sign (bit-serial), the frame-head counter and flywheel, the frame counter (a sequential divider), the head record (16-iteration CORDIC); `SyncFront` = NCO + `SlotSync`, the input held around each decision window, the status snapshot |
| `phase.py` | 3 | the slot ring of the NCO's 6-bit *input* (9 513 symbols, three per 36-bit word: three RAMB36 and a LUT RAM tail) and the NCO re-applied after it (its phase and carrier register per head from a 64-entry history), per head the gain by a divider and the steps by bit-serial products, the interpolation as running sums, the conjugate rotation and the gain product, Q.10 |
| `eq.py`, `frame.py` | 4 | the 11-tap FIR (44 products on 11 DSP48E1 in the 4× clock, one product kind per phase, one 11-leaf tree), the taps (at once, or armed for a frame), the training sums time-multiplexed on one complex MAC into a two-bank accumulator RAM with a readback port, latched at each frame's end or, after the host's SUMS_CAPTURE write, latched at the next frame's end and held until the next write (the host's 308 reads take about 15 frames through iiod: without the hold every read was torn and the taps never loaded, board check of build 6, 2026-11-03); `FrameCtl`: the frame bookkeeping and the layout queue at the FIR's centre |
| `tmcc.py` | 5 | soft bits, the 31 680 × 8 staging buffer (four lanes) and its burst into the decoder (four values per beat, context 1, the shared port's free cycles); `TmccResult` (dispersal, BCH, the information bits); `TmccLayout` (`allocation_ok`, the 120 slot entries for frame + 2) |
| `demap.py` | 6 | the max-log metric in the 4× clock, 8 points per fast cycle from two 32-point sets in eight LUT RAM lanes (16 products), minima of four and running minima per bit position and value, × K on two time-shared units, the de-interleaved positions, the codeword cursor, the pilot sums and learned points, n0 and K at a frame's end |
| `back.py` | 4–6 | the equaliser with FrameCtl, the routing record through the FIR pipeline, TMCC and demapper on its output, the decoder port's arbiter |
| `ldpc_if.py` | (7) | the decoder interface (the chain's port), `LdpcBridge` to `isdb_fpga/ldpc.py`'s ports (async FIFOs, the serialiser, the tags in commit order), and how the stub works |
| `finish.py`, `gf.py` | 8 | the BCH unit (syndromes 16 bits per clock, Berlekamp–Massey on one GF(2¹⁶) multiplier, Chien one position per clock, the roots flipped in the word buffers as the Rust decoder flips them: `FixRoots`), the slot PRBS, `Finish` (the result stream into slot, TMCC and status blocks, and a decoded frame's sums block after its status block; a frame's TMCC, status and sums blocks may follow the next frame's first slot blocks, with their contents the model's); `gf.py`: the field and a Python port of `isdb_fec::bch::decode` |
| `sums.py` | 8–9 | `SumsReader`: the sums block (`Slots` format 2, type 4) from the equaliser's latched accumulator bank, an entry at a time through the same one-cycle steal of its read port as the host's register READ, six 16-bit words per entry by a 6:1 select |
| `packer.py`, `slots.py` | 9 | the `Slots` block framing (format 2) with the front end's CRC-32C network, the status block's words; `slots.py`: the format in Python (reader, CRC, the sums block's fields) |
| `top.py` | | `SlotsChain` (the stages wired, the status snapshot), `SlotsRx` (the bitstream block: front end → FIFO → chain → bridge → decoder and its MMCM, the register file at the host's map, the output selection) |
| `nominal.py` | 4, 6 | the nominal points of every mode in one ROM (23 distinct sets, 510 × 24 bits, one RAMB18), shared by the equaliser and the demapper |
| `sim.py`, `stages.py`, `simx.py`, `simv.py`, `harness.cc`, `bchsim.py` | | simulation against the vectors (`python -m isdb_fpga.chain.sim VECTORS.json [--backend pysim\|cxxrtl\|verilator] [--symbols N]`) |

**Clocks** (fit, 2026-11-02; `fpga/tezuka/isdb_slots.xdc`). Three:

- `sync`, the symbol clock (`util_ad9361_divclk` = l_clk / 4, 40 MHz at
  40 Msps): the chain's stream, one symbol per clock;
- `fast`, its 4×: `axi_ad9361` l_clk itself (160.0 MHz measured). Chosen
  over an MMCM from divclk: the 4:1 ratio and phase hold by construction
  (util_clkdiv divides l_clk, and ADI's constraints already declare
  divclk a generated clock of rx_clk, so Vivado times the crossings as
  related), no MMCM, BUFG or lock to manage; either way ADI's rx_clk
  constraint (4 ns, its 61.44 Msps worst case) would apply, so the
  block's l_clk paths and its sync↔fast paths get `set_max_delay 6.250`
  (the real period, skew and hold kept). The crossing is clock-enable
  phasing, no FIFO: `common.Phase4`'s phase is a fast register (a counter
  kept in step by a toggle of `sync` registered twice in `fast`), and a
  `sync` value enters fast logic through one fast register (or a
  register's select, the demapper's squarer operand) with no logic of the
  `sync` side's after it: such a register holds a symbol clock's values in
  fast cycles 1, 2, 3 and the next 0, which the blocks address with
  `Phase4.late` (the phase one fast cycle late; the equaliser's products
  move two fast cycles, still inside their symbol clock); `sync` reads
  fast registers written by phase 2. Vivado (build 4, 2026-11-03) had
  timed the unregistered crossings at −2.9 ns (the equaliser's tap select
  into the DSPs) and −1.6 ns (the phase toggle into the BCH).
  The demapper's metrics (16 products for 32 points: 8 per fast cycle)
  and the equaliser's FIR (11 products, one kind per phase) run here. The
  chain is valid at 40 Msps only (l_clk 160 MHz);
- `ldpc`, the decoder's 200 MHz from an MMCME2_BASE in the block on the
  PS's clk_fpga_0 (100 MHz × 10 / 5), reached through `LdpcBridge`'s
  AsyncFIFOs (64 beats in, LUT RAM; 64 words out) and the tag FIFO,
  `set_max_delay -datapath_only 5.0` both ways. Not 160 MHz on the fast
  clock: the queue model (`ldpcqueue.py`, S = 34, K = 3, the default
  limit 14, TMCC staged, with the result stream at the chain's pace) gives
  9 overruns of 363 codewords on the measured distribution at 160 MHz, 0
  at 200 MHz;
  and 32APSK's 163 M LLR/s (one LLR per decoder cycle) exceeds 160 MHz.
  At 200 MHz the bridge's serialiser (n decoder cycles a beat of n
  values) needs 192.6 M cycles/s for 32APSK while a TMCC burst runs
  (`ldpcqueue.port_cycles`); its peak, a 5-LLR beat on every symbol
  clock, is exactly 5 cycles a clock, so the input FIFO holds only the
  crossing's few beats.

The BCH unit runs in `fast` (`finish.FastBch`): Chien one position per
fast cycle, Berlekamp-Massey's product steps two fast cycles each (the
operands registered, then the product used: Vivado had a locator read,
the GF(2¹⁶) multiplier and a locator write in one 6.25 ns cycle, 10 LUT
levels, −3.3 ns); `tools/bch_cycles.py` measures it: a 93/120 word with
12 errors takes 8 993 symbol clocks after its last beat (8 855 with
one-cycle steps), a TMCC word 2 703, an error-free word 7. Stage 1 holds the input
for about 0.7 % of the clocks (its decisions) and stage 3's ring when its
reader stalls; `SlotsRx` puts a 1 024-symbol FIFO in front, the front end
delivering 84 % of the clocks.

**Register map** (`SlotsRx`, the front end's command protocol of
`control.py` at 0x400–0x6FF; the host's copy is
`isdb_frontend::control::chain`). The one source is
`isdb_fpga/chain/registers.toml` (issue #30 item 5): per register the
address, access, width, reset, bit fields, read and write semantics, the
FEATURES flag that gates it, how a host reads it consistently and what
the host does with it, plus the CTRL bits, the command timing and the
host's open sequence and slow-loop steps. This table and `top.py`'s
docstring table are generated from it (`uv run --locked python
tools/regmap.py`; `--check` fails when either is stale);
`tests/test_registers.py` checks `top.py`'s REG_* constants, FEATURES
and `control.py` against it and simulates every register's documented
behaviour through `gpio_out` (pysim, three simulations of 10, 10 and
17 s: every
address's reset value, every read-only register and unmapped address
ignoring all-ones and all-zeros writes, walking ones and zeros through
the read/write registers, CARRIER and the taps going in use only at
START (the taps with INIT bit 0), TAPS_FROM arming the equaliser with
the frame written, the SUMS_CAPTURE strobe and its release by START, the
sum registers answering 3 symbol clocks after the decode with the
previous value held, FEATURES, the CTRL bits and the ID word); on the
host, `crates/isdb-frontend/tests/chain_registers.rs` checks every
address, field and reset constant of `isdb_frontend::control::chain`,
and `isdb-radio`'s `chain::tests::regmap` runs the open sequence and the
slow loop on its mock backend against the map's steps and the registers
each side may write. The host writes LAYOUT_LAG at open (its map's
`ChainOptions::layout_lag`) although this image builds lag 2 only and
ignores the write: the map records it (`ignored_write = 2`, the value the
host writes and the register reads), the one read-only register the host
writes on an image without a FEATURES flag for it. The host never touches
LLR_SCALE, LEARNED_FROM, MAX_ITER and AGC (the status blocks carry n0, K
and the learned frame; MAX_ITER and AGC keep their resets).

<!-- regmap:begin: generated from isdb_fpga/chain/registers.toml by tools/regmap.py -->
| Address | Register | Access | Reset | Bits | Read | Write | Host |
|---|---|---|---|---|---|---|---|
| 0x400 | VERSION | ro | 2 | — | the `Slots` format the chain writes: 2 (format 1 and the sums block); the ID word's bits 11-8 read the same | ignored | reads: open: must be a format this host reads (isdb_frontend::slots::readable) |
| 0x401 | FEATURES | ro, 28 bits | 0x0022_0EF3 | 0 TAPS (the host may write taps (TAPS, TAPS_FROM)), 1 SUMS (the training sums are readable (SUMS_FRAME, SUMS_COUNT, SUMS_R, SUMS_P)), 2 CONSTELLATION (not built, reads 0: the host may load a constellation at START (CONST, CONST_MASK, INIT bit 1)), 3 N0 (not built, reads 0: the host may load n0 at START (N0, INIT bit 2)), 4 SUMS_CAPTURE (the training sums are held from a host-armed frame end (SUMS_CAPTURE)), 5 DECODER_HEALTH (DECODER and RECLAIMED exist; the decoder frees the banks of abandoned codewords), 6 SUMS_SEQ (SUMS_CAPTURE's bits 2 and 15-8; a sum register's READ is answered in 3 symbol clocks, the register keeping its last value until then), 7 SUMS_STREAM (each frame's latched sums also leave in the `Slots` stream, a sums block after its status block: the host needs none of the sum registers), 15-8 MAX_ITER (the LDPC iteration limit: MAX_ITER's value), 27-16 STEP (the LDPC decoder's step (34; 0 sequential)) | what the image offers (the flags below), the LDPC iteration limit (MAX_ITER's value) and the decoder's step | ignored | reads: open; the slow loop once (SUMS_CAPTURE) |
| 0x402 | CARRIER | staged, signed | 0 | — | the carrier register in use (2^-32 turn per symbol; the chain tracks the carrier itself) | the carrier register's value for the next CTRL START (in use: CTRL START) | reads, writes: open writes the host's carrier; the slow loop's register path reads it every tick (informational) |
| 0x403 | LAYOUT_LAG | ro | 2 | — | the layout lag: 2 (lag 1 is not built) | ignored: the host's map defines it as the lag START loads (`ChainOptions::layout_lag`, 1 or 2); this image builds lag 2 only and ignores the write | writes: open writes the host's lag (2, the only one this image has) |
| 0x404 | INIT | rw, 3 bits | 0 | 0 TAPS (START puts the taps written to TAPS in use), 1 CONSTELLATION (START loads CONST for the modulations of CONST_MASK (FEATURES CONSTELLATION)), 2 N0 (START loads N0 (FEATURES N0)) | the value written | which host values CTRL START loads: bit 0 the taps written to TAPS (else the taps in use stay); bits 1 and 2 are stored and ignored (FEATURES CONSTELLATION and N0 are clear) | writes: open, last before START |
| 0x405 | STATUS | ro, 3 bits | 0 | 0 LOCKED (the slot sync is locked), 1 FRAMED (a frame head since the (re)start), 2 DECODING (the last frame decoded since the (re)start: a layout in force (a restart clears it)) | the chain's state | ignored | reads: the slow loop's register path every tick; the error report (`ChainControl::state`) |
| 0x406 | FRAME | ro | 0xFFFF_FFFF | — | the last whole frame's counter (0xFFFF_FFFF none) | ignored | reads: the slow loop's register path every tick (the identity's TAPS_FROM); the error report |
| 0x407 | SEQ | ro | 0 | — | the next `Slots` block's sequence number (0 after CTRL START) | ignored | reads: the error report |
| 0x408 | N0 (writable with FEATURES N0) | ro | 0xCD4C | — | n0 in force (Q.20; 52 556, Es/N0 13 dB, until learned) | ignored: on an image with FEATURES N0, n0 for START (INIT bit 2); this image ignores the write | writes: open writes the host's n0 only on an image with FEATURES N0 |
| 0x409 | LLR_SCALE | ro | 0x0001_3F3A | — | K in force: round(2^32 / n0) | ignored | — |
| 0x40A | LEARNED_FROM | ro | 0xFFFF_FFFF | — | the frame n0 and the constellation in force were learned on (0xFFFF_FFFF: the defaults) | ignored | — |
| 0x40B | CONST_MASK (writable with FEATURES CONSTELLATION) | ro, 5 bits | 0 | — | bit m: modulation m's points in force are learned, not the nominal ones | ignored: on an image with FEATURES CONSTELLATION, the modulations whose CONST points START loads; this image ignores the write | writes: open writes it only on an image with FEATURES CONSTELLATION |
| 0x40C | MAX_ITER | rw, 8 bits | 14 | — | the value written (FEATURES bits 15-8 read it too) | the LDPC iteration limit each codeword is committed with (reset `ldpc.DEFAULT_MAX_ITER`) | — |
| 0x40D | AGC | rw, 16 bits | 0 | — | the value written | the input AGC gain the status blocks report (Q6.10) | — |
| 0x410 .. 0x41A | TAPS (FEATURES TAPS) | staged | [5] 0x4000, else 0 | 15-0 RE (real part, Q2.14), 31-16 IM (imaginary part, Q2.14) | tap k in use (re + (im << 16), Q2.14; the identity after configuration) (consistency: the 11 registers change together at a frame start; TAPS_FROM reads their frame) | tap k of the next set (in use: TAPS_FROM N: from frame N + 2; CTRL START with INIT bit 0: at once) | writes: open writes the host's taps (the identity without), the slow loop each solve before TAPS_FROM |
| 0x41B | TAPS_FROM (FEATURES TAPS) | staged | 0xFFFF_FFFF | — | the frame the taps in use were solved from (0xFFFF_FFFF: the identity, or taps START loaded) | after TAPS: the frame N the taps were solved from; commits them (arms the equaliser with N and N + 2 in the clock it is written: 5f60649) (in use: the first frame >= N + 2 (the model's EQ_APPLY_FRAMES)) | reads, writes: the slow loop writes it after each set of TAPS and reads it every tick; the error report |
| 0x420 | SUMS_FRAME (FEATURES SUMS) | ro | 0xFFFF_FFFF | — | the frame of the latched training sums (0xFFFF_FFFF: none yet) (consistency: read before and after the sum registers: the same frame, or the latch moved) | ignored | reads: the slow loop's register path every tick and after the sums; the error report |
| 0x421 | SUMS_COUNT (FEATURES SUMS) | ro, 16 bits | 0 | — | training symbols in the latched sums | ignored | reads: the slow loop's register path every tick |
| 0x422 | SUMS_CAPTURE (FEATURES SUMS_CAPTURE) | strobe, 16 bits | 4 | 0 VALID (latched and held since the write), 1 ARMED (waiting for a frame end), 2 NO_LAYOUT (STATUS bit 2 clear: the last frame end had no layout, nothing latches and an armed capture waits), 15-8 SEQ (latches since configuration (wrapping)) | the capture's state: VALID, ARMED, NO_LAYOUT (FEATURES SUMS_SEQ), SEQ (FEATURES SUMS_SEQ) (consistency: SEQ read before and after a set of sum reads: equal, no latch moved in between) | any value: the next frame end with a layout latches the training sums and holds them (later frames are summed, not latched) until the next write. Before the first write every frame end latches; a restart (START, a lost sync, the chain's stop) releases a hold and an armed capture | reads, writes: the slow loop's register path: reads it every tick (first, with FEATURES SUMS_CAPTURE, and last), writes it to arm a capture; the error report |
| 0x430 .. 0x437 | COUNTERS | ro | 0 | frames, frames_undecoded, tmcc_failed, slots, ldpc_failed, bch_failed, iterations (low), iterations (high) | the status block's counters: whole frames, a frame's slots counted at its TMCC result (the slots of a frame a restart dropped are not counted); the iteration sum in two registers, low first (consistency: none: each moves at a frame's end and the iteration sum's two words can tear across one (informational: the error report)) | ignored | reads: the error report (`ChainControl::counters`) |
| 0x438 | TMCC_OVERRUNS | ro | 0 | — | TMCC codewords the decoder dropped (also counted as TMCC failed) | ignored | reads: the error report |
| 0x439 | DECODER (FEATURES DECODER_HEALTH) | ro, 28 bits | 0x0300_0000 | 15-0 OVERRUNS (codewords dropped for want of a free bank since configuration, slots and TMCC (wrapping)), 16 QUEUE_OVERFLOW (sticky: a commit the result queue could not take), 23-20 HELD (banks allocated now (0 once the chain has stopped, 3.3 ms after its input, and the decoder is idle)), 27-24 BANKS (K (3)) | the LDPC decoder's health, a snapshot from its clock under 1 us old (consistency: one word from one snapshot) | ignored | reads: open (banks held before START: warned); the error report |
| 0x43A | RECLAIMED (FEATURES DECODER_HEALTH) | ro | 0 | 15-0 ABORTED (banks freed by the abort after a frame drop or a restart (a lost sync, START, the chain's stop)), 31-16 REUSED (banks a codeword's begin took back from its context's abandoned codeword) | decoder banks freed since configuration (wrapping) | ignored | reads: open; the error report |
| 0x500 .. 0x607 | SUMS_R (FEATURES SUMS) | ro | 0 | — | entry i < 66 of R at 0x500 + 4i: re low 32, re high 16 sign-extended, im low, im high (the latched bank); a READ is answered 3 symbol clocks after it is decoded, gpio_in keeping the previous READ's value until then (consistency: the latch moves at a frame end: SUMS_CAPTURE's hold and SEQ, or SUMS_FRAME before and after; the same values leave in each frame's sums block (FEATURES SUMS_STREAM)) | ignored | reads: the slow loop's register path, after a capture held (or a new SUMS_FRAME) |
| 0x610 .. 0x63B | SUMS_P (FEATURES SUMS) | ro | 0 | — | entry i < 11 of p at 0x610 + 4i, as SUMS_R; a READ is answered 3 symbol clocks after it is decoded, gpio_in keeping the previous READ's value until then (consistency: as SUMS_R) | ignored | reads: as SUMS_R |
| 0x640 .. 0x6DF | CONST (writable with FEATURES CONSTELLATION) | ro | 0 | — | not built: reads 0 (point t of modulation m at 0x640 + 32m + t on an image with FEATURES CONSTELLATION) | ignored: on an image with FEATURES CONSTELLATION, the points START loads; this image ignores the write | writes: open writes the host's points only on an image with FEATURES CONSTELLATION |
<!-- regmap:end -->

**Simulation** (`tests/test_chain.py`, part of `mise run fpga:test`): three
backends behind one stream API (`simx.py`): Amaranth's Python simulator
(about 6 500 cycles/s on the stage-1 block, 2 000 on the back end) and
two compiled ones driven by the same stream harness (`harness.cc`):
Verilator (`simv.py`: Yosys `write_verilog`, Verilator 5, `make -j`;
built and cached in `build/verilator/`) and CXXRTL (the venv's Yosys and
the system C++ compiler, one translation unit; cached in `build/cxxrtl/`).
The whole-set tests run on `simx.compiled_backend()`: `FPGA_SIM` if it
names a compiled backend, else Verilator where it is installed, else
CXXRTL, else they skip; `FPGA_SIM=cxxrtl` (or `verilator`) also makes it
the default of `python -m isdb_fpga.chain.sim` ([backends](#simulation-backends)).
Every run also checks the [invariant monitors](#invariant-monitors).
Measured on this host, 2026-11-02, the synthetic sets (`chainvectors`,
B44 frames of 16APSK 93/120, −400 Hz drifting, 14 dB; the counts are
today's sets', the times those of 2026-11-02's 5-frame sets):

| Stage | Vectors | Compared | Python simulator (default test) | Compiled |
|---|---|---|---|---|
| 1 + 2 | chain-1-sync | r of every symbol and φ every 4 096, the correlations and accumulator of the first 30 000 positions, every period (257), every head (252), both carrier updates; its `relock` record (the same from symbol 3 321, 100 000 symbols, locked at peak position 6 975) | 120 000 symbols (lock, parity, 8 heads with FSync signs), 18 s | the whole set, 2 390 072 symbols (the frame heads, two carrier updates); the relock record |
| 3 | chain-2-phase | φ, gain, y, r of 18 592 symbols | the whole set, 3 s | 0.1 s |
| 4 | chain-3-eq (+ chain-4's layout) | z of the frame's 1 115 520 symbols, the 6 720 training references, the 308 sum words, count and frame | 18 592 symbols (2 slots' z, references and MACs), 13 s | the whole frame, 4 s |
| 5 | chain-4-tmcc | 31 680 soft bits, the 31 682 burst beats (port granted on half the cycles), 9 422 information bits, the result, 120 layout entries and their commit | the whole set, 40 s | 2 s; also with 7 bit errors corrected |
| 6 (4–6) | chain-3 → chain-5 (+ chain-4) | all 96 codewords' 1 077 120 beats (count, flags, tags), both codewords chain-5 carries in full (every position and LLR, the de-interleaved words), 16 learned points, n0 and K for the next frame, the TMCC soft bits; again with the next frame (a layout queued) starting a cycle after the frame's last symbol, as on the board's gapless stream, and 16 of its main symbols (n0 and K still the frame's; K in force in time) | 12 000 symbols (the first codeword whole), 7 s | the whole frame, 9 s; with the next frame's start, 8 s |
| 8 (BCH) | gf.py (the Rust decoder's port) | 32 random words of 9 614 and 34 776 bits with 0–20 errors: result, count, roots | 2 words | 32 words |
| bridge | — | the 40 MHz port to the 200 MHz decoder ports and back, two clocks | the test | — |
| 1–9 | chain-6-chain (+ chain-3's taps) | every `Slots` word: 5 755 224 symbols in, 632 603 words out (5 frames: TMCC and status blocks; frames 2–4 decoded, 360 slot blocks, 3 sums blocks; frames 3 and 4 with learned n0, K and points), the LDPC stub answering each frame after its TMCC codeword is committed | — | 2 400 000 symbols (frames 0 and 1's blocks, 1 366 words) by default: about 45 s on Verilator (7 s of it simulation, 380 000 cycles/s; the first build 23–43 s), 130 s on CXXRTL (27 000 cycles/s; the first build about 3 minutes); the whole set (`FPGA_CHAIN_FULL=1`) 171 s on CXXRTL before its harness settled combinational outputs (38 000 cycles/s) |

**Value coverage** (`tools/value_coverage.py`, issue #30 item 1;
`tests/test_coverage.py` in `mise run fpga:test`, about 5 s of JSON
parsing). A simulation checks only the values its set's expected outputs
contain: build 7's n0 that was never learned and its shifted learned
points passed every set because every expected status block had the
reset n0 and none had learned points. The tool reads every chain and LDPC
set in `build/` and reports, per field (the `Slots` header, slot, TMCC, sums
and status words, the status block per word; each stage set's heads,
periods, carrier updates, φ and gain, taps and sums, layout, points,
LLRs, learned points, n0 and K; each LDPC codeword's rate, schedule,
limit, iterations, convergence and LLRs), the number of distinct values
and the sets that carry them. It flags constant and absent fields by
their boundary (always 0, the reset value, "none", a flag never set) and
lists the boundary values never taken (an uncorrectable BCH word, the
iteration limit 14, i8 saturation), the enumerated values never seen
(modes, modulations, rates), the status words equal to their predecessor
in every block (a one-word shift passes there) and the `Slots` fields
that never change inside a set (a value a frame late passes there).
`python tools/value_coverage.py [--markdown] [--all-fields]
[--synthetic] [--no-excuses]`; the test writes the full report to
`build/value_coverage.md`. The test fails on a constant or absent field
that is in neither of the tool's two lists: `ALLOWED`, the constants
accepted by definition with the reason each cannot vary (the format
version, reserved bits, a dummy slot's words, layout lag 2), and
`KNOWN_GAPS`, the gaps of today's corpus with the input that would close
each. When the tool came every whole-chain set decoded one frame, its
last whole one, so no status block had a learned n0, K, constellation or
`taps_from` (33 entries); the window sets now decode frames 2–4,
chain-11 changes the taps in use (32 entries) and chain-12 fails a slot's
LDPC, a slot's BCH and a TMCC, corrects a TMCC and misses and strays a
frame marker (20 entries; still one mode, 16APSK 93/120); chain-13
sends four transmission modes and the host's AGC writes (8 entries);
chain-1 runs two frames past its first frame head (two carrier updates)
and relocks from another start (6 entries: the 32APSK points and
chain-5's single-mode, first-decoded frame); chain-14 sends 32APSK (5
entries: chain-5's single-mode, first-decoded frame). It also fails
on a known gap that a set now covers, so
the list shrinks as the generators (`chainvectors.rs`, `ldpcvectors.rs`)
grow. The gate reads the default synthetic corpus only; the recording
sets are reported, not gated.

**Estimates** (`mise run fpga:chain-estimate`: Yosys `synth_xilinx`,
before place and route; 6-bit input; LUT includes the LUT RAM cells'
LUTs, which Vivado counts as Slice LUTs). Before (piece 4) and after the
fit (2026-11-02) against the fit's targets:

| Block | LUT before → after | FF before → after | BRAM36 before → after | DSP48E1 before → after | Target LUT / BRAM36 / DSP |
|---|---|---|---|---|---|
| `SyncFront` (NCO, slot sync, carrier loop) | 4 194 → **3 783** | 4 627 → 4 086 | 5.5 → **5.5** | 7 → **4** | 3 500 / 5.5 / 6 |
| `SlotPhase` | 1 122 → **1 439** | 2 116 → 2 737 | 8 → **4** | 8 → **12** | — / 4 / — |
| `Equaliser` + `FrameCtl` (own nominal ROM) | 2 682 → **2 322** | 4 546 → 4 015 | 7.5 → **3** | 50 → **15** | 2 500 / 3 / 18 |
| `TmccSoft` | 134 → **183** | 65 → 63 | 8 → **8** | 0 → 0 | — |
| `Demap` (own nominal ROM) | 14 348 → **4 909** | 11 420 → 4 909 | 7 → **2.5** | 95 → **21** | 4 500 / 3 / 22 |
| `Finish` (`TmccResult`, BCH in the 4× clock, PRBS, packer, status) | 7 301 → **5 941** | 3 339 → 2 883 | 2 → **2** | 0 → 0 | 3 500 / — / 0 |
| **sum of the rows** | 29 781 → **18 577** | 26 113 → 18 693 | 38 → **25** | 160 → **52** | 16 000 / 26 / 60 |
| `Back` (eq, FrameCtl, TMCC soft bits, demapper, one shared nominal ROM) | 7 959 | 8 670 | 13.5 | 36 | |
| chain = `SyncFront` + `SlotPhase` + `Back` + `Finish` | **19 122** | 18 376 | **25** | **52** | 16 000 / 26 / 60 |
| + the result FIFO (`results.py`, 4 096 × 18) | (in `SlotsRx`) | | 2 | 0 | |
| + bridge (LUT RAM FIFOs) and the 1 024-symbol input FIFO | 713 | 733 | 0.5 | 0 | |
| `SlotsRx` without the decoder (front end, control, chain, result FIFO, bridge, registers; one run) | **26 752** | 26 595 | **28** | **164** | |
| of which the front end (`SymbolsRx`, its own run) | 5 879 | 5 426 | 0.5 | 112 | |

(The before figures counted logic LUTs only; the LUT RAM then was 11
cells in the equaliser and 6 in `Finish`. `chain_estimate.py back`,
`bridge` and `rx` are run by name; `SlotsRx` with the decoder in one run
is too large for the 4 GB WebAssembly Yosys, so the decoder is its own
run: 14 043 LUT, 76.5 BRAM36, 0 DSP at S = 34, K = 3.)

**Against the device** (xc7z020: 53 200 LUT, 140 BRAM36, 220 DSP48E1),
the receive-only image: the fixed part (`axi_ad9361`, the front end as
Vivado placed it, DMAs and clocking: ≈ 17 700 LUT, 33 BRAM36, 140 DSP) +
the decoder (14 043 / 76.5 / 0) + the chain and its glue (`SlotsRx` less
the front end: 20 873 / 27.5 / 52) = **52 616 LUT (98.9 %), 137 BRAM36
(97.9 %), 192 DSP48E1 (87 %)** (before the last LUT steps: 53 337,
100.3 %). DSP and block RAM fit; the LUTs are at the edge (Yosys figures;
Vivado was within 1 % of Yosys on the front end, 5 824 against 5 879).
94 % (50 008) needs about 2 600 LUT less. (Vivado, build 4, 2026-11-03:
45 275 LUT, 85 %, 119 BRAM36, 176 DSP: Vivado is about 7 % under Yosys,
so the image fits; its timing remedies, docs/plans/isdbs3-fpga-rx.md
"Vivado, build 4", cost 51 LUT and 785 FF in Yosys.) What did not reach its target
is the finish stage (5 941 against 3 500; by submodule, Yosys keeping the
hierarchy: the BCH unit 2 276 (`synth/bch_parts.py unit4`: its 4-bit
syndrome networks 407, Chien 216, multiplier 185, the rest the syndrome,
locator-step and Chien registers, the roots and the control), the glue
920, the status block's word mux 603, the layout 589, the packer 429,
the `Realign` 365 and the TMCC's extraction 320, two PRBS 293 (different
polynomials), two root fixers 204), the demapper (4 909 against 4 500: 25
compare-selects of 32-bit metrics, the eight 56-bit LUT RAM lanes, 384,
the 3-bit-per-clock divider for n0 and K, about 250) and stage 1
(3 783 against 3 500). Stage 3 grew (the NCO re-applied after the ring: its table and
four products) to bring its block RAM to 4.

How each block got there (each a commit): the demapper's metrics in the
4× clock, 8 points per fast cycle (16 products; the max-log minimum over
a label set is the same in any visiting order, so per-phase minima of
four and running minima per bit position and value are the 32-leaf trees'
values), the K products on two time-shared units (Delta split at bit 17,
K ≤ 2²⁴ so its bit 24 alone is a shift), the point sets in LUT RAM, the
slot pilot accumulators in LUT RAM, the frame sums on one read port, the
position products as constant tables; the equaliser's 44 products on 11
DSPs in the 4× clock, one kind per phase, one 11-leaf tree; one compact
nominal ROM (23 distinct sets, 510 × 24 bits) for both; stage 3's ring of
the NCO input (three symbols per 36-bit word); stage 1's FSync and SSync
trees shared (C = P ± Q) and its lock rule's products as shifts; the BCH
roots flipped in the buffers rather than a per-beat mask; one `Realign`
for the slot and TMCC BCH words; the BCH locator in three LUT RAM banks
whose roles rotate (no copy); one divider for n0 and K. Then (after the
first report, each a commit, LUT saved): the BCH unit in the 4× clock
with 4-bit syndrome steps (`FastBch`; with the result FIFO, below) and
the packer's sequence number through its one CRC network (`Finish` 6 624
→ 6 235); the slot setup's Delta and Gamma on one bit-serial multiplier
(`SlotPhase` 1 540 → 1 439); one read of the syndromes and one squarer
for the BCH's even syndromes and Berlekamp-Massey (`Finish` 6 235 →
5 941). Looked at and left: a RAM-backed register readback (the
readback's word select is a mux either way: a RAM would need a copy of
every register written on change, the same mux on its write side), the
status block's word mux (the same), the demapper's metric width (32 bits
are needed: not provably narrower).

**Integration** (fit, 2026-11-02):

- **The real decoder in the chain** (`stages.run_chain_real`, `python -m
  isdb_fpga.chain.sim build/chain-7-hdl.json --backend cxxrtl`;
  `RealDecoderChainTest`, opt-in `FPGA_CHAIN_REAL=1`): `SlotsChain` +
  `LdpcBridge` + `ldpc.LdpcDecoder` (S = 34, K = 3, every rate; its
  domain at 5× the symbol clock in the simulation), the input at the
  front end's 33.76 Msymbol/s, against chain-7-hdl (chain-6's input,
  decoded by the model with the decoder's settings: `Schedule::Layered
  { step: 34 }`, limit 14, offset 1), every block compared per (frame,
  kind, slot). **Bit-exact**: all 126 blocks of the three frames the model
  sends (frame 2 decoded: 96 slots, 24 dummies; three TMCC and status
  blocks), 0 decoder overruns, no queue overflow, no lost beat; the chain
  also sends 118 slot blocks of frame 3, which the input ends inside (the
  model sends nothing of an unfinished frame; the host discards them:
  `orphan_slots`). Verilator (README, [Simulation backends](#simulation-backends)): 8.2
  M symbol clocks in 92 s plus a 45-65 s build; on CXXRTL the same run takes
  about 100 minutes (about 1 500 cycles/s; its 10.7 MB of C++ compile in 7
  minutes). The first runs failed (slot 118 of frame 2 missing, its status
  counters, 89 overruns, a queue overflow): an HDL defect, not the
  harness's: the demapper tagged a frame's last codeword with the next
  frame's counter (its register moved on at the next frame's start before
  the codeword's last beats left), the finish stage took it for the next
  frame, reset the slot PRBS there, the BCH saw a wrong word and its Chien
  search held the result stream about 35 000 cycles, so the decoder's banks
  filled. Fixed (the frame travels with each beat); the stub-based test
  could not show it (the stub supplies the tags).
- **Block order.** The decoder returns results in commit order: frame N's
  TMCC codeword (its burst four values per port beat, about 4 slots at the
  front end's rate) follows frame N + 1's first slot codewords, so frame
  N's trailing dummy, TMCC and status blocks follow frame N + 1's first
  slot blocks; `Finish` keeps their contents the model's (it holds frame
  N's last valid slot and slot counters at frame N + 1's first result)
  and nothing is buffered. The host (`isdb-cli` `slotrx.rs`) releases a
  frame when its own tmcc or status block arrives; the chain sends a
  frame's slot blocks as they are decoded, so a frame dropped later (a
  loss, a lost sync, the end) leaves slot blocks without them, which the
  host discards (`orphan_slots`; the model sends nothing of such a
  frame). The bound: the
  TMCC burst (≈ 4 slots at the front end's rate; 25 slots at one symbol
  per clock) plus its decode (under a slot), so at most the blocks of the
  next frame's first 26 slots precede a frame's TMCC block; measured on
  chain-7, -8 and -9: 3 slot blocks.
- **Failure paths** (chain-12-fail, `RealDecoderChainTest.test_failures`,
  2026-11-04): a codeword of noise at the limit (14 iterations,
  unconverged, its block's bits and counters), a converged slot the BCH
  cannot correct, an uncorrectable TMCC (the frame two on undecoded), a
  TMCC word that needs an iteration and 5 BCH corrections, a missed and a
  stray frame marker: bit-exact with the real decoder, the monitors
  quiet. **Fixed defect** (found by this set): a TMCC word whose BCH
  corrected **six or more** errors hung the chain. With chain-12's frame
  1 TMCC given a sixth error in the decoder's output (position 11 209;
  `tools/real_variant.py build/chain-12-fail.json --no-bch --no-tmcc 2
  --tmcc-pos 1:2548,2748,4547,9042,10295,11209`), frame 1's TMCC block
  (word 7 211 of the stream) left its 9 header words (the last, 0xFB79,
  the header's checksum) and its 4 first payload words (count 6,
  iterations, frame: right) and nothing more: a bad CRC, the result
  stream stalled, the decoder's result queue overflowed at symbol clock
  3 050 575 (monitor `dec.queue_overflow`), then its tag queue
  (`br.tag_overflow`). The cause: `Finish` asked `TmccResult` for the
  information bits with a one-cycle `o_go` pulse (its state `TMCC_GO`,
  after the block's 4 first words), and `TmccResult` takes `o_go` only
  in its `HOLD`, after `FixRoots` has flipped the BCH's roots in its
  buffer, 3 cycles a root. The pulse came 17 cycles after the BCH's
  result, `HOLD` 2 + 3 n: from n = 6 roots the pulse was missed and the
  finish stage waited in `TMCC_BITS` forever. The stage bench (`TmccTest`)
  leaves `o_go` at its reset value 1, so it could not show it. `o_go` is
  now a level the finish stage holds through `TMCC_BITS` (the state
  `TMCC_GO` is gone); the BCH and its values are unchanged.
  `RealDecoderChainTest` checks 6 and 12 TMCC corrections (t = 12) and
  12 TMCC corrections right after three slots of 12 corrections each
  (frame 2's slots 0-2, committed before frame 1's TMCC word, which waits
  in the result FIFO for the shared unit), every block sent as the
  model's but the counts; 7 to 11 with FPGA_CHAIN_SWEEP=1. Finish stage
  (Yosys): 5 966 → 5 996 LUT, 2 937 FF unchanged.
  (`br.tag_overflow`). One to five errors pass at any position (the
  count and the bits as the model's); 13 (uncorrectable: Berlekamp-Massey
  fails, no Chien search) pass; the TMCC stage bench alone (`TmccTest`,
  own `Realign`, no `Finish`) corrects 7. So the fault is in `Finish`'s
  TMCC path with the shared BCH unit and `Realign` (`tmcc.TmccResult`
  with `own_align=False`) once the correction takes six or more roots.
- **Mixed modes** (chain-13-modes, `RealDecoderChainTest.test_mixed_modes`,
  2026-11-04): 16APSK 93/120, 8PSK 73/120, QPSK 61/120 and π/2-BPSK
  41/120 in one TMCC with each modulation's dummy slots, the host's AGC
  writes: bit-exact with the real decoder (the slot blocks' mode,
  modulation and rate codes, frame 3's learned points of all four
  modulations, the AGC in the status), the monitors quiet.
- **32APSK after the TMCC burst** (chain-14-apsk32,
  `RealDecoderChainTest.test_apsk32_after_the_tmcc_burst`, 2026-11-04):
  bit-exact with the real decoder over frames 1-3 (250 blocks, frame 3's
  learned 32APSK points in its status), the monitors quiet, `in_lost` 0,
  0 decoder overruns. **Fixed defect** (found by this set): 32APSK slots
  right after a frame boundary lost decoder input. B44 puts the 32APSK
  mode first, so frame N + 1's first codewords are demapped while frame
  N's TMCC burst takes the port's free cycles; `LdpcBridge`'s serialiser
  spent n + 1 decoder cycles per beat (a cycle to pop the beat, then
  one per value: 6 per 32APSK symbol at 32.6 M main symbols/s, 5 per
  burst beat on the 7.4 M free cycles/s: 232.6 M cycles/s against the
  decoder's 200 MHz; 195.6 M even without the burst), so its 64-beat
  input FIFO overflowed (monitor `br.in_fifo_overflow` from symbol clock
  2 857 405, `in_lost` 9 631 by input symbol 2 450 000) and frame 2's
  32APSK slots 0–5 left unconverged at the limit (slot 2 a decoder
  overrun), where the model converges in 1–3 iterations. Now a beat of
  n values takes n cycles, back to back: the serialiser pops the next
  beat on the cycle of the last lane (or with the `start` after a
  codeword's closing beat, one cycle more per codeword), its read enable
  a function of three registers (`have`, `at_last`, `ends`), and the
  tag queue is written in that cycle (the tag still from the closing
  beat). The demand while the burst runs (`ldpcqueue.port_cycles`,
  `tools/queue_sweep.py`):

  | main symbols | none | with the TMCC burst (4 a beat) | before (n + 1 a beat): none / with |
  |---|---|---|---|
  | 32APSK, 5 LLRs | 163.0 M | **192.6 M** | 195.6 M / 232.6 M |
  | 16APSK, 4 LLRs | 130.4 M | 160.0 M | 163.0 M / 200.0 M |

  of the decoder's 200 M cycles/s (3.7 % to spare at 32APSK during the
  burst; a symbol clock's beat never needs more than its 5 decoder
  cycles, so the FIFO only absorbs the crossing). More values per burst
  beat would not help: the burst's values cost the same cycles, only
  sooner (8 a beat: 222 M). The bridge (Yosys, `synth/chain_estimate.py
  bridge`): 293 → 291 LUT, 524 → 525 FF, 5 BRAM36. A first version
  pushed the tag a cycle after the `start` cycle, when the next beat had
  already replaced it: frame 1's TMCC result carried frame 2's slot 0
  tag; `BridgeTest.test_full_rate` (two 32APSK codewords at a beat a
  clock, the second with the burst: every decoder cycle used, the FIFO
  at most 4 beats, the tags with a beat right after each closing one;
  the n + 1 serialiser overflows in it) and `test_random_beats` (random
  beats of both contexts: the events in beat order, `start` alone in its
  cycle, every tag back) check the bridge.
- **A corrected slot no longer holds the decoder.** The BCH's Chien search
  took n symbol clocks (34 776 at 93/120, 3.1 slots) while the finish
  stage held the result stream, and with it a decoder bank: a few
  consecutive slots with BCH corrections would overrun the decoder. Now
  the BCH unit runs in the 4× clock (`finish.FastBch`: 4-bit syndrome
  steps at the input's pace, Berlekamp-Massey and Chien at one step per
  fast cycle: n / 4 = 8 694 symbol clocks) and a result FIFO
  (`results.py`, 4 096 × 18, 2 RAMB36; a header 5 entries, a word 1, only
  the first k bits of a codeword kept) decouples the finish stage from
  the decoder. Worst case: at 16APSK (4 valid slots in 5, a result every
  13 750 symbol clocks on average) the finish stage takes a result in
  about 2 200, sends it in about 2 200 and corrects in about 10 000 more,
  so it keeps up with every slot corrected and the FIFO holds at most
  the next result (2 179 entries of 4 096); 32APSK (every slot valid)
  keeps up while at most about 2 in 3 consecutive slots need correcting.
  chain-10-bch (chain-7's input, the decoder's hard decisions with 12,
  1, 7, 12, 3, 12, 9, 12 bit errors in eight consecutive 93/120 slots,
  frame 1, slots 10-17: `bch_errors` in the set, the bench XORs them into
  the decoder's words): **bit-exact with the real decoder**, the eight
  slots corrected, 0 overruns, no queue overflow, the FIFO's peak 1 426
  entries (`RealDecoderChainTest.test_bch_corrections`).
- **Queue at 160 MHz** (`ldpcqueue.run(..., finish_hz=40e6)`: the result
  stream at the chain's pace and the TMCC burst at the chain's): the
  measured distribution (3 frames, mean 2.1, three isolated codewords at
  the limit, TMCC staged) overruns 9 of 363 codewords at 160 MHz, 0 at
  200 MHz (limit 16 with the decoder before its 200 MHz pipelining; limit
  14 with it, below); so the decoder keeps its 200 MHz (an MMCM on
  clk_fpga_0) and the async bridge (`tools/queue_sweep.py`).
- **The iteration limit: 14 (2026-11-03).** With the decoder pipelined for
  200 MHz (93/120: 6 125 cycles per pass, from 5 420) the queue at the
  chain's pace (`ldpcqueue.run(..., finish_hz=40e6)`, the measured
  distribution, its long codewords at the limit; `tools/queue_sweep.py`)
  gives, of 363 codewords at 200 MHz:
  
  | K | limit | overruns | with two adjacent long pairs | BRAM36 (Vivado 119 + the bridge's FIFOs in block RAM) |
  |---|---|---|---|---|
  | 3 | 16 | 9 | 15 | ≈ 123.5 |
  | 3 | **14** | **0** | 6 | ≈ 123.5 |
  | 4 | 16 | 0 | 6 | ≈ 140.5: over the device |
  
  (K = 3 at limits 12 and 10: 0 as well; at 160 MHz, limit 14: 9.)
  32APSK 109/120 (every slot valid, 4 738 cycles per pass;
  `tools/queue_sweep.py --rate 109`, the input port at the bridge's pace
  of n cycles a beat, "Integration"), of 363 codewords at 200 MHz:

  | K | limit | overruns | with two adjacent long pairs |
  |---|---|---|---|
  | 2 | 14 | 15 | 15 |
  | 3 | **14** | **0** | 0 |
  | 3 | 16 | 0 | 6 |
  | 4 | 16 | 0 | 0 |

  So K = 3
  and the default limit 14: `ldpc.DEFAULT_MAX_ITER`, the chain's MAX_ITER
  reset, the model's `chain::ldpc::HDL_MAX_ITER` (`FixedLdpc::hdl`) and the
  host's `control::chain::MAX_ITER_RESET` agree; the host may write
  another limit (MAX_ITER), and K = 4 stays a build parameter (`banks`).
  The sensitivity cost is small: at 13.0 dB the layered decoder fails 3 of
  18 336 codewords at limit 16 and 47 at 12 (the piece-1 table), 14 between.
  With it the regenerated decoder-settings sets (chain-7-hdl, -8-loss, -9-fade, -10-bch; no codeword of them needs more than 4 iterations) are bit-exact with the real decoder, and the FPGA suite and cargo test pass.

- **S = 22 as a LUT fallback: not usable.** The decoder at S = 22, K = 3
  (`synth/ldpc_estimate.py 22`): 9 647 LUT (−4 396) but 92 BRAM36
  (+15.5): the device would need 152.5 of 140. At K = 2 (70 BRAM36, the
  two-bank estimate) the queue overruns 15-36 of 363 codewords at every
  limit from 8 to 16. And at K = 3 (passes before the decoder's
  pipelining) its queue at 200 MHz overruns 9 (21
  with the adjacent pairs) at limit 16, 0 only at limit ≤ 10 (≤ 8 with
  the pairs); S = 34 overruns 0 (6 with the pairs).
- **Loss and sync loss** (chain-8-loss: 6 144 symbols lost half-way into
  frame 1; chain-9-fade: the signal replaced by noise for 330 000 symbols:
  the slot sync lost after 8 periods and regained; both decode frame 3
  after the event; `LossChainTest` with the stub, `FPGA_CHAIN_FULL=1`, and
  `RealDecoderChainTest`): bit-exact with the stub (every Slots word, 7.6 M
  cycles each, 12 minutes on CXXRTL) and with the real decoder (every block
  of the model's frames 1-3, 0 overruns; about 105 s each on Verilator).
  The loss runs first deadlocked stage 1 (a symbol accepted in the cycle
  the restart took effect completed a window with the counters of
  before), fixed; no set had a loss before.
- **`SlotsRx`**: the decoder inside (S = 34, K = 3) with its MMCM
  (`mmcm=True`), the register map of `isdb_frontend::control::chain` (the
  host's; three registers added there: MAX_ITER, AGC, TMCC_OVERRUNS; the
  first image offers taps and sums, not a host constellation or n0, and
  lag 2 only), CTRL START restarting the chain; `SlotsRxRegisterTest`
  through `gpio_out`. `mise run fpga:slots-verilog` writes
  `build/isdb_slots_rx.v` (module `isdb_slots_rx`: clocks `clk`,
  `fast_clk`, `ldpc_ref`; about 374 000 lines).
- **Build 8 issues** (#27, #28, #29, a second board running build 8;
  2026-11-04; docs/plans/isdbs3-fpga-rx.md "Build 8 issues"). The
  register map (`top.py`'s docstring and `isdb_frontend::control::chain`)
  gains:

  | Address | Bits | Meaning |
  |---|---|---|
  | 0x401 FEATURES | 5 DECODER_HEALTH | DECODER and RECLAIMED exist; the decoder frees the banks of abandoned codewords |
  | 0x401 FEATURES | 6 SUMS_SEQ | SUMS_CAPTURE's bits 2 and 15-8; a sum READ is answered in 3 symbol clocks |
  | 0x405 STATUS | 2 DECODING | now also cleared by a restart (the restart drops the layouts) |
  | 0x422 SUMS_CAPTURE (read) | 2 NO_LAYOUT | STATUS bit 2 clear: nothing latches, an armed capture waits. A restart (START, a lost sync, the chain's stop) releases a hold and an armed capture (so a run reading the sums blocks is not starved by the hold a register-reading run left) |
  | 0x422 SUMS_CAPTURE (read) | 15-8 SEQ | latches since configuration (wrapping): the same before and after a 308-word set, no latch moved |
  | 0x439 DECODER (ro) | 15-0 OVERRUNS, 16 QUEUE_OVERFLOW, 23-20 HELD, 27-24 BANKS | the decoder's dropped codewords (wrapping), its sticky queue overflow, the banks allocated now, K (3); a snapshot under 1 µs old |
  | 0x43A RECLAIMED (ro) | 15-0 ABORTED, 31-16 REUSED | banks freed by the abort (after a frame drop, a lost sync, START or the chain's stop), banks a begin took back (wrapping) |
  | 0x500 .. 0x63F SUMS (ro) | | a READ answered 3 symbol clocks after it is decoded, the readback keeping the previous READ's value until then (it showed 0 for up to about 4 400) |

  **START and the decoder**: the decoder's clock is not reset by START.
  The restart (as a lost sync or a dropped frame) gives up the slot
  codeword being loaded and the back end sends the decoder its abort,
  which frees the bank ([Abandoned
  codewords](#the-ldpc-decoder-ldpcdecoder)); committed codewords decode
  and leave as always. A stop (CTRL without SLOTS) lets them drain and
  stops the chain's input (deselected, the FIFO takes no symbol); after
  `top.STOP_AFTER` symbol clocks without input (131 072, 3.3 ms) the chain
  restarts itself (the chain's stop, a loss of 0 symbols without START's
  carrier, taps and sequence number), which aborts the codeword the input
  stopped inside. So after a stop every bank is free (DECODER HELD 0)
  before any START, no context is open, the bridge's FIFOs, tag queue and
  the result FIFO are empty; RECLAIMED ABORTED counts the stop's abort
  when it freed a bank (build 9 read "1 of 3 banks held" at every open:
  the previous run's stop had left its open codeword's bank for START's
  abort). A START while the chain runs may be followed by blocks of
  codewords committed before it and by the TMCC codeword of a burst
  already begun (never cut; each block carries its frame counter; the
  layout it decodes after the restart stays queued, the restart epoch
  below). START with INIT bit 0 puts the taps last written to TAPS in use
  at once (an armed TAPS_FROM set dropped); without it the taps in use and
  an armed set stay.

  **Without a layout** the chain still sends, per whole frame, its TMCC
  block (604 words with header and trailer) and its status block (79, + 4
  · 2^m per learned modulation m): 683 to 807 words per 33.05 ms, at
  least 20 668 words/s; nothing while the sync is unlocked or no frame
  ends whole.

  Fit (Yosys): `SlotsRx` without the decoder 26 395 → 26 487 LUT, 27 119
  → 27 245 FF, BRAM and DSP unchanged; the decoder 12 510 → 12 538 LUT.
- **Sums in the stream** (`Slots` format 2, 2026-11-04;
  docs/plans/isdbs3-fpga-rx.md "Sums in the stream"). Each decoded
  frame's training sums leave in the stream as a sums block (type 4)
  right after the frame's status block: the frame, the count and the 77
  complex 48-bit sums as six words each (465 payload words, 476 with the
  header and the CRC-32C trailer; `isdb_frontend::slots` "sums"), about
  14 400 words/s. `SumsReader` (sums.py, hooked into `Finish` after the
  status block) reads the latched bank through `eq.py`'s second readback
  request (`sr_*`: the accumulator RAM's read port for one cycle, a MAC
  due then waiting a cycle, a host READ in the same cycle first), an entry
  every 10 cycles or so; a frame whose sums did not latch at its end (no
  layout, or a SUMS_CAPTURE hold keeping an earlier frame) gets none, as
  the model (which has no capture). The chain's arithmetic is unchanged.
  The register map gains:

  | Address | Bits | Meaning |
  |---|---|---|
  | 0x400 VERSION | | 2: the `Slots` format (format 1 and the sums block); the ID word's bits 11-8 read 2 |
  | 0x401 FEATURES | 7 SUMS_STREAM | each frame's latched sums also leave in the stream, a sums block after its status block: a host needs no sum register (and arms no capture) |

  SUMS_CAPTURE, its hold and the sum registers stay: the proven read path,
  which this host still takes on an image without the bit, or without a
  consumer of the stream to hand it the blocks (a host binary of builds
  1-9 refuses this image at open: it reads `Slots` format 1 only, and the
  ID word and VERSION read 2). Tests: every whole-chain set's sums block bit-exact with the model
  (chain-6 with the stub, `FPGA_CHAIN_FULL=1`; chain-7 to chain-10 with the
  real decoder, `FPGA_CHAIN_REAL=1`; the chaos runs, which compare it as a
  status block after an undrained restart, and the restart test, which
  checks it is there); `SumsBlockTest` (`Finish` against a stand-in
  bank with sums over the whole 48-bit range: the vectors' stay below
  2^33; and no block when the latch holds another frame); the three
  monitors above, quiet everywhere and each fired by its defect; the value
  coverage reads the block's fields (`sums.r`, `sums.p` vary;
  `sums.count`, `sums.frame_lag` and `sums.after_status` are constant by
  definition, in `ALLOWED`). Fit (Yosys, `mise run fpga:chain-estimate`,
  at `PYTHONHASHSEED=0`): `SlotsRx` without the decoder 26 487 -> 26 641
  LUT (+154, 0.29 % of the device), 27 245 -> 27 415 FF (+170: 96 of them
  the entry's register), 32.5 BRAM36 and 164 DSP48E1 unchanged; by stage
  `Finish` 5 832 -> 5 988 LUT, 2 960 -> 3 024 FF, the equaliser with
  `FrameCtl` 2 502 -> 2 528 LUT, 4 724 -> 4 830 FF. Build 9 placed 43 730
  LUT (82.2 %): about 82.5 % with this. Not built.
- **Chaos runs** (issue #30 item 3; `isdb_fpga/chain/chaos.py`,
  `tests/test_chaos.py`, `tools/chaos_run.py`, `tools/chaos_table.py`).
  `run_chain_restarts` replays #28's one recipe; a chaos run draws random
  host events from a seed into a real-decoder set (chain-7-hdl, -8-loss,
  -9-fade, -10-bch) and runs them on `RealChainBench` (Verilator):
  - **what it injects**: a long stop (no input for 200 000 - 400 000
    symbol clocks: committed codewords drain, and after 131 072 the
    chain's stop restarts it, aborting the open codeword) then START (C_START: a loss of 0 symbols, CARRIER 0 and the taps last
    written loaded, the sequence from 0); a short stop (100 - 20 000
    clocks: the chain has not drained) then START; a START while the input
    runs; an input loss (C_LOSS, 1 - 150 000 symbols not delivered, the
    chain told so); a TAPS write (identity, chain-3's or those perturbed:
    the next START loads them, as INIT bit 0). After a START the input
    continues, skips the front end's symbols of the stop, or replays from
    an earlier index (a new run), within a budget of delivered symbols. A
    restart drops the layouts, so the chain decodes again only in its
    third whole frame: three in four events go into that frame of the
    current run, where slot codewords are loading (the abort path);
  - **what it checks**, from the bench's ports after each event,
    350 000 symbols after each restart and at the end: never a queue
    overflow, a lost beat, a port loss, a late beat, a TMCC or decoder
    overrun, more than K banks held; after a long stop (before START) the
    chain stopped once (`stops`), every bank free, a bank free to load, the
    bridge's FIFOs, tag queue and result FIFO empty, and 3 000 clocks after
    its START the same and one more abort beat; the same 350 000
    symbols after any restart (no codeword can begin before a whole frame);
    the `Slots` stream parses and its sequence numbers run from 0 after
    each START (the first block whose first word follows the START's
    cycle: `ChaosBench` stamps both). Invariant monitor flags (ports
    `mon_*` or the bench's `MONITOR_PORTS`) are read and reported when the
    bench has them;
  - **the model restarted at the same points**: `cargo run --release -p
    isdbs3-phy --example chainscenario -- SET EVENTS OUT` runs
    `isdbs3_phy::chain` over the same delivered input with `Chain::loss`
    and `Chain::start` (CTRL START's effect: `loss(0)`, CARRIER, the taps
    last written in use, `taps_from` none, an armed TAPS_FROM set dropped; a new
    `Slots` writer); with no events it reproduces each set's
    `slots_words` (`ChaosModelTest`, by default). Every block is compared
    per (frame, kind, slot), as in `run_chain_real`, frames whose slot
    blocks the chain sent before a drop (orphans) counted, not compared.
    Allowed by design: the model has no clock and hands a frame out when
    its last look-ahead symbol is in, while the HDL's pipeline (stage 3's
    slot, the FIR, the demapper, the TMCC burst and decode) is still in
    it, so a restart that does not let the chain drain within 8 slots of a
    frame's end may find the HDL inside a frame the model handed out (the
    HDL drops it), or keep that frame's layout (the restart epoch, "What
    is left": the frames it decodes alone are excused those slot blocks
    only; their TMCC blocks are compared as any other and their status
    blocks but for the counters, `CompareTest`); from such an event on
    only the decoded bits, BCH results and convergence are compared. Otherwise iteration counts must match
    too: both sides keep the NCO's phase across a restart (unlike
    `run_chain_restarts`, whose replay is compared with the model's run
    from reset).
  - **running it**: `FPGA_CHAIN_REAL=1` runs
    `ChaosTest.test_pinned_events` (chain-7-hdl, `chaos.CI_EVENTS`: an early
    loss, #28's stop inside frame 2's first codewords and START with a
    replay, a short stop and a START before the replay's first frame head;
    7.4 M symbols, 9.8 M clocks: about 4-5 minutes with the build on
    this host); `FPGA_CHAIN_CHAOS=20` (or
    `5-9`, `3,7,11`) adds a test per seed and set; `python
    tools/chaos_run.py SET SEED` (`--draw`, `--replay FILE`, `--place
    EVENTS.json`, `--pinned`) prints the events, the health after each and
    the comparison, and `tools/chaos_table.py` tabulates the results in
    `build/chaos/`.

  Results on today's HDL (2026-11-04, Verilator 5.054, `FPGA_CHAIN_CHAOS=20`:
  80 runs, 409 restarts (115 long stops, 88 short, 82 STARTs while
  running, 87 losses, 37 of chain-8's own) and 81 tap writes, 6.8 - 13.2 M
  symbols each, 29.7 minutes on 12 xdist workers, 150 - 320 s and about
  1.2 GB per run):
  - **the decoder and bridge are healthy everywhere**: in every sample of
    every run no bank leaked (every long stop's START and every settled
    restart: 0 held, a bank free, the four queues empty; up to 2 held at an
    event: a committed codeword and the open one), no overrun, no queue
    overflow, no lost or late beat, at least one abort beat per restart; the
    stream parses everywhere;
  - **defect (fixed): the sequence number survived a START during a
    block.** `Packer` restarted the sequence only in its IDLE state, and
    `SlotsRx` (as the bench) pulses `start` for one cycle, so a START while
    a block was leaving (a 93/120 slot block takes about 2 200 of a slot's
    11 000 clocks) was ignored for the sequence: the next block continued
    it (8 of 80 runs, e.g. chain-7 seed 15: START at input 3 618 132 while
    slot block 30 of frame 2 left; the next block had sequence 31). The
    packer now latches the START until the block is done ([invariant
    monitors](#invariant-monitors));
  - **defect (fixed): the sync's period statistics after a restart whose
    next slot head falls about 0 - 30 symbols before the end of the first
    sync period** (the restart 5 - 30 symbols after a head): every later
    status block's peak and sum (words 0-5) differed from the model's, the
    decoded blocks did not (chain-7 with one START at input 502 989 or
    503 014: frames 0-2 differed; at 502 974 or a few thousand symbols
    after a head they were equal; a loss of 2 999 / 3 000 at input
    500 000 resumes 15 - 16 after a head: the same; chain-9 seed 20 met it
    at random). The cause was stage 3, not stage 1: the sync locks with
    its heads where the period's peak is, here 4 - 29 positions before
    the period's end, so each head's decision is followed a few symbols
    later by a period end's; `SlotPhase` took the status snapshot when it
    queued the head's slot, after its gain division and step products
    (about 80 cycles), by which time the period end had replaced it with
    the next period's statistics. The model's slot carries the statistics
    after the push that gives its head. `SlotPhase` now takes the first
    snapshot after the head's record (`SyncFront.o_snap_valid`, the
    snapshot's strobe), and for slot 1's head writes it straight into the
    frame's snapshot (`o_snap`: the previous frame ends at the FIR after
    slot 0's first symbols, so after it), which drops the two 210-bit
    snapshot registers its slot queue carried:
    `PhaseTest.test_snapshot_of_the_head` (stage 3
    alone, a period end's snapshot 40 cycles after each head's: 2001
    before, 1001 now) and `ChaosTest.test_restart_just_after_a_slot_head`
    (`FPGA_CHAIN_REAL=1`, frames 0-2 equal);
  - **a divergence, then counted apart (fixed: the HDL counts as the
    model)**: the chain's status counters (slots, iterations, LDPC and BCH
    failures) included the slots it decoded of a frame a restart dropped
    later (the orphans), the model's count whole frames; in 158 status
    blocks of 75 runs, always exactly the orphans before them. The model's
    documentation is the arbiter (`chain::frame::Counters`, counted when a
    frame completes; the status block is its format) and matching it costs
    little: `Finish` counts a frame's slots aside (7-bit counts and 15-bit
    iterations for the frame in hand and the one before it, whose TMCC
    result follows the next frame's first slots) and adds them at the
    frame's TMCC result, replacing the 160 bits of counters it held for
    that late TMCC; a frame that never completes is never added
    (`OrphanCountersTest`). The harness's "known divergence" branch is
    gone: a status block must be equal;
  - **by design, never seen differing**: frames handed out by the model
    within 8 slots before an undrained restart (the HDL's pipeline still in
    them) and the restart epoch were compared loosely, and every one was
    equal anyway: 170 frames equal block for block, 158 equal but for the
    counters above, 2 differing (the period statistics).

  After the fixes (2026-11-04, Verilator 5.054, `FPGA_CHAIN_CHAOS=8
  FPGA_CHAIN_REAL=1 pytest -n 8 tests/test_chaos.py`: 38 tests, 18 minutes;
  32 runs, 49 long stops, 34 short, 31 STARTs while running, 37 losses,
  17 of chain-8's own, 35 tap writes, 6.8 - 12.8 M symbols each, 148 -
  294 s per run): **0 problems**, all 130 frames the model sends equal
  block for block (status blocks included, counters and all), the
  sequence numbers from 0 after every START; every long stop's
  "stopped" sample (before START) and every run's end with **0 banks
  held**, a bank free and the queues empty, the chain stopped once each
  (`stops`); at most 2 banks held at an event. The two seeds that met
  the defects at random (chain-7 seed 15, chain-9 seed 20) pass too.
  Fit of the four fixes (Yosys, against `4eb0602`): `SlotsRx` without the
  decoder 26 487 → 26 461 LUT, 27 245 → 26 970 FF (the finish stage
  5 832 → 5 809 LUT, 2 960 → 2 873 FF; stage 3's slot queue no longer
  carries the 210-bit snapshot); block RAM and DSP unchanged.

**What is left** (the receive-only image meets timing and, with fit
`682032e`, decodes BS7 on the board as the model does: 1 LDPC failure in
30 s, 2.15 mean iterations, n0 learned; VERIFICATION.md "Build 8 on the
board"): the slow loop's gate checks the centre tap and the residual
only, and let through one set with odd outer taps (a whole-vector check
would hold it);
the restart epoch for a layout committed after a restart (the model's
insert happens before it); an image with the sums in the `Slots` stream
(built, and the host's stream path checked on the board: "Sums in the
stream" above; the HDL and the host are done and tested in simulation);
`SlotsRx` as a whole with its front end in
simulation (the chain's simulations start at the symbols); a host CONST /
N0 load and layout lag 1 (FEATURES says so); the recording sets
(`chain-rec-*`, opt-in) were not generated here.

### A timed host–FPGA model

Issue #30 item 4: `tools/hostfpga_model.py` (Python standard library only,
seeded) and `tests/test_hostfpga_model.py` (in `mise run fpga:test`; no
simulator or vectors, about 7 s on one core). Two of the board's defects
were properties of the host and the FPGA over time, not of either alone:
build 6 latched the training sums every 33 ms frame while the host took
about 15 frames to read them (an interface timing assumption nobody had
stated), and #27 / #29 were races on `direct_reg_access`'s shared address
between the host's two libiio contexts and a board-side script. A
discrete-event model finds both from the register map, the frame period
and the measured iiod cost, without hardware.

**What it simulates.** The chain's frame clock (33.05 ms, frame 0 a fixed
time after START, frames 0 and 1 without a layout) and the register file of
`top.py`'s docstring (`REGISTERS`, each register's kind: constant, config,
live, per frame, latched, version): the command protocol on `gpio_out` /
`gpio_in`, the sums latched at each decoded frame's end, SUMS_CAPTURE's
arm / latch / hold / release and SEQ, TAPS_FROM in force from frame N + 2
(or the next frame start), START, the old HDL's sum READ (0 for up to
4 400 symbol clocks) and build 9's (3 clocks). The `Slots` stream: a decoded
frame's 210 000 words over the frame, its TMCC, status and (format 2) sums
blocks 4.5 slots into the next, cut into DMA buffers, each arriving at the
consumer after its transfer. `direct_reg_access`: one cached address per
IIO device, set by every write to the attribute (an address alone, or an
address and a value); a read returns the register at the cached address, so
another client's address write between a read's two iiod requests returns
the other register. The clients: the host's open (the capture context), its
slow loop (its own context; the register path as `SlowLoop::tick_at` does
it, with build 6-8's single reads, #27's diagnostic double read or PR #31's
verified reads; or the stream path, `tick_stream`, falling back to the
register path when the sums blocks stop, `stream_stalled`), the capture
context's
per-buffer ADC overflow poll of builds 6-8, the consumer (`ChainHealth`:
status and sums blocks, the revert watch), Tezuka's `api_controller.sh`
(0x80000088 every 3.4-4.2 s) and #29 E2's 5 Hz reader of 0x80000000. Every
value read carries the register it came from, so foreign values are
counted exactly.

**Parameters** (dataclasses at the top of the file, each with its source;
`--set SECTION.FIELD=VALUE` changes one from the command line, `--sweep`
runs several values):

| Parameter | Value | Source |
|---|---|---|
| `Profile.request_s`, this bench (`BENCH`) | 0.38 ms per iiod request | build 7: 308 single reads (4 requests a word) in about 15 frames, the first write 0.90 s after the open |
| `Profile.wire_s`, `BENCH` | 0.10 ms | fitted to the false ADC overflow flags of builds 6 and 7 (27 and 8 in 30 s; the model 30 and 7) |
| `Profile.request_s`, the reporter's board (`ISSUE29`) | 0.75 ms | #29: a set with the diagnostic double read (6 requests a word) in 1.3-1.5 s |
| `Profile.wire_s`, `ISSUE29` | 0.36 ms | fitted to #29 E2 (34 of 50 sets, 52 words) |
| `Signal.frame_s`, `lock_s`, `layout_lag` | 33.05 ms, 55 ms, 2 | the frame; START to frame 0, fitted to build 7 (frame 0 began 41-74 ms after START) |
| `Signal.decoded_words`, block sizes, `block_delay_slots` | 210 000; 604 / 143 / 476; 4.5 | README "Without a layout", "Sums in the stream", the block order |
| `Host.buffer_words` | 65 536 (262 144 in builds 6-8) | `SLOTS_BUFFER_WORDS`, the default buffer before #28 |
| `Host.poll_s`, `every_s`, `recover_after_s`, `stream_stale_s`, `max_reads`, revert | 0.2 s, 2 s, 1.5 s, 0.5 s, 8, 10 frames / 5 s | `crates/isdb-radio/src/chain.rs` |
| `Host.sums_stall_frames`, `stream_back_blocks` | 90 decoded frames, 3 blocks | `SlowLoopOptions::sums_stall`, `STREAM_BACK_BLOCKS` |
| `Host.taps_words` | 25 command words | the 11 taps and TAPS_FROM |
| `Board.api_period_s` | 3.4-4.2 s | #29 |

`wire_s` is the window from the board's apply of a read's address write to
its sample of the value (a write applies at the end of iiod's processing, a
read samples at its start): the network round trip and the client's
turnaround, the only part of a request another client can hit. Each
profile has this one fitted number; every other comparison below is a
prediction.

**Running.** `python3 -I fpga/tools/hostfpga_model.py` runs every scenario
(`--list` names them; a few seconds), `--seeds 5 --mean` averages seeds,
`--json` for the raw counts, for example `python3 -I
fpga/tools/hostfpga_model.py stream --set host.buffer_words=262144`.

**The reproductions** (the model's mean over 5 seeds; the board's figures
from VERIFICATION.md, #27 and #29):

| | Board | Model |
|---|---|---|
| (a) build 6 (no SUMS_CAPTURE), 30 s | 46 sets read, every one torn, no taps; 27 overflow flags | 44 sets, 44 torn (the words of each from several latches), no write; 30 flags (fit) |
| (a) build 7 (SUMS_CAPTURE), 30 s | first write 0.90 s after the open, solved from frame 5, in force from frame 26; 12 solves in 24 ticks; 8 flags; one bad solve of twelve, cause not found | 0.91 s, frame 4, frame 26; 11 in 22; 7 flags (fit); 3.2 of 11 solves from a set with a word moved by 10⁹ or more (the host's own overflow poll: the model's explanation of that bad solve) |
| (a) build 9 (verified reads, the script running), 600 s | 200 written in 400 ticks; 1 reread in 65 201 reads; the first set in force at frame 33-34 | 203 in 406; 2.4 rereads in 66 193, none accepted; frame 34 (a set takes 0.71 s instead of 0.47: 6 requests a word) |
| (b) #27 run B: the host's per-buffer poll, the script | 47 of 50 sets with a differing word, 186 words (3.7 a set) | 81 of 82, 4.3 words a set |
| (b) #27 run C / #29 E3: the script alone, 600 s | 7 of 146 / 7 of 158 sets | 5.6 of 162 |
| (b) #29 E1: neither | 0 of 163 | 0 of 162 |
| (b) #29 E2: a 5 Hz reader of 0x80000000 | 34 of 50 sets, 52 words | 31.8 of 49, 49 words (fit) |
| (b) PR #31's verified reads under run B's / run C's load | not run | 355 / 7 rereads, no wrong word accepted, no wrong set solved, nothing unsettled |
| (c) the stream path (format 2), the script running | (no image yet) the plan expects `taps_from` 2 in force from frame 4 or 5 | the first sums block (frame 2's) at the consumer 10 ms after frame 2's end, TAPS_FROM 2 written 0.171 s after the open, in force from frame 4 (from 4 or 5 on the slower `ISSUE29` network); no sum, SUMS_FRAME or status register read: the loop's only reads are `gpio_out`'s at attach (2) |

Not reproduced: #27 run A (build 8's released host: single reads and the
poll, 600 s) had 231 overflow flags and 13 bad sets written and 30 rejected
of 190; the model gives 535 flags and 151 of 186 solves from a set with a
large wrong word. Its per-set exposure under the same poll is the one run B
measured (where the model agrees), so either run A polled less often than
the model's 24 buffers per second or the board's outcome criteria (more than
half of the codewords failed; the centre-tap gate) count only part of the
wrong sets; the model computes no taps to tell. Run B's loop also read 50
sets in 300 s where the model reads 82 (a set every 6 s on the board).

**Properties** (each a test): every set the host solves from is one
snapshot (no scenario solves a set whose words came from two latches);
the static rule on the map (`check_groups`): a multi-word value the host
reads is latched and versioned, and held for at least the host's read time
(build 6 fails it: latched every 33 ms, read in 0.47 s); every live register
the register path polls (CARRIER, STATUS, FRAME, TAPS_FROM) is one word used
alone; no host sequence waits for ever (`liveness`: the first taps within
the lock, two frames, two polls and a set; a solve at least every period
plus one retried set; on the stream path both bounds also allow a stall
of the sums blocks, 90 decoded frames, a period and a register set; a
stretch without a layout longer than `recover_after` ending in a
recovery); the sums blocks the stream path waits for all arrive, each
within a buffer's fill at the decoded rate (10.3 ms) plus the block delay
and the transfer, and no stream scenario without a foreign hold falls
back to the registers. The verified reads accept
no wrong word under any modelled load; the old HDL's transient 0 is never
seen over iiod (0.4 ms and more between the READ and the read) but is by a
host on the board (`LOCAL` profile), even through the verified reads, and
not with build 9's READ.

**What it found** (`KNOWN_GAPS` lists the liveness gaps; a fix makes the
test fail until the entry goes):

- *The stream path had no timer on the sums blocks* (fixed in the host).
  If another client arms SUMS_CAPTURE (a second host on the board, a
  register-path tool), the latch holds, no sums block leaves, the status
  blocks still say decoded, and the loop waited for ever without a warning.
  The host now counts the decoded frames since the last sums block; after 90
  it warns and reads the sums from the registers until blocks come again at
  the free latch's rate (`sums_stalls`, `stream_returns`, `fallback_sets`;
  `stream-foreign-capture`: the stall seen at the first tick after 90
  frames, 2 register sets, back to the stream after the restart at 20 s).
  A residual: a restart that lands between a fallback tick's read of
  SUMS_CAPTURE and its capture write (about 16 ms of reads a tick) has the
  free latch held again, and the host stays on the registers, still
  solving, until the next restart (1 of 40 restart times in the test's
  sweep). The same fallback turns `stream-held-prefix` (the HDL before
  dedbc14) from no taps into taps from the registers after 3.3 s.
- *A released capture was never armed again* (fixed in the host). With
  PR #40's HDL a restart (START, a lost sync, the chain's stop) releases
  the capture the register path armed; the loop waited for VALID for ever
  (`registers-restart`, a restart at 9.4 s: one "no capture held"
  recovery, then no solve for the 20 s left). A tick that reads
  SUMS_CAPTURE neither VALID nor ARMED while it waits arms another at the
  next poll (`rearms`).
- *A layout lost during a set discarded the held set as torn* (fixed in the
  host). The register path compared SUMS_CAPTURE's whole word before and
  after the 308 words (`HELD_EXTRA`), but its NO_LAYOUT bit follows the
  layout while a capture is held: a good set was dropped and the warning
  said the capture did not hold ("moved from frame N to N"). It compares
  SEQ, VALID and ARMED only (`no_layout_moved` counts the sets kept).
- *The verified reads cost 8 frames on the register path*: the first solved
  taps in force from frame 34 instead of 26 (VERIFICATION.md "Build 9
  residuals" measured +33-34).
- *On the stream path a lost layout is seen late*: the status blocks of a
  stretch without a layout arrive in bursts (a 65 536-word buffer takes up to
  3.2 s at the no-layout rate), the stale rule reads them as no layout, and
  the recovery comes within the loop's period, `recover_after` and the
  stale time (3.9 s after the layout went in the model's run; 1.8 s on the
  register path, which reads STATUS at its next tick).
- COUNTERS' iterations low / high are a 64-bit live value read as two words
  without a snapshot (`GROUPS_ALLOWED`: torn only at a carry of the low word,
  about every 8 days of decoding; the counters are a report).

**What it does not model**: the signal and the numerical solve (a wrong word
is counted, and classed by how far it moves its entry; the host's sum checks
and gate, which catch most such sets, act on values the model does not
compute), n0 and the revert's quality rules (a written set is accepted once
ten status blocks show it), the decoder, iiod's contention between clients,
Ethernet losses and TCP's buffering beyond a fixed transfer time, and mid-run
restarts other than stretches without a layout.

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
  `ldpc-5-109-layered34` (the extreme rates), `ldpc-14-49-layered34` …
  `ldpc-21-105-layered34` (the other eight rates, 49, 61 unshortened,
  73, 81, 89, 97, 101 and 105/120: four codewords each, two easy, one
  near the threshold, one failing at the limit 12; with sets 4–6 every
  B44 code at the HDL's step), `ldpc-6-tmcc-layered34`
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
  The frame-average budget is 6.61 M cycles / (121 × 6 125) ≈ 8.9
  passes per codeword (with the 200 MHz pipeline of 2026-11-03; 10.1
  before it) against a measured mean of 3.1 (2.1 iterations
  + 1): a codeword may run to the limit as long as the frame's total
  stays inside the budget. Not a symbol-level queue before the demapper
  (it would need a demapper faster than one symbol per clock), not
  limit 8 (1 338 failures of 18 336 near threshold against 3 at 16).
  **The TMCC codeword** is staged by the TMCC stage across the frame and
  loaded as a burst after the frame's end, on context 1 (`in_ctx`), its
  writes sharing the single input port with the slot LLRs cycle by cycle
  (the port carries one write per cycle; the slot LLRs use ≈ 0.81 of the
  cycles at 200 MHz, so the 31 680-write burst takes ≈ 167k cycles, about
  three slots); the default K = 3 assumes that. The bridge gives the
  decoder a beat of n values in n cycles, so 32APSK's LLRs with the
  burst's need 192.6 M of the 200 M cycles/s (`ldpcqueue.port_cycles`;
  before 2026-11-04, n + 1 cycles a beat: 232.6 M, chain-14's defect,
  "Integration"). Interleaved TMCC LLRs
  would hold a bank for the whole frame and need K = 4.
  `isdb_fpga/ldpcqueue.py` models the queue over frames (tests
  `LdpcQueueModelTest`, S = 34, 200 MHz, 93/120, limit 16; the cycles per
  pass of the 200 MHz pipeline, 6 125 at 93/120 and 5 180 for the TMCC):

  | frames (×3) | K = 2 | K = 3 | K = 4 |
  |---|---|---|---|
  | measured distribution (mean 2.1, three isolated codewords at 16), TMCC staged | 13 overruns of 363 | **0** | 0 |
  | the same with two adjacent pairs at 16, TMCC staged | 16 | 6 | 0 |
  | the measured distribution, TMCC interleaved | 68 | 7 | 0 |
  | all 120 slots at the limit (×2), TMCC staged | 115 of 242 | 113 | 112 |

  (The same as with the cycles before the pipelining, 5 420 per pass,
  but for the all-at-limit row, 104 / 96 / 95.)

  (K = 2 overruns even without long codewords: 4 in 3 frames, the TMCC
  burst holding a bank across three slots.) The HDL tests check the
  overrun record in order (K = 2, a codeword begun while both banks are
  held), two contexts open at once with the results in commit order, and
  the limit as a run-time input (toy codewords at 16 against the model).
- **Abandoned codewords** (issue #28, 2026-11-04). A codeword the chain
  gives up before its last LLR (a frame dropped, a restart: a lost sync or
  the host's START, the input stopped mid-slot) used to keep its bank
  until the bitstream was reloaded: on the board 0, then 2, then 50 of 96
  slots dropped per frame over consecutive stop/START runs, then every
  codeword, the TMCC's too (no layout, no slot blocks). Two rules now: a
  `begin` on a context that still holds an uncommitted bank takes that
  bank again (`reused`; the load pipeline is in order, so the new
  codeword's writes follow the old one's), and `abort` (`in_ctx`) gives up
  the context's open codeword and frees its bank at once (`aborted`; a
  commit of the closed context without a `begin` would be a dropped
  codeword's overrun record). Committed codewords are never touched: they
  decode and leave in order, so the result stream, the bridge's tag queue
  and the finish stage stay in step. `held` counts the banks allocated.
  The chain sends the abort as an OP_ABORT beat on context 0 after each
  frame drop and restart (`back.py`, once the demapper's pipeline has
  emptied), in order with the other beats through the bridge. Tests:
  `test_abort_frees_an_abandoned_bank` (five half-loaded codewords each
  aborted: no bank held after each, then K codewords back to back all
  decode), `test_a_begin_takes_back_its_contexts_abandoned_bank` (five
  given up without an abort, a context-1 codeword begun twice: bit-exact,
  nothing dropped, every bank free after), and the chain-level
  `RealDecoderChainTest.test_stop_and_start_free_the_decoder`.
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
  the read and write phases (LUTRAM, `ram_style`); the ROMs (entries,
  chains, gaps). Every lane bank port is a register per bank and lane
  (read address; write address, data, enable), and every block RAM's
  output goes into a register first (its DO_REG).
- **Clock: 200 MHz, timed by Vivado** (2026-11-03). The first full build
  (85 % LUT, 119 BRAM36) failed setup in the decoder: WNS −3.016 ns on
  5 063 endpoints (closing at about 125–130 MHz), and −1.005 ns on 228 in
  the bridge (`chain/ldpc_if.py`). Pipelining only (every result bit-identical
  on every vector set; the cycle counts change), checked with
  `synth/ldpc_vivado.py` (out of context: synth_design, place, route of
  the decoder alone, 2–3 min; `--bridge` for the bridge with its two
  clocks) and summarised from a build's `failing.tcl` log with
  `synth/failing_summary.py`:

  | Path class (the build's worst) | before | fix |
  |---|---|---|
  | the sequencer's issue → its counters' enables (gap ROM, per-rate muxes) | −3.0 ns | the rate's constants registered at a SETUP cycle; the wait a countdown loaded a step ahead; issue from three registers |
  | fill bounds → the lanes' write enables | −2.9 | the bounds registered a cycle ahead |
  | bank read → K:1 mux → rotation → L | −2.3 | the BRAMs' output registers; the mux and the rotation's two halves each a stage |
  | the bank ports' muxes (engine / output / loader) → BRAM pins | −1.9 | per-bank, per-lane port registers |
  | min − offset → the record memory | −1.5 | subtract, then clamp in the next stage |
  | t = sat(L − r) | −1.5 | the add and the saturation in two stages |
  | the write side's rotation, its amount computed in place | −1.45 | the amount registered, the rotation in two stages |
  | the record's hold, the min-sum's resets (k == 0 fan-out) | −1.4 | k == 0 carried as a flag |
  | the output stream: BRAM → shift → buffer | −2.0 | a prefetch queue of 8 words and a head word; the packer's comparisons registered |
  | the bridge's input FIFO (LUTRAM, 512 × 216 bits) | −1.0 | the AsyncFIFOs in block RAM (`ram_style`), the output FIFO's writes and `o_ready` registered |

  Out of context, S = 34, K = 3, all eleven rates: **WNS +0.391 ns at
  5.0 ns** (from −2.738 ns for the source before, the same flow), 11 790
  LUT, 15 232 FF, 79.5 BRAM36, 0 DSP (the first build's decoder:
  12 632 LUT, 8 789 FF, 79.5 BRAM36); the bridge +0.231 ns (its last
  three paths inside Amaranth's AsyncFIFO: the gray pointer → the next
  read address → the BRAM), 257 LUT, 4.5 BRAM36 (658 LUT, 0 BRAM36
  before). The cost: WRITE_LATENCY 8 → 16 and the pass turnaround
  (DECIDE) 12 → 19 cycles, both measured by `synth/ldpc_latency.py`, so
  3–13 % more cycles per pass (below).

**Cycles per iteration** (`synth/ldpc_cycles.py`: the static schedule's
steady state, the hazard gaps included; the read and write phases of
consecutive steps overlap, a step taking d cycles, d the layer degree)
and the iterations per codeword if all 121 codewords of a frame (120
slots + the TMCC) ran equally long, one pass more than the iterations
(the syndrome of the last one): the frame-average budget, not a limit
(the queue above lets single codewords run longer):

| S | rate | d | cycles per pass | iterations per codeword, 100 MHz | 200 MHz |
|---|---|---|---|---|---|
| 17 | 41/120 | 5 | 8 709 | 2.1 | 5.3 |
| 17 | 93/120 | 16 | 10 914 | 1.5 | 4.0 |
| 17 | 109/120 | 32 | 9 125 | 2.0 | 5.0 |
| 22 | 41/120 | 5 | 6 745 | 3.0 | 7.1 |
| 22 | 93/120 | 16 | 8 697 | 2.1 | 5.3 |
| 34 | 41/120 | 5 | 4 508 | 5.1 | 11.1 |
| 34 | 61/120 (TMCC) | 7 | 5 180 | 4.3 | 9.5 |
| 34 | 93/120 | 16 | 6 125 | 3.5 | **7.9** |
| 34 | 109/120 | 32 | 4 738 | 4.8 | 10.5 |
| 187 | 41/120 | 5 | 1 954 | 13.0 | 27.0 |
| 187 | 93/120 | 16 | 1 467 | 17.6 | 36.2 |

(With the 200 MHz pipeline, 2026-11-03: WRITE_LATENCY 16, DECIDE 19.
Before it, at 8 and 12: 4 357, 4 842, 5 420 and 4 315 cycles at S = 34
for 41, 61, 93 and 109/120. The plan's estimate, P/S steps × (d + 6),
counted the pipeline per step; here only the hazard gaps add to d per
step: 89 of 297 steps at 93/120, S = 34, wait, 1 373 cycles in all.)

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
rotators), so it is out. (These Yosys figures are of the source before the
200 MHz pipeline; the current ones are Vivado's, under "Clock" above.)

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

**Simulation backends (2026-11-02).** `simldpc` runs the decoder on
Amaranth's Python simulator or compiled with Yosys's CXXRTL backend
through the chain's `chain.simx` (write_cxxrtl, `c++ -O1`, a library per
design cached in `fpga/build/cxxrtl` by its RTLIL, loaded through
ctypes); the compiled one is the default where a C++ compiler is found,
`FPGA_SIM=pysim` forces the Python one. The B44 sets run on one
all-rates decoder per step (one library); a process keeps one engine per
design and reuses it across files (the decoder is idle and history-free
after a run). For the compiled build `LdpcDecoder(..., sim=True)` makes
the S-wide concatenations a bounded-depth tree (the C++ compiler refuses
a 187-deep one), the memories' depths powers of two and idle addresses 0
(CXXRTL checks every port access); the synthesis build is without them
(S = 34, K = 3: 14 043 LUT, 76.5 BRAM36, the same as before but 9 LUT
for the record and sign memories' read enables). Measured on this host
(Ryzen 9 7950X), one core:

| Design | compile (write_cxxrtl + c++ -O1) | CXXRTL | Python simulator |
|---|---|---|---|
| S = 17, all rates | 25 s | 207 000 cycles/s | 1 170 cycles/s |
| S = 34, all rates | 59 s | 63 000 cycles/s | 630 cycles/s |
| S = 187, all rates | 977 s | 3 100 cycles/s | — |
| toy codes (n = 2 244), each | 15–65 s (S = 187: ≈ 19 min) | — | — |

So the default test now covers every codeword of every layered B44 set
at steps 17 and 34 (the Rust model's 11 sets and the Python model's 2:
65 codewords, traces included) and every toy set but S = 187; S = 187
(its B44 set and its toy set: the compile alone exceeds the CI budget)
is opt-in with `FPGA_LDPC_ALL=1` (bit-exact on both, 2026-11-02).
`LdpcPysimTest` keeps the Python simulator's path covered. The LDPC
tests under `pytest -n auto --dist worksteal` on this host: 68 s with
the libraries cached, about 150 s with them built (longest:
`test_b44_step34`, 67 s / 148 s), against 193 s (`test_b44_vectors`,
3 codewords) and 143 s (`test_toy_codes`) for the previous Python-only
set.

**Verilator (2026-11-02).** Where Verilator is installed it is the
compiled default ([Simulation backends](#simulation-backends)): the S = 34
decoder builds in 22 s (one make job; 11 s with four, CXXRTL 58 s) and
runs at about 500 000 harness cycles per second (CXXRTL 31 000; the
table above counts the codewords' decoder cycles over the wall time, and
is from before the harness's second step at the edge); S = 187 builds in
about a minute and runs at 158 000 (CXXRTL: 21 minutes, 1 900), so
`test_b44_step187` and `test_toy_step187` run by default on Verilator
(about 80 s each, the build included); on CXXRTL they stay opt-in
(`FPGA_LDPC_ALL=1`). Both backends give the same records on every set,
S = 187 included (`synth/sim_compare.py ldpc-17 ldpc-34 ldpc-py toy
plans ldpc-187 toy-187`).

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
fpga/tezuka/build.sh WORK isdbs3-rx # receive only: SlotsRx (front end, ISDB-S3 chain, LDPC decoder)
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

The `isdbs3-rx` mode (`apply.sh --isdbs3-rx`, which runs
`apply_s3rx.sh`) is the receive-only image of
[the ISDB-S3 receive chain](#the-isdb-s3-chain-slotsrx): `SlotsRx` on the
RX path (`isdb_slots.tcl`: the same ADC-path connections as
`isdb_symbols.tcl`, `fast_clk` = `axi_ad9361/l_clk`, `ldpc_ref` =
`clk_fpga_0`, the GPIO pair; `isdb_slots_top.v` around the generated
`isdb_slots_rx.v` registers the fast domain's reset twice on l_clk;
`isdb_slots.xdc` implementation-only, late) and nothing that receiving
does not use:

- no maia-sdr core (spectrometer, DDC, recorder: 3 412 LUT, 29 BRAM36,
  18 DSP in the deployed image) and its clock wizard: `isdb_rx_base.tcl`
  replaces `maia.tcl`, `maiafirtoiq.tcl` and `sweeper.tcl` with what the
  rest needs (the two DMAs on HP2, the `mux_decim_i/q` sample muxes on
  path 0; HP1 off). Tezuka's web spectrum (`maia-httpd`) has no core to
  open: its first register read takes a bus error, as Tezuka's own
  scripts do on the blocks a board lacks; the stock `system_top.bin`
  stays on the card for it;
- no `SymbolsTx`, no DVB-S2 transmitter, no ×8 TX FIR, and the AD9361
  core's TX datapath off (`DAC_DATAPATH_DISABLE 1`, `dac_dds` dropped: no
  DDS, no DMA data into the DAC, no TX IQ correction: `axi_ad9361` 7 603 →
  2 647 LUT, 28 → 12 DSP). The core's TX PN generator stays (the parameter keeps it),
  which the ad9361 driver's TX interface tuning uses; the DAC DMA, the
  unpacker and the DAC FIFO stay (the kernel's device tree probes the
  DMA; 546 LUT, 2 BRAM36). Nothing can be transmitted with this
  image;
- Tezuka's cs12 packers stay (`isdb capture` with 8 or 12 bits).

`ram_style.py` marks the block's memories of depth ≤ 64 as LUT RAM in the
copy of `isdb_slots_rx.v`: left to itself, Vivado put 35 of them in block
RAM (the decoder's 64 × 714 input FIFO in ten RAMB36, the bridge FIFOs,
31 of the front end's 64 × 18 coefficient banks), 146.5 BRAM36 of 140 in
all. `vivado-batch.sh` runs this directory's Tcl on a checkpoint:
`timing.tcl` (WNS / WHS, per clock), `utilization.tcl`, `ram_report.tcl`
(`report_ram_utilization -detail`), `failing.tcl` (failing endpoints by
clock pair and block), `paths.tcl` (the worst paths per clock and the
exceptions), `skew_check.tcl` (the symbol ↔ fast crossings) and
`reimpl.tcl` (place and route an optimised checkpoint again with other
directives);
`build.sh` keeps the final checkpoint in `WORK/out/MODE/`.

**Status of `isdbs3-rx` (2026-11-03): with the fit's timing remedies
(`7f70e26`) every constraint is met and the image runs on the bench
board** (`isdbs3_rx_top.bin`; VERIFICATION.md "ISDB-S3 receive chain in
the FPGA: the image"; rollback: `tools/pluto-image.sh rollback
uEnv.txt.prev-txrx2`). Build 6's final checkpoint met setup everywhere
and left one hold violation outside the block, Tezuka's RGMII receive
IDDR (−0.433 ns, the Vivado 2025+ router): `holdfix.tcl`
(`vivado-batch.sh holdfix.tcl FINAL.dcp OUTDIR`: phys_opt
`ExploreWithHoldFix` and the failing nets rerouted, until met) repairs
it on the checkpoint and writes the bitstream and an XSA, which
`sdbin.sh` converts. The decoder crossing's 5 ns rule names the two
clocks (with the bridge FIFOs in block RAM, a cell rule caught their
same-clock paths). Build 6 after `holdfix.tcl`: **43 817 LUT (82.4 %),
49 845 FF, 123.5 BRAM36 (88 %), 176 DSP (80 %); WNS +0.045 ns, WHS
+0.011 ns, 0 failing endpoints**; per clock (WNS / WHS, ns): `rx_clk`
+0.045 / +0.011, decoder `c0` +0.194 / +0.042, symbol clock +1.625 /
+0.044, symbol → l_clk +0.255, l_clk → symbol +0.593, symbol ↔ decoder
+3.237 / +3.108, `clk_fpga_0` +1.143, `RGMII_rxc` +0.100 / +0.192.
`isdbs3_rx_top.bin`: 2 746 304 bytes, sha256
`41997abe5f455fb8117ccda14d2c414ead397d39d9aac2508b9ced0bf54995ac` (kept on
the card as `isdbs3_rx6_top.bin`). Build 7 (fit `d4042bb`, SUMS_CAPTURE)
met every constraint in the project flow without `holdfix.tcl`: WNS
+0.067 ns, WHS +0.029 ns; 43 731 LUT, 123.5 BRAM36, 176 DSP;
`isdbs3_rx_top.bin` 2 833 440 bytes, sha256
`717f7e34adaf5cadd906aa70bd2a8b482a3b5ceb6c328f2f52299ceed28e6a2a`, the
image on the card (`uEnv.txt.prev-rx6` selects build 6).

Before the remedies (fit `5c8b9eb`; final checkpoints, `timing.tcl`,
`utilization.tcl`, `failing.tcl`; the ADI flow stops at "Timing
Constraints NOT met" and writes no XSA):

| xc7z020clg484-2 | LUT | FF | BRAM36 | DSP | WNS | WHS | failing endpoints |
|---|---|---|---|---|---|---|---|
| build 2 (LUT RAM memories) | 45 506 (85.5 %) | 43 109 | 119 (85 %) | 176 (80 %) | −5.934 ns | +0.050 ns | 10 600 |
| of which `isdb_slots` | 39 026 (3 666 LUT RAM) | 32 531 | 112.5 | 164 | | | |
| — front end (`core`) / chain / decoder / bridge / control | 6 102 / 18 346 / 12 819 / 661 / 1 027 | | 1 / 31.5 / 79.5 / 0 / 0 | 112 / 52 / 0 / 0 / 0 | | | |
| build 4 (+ 25 ns chain rule, reset false paths, BUFR + BUFG fast clock) | 45 275 (85.1 %) | 43 030 | 119 (85 %) | 176 (80 %) | **−3.257 ns** | +0.016 ns | 7 429 |

Per clock, worst setup in ns (build 2 → build 4):

| Clock (paths) | Build 2 | Build 4 | Failing endpoints (build 4) |
|---|---|---|---|
| `rx_clk` = l_clk inside the block (6.25 ns rule): the finish stage's BCH unit | −3.834 | **−3.257** | 812 |
| `rx_clk` inside the block: chain `back` (equaliser, demapper) | −1.623 | −1.379 | 115 |
| symbol clock → l_clk crossings (6.25 ns): `back` / `finish` | −5.934 / −4.234 | **−2.863** / −1.563 | 711 / 466 |
| `c0`, the decoder's 200 MHz (5 ns): `ldpc` / bridge | −2.708 / −1.355 | **−3.016** / −1.005 | 5 063 / 228 |
| `rx_clk`, ADI's own paths (4 ns) | −2.654 | −0.525 | ≈ 25 |
| `clk_div_sel_0_s`, the symbol clock (16 ns; the chain's 25 ns) | +0.194 | +0.663 | 0 |
| l_clk → symbol clock crossings | +0.041 | +0.305 | 0 |
| `clk_fpga_0` / `RGMII_rxc` | +0.571 / +0.100 | +1.001 / +0.100 | 0 |
| hold, every clock | +0.050 | +0.016 | 0 |

The failures are the block's logic depth at the fit's clocks, not the
image's resources: the BCH unit (`FastBch`, a Berlekamp–Massey step of
syndrome read, GF(2¹⁶) product and locator write in one 160 MHz cycle:
9–10 ns), the decoder at 200 MHz (the TMCC fill's bounds into 34 block
RAM enables and the layer-table loads: 7.2–8.0 ns; it closes at about
125–130 MHz), the equaliser's symbol-clock → DSP inputs; and, until the BUFR +
BUFG fast clock, a 4.2 ns skew between `util_ad9361_divclk` and l_clk
that the fit's phase-aligned 4× scheme assumed away. ADI's own `rx_clk`
paths (4 ns, its 61.44 Msps case) lost their margin under the placement
pressure (−2.65 ns in build 2, −0.53 ns in build 4). Directives (`reimpl.tcl`:
`ExtraTimingOpt`, `AggressiveExplore`) changed nothing material. What the
HDL needed is in docs/plans/isdbs3-fpga-rx.md, Results, "Piece 6"; the
fit's `7f70e26` did it (this piece did not change the block's HDL).

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
