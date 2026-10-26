# fpga — FPGA implementations of receive stages

Amaranth HDL for the stages of the receive chain that a radio's FPGA
can run instead of the host CPU (plan: the run-time choice between CPU
and FPGA implementations of a stage, like a SIMD kernel). Each stage has
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

**Clock.** The block runs on `util_ad9361_adc_fifo/dout_clk` =
`util_ad9361_divclk` = `axi_ad9361` l_clk / 4 (maia-hdl
`projects/common/xilinx_ad9361.tcl`: `SEL_0_DIV 4` with `lvds`, which
Tezuka's plutoskyr2 build sets; `/ 2` only when the AD9361 core's ADC
and DAC are both in r1_mode). Measured on the board (read-only, 2026-10-26):
`cf-ad9361-lpc` register 0x80000054 (`adc_clk_count`) = 0x1999A, i.e.
l_clk = 104 858 × 100 MHz / 2¹⁶ = 160.0 MHz; register 0x80000044
(control) bit 2 (`r1_mode`) = 0 on both cores; `rx_path_rates` RXSAMP
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
| `isdb_fpga/top.py` | `SymbolsRx`: the block for the bitstream (front end + control + output selection), and where it connects in Tezuka's block design |
| `isdb_fpga/sim.py` | simulation against a vectors file; `python -m isdb_fpga.sim VECTORS.json [--pattern every\|wfifo\|jitter] [--load-port] [--via-gpio]` |
| `tests/test_isdb_fpga.py` | unit tests (the CRC-32C network against a bit-serial register and the standard check value) and the vector tests |
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
  (`apply.sh`), which reception does not use: 66 + 112 = 178 of 220.

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

A dedicated IIO device per output boundary, with its format version as
an attribute (the plan's longer-term form), would need a device-tree
node and a driver. The GPIO route needs neither.

### Building the PlutoSky R2 bitstream (`tezuka/`)

```sh
fpga/tezuka/build.sh WORK stock   # the unmodified design
fpga/tezuka/build.sh WORK isdb    # with SymbolsRx
fpga/tezuka/sdbin.sh WORK/out/isdb/plutoskyr2.xsa isdb_top.bin
```

`build.sh` clones the pinned upstream revisions into WORKDIR (outside
the repository; several GB), builds maia-hdl's IP cores and the
`plutoskyr2` XSA. `apply.sh` (called in `isdb` mode) regenerates
`fpga/build/isdb_symbols_rx.v`, sets an `isdb_symbols` flag in the
plutoskyr2 case of maia-hdl's `system_bd.tcl` and sources
`isdb_symbols.tcl` after `cs12_cs8.tcl`, drops the plutoskyr2 case's
`dvb` flag (the DVB-S2 transmitter, for its DSP slices) and adds
`isdb_symbols.xdc` (below). `sdbin.sh` turns an XSA into
the file U-Boot loads from the SD card (Tezuka's `postimage-sd.sh`
conversion).

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

**Status (2026-10-26): the format-4 image builds and closes timing with
Vivado 2026.1 (ADI's "Timing Constraints" check passes); `isdb_top.bin`
is ready to deploy. Not yet run on the board.**

| xc7z020clg484-2 | LUT | FF | BRAM36 | DSP | WNS | WHS |
|---|---|---|---|---|---|---|
| Tezuka's own build (2023.1, `doc/fpga_usage.md`) | 23 005 | 32 111 | 46 + 19 BRAM18 | 106 | — | — |
| stock, rebuilt with 2026.1 | 23 211 (44 %) | 32 304 (30 %) | 55.5 | 106 (48 %) | +0.100 ns | +0.024 ns |
| format 4, without the DVB-S2 transmitter | 21 694 (41 %) | 28 421 (27 %) | 62 | 178 (81 %) | **+0.100 ns** | **+0.022 ns** |
| of which `isdb_symbols` | 5 825 | 5 519 | — | 112 | — | — |

Per clock, after route and physical optimisation (all met): the block's
`clk_div_sel_0_s` (constrained at 62.5 MHz from ADI's worst-case 250 MHz
`rx_clk`; it runs at 40 MHz) +1.523 ns; `clk_div_sel_1_s` (125 MHz, the
r1_mode case: Tezuka's own logic only, the block's paths there are false)
+0.652 ns; `clk_fpga_0` +0.548 ns; maia-sdr's `clk_out3` (187.5 MHz)
+0.675 ns; `rx_clk` +0.445 ns. The design's overall worst slack, +0.100 ns,
is the same path class as the stock build's (Tezuka's PlutoSky R2 has the
smallest margin of its boards). Reports:
`WORK/out/isdb/v4b_timing_summary.rpt`,
`v4b_utilization*.rpt`.

| Artefact (`WORK/out/isdb/`) | Bytes | sha256 |
|---|---|---|
| `plutoskyr2.xsa` | — | `12abb5eb1047290101d54ea82e8bfb73e3f978819fa14131d6839c340a8f3368` |
| `isdb_top.bin` (from `sdbin.sh`) | 2 785 408 | `479141e02d8a151dd8475ac196b658beee7820ad9c298123191688aaa3ed1fd3` |

How it got there (each step a commit): the first format-4 build put the
block at 154 DSP48E1 (Vivado moved the adder trees and the loop's adds
into DSP post-adders), filling the device (220 / 220), and missed timing
in Tezuka's own paths (`interclk` → `cs12_8mux`, maia-sdr's DDC) from the
placement pressure; `isdb_symbols.xdc` had been read during synthesis,
before `util_clkdiv`'s generated clocks exist, so the block was still
timed in the r1_mode clock. `use_dsp = "no"` on the adders and the XDC
as implementation-only, processed late, fixed both.

`isdb_symbols.xdc`: `gpio_out` into the block's synchroniser is a false
path (Tezuka's and ADI's constraints declare the same register's bits 0–2
false paths); `gpio_in` into `axi_ad9361`'s `up_rdata` a datapath-only
max delay (the word is static whenever the host reads it); the block's
paths in `clk_div_sel_1_s` (l_clk / 2, only with r1_mode on both AD9361
cores, never on this board) false. The host refuses the symbol path if
r1_mode is ever set (`isdb-radio` pluto backend reads register
0x80000044 bit 2 of both cores).

**What the image leaves out:** Tezuka's DVB-S2 transmitter (`dvb.tcl`,
40 DSP48E1), for the DSP budget. To transmit DVB-S2, boot the stock
`system_top.bin`.

**Deploying.** Never flash. The card's FAT
partition already holds several bitstreams (`system_top.bin`,
`tezuka_top.bin`); U-Boot loads `${bitstream_image}` (`fatload mmc 0 …`),
which `uEnv.txt` sets (`bitstream_image=system_top.bin` in v0.3.24).
On a **copy** of the card (keep the original as the rollback):

1. copy `isdb_top.bin` (from `sdbin.sh`) to the FAT partition, next to
   `system_top.bin`;
2. in that partition's `uEnv.txt`, set `bitstream_image=isdb_top.bin`;
3. boot from SD as today (DIP switches unchanged: both at 3.3 V).

Rollback: `bitstream_image=system_top.bin`, or the original card.
`sdbin.sh` was checked against the board: on the XSA v0.3.24 ships it
reproduces the card's `system_top.bin` byte for byte (sha256
`4e9ea3825f2c9fd08906f0398fff2a652736a843503f21407793fc300c045c76`).

**Verifying after deployment** (receive only):

1. `isdb symbols --radio pluto:uri=ip:192.0.2.30 --freq BS7 --probe`
   reads the ID word 0x15DB4020 (magic, format 4, 32 taps); the stock
   bitstream reads 0.
2. `isdb symbols --radio pluto:uri=ip:192.0.2.30 --freq BS7 --system
   isdb-s3 --offload auto --seconds 10 --out fpga.sym`: 0 loss events
   from the CRC-32C reader, the link rate at 51 MB/s (6 bits).
3. Bit-exactness on the board: capture the same signal as IQ and run
   `isdb iq symbols --system isdb-s3` with the same parameters; the words
   must be identical (the FPGA vectors are the bench version of this).
4. Rest of the chain: `isdb iq s3sync --read fpga.sym` (Es/N0 as from
   the CPU front end) and `isdb rx --system isdbs3` on the symbols.

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
