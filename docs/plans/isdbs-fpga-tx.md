# Plan: ISDB-S transmitter in the PlutoSky R2 FPGA (real-time radio TX)

Written 2026-10-29 from three investigations run after
[isdbs-loopback.md](isdbs-loopback.md) landed (`isdb tx` / `isdb loopback
--system isdbs` on the CPU). That plan's §2 left one gap: a radio cannot
take ISDB-S at its symbol rate over the Pluto's link (40 Msps of cs12 is
160 MB/s against a ≈ 65 MB/s Gigabit Ethernet ceiling), so `--radio`
underruns. This plan closes it the way reception was closed: the host
keeps the coder, the FPGA runs the one stage whose output is too wide
for the link. Evidence and line references are in the three reports:

| Report | Question |
|---|---|
| [isdbs-fpga-tx/dac-path.md](isdbs-fpga-tx/dac-path.md) | Tezuka's DAC datapath, clocks, control registers, its DVB-S2 TX and TX FIR as precedents, the DSP budget, the AD9361 digital loopback, firmware limits |
| [isdbs-fpga-tx/boundary-host.md](isdbs-fpga-tx/boundary-host.md) | the host→FPGA boundary (labels), word format, link budget, host changes by crate, the on-board self-test, the test ladder |
| [isdbs-fpga-tx/interpolator.md](isdbs-fpga-tx/interpolator.md) | the interpolator: MER versus phases/taps, DSP/BRAM budget with Yosys estimates, the fixed-point model spec, underrun behaviour, loopback acceptance |

Study tooling: `crates/isdb-frontend/examples/txinterp.rs` (the
interpolator model, MER grid, loopback into `FastFrontend`, underrun
gaps), `fpga/synth/tx_sketch.py` (the Amaranth datapath sketch and its
Yosys estimate), `fpga/synth/tx_sketch_sim.py` (the sketch against a
Python model, bit for bit).

## 1. Decision

**Boundary: `TxSymbols`, one 3-bit constellation code per symbol**, host
→ radio, at 28.86 Mbaud. Every ISDB-S symbol (header and burst BPSK,
BPSK / QPSK / TC8PSK main signal) is one of the eight points e^{jkπ/4}
of annexed table 46 (boundary-host.md §1, label table there), so the
code stream is lossless and costs 11.5–14.4 MB/s depending on the
packing, against 32.5 MB/s already streamed to this board for ISDB-T
TX. The FPGA block (`SymbolsTx`) unpacks the codes, maps them to I/Q and
interpolates them with the root-raised-cosine pulse to the DAC rate
(40 Msps, 1.386 samples per symbol, one sample per clock). Nothing else
moves: the `isdbs_phy::tx::Transmitter` already produces the symbols
above real time, and the shaper it runs today is what the block
replaces.

**Interpolator: P = 512 phases, span 12 (24 taps), nearest phase on
half-phase rows, no multipliers.** Because every point is on the 45°
grid, a tap needs one ROM value (the coefficient for even angle indices,
the coefficient / √2 for odd ones) and a sign/zero mux per component:
0 DSP48E1, about 2.0k LUT, 2.3k FF and 12 RAMB18 (the pulse's symmetry
halves the ROM: one true-dual-port RAMB18 per tap pair). TX ISI MER
54.4 dB; in the model's digital loopback the receive block's 6-bit
output reads 36.7 dB (the f32 shaper: 36.2 dB). The TX+RX image stays at
178 of 220 DSP48E1 (interpolator.md §1–3).

**Placement: between `util_ad9361_dac_upack` and `axi_ad9361_dac_fifo`
in the divclk domain**, the pattern of Tezuka's `txfir.tcl`: the block
owns `upack/fifo_rd_en` (it pulls one DMA word when its symbol
accumulator needs codes), takes `fifo_rd_data_0/1`, `fifo_rd_valid` and
`fifo_rd_underflow`, drives the rfifo's `din_data_0/1` with `din_valid_in`
forced high while selected, and passes today's nets through when off.
It replaces the ×8 TX FIR interpolator (useless at 40 Msps; 20 DSP48E1
back) (dac-path.md §1.4–1.6).

**Control: the DAC core's GPIO pair** (`up_dac_gpio_out` 0x800000BC,
`up_dac_gpio_in` 0x800000B8 on `cf-ad9361-dds-core-lpc`, the same
offsets as the RX block's on the ADC core, derived from ADI's
`up_dac_common` word 0x2E/0x2F; `gpio_in` is unconnected in Tezuka so
it reads 0 without the block), the RX block's command protocol reused
(`control::Encoder`, toggle bit 31, ops NOP/ADDR/HI/WRITE/READ/CTRL/
WRITE_S), a distinct ID magic, bits 0–3 of `gpio_out` left alone (bit 0
is the TX FIR's enable, also written by the kernel's interpolation
attribute; 1–3 belonged to the DVB-S2 path the image drops)
(dac-path.md §3, boundary-host.md §4.4).

**Underrun: emit the zero symbol, set a sticky flag, count missing
symbols.** The DAC never stops; the receiver loses that superframe and
re-syncs on the next frame in every variant (interpolator.md §5).

## 2. Signal and format (to freeze in the model, then the HDL)

**Frozen in step 2** (`isdb_frontend::txformat`, `shape::fixed`,
`control::tx`; crate README "The TX boundary: symbol codes", "Transmit
interpolator (the FPGA model)", "The TX register map"). Where the list
below differs, the model rules:

- code 0 is the zero symbol, 1..7 are reserved (the zero symbol), 8 + k is
  the point e^{jkπ/4}: bit 3 marks a *point*, so zero padding is silence;
- the second packing: 8 codes per two-channel DMA sample, code 0 in I bits
  3..0, codes 4..7 in the Q word;
- no forced carry at START: the line starts with zero codes, frac = 0,
  sample 0 is 0 and the first code enters at the first carry;
- a stalled word request enters 8 zero symbols, each counted in MISSED;
  FIRST_MISS latches CONSUMED at the first; the sticky flag is STATUS bit 2;
- `Encoder::configure_tx` writes taps 0..S of each row (the stored half).

- Codes: 4 bits per symbol, bits 2..0 the angle index k (point
  e^{jkπ/4}; table-46 label → k is a fixed 8-entry map on the host:
  000→0, 001→1, 011→2, 010→3, 100→4, 101→5, 111→6, 110→7), bit 3 = the
  zero symbol. Two packings were proposed; **pick one when the model is
  written**:
  - boundary-host.md §2.2: 16-bit words, 5 × 3-bit labels, bit 15 = 1
    marks a data word (0x0000 = idle, which is what libiio's zero padding
    and an upack underflow deliver); 11.54 MB/s; `voltage0` alone if the
    driver accepts a single TX channel, else both channels and 10 labels
    per 32-bit read;
  - interpolator.md §4: 8 × 4-bit codes per 32-bit DMA sample (both
    channels enabled, as the DAC side needs anyway: with only `voltage0`
    enabled Q0 would come from the DDS, dac-path.md §1.6); 14.43 MB/s; no
    valid bit, the zero code is the idle symbol.
  Both fit; the second is simpler in HDL (no mod-5 counter, no partial
  words) and needs no single-channel buffer, so it is the default
  choice unless the single-channel mode is wanted for another reason.
- Phase accumulator Q0.24, `step = round(2²⁴·R/fs)`: −0.028 ppm at
  40 Msps, exact at 57.72 and 61.44 Msps; the receiver absorbs it.
- Backoff folded into the coefficients (`A = round(G·p·D·2^S)`,
  D = 2048·10^(−b/20), S = 7 at 10 dB); peak ≤ 1 047 of 2048 LSB, so no
  saturation at backoff ≥ 4.2 dB; the level is set with the AD9361's TX
  attenuation. The 12-bit word goes MSB-aligned into `dac_data[15:4]`
  (the DAC keeps the top 12 bits).
- Rows at (q + ½)/P picked by truncation (same accuracy as nearest of
  P + 1 rows, exact symmetry `c[q][t] = c[P−1−q][2S−1−t]`, which the FPGA
  relies on and `validate` must enforce, as it enforces the RX's TED
  zeros).
- Default bank (ISDB-S, 40 Msps, 10 dB) as BRAM init in the bitstream;
  reload through the GPIO port only for another rate (12 288 writes at
  0.62 ms = 7.6 s, so the init matters).
- Pipeline at one sample per clock, latency 8 clocks, no feedback (no
  loop delay); the deepest stage is a 24-bit add.

## 3. Work, in order

Each step is a commit with tests and docs, as the RX path was built
(plan isdb-s.md §7: CPU reference first, frozen, then HDL, then image).

1. **Fix the DAC-side `r1_mode` guard** (`isdb-radio` pluto backend):
   it reads 0x80000044 bit 2 on both cores; on the DAC core that bit is
   `ext_sync_disarm` and `r1_mode` is 0x80000048 bit 5 (ADI
   `up_dac_common.v`, confirmed). Small, independent, do first.
2. **Fixed-point model** (done; frozen as §2 says)
   `isdb_frontend::shape::fixed` (`TxDesign` → `TxParams` with
   `validate`, the model, codes/packing, vectors):
   bit-exact spec in interpolator.md §4; tests against the f32 `Shaper`
   (≥ 40 dB through an ideal matched filter), through `FastFrontend` at
   40 Msps (≥ 36.0 dB at the 6-bit output), chunking invisible, underrun
   gaps, symmetry enforced. `control::tx` register map, ID word,
   `Encoder::configure_tx`. The CLI's `--offload cpu` for files can use
   this model so that the file path and the FPGA path are the same bits.
3. **Label path in `isdbs-phy`** (done: the codes come from one encoder
   path with the points; loopbacks from the codes byte-exact, symbol and
   sample domain through the fixed model): `Transmitter::superframe_codes`
   (codes produced natively from the bits, not recovered from the f64
   points: `qpsk_point` and `psk8_point` differ by an ulp), refuse a
   dummy combination with slots (the one case that leaves 0+0i symbols),
   `psk8_point(code) ≈ symbol` tests on every layout.
4. **Host transport** (done, tested with the mock radio, a model of the
   block's command decoder and dry runs; the on-board self-test is
   written, `ISDB_RS_PLUTO_TX_LOOPBACK_URI`, and waits for the image.
   Measured: the codes' producer at 11.1× real time, release, 1.0 ms per
   superframe, against 1.14× with the shaper; the transmitter without
   the `f64` points was cheaper than the 3.3 ms estimated. Deviations:
   START|SELECT at open and RUN|SELECT only once the stream's first
   buffer is in the board, since the block counts every word requested
   before the first arrives as missed; the sink converts the device's
   kernel queue ahead before it starts the stream, so the queue fills
   from codes at the link's speed; `Offload::TxSymbols` carries the
   phases too, so `auto` can tell a block of another geometry;
   `isdb-iio` unchanged; isdb-radio README "TX symbol codes")
   (`isdb-radio`, `isdb-iio`): `Offload::TxSymbols`,
   a TX-direction probe (`offloads` parses capture args today),
   `SymbolSink`, `QueuedSink` generic over its converter (`Quantizer` or
   the code packer; prefill, back-pressure, drain and underrun counting
   reused with the mock tests), `open_tx_symbols` on the Pluto: configure
   as today (attenuation last, LO down with `loopback`, DDS off), probe
   the ID, refuse r1_mode and DAC GPIO bit 0 set, configure, START,
   RUN|SELECT, **read STATUS and require "selected"** (codes sent to a
   plain DAC would go out as samples), CTRL 0 on stop/drop; `kbuffers`
   for TX; the block's MISSED / FIRST_MISS counters in status and
   summary. `isdb tx --system isdbs --offload auto|fpga|cpu`: the
   producer hands codes instead of shaped samples (the measured 1.13×
   real time becomes an estimated ≈ 3×; measure), the gate and
   `--dry-run` unchanged (`--dry-run` does not probe), `describe_tx`
   names the FPGA modulator and the link rate.
5. **HDL** (done: bit-exact on the model's five vector sets and two of a
   Python twin, every clock, rfifo bursts, DMA stalls, bank through the
   port and through `gpio_out`, START while running; Yosys 0 DSP48E1,
   2.7k LUT, 2.9k FF, 12 RAMB18; second Tcl `isdb_symbols_tx.tcl`;
   fpga/README.md "The transmit block")
   `fpga/isdb_fpga/tx.py` (`SymbolsTx`: unpacker, accumulator,
   the mux datapath of the sketch, adder trees with `use_dsp = "no"`,
   rounding/saturation, counters, `Control` reuse with the TX register
   map), `top.py` gains the TX block, `sim.py` runs the TX vectors
   (every clock, DMA gaps, bank loaded through the port, configured
   through `gpio_out`; one LSB off in a tap must fail), `tezuka/
   isdb_symbols.tcl` extended (or a second Tcl) with the upack/rfifo
   rewiring in place of `txfir.tcl`, the XDC (DAC `gpio_out` false path,
   `gpio_in` datapath-only, the r1 clock case false), `apply.sh` drops
   the `txfir` flag, `README.md`.
6. **Image**: build (Vivado, outside the repo as `fpga/tezuka/build.sh`
   does), timing, utilisation (expect 178 DSP, ≈ 23.7k LUT, +6 BRAM36),
   deploy on a card copy, the on-board checks of §4.
7. **Docs and verification**: VERIFICATION.md rung "ISDB-S transmitter
   in the FPGA", README feature list, transmitting.md, CHANGELOG,
   isdb-s.md status.

## 4. Verification ladder

**Done 2026-10-29 through rung 3** (VERIFICATION.md "ISDB-S transmitter
in the FPGA: the image" and "on the board"): the image builds and meets
timing, both blocks answer on the board, and the loopback self-test
passes byte-exact. **Rung 4 done the same day** (VERIFICATION.md "ISDB-S
transmitter in the FPGA: into the PIX-DT295"): conducted through a 30 dB
pad and a DC block, the PIX-DT295 locks at the first tune and records a
clean TS with the multiplexer's SI; the live feed's PCR timing is the
open follow-up (a PCR pacer in the ISDB-S producer), with a
satellite-shaped NIT.

1. Model unit and property tests (step 2); the symbol-domain and
   sample-domain loopbacks of isdbs-loopback.md with the fixed model.
2. HDL simulation bit-exact on the model's vectors (step 5).
3. **On-board, nothing radiated** (`loopback=on`: AD9361 register 0x3F5
   bit 0, the digital data-port loopback TX1→RX1, TX LO down): `isdb tx
   --system isdbs --radio pluto:loopback=on --offload fpga --rf-enable`
   in one process and `isdb rx --system isdbs --radio pluto --offload
   fpga` in another (two libiio contexts, both at 40 Msps; the RX
   acquires on a short IQ capture as today). Acceptance: every TS
   byte-exact against its input, 0 uncorrectable, 0 host underruns,
   MISSED = 0, 0 RX losses, 8PSK MER ≥ 36.0 dB at the 6-bit output, the
   measured trim within ±0.05 T of 0. Stronger: a buffered 12-bit IQ
   capture in loopback equal to the model's samples after an offset
   search. An opt-in test under a new variable (`ISDB_RS_PLUTO_URI` is
   documented receive-only), plus a deliberate starvation run whose
   FIRST_MISS lands in the superframe the receiver reports lost.
4. Over the air, conducted, into the PIX-DT295 after isdb-s.md §2's
   prerequisites (unchanged).

## 5. Board checks before any of the above means anything

**Done 2026-10-29: every row below is answered in VERIFICATION.md "ISDB-S
transmitter in the FPGA: board checks"** (the DAC GPIO pair reads as
derived, the underflow flag is bit 0, a single-channel TX buffer works,
the link is clean to 64 MB/s and starved at 80, the digital loopback is
sample-exact, the chip is an AD9361). Kept as the list of assumptions:

| Check | Settles |
|---|---|
| `cf-ad9361-dds-core-lpc` 0x800000B8 reads 0 on the stock and isdb images; 0x800000BC bit 0 state | the TX control pair and discovery |
| DAC 0x80000048 bit 5 at 40 Msps; `ad9361-phy` `tx_path_rates` | r1_mode guard, the DAC clock relation |
| 0x80000088 bit 0 versus Tezuka's `& 4` while a TX stream is starved | the device-side underflow flag |
| a TX buffer with `voltage0` alone (`start_tx`) | single-channel packing viability |
| cs12 TX at 4 / 6 / 8 / 10 Msps into `loopback=on`, underruns counted | the TX-direction link ceiling (never measured; 32.5 MB/s is only a point that works) |
| `/etc/init.d` on the card: does `S96plutostream` start `pluto_stream` / `pluto_mqtt_ctrl` | the `/dev/mem` hazard below |
| UG-570 register 0x3F5: is the loop outside the AD9361's TX/RX filters | loopback transparency (bit-exact or MER-only acceptance) |
| `ad9361-phy` compatible string | AD9361 (40 MHz TX bandwidth) versus AD9363A (20 MHz) |

**Hazard found (dac-path.md §3):** Tezuka's `pluto_stream` and
`pluto_mqtt_ctrl` read-modify-write both GPIO-out registers through
`/dev/mem` with 16-bit masks, which clears bits 31–16 and would flip the
toggle bit the RX and TX command protocols rely on, and at start-up
`pluto_stream` writes the DVB switch registers that do not exist in any
image without `dvb.tcl` (the isdb RX image included). `tools/
pluto-autorun.sh` kills both daemons at boot (and again 20 s later), so
the bench board is covered after boot; the window before S98 and the
effect of the absent-register writes are unverified.

## 6. Risks and open questions

- The interpolator's "span 12 reads 0.5–0.7 dB better than span 16
  through the RX front end" (interpolator.md §1) is a measurement
  artifact, settled in step 2: `metrics::psk_mer` counted the receiver's
  outputs over the zero tail after the last symbol (2·span − 1 symbols,
  longer for span 16). In the steady state the model reads 36.96 dB at
  the 6-bit output (44.41 dB before the quantiser) at span 12, 36.98 /
  44.45 dB at span 16, and the f32 shaper 36.98 / 44.54 dB
  (`isdb-frontend` `examples/txloop.rs`). Span 12 stays (span 8's
  out-of-band peak is −41.5 dB, span 12's −49 dB).
- Timing at 40 MHz is unverified in Vivado (the deepest stage is a
  24-bit add; the RX block's paths are longer and close with the stock
  margin +0.100 ns).
- The AD9361 digital loopback's transparency decides whether the
  on-board test can be bit-exact or only MER- and TS-level.
- The TX-direction link ceiling has not been measured; both packings sit
  far below the one rate known to work.
- The ADI HDL read for the DAC path was `c9a501d`, not the pinned
  `cf81ab15`; the cores involved change rarely, a diff would settle it.
- The block's first request after RUN finds the unpacker empty (8–16
  symbols missed at CONSUMED 0, in the lead-in); a prefetch of the first
  word(s) while selected but not running would remove it (an HDL change
  and a rebuild).
- ISDB-S3 TX is out of scope: its 16/32APSK points are not on a 45°
  grid (per-ring ROMs or multipliers) and α = 0.03 needs a span of 32–64.
