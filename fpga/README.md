# fpga — FPGA implementations of receive stages

Amaranth HDL for the stages of the receive chain that a radio's FPGA
can run instead of the host CPU (plan: the run-time choice between CPU
and FPGA implementations of a stage, like a SIMD kernel). Each stage has
a Rust reference model in the workspace that it must match **bit for
bit**. The model writes test vectors, and the HDL simulation here
reproduces them.

The first stage is the single-carrier front end of
[`isdb-frontend`](../crates/isdb-frontend/README.md) (IQ → `Symbols`,
format version 2) for ISDB-S and ISDB-S3 on the PlutoSky R2
(XC7Z020). It turns 8-bit IQ at 40 Msps (80 MB/s, more than the board's
Gigabit Ethernet path sustains) into 6-bit symbols at 43 MB/s (ISDB-S)
and 51 MB/s (ISDB-S3). Format 2 adds the output bank `k` that carries
the host's timing trim ([`isdb-frontend` README, "The timing
trim"](../crates/isdb-frontend/README.md#the-timing-trim)): the output
and the AGC come from `k`, the timing detector from `h` and `g`.

## Layout

| Path | Content |
|---|---|
| `isdb_fpga/frontend.py` | `SymbolFrontend`: mixer, polyphase matched, derivative and output filters (one 54-bit `h | g | k` word per lane and cycle, T/4 lanes, 4 cycles per input sample), timing loop, AGC, quantiser, packer. The cycle-by-cycle schedule is in its docstring |
| `isdb_fpga/control.py` | `Control`: the command decoder and register file on `up_adc_gpio_out`, the ID word and readback on `up_adc_gpio_in` (`isdb_frontend::control`) |
| `isdb_fpga/top.py` | `SymbolsRx`: the block for the bitstream (front end + control + output selection), and where it connects in Tezuka's block design |
| `isdb_fpga/sim.py` | simulation against a vectors file; `python -m isdb_fpga.sim VECTORS.json [--jitter] [--load-port] [--via-gpio]` |
| `tests/test_isdb_fpga.py` | unit tests (CRC network) and the vector tests |
| `synth/estimate.py` | Yosys `synth_xilinx` resource estimate |
| `synth/verilog.py` | Verilog of `SymbolsRx` (through the venv's Yosys) |

## Running

The Python environment is a uv project pinned in `pyproject.toml` /
`uv.lock` (Amaranth 0.5.10, YoWASP Yosys 0.70, which runs as WebAssembly
in user space). Nothing here needs a vendor toolchain:

```sh
mise run fpga:test       # vectors from the Rust model, then the HDL simulation
mise run fpga:estimate   # resource estimate of the 32-tap block
mise run fpga:verilog    # fpga/build/isdb_symbols_rx.v for a vendor flow
```

Results (format 2, 2026-10-25):

- **Simulation:** bit-exact on every vector set, with every symbol's
  internals (`y`, `dy`, `yk`, `z`, `dz`, `zk`, `q`, the loop errors) and
  every output word compared:
  - synthetic ISDB-S at 16 taps and ISDB-S3 at 32 taps, and both with
    a trim (`k` ≠ `h`: −0.075 and −0.17 symbol), the default
    `mise run fpga:test` set;
  - recordings of BS7 (ISDB-S3, trim −0.168) and BS1 (ISDB-S, trim
    −0.05), 32 taps;
  - with irregular sample spacing and the coefficients (all three
    banks) loaded through the port, and with the whole configuration
    sent as the host's `gpio_out` command words, both on a trimmed set;
  - one LSB off in `k` fails the comparison (the output path is really
    compared).
- **Estimate** of `SymbolsRx` (Yosys, before place and route; Vivado's
  numbers will differ):

  | Taps | DSP48E1 | LUT | FF | Block RAM |
  |---|---|---|---|---|
  | 32 (format 2) | 64 | 3.9k | 4.6k | 1 RAMB18 + 8 RAMB36 |
  | 64 (format 2) | 112 | 4.2k | 5.5k | 1 RAMB18 + 16 RAMB36 |
  | 32 (format 1) | 46 | 3.6k | 4.5k | 9 RAMB18 |

  Tezuka's PlutoSky R2 build already uses 106 / 220 DSP, 23.0k / 53.2k
  LUT, 32.1k / 106.4k FF and 46 + 19 / 140 BRAM36 (Tezuka
  `doc/fpga_usage.md`). The 32-tap block (64 DSP) fits next to
  everything (170 / 220 DSP), more comfortably without the DVB-S2
  transmit chain (60 DSP) in an RX-only image; 64 taps (112 DSP) would
  not, and gain only 0.2 dB on ISDB-S3.

Timing closure is not checked (Yosys has no 7-series timing model). The
block runs at the ADC clock, 4 × the sample rate: 160 MHz at 40 Msps.

## Integration (the hardware step, not done)

`SymbolsRx` goes between Tezuka's sample path and the ADC packer
(maia-hdl `projects/common/cs12_cs8.tcl`, Tezuka's fork F5OEO/maia-sdr,
branch `refactor`):

| Port | Connects to |
|---|---|
| `adc_valid`, `adc_i`, `adc_q` | `mux_decim_i/valid_out`, `mux_decim_i/data_out`, `mux_decim_q/data_out` (12-bit samples) |
| `pass_valid`, `pass_data0`, `pass_data1` | `cs12_8mux_chan0/valid_out`, `cs12_sync_chan0/data_out0`, `data_out1` (today's path, passed through) |
| `pack_valid`, `pack_data0`, `pack_data1` | `util_ad9361_adc_pack` `fifo_wr_en`, `fifo_wr_data_0`, `fifo_wr_data_1` |
| `gpio_out` | `axi_ad9361/up_adc_gpio_out` (bit 0 stays Tezuka's 12-bit sync control) |
| `gpio_in` | `axi_ad9361/up_adc_gpio_in` (unconnected in Tezuka v0.3.24) |
| clock | `util_ad9361_adc_fifo/dout_clk` (the sample path's clock) |

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

A dedicated IIO device per output boundary, with its format version as
an attribute (the plan's longer-term form), would need a device-tree
node and a driver. The GPIO route needs neither.

**Building a bitstream** needs, beyond this repository:

1. **Vivado.** Tezuka's maia-sdr fork builds with Vivado 2023.1
   (`sourceme.first`; Tezuka issue #494 used 2023.2). ML Standard, the
   free edition, supports the XC7Z020. The download needs an AMD
   account. A Zynq-7000-only installation takes tens of GB; AMD's
   installer shows the exact size (not measured here, and this machine
   has 118 GB free).
2. **The Verilog of `SymbolsRx`** (`mise run fpga:verilog`), packaged
   like maia-hdl's `ip/dvbs2rx` (generate Verilog, then
   `package_ip.tcl`) and instantiated in `system_bd.tcl` for
   `plutoskyr2` with the connections above.
3. **A build:** `make all` in `maia-hdl/projects/tezuka` with
   `PROJECT_NAME=plutoskyr2`, then Tezuka's `board-fpga-reconfigure`
   and image build.
4. **Booting it from a spare SD card.** Never flash it.

## Licences

The code here is Apache-2.0 like the rest of the repository. maia-hdl
is MIT and Tezuka is GPL-3.0: neither is copied here, and both are
referenced by path and branch only.
