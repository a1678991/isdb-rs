# ISDB-S transmit block on the PlutoSky R2 bitstream: the DAC path (investigation I1)

Read-only investigation, 2026-10-29. No code changed, no device accessed.

## Sources and revisions

| Abbrev. | Path | Revision |
|---|---|---|
| `F5` | `~/dev/sdr/maia-sdr-f5oeo/maia-hdl` (F5OEO fork, branch `refactor`) | HEAD `9c1cd64`. The bitstream was built from `f7092ba`. `git log f7092ba..HEAD` touches only `projects/common/uart16550.tcl` and the fishball7020 case of `projects/tezuka/system_bd.tcl`, so **every TX file below is identical at f7092ba**. |
| `ADI` | `~/dev/sdr/adi-hdl` (sparse checkout; the other files were read with `git show HEAD:path`) | `c9a501d` (2026-10-20). **This is not the pinned `cf81ab15`**, which is absent from this clone. The fork's own `maia-hdl/adi-hdl` and `maia-hdl/dvb_fpga` submodules are **not initialised** (empty directories). |
| `DVB` | OpenResearchInstitute/dvb_fpga `297a5b7e`, fetched with `gh api` (`build/vivado/dvbs2_encoder_wrapper.vhd`, `rtl/dvbs2_encoder.vhd`) plus the fork's patches `F5/patches/dvb_fpga/*.patch` | `297a5b7e` |
| `TZ` | `~/dev/sdr/tezuka_fw` | v0.3.24 `a304a1f` |
| `PS` | F5OEO/pluto-ori-ps `d76565f` (branch `tezukadvb`: `pluto_stream`, `pluto_mqtt_ctrl`), fetched with `gh api`. The pin is in `TZ/package/pluto_stream/pluto-stream.mk:12`. | `d76565f` |
| `LX` | analogdevicesinc/linux `bf1d49f` (the kernel Tezuka builds, `TZ/configs/plutoskyr2_defconfig:25`): `drivers/iio/adc/ad9361.c`, `ad9361_conv.c`, `ad9361_regs.h`. `cf_axi_dds.c` / `.h` were read at `main`. | `bf1d49f` |

The DSP figures for the ADI blocks below assume `c9a501d` behaves like `cf81ab15` for `util_rfifo`, `util_upack2`, `up_dac_common` and `axi_dmac`. These cores change rarely, but a diff against `cf81ab15` would settle it.

No utilisation report exists on this machine: `~/dev/sdr/out` does not exist, and no `*utilization*.rpt` is under `~`. The `WORK/out/isdb/v4b_utilization*.rpt` that `fpga/README.md` cites is not present. Resource figures come from `fpga/README.md` and `TZ/doc/fpga_usage.md`.

---

## 1. The DAC datapath in the plutoskyr2 block design

### 1.1 Which TX Tcl is sourced

`F5/projects/tezuka/system_bd.tcl:70-75` sets these flags for plutoskyr2: `lvds`, `txfir`, `dac_dds`, `dvb`. The files are sourced in this order:

1. `xilinx_ad9361.tcl` (`:91`): the base TX path.
2. `txfir.tcl` (`:108`)
3. `dvb.tcl` (`:111`)

`rxfir.tcl` is not used by plutoskyr2. `firinterp32.coe` is used only by `common/test.tcl`.

### 1.2 Base path (`F5/projects/common/xilinx_ad9361.tcl`)

```
DDR --HP2--> axi_ad9361_dac_dma (axi_dmac, MM src -> AXIS dest, 64 bit, CYCLIC 1)   :249-256, maia.tcl:69
   m_axis --> util_ad9361_dac_upack/s_axis (util_upack2, 4 ch x 16 bit)              :260-263, :281
   fifo_rd_data_0..3 --> axi_ad9361_dac_fifo/din_data_0..3 (util_rfifo, 16->16 bit) :274, :193-196
   axi_ad9361_dac_fifo/dout_data_0..3, dout_valid_0..3, dout_enable_0..3
                     --> axi_ad9361/dac_data_i0,q0,i1,q1, dac_valid_*, dac_enable_*  :201-212
```

**Handshake nets**

| Net | Line |
|---|---|
| `axi_ad9361_dac_fifo/din_valid_0` → `upack/fifo_rd_en` (the pull request) | `:285` |
| `upack/fifo_rd_valid` → `dac_fifo/din_valid_in_0..3` | `:272` |
| `upack/enable_i` ← `dac_fifo/din_enable_i` (the DAC channel enables, synchronised) | `:271` |
| `upack/fifo_rd_underflow` → `dac_fifo/din_unf` | `:286` |
| `dac_fifo/dout_unf` → `axi_ad9361/dac_dunf` | `:213` |

**Clocks**

| Item | Clock | Line |
|---|---|---|
| upack | `util_ad9361_divclk/clk_out` | `:277` |
| DMA `m_axis_aclk` | divclk | `:280` |
| DMA `m_src_axi_aclk` | `sys_cpu_clk` | `:336` |
| rfifo `din_clk` | divclk | `:199` |
| rfifo `dout_clk` | `axi_ad9361/l_clk` | `:197` |

**Widths.** The DMA bus is 64 bit (`DMA_DATA_WIDTH_DEST 64`, `:256`). The device tree agrees: `TZ/board/tezuka/plutoskyr2/kernel-dts/xilinx/fishball.dtsi:196-215`, `tx_dma` with 64/64 bus widths.

The upack and the rfifo carry 16-bit samples per channel (I0, Q0, I1, Q1). With I0 and Q0 enabled, one 64-bit DMA beat holds two (I, Q) pairs.

`axi_ad9361` keeps `dma_data[15:4]` (12 bit) when the channel's data select is DMA (`ADI/library/axi_ad9361/axi_ad9361_tx_channel.v:297-301`). This matches the host's `Cs12`: 12 bits left-aligned in `i16`.

### 1.3 How the DAC side pulls

**`util_rfifo`** (`ADI/library/util_rfifo/util_rfifo.v`, `git show`)

- The `dout` side (l_clk) advances its read address on every `dac_valid` (`dout_valid_0`). Every 8 samples it toggles `dout_req_t`.
- The `din` side (divclk) synchronises that toggle. It answers with `din_valid` (= `din_valid_0..7`, the request) **held for 8 consecutive din clocks** (`din_req_cnt` preset 0x78, counted while bit 6 is set).
- It writes `din_data_*` into the 16-entry memory only on cycles where `din_valid_in_0` is high (`din_wr <= din_valid_in_0` for ratio 1).

So the request pattern is **bursts of 8 strobes**, which averages exactly the DAC sample rate. This mirrors `util_wfifo` on the ADC side.

**`util_upack2`** (`ADI/library/util_pack/util_upack2/util_upack2_impl.v`)

- Only `fifo_rd_en[0]` is used.
- `s_axis_ready = ready & s_axis_valid & fifo_rd_en`: it takes a new 64-bit beat only when its current beat's enabled samples are used up.
- `fifo_rd_data` and `fifo_rd_valid` are registered one clock after `fifo_rd_en`.
- If `s_axis_valid` is low when read: data is zeroed, `fifo_rd_valid = 0` and `fifo_rd_underflow = 1`.

**Underrun chain.** `fifo_rd_underflow` → rfifo `din_unf` → (2 flops) → `dout_unf` → `axi_ad9361/dac_dunf` → `up_dac_common` `up_status_unf`. That flag is DAC register word 0x22 bit 0, sticky, write 1 to clear (`ADI/library/common/up_dac_common.v:380-385, :483`). On the `cf-ad9361-dds-core-lpc` device it is debug address **0x80000088 bit 0**. Tezuka polls exactly this:

- `TZ/board/tezuka/common/overlay_tezuka/root/api_controller.sh:516-519`
- `PS/mqtthandlestream.cpp:982-987, 1255-1260`

On underrun the DAC does not stop. The rfifo simply does not write (`din_valid_in` low), so its read side replays the stale 16-entry memory.

**Can a block pull DMA words at its own rate?** Yes: nothing ties the DMA to the sample rate.

- `axi_dmac` is a pull stream with a store-and-forward buffer of `FIFO_SIZE × MAX_BYTES_PER_BURST` (defaults 8 × 128 B = 1 KiB, `ADI/library/axi_dmac/axi_dmac.v:64-65`). Its source and destination clocks are asynchronous (`ASYNC_CLK_SRC_DEST`, `axi_dmac_ip.tcl:331-336`; the docs are `docs/library/axi_dmac/index.rst:91-96, 230-246`).
- The upack consumes a beat only on `fifo_rd_en` (or a tready, upstream of it).
- **Tezuka's TX FIR already does this:** in ×8 mode it drives `upack/fifo_rd_en` with a 1-in-8 pulse while the rfifo is fed every clock (§1.4).

A cyclic DMA (`CYCLIC 1`, and libiio `create_buffer(..., cyclic=true)`) would also let one superframe of labels repeat with no host streaming. This would be useful for bench tests.

### 1.4 Tezuka's TX FIR interpolator: the divclk-side mux precedent (`F5/projects/common/txfir.tcl`)

**Wiring**

1. It disconnects `dac_fifo/din_valid_0 → upack/fifo_rd_en` and `upack/fifo_rd_data_0/1 → dac_fifo/din_data_0/1` (`:2-4`).
2. It instantiates ADI's `ad_add_interpolation_filter "tx_fir_interpolator" 8 2 1 {61.44} {7.68} interpolator_x8.coe` (`:7-8`). The proc is in `ADI/projects/common/xilinx/adi_fir_filter_bd.tcl:139-238`: ×8, two channels (I0, Q0), one path each, a Systolic-MAC `fir_compiler`, 128 symmetric taps (`F5/projects/common/interpolator_x8.coe`), clocked by divclk (`:12`).
3. It inserts itself between the upack and the rfifo:
   - rfifo `din_valid_0/1` → filter `dac_valid_0/1` (`:17-18`);
   - upack `fifo_rd_data_0/1` → filter `data_in` (`:21-22`);
   - filter `data_out` → rfifo `din_data_0/1` (`:15-16`);
   - filter `valid_out_0` → `upack/fifo_rd_en` (`:33`).

**Inside the filter's out_mux** (`adi_fir_filter_bd.tcl:215-236`)

- **Inactive:** pass-through. Data is `data_in`, valid is `dac_valid`.
- **Active:** the upack is read on `rate_gen` pulses (1 every 8 clocks), and the FIR output goes to the rfifo.

**Control and fixes**

- `active` = `up_dac_gpio_out[0]` through `interp8_slice` (`:25-29`), synchronised inside (`cdc_sync_active`).
- Tezuka adds a 2-deep tvalid delay (`:36-65`).
- The rfifo `din_valid_in_0/1` becomes `OR(upack/fifo_rd_valid, active)` (`dac_fifo_valid_or`, `:73-87`): in FIR mode the rfifo writes on every clock.

**This is the template for our block:**

- the same three ports (request in, `fifo_rd_en` out, data and valid_in to the rfifo);
- a pass-through when off;
- a mode bit from `up_dac_gpio_out`.

It is the exact analogue of `isdb_symbols.tcl` taking over the ADC packer's nets.

### 1.5 Tezuka's DVB-S2 chain: the AXIS-side mux precedent (`F5/projects/common/dvb.tcl`)

**Rewiring of the DMA output**

- `dvb.tcl` disconnects the DMA `m_axis` from the upack and reclocks `m_axis_aclk` to `sys_cpu_clk` (`:39-44, :57-66`).
- A same-clock `axis_data_fifo dma_domain_fifo` is needed only because "the DMA IP locks its CLK_DOMAIN property" (`:46-55`).

**Data path**

```
DMA (64b, sys_cpu_clk) -> dma_domain_fifo -> switchsrc (axis_switch 1x2, AXI-lite 0x43C03000)
   M00 -> dvbs2_encoder_wrapper_0 (64b in, 32b IQ out, AXI-lite 0x43C10000) -> dw_enc_out (32->64) -> switchfir/S01
   M01 -----------------------------------------------------------------------------------> switchfir/S00
switchfir (2x2, 0x43C02000)
   M01 -> dw_rrc_in (64->32) -> rrc_2interpol (fir_compiler, x4) -> dw_rrc_out (32->64) -> switchdest/S00
   M00 ----------------------------------------------------------------------------------> switchdest/S01
switchdest (2x1, 0x43C01000) -> interclk (axis_data_fifo, async, 16 deep: sys_cpu_clk -> divclk) -> upack/s_axis
```

References: `:12-19` (the header diagram), `:70-95, :128-158, :219-244`.

**Clocks.** `sys_cpu_clk` = FCLK0 = 100 MHz (`F5/projects/boards/plutoskyr2/ps7.tcl:16`). So the whole DVB chain, its RRC included, runs at 100 MHz behind back-pressure. It is **not** in the divclk domain.

**Pacing.** It comes entirely from the DAC through `tready`: rfifo request → (txfir) → `upack/fifo_rd_en` → upack `s_axis_ready` → `interclk` → switches → RRC → encoder → DMA.

### 1.6 What this means for the TX block (two placements, both with precedent)

**A. Between the upack and the rfifo, in divclk (the txfir pattern).** Recommended.

| Our port | Connects to |
|---|---|
| request in | `dac_fifo/din_valid_0` |
| label read strobe out | `upack/fifo_rd_en` |
| label data in | `upack/fifo_rd_data_0/1`, `fifo_rd_valid`, `fifo_rd_underflow` |
| I/Q out | `dac_fifo/din_data_0/1` |
| valid out | `din_valid_in_0/1` (forced to 1 when active, like `dac_fifo_valid_or`) |
| pass-through when off | today's nets |

- The block produces one I/Q sample per request strobe (clock-enable the pipeline with `din_valid_0`). It pulls a label word only when its symbol NCO needs one: 28.86/5 ≈ 5.77 M words/s with 5 labels per 16-bit word, or half that with two words per read.
- The output sequence depends only on the labels and the parameters (there is no feedback), so a bit-exact model is independent of the request timing.
- In an image that keeps txfir, chain upack → txfir → our block → rfifo, or simply replace txfir (§4).

**B. On AXIS, between the DMA and the upack, in divclk (the dvb.tcl pattern, without its reclocking).** Without `dvb.tcl` the DMA `m_axis` is already in divclk (`xilinx_ad9361.tcl:280`).

- The block takes 64-bit label beats with tready and emits 64-bit I/Q beats on the upack's `s_axis_ready`.
- This needs a small output skid FIFO and a free-running pipeline gated by "FIFO not full".
- It is simpler to splice (two interface nets), but the request timing is less direct.

**Either way the host must enable both `voltage0` and `voltage1` of `cf-ad9361-dds-core-lpc` (TX1 I and Q).**

- `axi_ad9361`'s `dac_enable_*` follows each channel's data select (`axi_ad9361_tx_channel.v:272`: enable = `DATA_SEL == DMA`).
- The driver sets DMA only for channels in the scan mask and DDS for the rest (`cf_axi_dds.c` `cf_axi_dds_update_scan_mode`).
- With only `voltage0` enabled, Q0 would come from the DDS, not from our block.

So each libiio "sample" is 32 bits, carrying two 16-bit label words. This is the same as `pluto_stream`'s BBFRAME mode, which enables `tx0_i` and `tx0_q` and pushes bytes (`PS/mqtthandlestream.cpp:599-603, 1239`).

---

## 2. Clocks on the DAC side

- **TX datapath clock = `util_ad9361_divclk/clk_out`**, the same net as the RX block's clock. It drives the upack, the rfifo `din`, the DMA `m_axis` and txfir.
- `util_clkdiv` takes `SEL_0_DIV 4` / `SEL_1_DIV 2` with `lvds` (`xilinx_ad9361.tcl:148-158`). Its select is `util_reduced_logic` over `{adc_r1_mode, dac_r1_mode}` (`:138-146`; no `C_OPERATION` is set, so it takes the IP default, AND, which is consistent with the README).
- The file's comment: "interface runs at 4x in 2r2t mode, and 2x in 1r1t mode" (`:135-136`).
- With the measured l_clk = 160 MHz at 40 Msps (`fpga/README.md` "Clock"), divclk = 40 MHz = the DAC sample rate, and `dac_valid` occurs every 4 l_clk. The TX block therefore runs at one sample per clock, like `SymbolsRx`. It is timed in the 62.5 MHz `clk_div_sel_0_s` constraint, and the r1 case is false-pathed as for RX.

**CDCs**

| Crossing | Where |
|---|---|
| DDR/AXI (100 MHz) → divclk | inside `axi_dmac` (async burst memory) |
| divclk → l_clk | `util_rfifo` (toggle request, 16-entry dual-clock RAM) |
| `up_dac_gpio_out` (AXI clock) → divclk | a two-flop sync in our block, as in `Control`; Tezuka false-paths bits 0–2 in `F5/projects/boards/plutoskyr2/system_constr.xdc:217-221` |

> **Host bug found (`crates/isdb-radio/src/backends/pluto.rs:168-169, 903-907`).** The r1_mode guard reads 0x80000044 bit 2 on *both* devices. On the DAC core, word 0x11 (0x44) is the sync-control register, and bit 2 there is `up_dac_ext_sync_disarm` (`up_dac_common.v:463-466`). The DAC r1_mode is word 0x12 = **0x80000048 bit 5** (`up_dac_common.v:272-279, 467-468`; LX `cf_axi_dds.h` `ADI_REG_CNTRL_2 0x0048`, `ADI_R1_MODE (1 << 5)`). The README's "bit 2 = 0 on both cores" is therefore true but meaningless for the DAC core. The TX block will rely on the same divider, so the check should be fixed before it does.

---

## 3. Control plane: `up_dac_gpio_out` / `up_dac_gpio_in`

**Registers.** `up_dac_common` (`COMMON_ID 6'h10`, decoded at `up_waddr[13:7] == {COMMON_ID,0}`, `up_dac_common.v:49, 192-193`) has GPIO in at word 0x2E and GPIO out at word 0x2F (`:422-423, 486-487`). The ADI regmap text is `ADI/docs/regmap/adi_regmap_dac.txt:481-509`.

- In the `axi_ad9361` address space these are bytes 0x40B8 / 0x40BC.
- `cf-ad9361-dds-core-lpc` is mapped at 0x79024000 (`fishball.dtsi:236-245`), i.e. core offset 0x4000.
- The driver's debugfs `direct_reg_access` treats `DEBUGFS_DRA_PCORE_REG_MAGIC` = 0x80000000 as "core register at offset" (LX `cf_axi_dds.h`; `cf_axi_dds.c` `cf_axi_dds_reg_access`).

**So: `gpio_out` = 0x800000BC and `gpio_in` = 0x800000B8 on `cf-ad9361-dds-core-lpc`.** These are the same offsets as RX, on the other IIO device. This is confirmed by `ADI_REG_DAC_GP_CONTROL 0x00BC` in LX `cf_axi_dds.h`, and by Tezuka using 0x80000088 on this device for the DAC status.

**Users at this revision**

| Bit of `up_dac_gpio_out` | User |
|---|---|
| 0 | txfir `active`, `txfir.tcl:25-29`. Also **the kernel**: `cf_axi_interpolation_set()` read-modify-writes `ADI_REG_DAC_GP_CONTROL` bit 0 when the DDS `sampling_frequency` is set to parent/8. This is enabled by `adi,axi-interpolation-core-available` (`fishball.dtsi:243`). |
| 1 | DVB encoder `rst_n`, `dvb.tcl:112-117` |
| 2 | RRC coefficient set, `dvb.tcl:193-198` |
| 3 | RRC config tvalid, `dvb.tcl:200-205` |

- Bits 31–4 are unused.
- `up_dac_gpio_in` is **unconnected** (no hit for `up_dac_gpio_in` in `F5/projects`). It reads 0 on the stock bitstream, so the RX discovery scheme (an ID word, 0 = absent) carries over unchanged.
- Recommendation: a separate TX command channel on `cf-ad9361-dds-core-lpc` 0x800000BC/B8, with an ID magic distinct from 0x15DB, leaving bits 0–3 alone.

> **Hazard: Tezuka's daemons rewrite these registers through `/dev/mem` with 16-bit masks.**
>
> - `pluto_stream` and `pluto_mqtt_ctrl` start at boot (`S96plutostream` is installed by `TZ/package/pluto_stream/pluto-stream.mk:52`; `BR2_PACKAGE_DATV=y` in `plutoskyr2_defconfig:159` selects the package).
> - They read-modify-write `0x79020000+0x40BC` (DAC GPIO out) with `& 0xFFFE` / `0xFFFD` / `0xFFF3` (`PS/mqtthandlecommand.cpp:579-587`; `PS/mqtthandlestream.cpp:253-275`).
> - They read-modify-write `0x79020000+0xBC` (**ADC** GPIO out, the RX block's channel) with `& 0xFFF1` (`PS/mqtthandlecommand.cpp:927-948`, MQTT `rx/format`).
> - These masks **clear bits 31–16**. They would flip the toggle bit 31 of the RX `Control` protocol, and of any TX equivalent, whenever those MQTT commands run.
> - At start-up, `pluto_stream` also calls `SetTxMode(tx_passtrough)` → `SetFPGAMode()` (`PS/mqtthandlestream.cpp:1508, 279-327`). That function writes the DVB `axis_switch` registers at 0x43C01000–0x43C03000, which **do not exist in an image without `dvb.tcl`**. This already applies to the current isdb image. The effect of a `/dev/mem` write to an unmapped GP0 address (DECERR → SIGBUS, or an abort) is unverified; check it on the board.
> - Whether `S96plutostream` actually runs on the user's card, and what the TX mode is at boot, is settled by `/etc/init.d` on the board.

---

## 4. How Tezuka feeds its DVB-S2 transmitter

**What the TX DMA carries.** Byte-oriented BBFRAMEs, not symbols:

- each frame is `0xB8`, a MODCOD byte, the BBFRAME data, then zero padding to 8 bytes (`PS/mqtthandlestream.cpp:1175-1191`);
- they are pushed with `iio_buffer_push_partial(m_txbuf, total_len/4)` on `cf-ad9361-dds-core-lpc` with `voltage0` and `voltage1` enabled (`:599-603, 1239`);
- buffers are ≈ 20 × max BBFRAME, 8 kernel buffers (`:1412-1424`);
- when no data is queued, the host inserts padding TS packets (PID 0x1FFE) or GSE (`:1553-1568`);
- dummy PLFRAMEs can be enabled in the encoder (`SetDVBS2Dummy`, bit 18 of 0x43C10000, `:378-385`).

**Encoder.** The fork patches `dvbs2_encoder_wrapper` to drop `inline_config_adapter` (`F5/patches/dvb_fpga/remove_inline_config_adapter.patch`). It makes `axi_bbframe_length_enforcer` self-synchronising on SYNC 0xB8 + MODCOD (`padding_ok.patch`). It is applied by `F5/Makefile:15-20`.

The encoder chain (`DVB/rtl/dvbs2_encoder.vhd:198-834`):

1. width → 8 bit
2. BBFRAME length enforcer
3. BB scrambler
4. BCH
5. LDPC
6. bit interleaver
7. **constellation mapper with an AXI-lite-programmed IQ mapping RAM** (`:725-760`; `pluto_stream` loads it with `SetDVBS2Constellation`)
8. PL framer

Output: one symbol per AXIS beat, `IQ_WIDTH 32` = 16-bit I + 16-bit Q (`dvbs2_encoder_wrapper.vhd` generics). It uses 2 DSP48E1 and 16 BRAM36 (`TZ/doc/fpga_usage.md:23`).

**RRC `rrc_2interpol`** (`dvb.tcl:160-205`)

- Xilinx `fir_compiler` 7.2, Interpolation, `Interpolation_Rate 4`, `Number_Paths 2` (I, Q), Systolic MAC, `Coefficient_Structure Inferred`, `ColumnConfig 17`.
- 16-bit coefficients and data, 16-bit output, symmetric rounding to zero.
- Nominal `Clock_Frequency 61.44` / `Sample_Frequency 15.36`, i.e. 4 clocks per input and one output per clock. It actually runs at 100 MHz behind tready, with `S_DATA_Has_FIFO`.
- `Coefficient_Sets 2`: `rrc_interp_firwin2.coe` holds 2 × 129 taps, both symmetric, peak 32767 at the centre, DC sum ≈ 124 k and 126 k (≈ 0.95 per phase). These are `pluto_stream`'s `ROFF20` / `ROFF15` (`PS/mqtthandlestream.cpp:240-246`).
- The set is selected by DAC GPIO bit 2, latched by a bit-3 pulse (`SwitchFirFilter`, `:268-276`).
- About 33 taps per phase; **38 DSP48E1, 753 LUT, 3124 FF** (`fpga_usage.md:26`).

**Rate control.** There is no rate generator in the DVB chain:

- symbol rate = DAC rate / 4 (RRC) / {1, 8} (txfir, bit 0) / {1, 4} (the AD9361's own TX FIR);
- the host picks the AD9361 sample rate as that integer multiple (`PS/mqtthandlecommand.cpp:753-917`, `ComputeTxSRDVBS2`, `FPGA_DVBS2 4.0`, `FirInterp8()`);
- the DAC's pull, through back-pressure, sets the pace; underflow is polled at 0x80000088.

**Only integer samples-per-symbol is supported.** Our 40/28.86 = 2000/1443 needs our own fractional (NCO) polyphase interpolator. This is new relative to Tezuka. The exact ratio 1443/2000 allows a drift-free modulo-2000 phase counter.

**The TX FIR interpolator (×8, 20 DSP)** serves low symbol rates: DAC ≥ 2.083 Msps with symbol rates down to kHz. At 40 Msps it is never active for ISDB-S.

- **Our block replaces it** in an isdb TX+RX image, which gives back 20 DSP.
- Or our block sits after it, with txfir held in bypass. But bit 0 is also driven by the kernel's interpolation attribute and by `pluto_stream`.
- Placing our block *before* txfir is pointless: it would interpolate our 40 Msps by 8.

---

## 5. Resource picture

| DSP48E1 (xc7z020: 220) | Count | Source |
|---|---|---|
| `axi_ad9361` (2R2T, DAC DDS on) | 28 | `fpga_usage.md:22` |
|   of which the DAC DDS (`DAC_DDS_DISABLE 0`, from the `dac_dds` flag) | 8 (and 4.6 k LUT, 6.1 k FF) | `fpga_usage.md:124-144` |
|   of which the 2nd RX/TX channel (`MODE_1R1T 0`) | 10 (and 1.0 k LUT) | `fpga_usage.md:152-174` |
| `maia_sdr` (spectrometer, DDC, recorder) | 18 (3.4 k LUT, 21 BRAM36 + 16 BRAM18) | `fpga_usage.md:24` |
| DVB-S2: encoder 2 + `rrc_2interpol` 38 | 40 (≈ 7 k LUT, 16 BRAM36, plus the switches) | `fpga_usage.md:23-31` |
| `tx_fir_interpolator` | 20 (564 LUT) | `fpga_usage.md:29` |
| `SymbolsRx` (format 4, 32 taps) | 112 (5.8 k LUT after P&R) | `fpga/README.md:183-184` |

The DDS and 1R1T deltas were measured on the Tezuka author's private 7010-reduction checkout, "not this repo" (`fpga_usage.md:118-123`). The v0.3.24 plutoskyr2 case still sets `dac_dds` and 2R2T. The table also mislabels `cs12_8mux` as a TX block; it is the RX 12→8-bit packer (`cs12_cs8.tcl`).

**Budgets**

| Image | DSP before TX block | Free | 64-DSP TX (32 taps × I/Q) | 32-DSP TX (16 taps) |
|---|---|---|---|---|
| Current isdb RX image (no DVB) | 178 | 42 | no | **fits: 210 (95 %)** |
| RX + TX, txfir dropped | 158 | 62 | no (222) | **fits: 190 (86 %)** |
| RX + TX, txfir dropped, DAC DDS off | 150 | 70 | 214 (97 %): risky, see the 154-DSP history in `fpga/README.md:202-209` | 182 |
| RX + TX, txfir and DDS dropped, `maia_sdr` removed | 132 | 88 | **fits: 196 (89 %)** | 164 |
| Dedicated TX image (no RX block, no DVB, no txfir) | 46 | 174 | fits easily (110) | 78 |

What each removal costs:

- **DVB-S2 and txfir:** unused by ISDB-S at 40 Msps.
- **DAC DDS:** the host only uses it to switch the DDS off (`pluto.rs:1658`, which writes `altvoltage0` `raw` = 0). With `DAC_DDS_DISABLE` the DDS channels may disappear, so that call must tolerate it. The Tezuka MQTT `siggen/*` topics stop working (`fpga_usage.md:146-150`).
- **`maia_sdr`:**
  - its DDC feeds `mux_decim_i/q` (`F5/projects/common/maiafirtoiq.tcl:14-57`), which is `SymbolsRx`'s input. Without maia, `isdb_symbols.tcl` must take `util_ad9361_adc_fifo/dout_data_0/1` and `dout_valid_0` directly;
  - `maia_sdr` is also the recorder behind `isdb capture recorder=maia` and Tezuka's web waterfall (`maia-httpd`);
  - the default `recorder=iio` and the symbol stream do not need it.
- **1R1T:** loses `channel=2` and `mode 2r2t`, and **changes the clock relation**: with r1_mode on both cores the divider becomes /2. That needs re-verification of "one sample per clock" for both blocks.

**Design lever worth evaluating before choosing 64 vs 32 DSP:**

- Every input is one of the 8 TC8PSK points, so each component is in {0, ±1/√2, ±1}.
- A tap product c[p][t]·s therefore needs no multiplier. Store c and c/√2 per (phase, tap) in the per-tap coefficient ROMs, like `SymbolsRx`'s per-tap 64-entry ROMs, and apply sign and zero muxes.
- That leaves only the adder trees: 31 adds × 2 at ≈ 20 bit, ≈ 1–1.5 k LUT. The interpolator would cost about 0 DSP48E1.
- At 32 taps it would then fit even next to the full RX block with txfir kept, on LUT budget (the RX image is at 41 % LUT).
- This is a design note, not verified by synthesis.

---

## 6. AD9361 digital loopback as an on-board TX→RX self-test

`loopback=on` writes `ad9361-phy` debugfs `loopback` = 1, which runs `ad9361_bist_loopback(phy, 1)` (LX `ad9361.c:990-1032`). It sets `REG_OBSERVE_CONFIG` (0x3F5) bit 0 `DATA_PORT_LOOP_TEST_ENABLE` (`ad9361_regs.h:532, 2742`). The comment reads "loopback (AD9361 internal) TX->RX".

**The loop point is the AD9361's digital data port.** The 12-bit TX words the FPGA serialises on the LVDS bus come straight back on the RX data port. So:

```
TX block -> rfifo -> axi_ad9361 TX channel (dma_data[15:4]) -> LVDS
         -> AD9361 data port -> LVDS -> axi_ad9361 RX -> util_wfifo -> mux_decim -> SymbolsRx
```

- With the TX LO powered down, as `pluto.rs:1642-1683` already does, nothing is radiated.
- It loops TX1→RX1 (`ad9361_int_loopback_fix_ch_cross`, `:978-988`).
- RX and TX share the data-port rate, 40 Msps in 2R2T LVDS.

**Not verified from these sources:** whether any AD9361 RX digital processing (RX FIR, decimators, DC/quad correction) lies between the loop point and the data port. The driver code implies it is at the port. UG-570's description of register 0x3F5 would settle it. If it is at the port, the RX block sees the TX block's samples bit-exactly, which is a strong end-to-end check of the two fixed-point models.

**A second loopback needs no AD9361 at all.** `axi_ad9361`'s RX channel can select the DAC data: `adc_data_s = (adc_data_sel == 0) ? adc_data : dac_data` (`ADI/library/axi_ad9361/axi_ad9361_rx_channel.v:129`). The select is `REG_CHAN_CNTRL_3` [3:0] (`up_adc_channel.v:353-355, 429`), i.e. debug address 0x80000418 + 0x40·ch on `cf-ad9361-lpc`.

The two loopbacks differ:

- The FPGA-internal path skips the LVDS I/O. It isolates block bugs from interface or timing issues.
- The driver's `loopback=2` is the opposite direction: RX→TX via DDS `DATA_SEL_LB` = 8 (`ad9361_conv.c:334-365`).

---

## 7. Firmware constraints on 40 Msps TX with 40 MHz RF bandwidth

**RF bandwidth**

- `out_voltage_rf_bandwidth_available` is "[200000 1 40000000]" (LX `ad9361.c:7222-7227`). The store path clamps with `ad9361_validate_rf_bw()`, which allows 0.2–56 MHz for AD9361 but **0.2–20 MHz for AD9363A** (`:859-867`).
- The TX analog filters then clamp the baseband half-bandwidth to ≤ 20 MHz:
  - primary filter corner ≈ 1.6 × BBBW = 32 MHz (`ad9361_tx_bb_analog_filter_calib`, `:2459-2470`);
  - secondary filter `BBBW*5π` rad/s ≈ 50 MHz, clamped at 20 MHz BBBW (`:2497-2510`).
- So 40 MHz RF is the maximum and is accepted. ISDB-S occupies ±19.48 MHz, inside the 32 MHz corner, with small edge droop; the RX front end was validated at this rate.

**Chip variant**

- The DT says `compatible = "adi,ad9361"` (`fishball.dtsi:259`) with `adi,2rx-2tx-mode-enable` (`fishball.dts:18-26`).
- U-Boot's `adi_loadvals_pluto` would rewrite `attr_val ad9361 → ad9363a` (`TZ/board/tezuka/plutoskyr2/uboot-env.txt`). But plutoskyr2's `adi_loadvals` runs `adi_loadvals_libre`, and `sdboot` does not run `adi_loadvals` at all.
- So an SD boot keeps AD9361 and 2R2T, consistent with the measured r1_mode / l_clk. The board's `/sys/bus/iio/devices/*/name` and `ad9361-phy` `compatible` would confirm it. An AD9363A would cap TX bandwidth at 20 MHz.

**Rates and FIR**

- The DT defaults are for 30.72 Msps: `adi,tx-path-clock-frequencies` (`fishball.dtsi:340`) and `adi,rf-tx-bandwidth-hz = <18000000>` (`:333`). The host overrides both at run time (40 Msps and 40 MHz in `crates/isdb-cli/src/txs.rs:65-73`).
- No AD9361 TX FIR is loaded by Tezuka at boot. `api_controller.sh` and `sweep.sh` only clear `in_out_voltage_filter_fir_en`. `pluto_stream` loads FIRs (`setad9363filter`) only for its interpolated DVB modes (`PS/mqtthandlecommand.cpp:679-700`).
- At 40 Msps the AD9361 runs TX with FIR off and its fixed half-bands, as `isdb tx --system isdbs` does today.
- The actual `tx_path_rates` at 40 Msps were not read (no device access). The board's `ad9361-phy` `tx_path_rates` would settle it.

**Interpolation attribute.** `adi,axi-interpolation-core-available` (`fishball.dtsi:243`) makes the DDS device's `out_voltage_sampling_frequency` offer parent/1 and parent/8. Writing parent/8 sets DAC GPIO bit 0. A TX block that reserves bit 0 is unaffected. One that removes txfir leaves bit 0 harmless, but the attribute would then mis-report the rate if someone writes it.

**Kernel patches.** None of Tezuka's patches applied to plutoskyr2 (`patches/linux/adi-6.12.y`) touches the TX data path. `0002-extend_freq.patch` lowers the TX/RX LO minimum to 46.875 MHz. The x4 TX-FIR patch lives in another directory and is not applied.

---

## Open items (and the file or measurement that settles each)

1. Whether `c9a501d` and the pinned `cf81ab15` differ in `util_rfifo`, `util_upack2`, `up_dac_common` or `axi_dmac`: diff the two ADI revisions, which needs a fetch of `cf81ab15`.
2. The AD9361 data-port loopback point relative to the RX digital chain: UG-570, register 0x3F5.
3. Whether `S96plutostream` runs on the deployed card, and the effect of its writes to the absent DVB switch registers and its 16-bit-mask RMWs of both GPIO-out registers: the board's `/etc/init.d` and `dmesg`.
4. Whether the DDS channels survive `DAC_DDS_DISABLE` for the host's "DDS off" call: LX `cf_axi_dds.c` probe (the `dds_disabled` / CONFIG register handling).
5. The `tx_path_rates` and the DAC `r1_mode` (0x80000048 bit 5) at 40 Msps: a read-only check on the board.
