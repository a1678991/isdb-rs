# fpga — FPGA implementations of receive stages

Amaranth HDL for the stages of the receive chain that a radio's FPGA
can run instead of the host CPU (plan: the run-time choice between CPU
and FPGA implementations of a stage, like a SIMD kernel). Each stage has
a Rust reference model in the workspace that it must match **bit for
bit**. The model writes test vectors, and the HDL simulation here
reproduces them.

The first stage is the single-carrier front end of
[`isdb-frontend`](../crates/isdb-frontend/README.md) (IQ → `Symbols`,
format version 3) for ISDB-S and ISDB-S3 on the PlutoSky R2
(XC7Z020). It turns 8-bit IQ at 40 Msps (80 MB/s, more than the board's
Gigabit Ethernet path sustains) into 6-bit symbols at 43 MB/s (ISDB-S)
and 51 MB/s (ISDB-S3). Format 2 adds the output bank `k` that carries
the host's timing trim ([`isdb-frontend` README, "The timing
trim"](../crates/isdb-frontend/README.md#the-timing-trim)): the output
and the AGC come from `k`, the timing detector from `h` and `g`. Format
3 changes only the block trailer, to a CRC-32C (two words) that the
packer updates one 16-bit payload word per clock: `crc32c_word` is a
one-level XOR network per register bit (11 to 22 inputs, 560 in all),
derived from the polynomial by symbolic simulation of the bit-serial
register, not typed in. The sequence number is folded in while the
header goes out, so the trailer costs one cycle per block more than
format 2's.

## Layout

| Path | Content |
|---|---|
| `isdb_fpga/frontend.py` | `SymbolFrontend`: mixer, polyphase matched, derivative and output filters (one 54-bit `h | g | k` word per lane and cycle, T/4 lanes, 4 cycles per input sample), timing loop, AGC, quantiser, packer. The cycle-by-cycle schedule is in its docstring |
| `isdb_fpga/control.py` | `Control`: the command decoder and register file on `up_adc_gpio_out`, the ID word and readback on `up_adc_gpio_in` (`isdb_frontend::control`) |
| `isdb_fpga/top.py` | `SymbolsRx`: the block for the bitstream (front end + control + output selection), and where it connects in Tezuka's block design |
| `isdb_fpga/sim.py` | simulation against a vectors file; `python -m isdb_fpga.sim VECTORS.json [--jitter] [--load-port] [--via-gpio]` |
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

Results (format 2, 2026-10-25; format 3 re-checked 2026-10-26):

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
    compared);
  - format 3: bit-exact on the regenerated default set, trailers
    included; leaving out the CRC's final XOR fails at the first
    trailer word (word 169 of the 5-bit set).
- **Estimate** of `SymbolsRx` (Yosys, before place and route; Vivado's
  numbers will differ):

  | Taps | DSP48E1 | LUT | FF | Block RAM |
  |---|---|---|---|---|
  | 32 (format 3) | 64 | 4.2k (4 175) | 4.6k (4 583) | 1 RAMB18 + 8 RAMB36 |
  | 32 (format 2) | 64 | 3.9k (3 896) | 4.6k (4 566) | 1 RAMB18 + 8 RAMB36 |
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
The format-3 CRC is not expected to be the critical path: its widest
output bit XORs 22 inputs, two levels of 6-input LUTs (6 × 6 = 36 ≥ 22)
between the payload-word register and the CRC register, against the
multi-level adder and DSP paths of the filters (an estimate from the
logic depth, not a timing report).

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
| clock, reset | `util_ad9361_adc_fifo/dout_clk`, `dout_rstn` inverted (62.5 or 125 MHz, see below) |

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
`isdb_symbols.tcl` after `cs12_cs8.tcl`. `sdbin.sh` turns an XSA into
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

**Status (2026-10-26): the stock design builds and closes timing with
Vivado 2026.1; the design with `SymbolsRx` builds but does not close
timing, so there is no bitstream to deploy yet.**

| xc7z020clg484-2 | LUT | FF | BRAM36 | DSP | WNS | WHS |
|---|---|---|---|---|---|---|
| Tezuka's own build (2023.1, `doc/fpga_usage.md`) | 23 005 | 32 111 | 46 + 19 BRAM18 | 106 | — | — |
| stock, rebuilt with 2026.1 | 23 211 (44 %) | 32 304 (30 %) | 55.5 | 106 (48 %) | +0.100 ns | +0.024 ns |
| with SymbolsRx (after route) | — | — | — | — | −3.281 ns | −3.647 ns |

**Update 2026-10-26: the block cannot work as designed on this board,
whatever its timing.** It was designed for 4 clock cycles per input
sample (`CYCLES = 4`, a time-multiplexed MAC) and for a loop latency of
16 cycles inside the model's `LOOP_DELAY` of 6 symbols, which the model
documents as "24 clock cycles at 4 cycles per sample". On the board
there is **one clock per sample**:

- The board runs the AD9361 in LVDS 2R2T mode (device tree
  `adi,lvds-mode-enable`, `adi,2rx-2tx-mode-enable`; at 40 Msps the
  driver reports `RXSAMP:40000000`). The LVDS data clock `rx_clk` is
  4 × fs = 160 MHz, and `util_ad9361_divclk` divides it by 4
  (`SEL_0_DIV = 4`, `xilinx_ad9361.tcl` with `lvds` set): the block's
  clock is **40 MHz = fs**. The "160 MHz" of the plan was `rx_clk`,
  which the block never sees. The 125 MHz failure below is the BUFGMUX's
  other input (`clk_div_sel_1_s`, the 1R1T divider at the 250 MHz
  maximum `rx_clk` Vivado constrains); 2R2T at 40 Msps never selects it.
- `util_wfifo` (`util_ad9361_adc_fifo`) moves samples into that domain
  in bursts: every 8 samples written in `l_clk`, it reads them out on
  8 consecutive `dout_clk` cycles (`dout_req_cnt`, `dout_valid`). So
  `adc_valid` is high on back-to-back cycles, not every 4th.

With one sample per clock the MAC would have to be fully parallel
(`CYCLES = 1`), and symbols (1.18–1.39 samples apart) can be due on
consecutive clocks, so `LOOP_DELAY = 6` symbols can be as few as 6 clock
cycles. The loop path (delay line → 32-tap dot product → round/saturate →
× G → round/saturate → TED and power products → shift/saturate → kp·e,
ki·e → rounding → integrator and period clamp) needs about four
multiplier stages plus adder trees: it cannot be closed in 6 cycles, even
at 40 MHz, and pipelining makes it longer. Timing at 40 MHz itself is
easy (the unpipelined dot product is 9.2 ns of 25 ns).

What it needs is a model change, owned by `isdb-frontend` (not made
here; it changes the bit-exact output and the FPGA vectors):

1. **`LOOP_DELAY` in clock cycles of the real rate.** At one sample per
   clock and ≥ 1 symbol per 1.18 samples, a latency of N clocks needs
   `LOOP_DELAY ≥ ceil(N / 1.0)` symbols in the worst case. A
   straightforward full-rate pipeline (registered dot product in 2–3
   stages, ×G, TED/power, loop terms, shadow commit) is about 16–20
   cycles: **`LOOP_DELAY = 24`** leaves margin. The loop stays stable:
   the timing loop bandwidth is 5·10⁻⁶ R (ISDB-S3) to 2·10⁻⁴ R, so 24
   symbols of delay is negligible phase margin loss, but it must be
   checked on the recordings (lock, MER) before it is adopted.
2. **Or keep `LOOP_DELAY = 6` and decimate first:** no, the input rate is
   fixed by the 40 Msps the receivers need.
3. **The parallel MAC itself:** today's 64 DSP48E1 (Yosys, 32 taps) are
   48 for the MAC (8 lanes × 6 products, each 18 × 14 bits, one DSP per
   product, time-shared over 4 cycles) plus 16 for mixer, gain, TED and
   loop. At one cycle per sample the MAC is 32 × 6 = 192 DSPs, about
   208 in all; with Tezuka's 106 that is ~314 of the xc7z020's 220. It
   needs the multiplies to drop: the three banks need not all be full
   length (e.g. `g` only feeds the TED; `k` equals `h` when untrimmed),
   or fewer taps. Each is again a model change.

So the next step is a design decision on the model (loop delay, MAC
budget), then a re-derivation of the FPGA schedule, not a timing fix.

The two causes found on the post-opt checkpoint (still valid as timing
facts, but superseded by the above):

1. **The ADC clock is faster than designed for.** `dout_clk` of
   `util_ad9361_adc_fifo` is `util_ad9361_divclk`, `rx_clk` divided by 4
   (`clk_div_sel_0_s`, 62.5 MHz at the constrained 250 MHz `rx_clk`) or by 2
   when both `adc_r1_mode` and `dac_r1_mode` are set (`clk_div_sel_1_s`,
   125 MHz). The front end passes at 62.5 MHz (WNS +5.4 ns) and fails at
   125 MHz (WNS −2.58 ns, 288 endpoints): the worst path is a cascade of
   five DSP48E1 on PCIN, 9.2 ns of DSP delay against 8 ns, i.e. one
   filter dot product with no pipeline register between the DSPs (the
   earlier "160 MHz" estimate assumed the DSPs' internal registers would
   be used; the generated multiply-adds don't allow it). Fix: pipeline
   the dot products in `frontend.py` (a few cycles of latency, absorbed
   by the loop delay the model already has), keeping `mise run
   fpga:test` bit-exact.
2. **`gpio_in` crosses clock domains untimed.** The ID/readback word
   (ADC clock) is read by `axi_ad9361`'s register file in `clk_fpga_0`;
   Vivado times it as a 2 ns synchronous path (−4.06 ns). It is
   quasi-static (the host reads it milliseconds after a command), so it
   needs a `set_max_delay -datapath_only` / false-path constraint (or a
   register on the AXI side), as ADI does for its own status words.

**Deploying (once a build closes timing).** Never flash. The card's FAT
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
   reads the ID word 0x15DB…; the stock bitstream reads 0.
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
