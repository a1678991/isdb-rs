# ISDB-S transmit symbols boundary (host → PlutoSky R2 FPGA): host side

Investigation only, 2026-10-29. Repository `~/dev/isdb-rs` at `ccdc99c`.
ADI HDL facts come from `~/dev/sdr/adi-hdl` (`c9a501d`); Tezuka's from
`~/dev/sdr/maia-sdr-f5oeo/maia-hdl` and `~/dev/sdr/tezuka_fw`.
**[confirm]** marks a claim that the HDL agent or a board test still has to settle.

## 0. Summary

- Every ISDB-S symbol the transmitter makes is one of the 8 unit-modulus points of
  `psk8_point(label)`. A 3-bit label per symbol therefore loses nothing. There is one edge
  case (§1.3).
- Proposed wire format: a plain stream of 16-bit words, **5 labels per word, bit 15 = 1 marks
  a data word**. A word with bit 15 = 0 is idle, which covers zero padding. There is no
  header, CRC, sequence number or frame marker: the DMA is lossless and in order, and its
  only failure is an underrun, which inserts silence (a time slip) without losing or
  reordering any symbol. The block counts its own underruns.
- Rate: 28.86 Mbaud / 5 × 2 B = **11.54 MB/s**. That is 36 % of the 32.5 MB/s of ISDB-T cs12
  that already streams to this board without underruns. 8-bit I/Q per symbol would be
  57.7 MB/s.
- Host work: an `Offload::TxSymbols` that is discovered through the **DAC** core's GPIO
  (`cf-ad9361-dds-core-lpc`, debug 0x800000B8 / 0x800000BC, the same offsets as the ADC
  core, derived from ADI's `up_dac_common`); a `SymbolSink` trait; `QueuedSink` generalised
  over its converter (`Quantizer` or `LabelPacker`); `Backend::open_tx_symbols`; a TX
  register map driven by the existing `control::Encoder`; and a label path in the
  `txs.rs` producer that replaces the shaper.

## 1. Every symbol is a TC8PSK point (question 1)

### 1.1 Evidence

- `psk8_point` (`crates/isdbs-phy/src/demod.rs:87-99`) maps label k to `from_polar(1, k·π/4)`:
  unit modulus. The angle table is checked by `spec_table_46_points` (`demod.rs:448-472`).
- Header: `frame_symbols` (`crates/isdbs-phy/src/tx.rs:75-101`) sends the coded W1/TMCC/W2-W3
  bits as `bpsk(x)`/`bpsk(y)`, where `bpsk` = (±1, 0) (`tx.rs:52-54`). The bursts use the
  same `bpsk` (`tx.rs:92-97`). The output is zero-initialised (`tx.rs:81`), and header (192)
  + bursts (192 × 4 = 768) + main (38 976) = 39 936 = `FRAME_SYMBOLS`
  (`crates/isdbs-core/src/frame.rs:27-38`), so every position is written.
- Main signal, `encode_frame_with_state` (`demod.rs:332-414`):
  - BPSK: `Cplx::new(±1, 0)` (`demod.rs:352-360`).
  - QPSK, punctured or not: `qpsk_point(p1, p0)` = (±1/√2, ±1/√2) (`demod.rs:103-110`). With
    `qpsk_p1_on_q` it is (im, re) (`:363-368`). Unit modulus either way.
  - TC8PSK: `psk8_point((c2<<2)|(c1<<1)|c0)` (`:398-405`).
  - The runs of `slots::runs` (`crates/isdbs-core/src/slots.rs:33-53`) fill `MAIN_SYMBOLS`
    exactly when the assigned combinations sum to 48 (`spec_fig_4_examples`).

### 1.2 Label table (default `Mapping`: QPSK P1 on I, TC8PSK C1 = 133₈)

| Symbol source | Point | Label C2C1C0 |
|---|---|---|
| header and bursts, BPSK main: bit 0 | +1 (0°) | `000` |
| header and bursts, BPSK main: bit 1 | −1 (180°) | `100` |
| QPSK (P1,P0) = 00 | +45° | `001` |
| QPSK 10 | +135° | `010` |
| QPSK 11 | −135° (225°) | `101` |
| QPSK 01 | −45° (315°) | `110` |
| TC8PSK [C2 C1 C0] | table 46: 000 0°, 001 45°, 011 90°, 010 135°, 100 180°, 101 225°, 111 270°, 110 315° | the label itself |

- QPSK in closed form: `C2 = P0`, `C1 = P1⊕P0`, `C0 = ¬(P1⊕P0)`. The QPSK labels are exactly
  the four with C1 ≠ C0.
- With `qpsk_p1_on_q` (I/Q swap, a reflection about the 45° line): 001→001, 010→110,
  101→101, 110→010.

### 1.3 Caveats

- **Do not derive labels from the f64 points.** `qpsk_point` uses `FRAC_1_SQRT_2`, while
  `psk8_point` uses `cos(π/4)`, so the two can differ by an ulp. Generate labels natively
  instead (§4.5). If labels are ever recovered from points, use the nearest point, never
  equality.
- **Edge case.** `TmccInfo::validate` (`crates/isdbs-core/src/tmcc.rs:262-291`) accepts a
  Dummy (1111) entry with slots ≠ 0. `runs` gives no symbols for it, so the tail of the main
  signal would stay at 0+0i (`demod.rs:342`), which no label represents.
  - §6.1.2 requires 0 slots for a dummy entry (`CombinationEntry::DUMMY`, `tmcc.rs:66-69`),
    and `TmccInfo::assign` always builds it that way, so the CLI never hits this.
  - The label path should still refuse it, in `Transmitter::new` or the label encoder.
- **`Mapping::conjugate`** (z → z̄, `demod.rs:409-413`; default false, not used on air) is a
  fixed label permutation k → −k mod 8: 000→000, 001→110, 011→111, 010→101, 100→100,
  101→010, 111→011, 110→001. Represent it as a host-side 8-entry relabelling LUT in the
  label encoder, as `qpsk_p1_on_q` is. Equivalently, if the block's mapper is a loadable
  8-entry I/Q table (§2.5), conjugation is just a different table. No HDL control bit is
  needed.

## 2. Word format of the TX DMA (question 2)

### 2.1 How the DMA is drained today (evidence)

- Stock DAC path (maia-hdl `projects/common/xilinx_ad9361.tcl:249-286`): `axi_dmac`
  (`axi_ad9361_dac_dma`) → `util_upack2` → `axi_ad9361_dac_fifo` → `axi_ad9361`.
  - `upack/fifo_rd_en` is driven by `dac_fifo/din_valid_0`, so the DAC pulls at its sample
    rate.
  - `upack/fifo_rd_underflow` → `dac_fifo/din_unf` → `axi_ad9361/dac_dunf`.
- **Precedent for draining at a lower rate:** Tezuka's TX ×8 interpolator
  (`projects/common/txfir.tcl:1-33`, enabled for plutoskyr2 in
  `projects/tezuka/system_bd.tcl:70-75`) disconnects that path and drives
  `util_ad9361_dac_upack/fifo_rd_en` from `tx_fir_interpolator/valid_out_0`. The DMA then
  runs at fs/8.
  - So a TX symbol block that asserts `fifo_rd_en` once per 5 symbols draws exactly 11.54 MB/s.
  - The kernel and iiod follow consumption; nothing on the host sets the rate.
  - **[confirm]** placement against `tx_fir_interpolator`; see §2.6.
- `isdb-iio`: `start_tx` (`crates/isdb-iio/src/lib.rs:588-614`) → `open_stream`
  (`:501-585`). The non-raw size check is `size != 2 * channels.len()` (`:536`), so **one
  channel (`voltage0`) is accepted** and gives one `i16` per sample slot. `TxStream.sender`
  is `SyncSender<Vec<i16>>` (`:267-273`); packed `u16` words go in by bit cast, and
  `tx_loop` writes `x.to_le()` (`:908-913`).
  - **[confirm on the board]** that `cf_axi_dds` accepts a scan mask with `voltage0` alone.
    Tezuka's RX does this on `cf-ad9361-lpc`; the TX side has not been tried.
  - Fallback if it does not: enable `voltage0`+`voltage1` and let the block read 32 bits per
    `fifo_rd_en` (two words, 10 labels).

### 2.2 Proposed packing (format "TX 1")

```
bit 15      14..12   11..9    8..6     5..3     2..0
 V=1       label4   label3   label2   label1   label0     (label0 first in time)
```

- **Unpack in the HDL:** a mod-5 counter and a 5:1 mux of 3 bits; the read strobe fires when
  the counter wraps. The labels are LSB-first, like the RX payload's bit order
  (`crates/isdb-frontend/src/format.rs:22-25`).
- **V (bit 15):** 1 = five labels follow; 0 = idle, no symbols.
  - Zero padding is always idle. That covers `tx_loop`'s zero-filled underrun buffers
    (`isdb-iio/src/lib.rs:919`), the `kernel_buffers` zero buffers it pushes at the end
    (`:935-945`), and the zeros upack outputs on underflow **[confirm: util_upack2 data on
    underflow]**.
  - On an idle or absent word the block feeds zero symbols into its pulse filter, so the
    output decays to silence instead of sending label-000 carrier bursts.
- **Partial last word:** pad with label 000 and V = 1. These up to 4 extra +1 symbols come
  after the null flush frame and are harmless.
- **Remainder across calls:** 319 488 mod 5 = 3 and 39 936 mod 5 = 1, so the packer carries
  the remainder across superframes, as `format::Packer` carries bits. The block needs no
  word alignment.
- **Alternative considered:** 16 labels per 3 words (exactly 3 bits per symbol,
  10.82 MB/s, and 2 496 groups per frame) needs a 48-bit register, and leaves no spare bit
  to tell padding from data. The 0.7 MB/s it saves does not matter at these margins.
  Rejected.
- **Alternative considered:** 4 labels per word (14.43 MB/s) is a trivial unpack but has no
  real advantage over 5. Rejected.

### 2.3 Rate

- Per word: 28.86 M / 5 = 5.772 Mwords/s = **11.544 MB/s** (raw label bits 10.82 MB/s;
  overhead 6.7 %).
- Per superframe: 63 897.6 words = 127.8 kB per 11.07 ms.
- Buffers: the default TX buffer of 262 144 words (`pluto.rs:122`) holds 45.4 ms. The
  4 kernel buffers (`KERNEL_BUFFERS`, `pluto.rs:124`) hold 182 ms.
  - The RX side needed ≥ 8 buffers to ride out a ≈ 200 ms BBR PROBE_RTT pause of the
    *board's* TCP send (VERIFICATION.md "Losses and drift over minutes", plan §0.18).
  - For TX the host is the TCP sender (its own congestion control), so that pause should not
    apply. Still, offer `kbuffers` for TX too, with a default of 8 (363 ms).
    **[confirm by a long run]**

### 2.4 Loss and sync: what the TX boundary needs, compared with RX

- **RX needs a self-checking format.** The device-side loss (an ADC/DMA overflow) happens
  upstream of the host. The overflow bit (0x80000088 bit 2 on `cf-ad9361-lpc`) is sticky
  and has no position (VERIFICATION.md "The loss detector", ≈ line 634). The host therefore
  needs sync words, sequence numbers and a CRC-32C to find what is missing
  (`format.rs:1-80`, `Reader` → `Event::Loss`). `symstream` closes the contiguous stretch
  at a loss and restarts (`crates/isdb-cli/src/symstream.rs:1-15`).
- **TX needs none of that.** The host is the source; the DMA is lossless and in order.
  - The only failure is an underrun. The DAC then gets idle samples for a while and the
    stream resumes with the next word.
  - No symbol is dropped or reordered. The rest of the transmission arrives shifted in time
    (a slip). A receiver's frame sync re-acquires on W1/W2/W3, and the superframe around the
    gap is likely lost, because the interleaver spreads the damage.
  - The FPGA needs no frame structure, so a frame-start marker would only help to locate a
    slip, and a counter does that better. **Decision: a plain stream** with:
    1. V-bit idle words (above);
    2. **CTRL run/start/select** through the GPIO command plane (§4.4): START resets the
       phase accumulator, the filter delay line and the counters;
    3. **block counters** readable with READ:
       - `CONSUMED`: data words since START;
       - `MISSED`: word slots where `fifo_rd_valid` was low *after the first data word*,
         that is, DMA starvation; padding before the stream starts does not count, like
         `tx_loop`'s `started` (`isdb-iio/src/lib.rs:862`);
       - `FIRST_MISS_AT`: `CONSUMED` latched at the first miss, which locates the slip
         (÷ 63 897.6 gives the superframe);
       - a sticky STATUS bit.
- **How the host learns of an underrun, three layers:**
  1. Host starvation: the libiio stream thread found its queue empty and pushed a
     zero-padded buffer. Counted in `isdb_iio::Counters::underruns`
     (`isdb-iio/src/lib.rs:254-265`, incremented at `:920-929`) and shown by
     `QueuedSink::status`/`finish` (`crates/isdb-radio/src/queued.rs:214-285`). Unchanged.
  2. Device starvation: iiod or the network did not refill the DMA in time.
     - ADI `up_dac_common` REG 0x22 = `{27'd0, if_busy, 3'd0, up_status_unf}`
       (`adi-hdl/library/common/up_dac_common.v:483`), with bit 0 write-1-to-clear
       (`:380-386`), fed by `dac_dunf`.
     - Debug address: **0x80000088 on `cf-ad9361-dds-core-lpc`, bit 0.**
     - Discrepancy **[confirm]**: Tezuka's `api_controller.sh:516-519` tests `& 4` on the
       same address. ADI's source says bit 0; read it on the board while starving a TX
       stream.
     - The block's `MISSED` counter makes this bit redundant for the symbol path; keep
       reading it for the IQ path.
  3. On-air check: the receiver's discontinuity report (§5).

### 2.5 Mapper and level (for the HDL agent; host consequences)

- **Mapper:** label → (I, Q) through an 8-entry table.
  - Fixed table 46 with known coefficient words is enough for ISDB-S.
  - A host-loaded table (8 × 2 words) makes `conjugate` and other relabellings free and lets
    a 5-bit variant serve ISDB-S3 later (33.76 Mbaud × 5 bits ≈ 21 MB/s). Out of scope here.
- **Level:** `TxOptions::backoff_db` (`crates/isdb-radio/src/lib.rs:217-228`) becomes a gain
  register of the block, not a `Quantizer` scale. `describe_tx` must say so instead of
  printing `Quantizer::<Cs12>::describe_level` (`pluto.rs:759-762`).

### 2.6 Interaction with Tezuka's DAC GPIO bits (new finding)

On plutoskyr2, `up_dac_gpio_out` is already wired:

- **bit 0**: the ×8 TX FIR interpolator's `active` (`txfir.tcl:25-29`). This stays in the
  isdb image, whose 66 kept DSPs include "20 the TX FIR interpolator"
  (`fpga/README.md:93-97`).
- **bits 1–3**: the DVB-S2 reset, FIR select and FIR config-valid (`dvb.tcl:112-117,
  192-205`). These are free in the isdb image, which drops `dvb` (`fpga/tezuka/apply.sh`).
- `up_dac_gpio_in`: no use found, so it presumably reads 0 on stock images and discovery
  is safe. **[confirm with one read on stock Tezuka]**

Consequences:

- The TX command word must leave bit 0 alone. `control::Encoder` already keeps bit 0
  (`crates/isdb-frontend/src/control.rs:151-174`), so it can be reused as is.
- The block must own `upack/fifo_rd_en` while selected and sit downstream of (or bypass)
  the interpolator **[HDL agent]**.
- The host should refuse the symbol path when bit 0 is set, unless the HDL makes it
  irrelevant.

## 3. Link budget (question 3)

| Stream | MB/s | Status |
|---|---|---|
| ISDB-S labels, 5/word (proposal) | **11.54** | — |
| ISDB-S labels, 16 per 3 words | 10.82 | — |
| ISDB-T cs12 at 512/63 MHz, TX | 32.5 | streams: TV test 8.1 s, 0 underruns (VERIFICATION.md ≈ line 352-362); "Gigabit Ethernet carries 512/63 MHz (32.5 MB/s) in one direction" (`docs/transmitting.md:78`) |
| ISDB-S 8-bit I/Q at 1 sample/symbol | 57.7 | over every measured TX figure |
| ISDB-S cs12 at 40 Msps (today's `--radio` path) | 160 | underruns (`docs/plans/isdbs-loopback.md` §2) |
| RX ceilings (board → host) | 63–66 (iiod) / ≈ 70–72 (services stopped) | VERIFICATION.md table ≈ line 599-611; plan §0.3 |

- **Margin.** 11.54 / 32.5 = 0.36 of the proven TX rate (2.8× headroom), and about 16 % of
  the RX-direction ceiling.
  - No TX-direction *ceiling* has been measured; 32.5 MB/s is only a point known to work.
    The board's CPU, not the cable, limits RX (no TSO on the GEM, `maxmtu` 1518), and the
    receive path costs different CPU.
  - **[measure]** Step cs12 TX at 4/6/8/10 Msps (16–40 MB/s) into `loopback=on` and count
    underruns.
- **Why labels and not 8-bit I/Q.**
  - 8-bit I/Q is 5.3× the data and carries no more information (the point set has 8 members).
  - It would sit above the only proven TX rate.
  - With the 43.4 MB/s RX stream of the self-test (§5) it would load the board's CPU with
    101 MB/s, past the ≈ 70 MB/s it moves in one direction.
  - The block needs the RRC interpolator either way.
  - With labels, TX + RX is 11.5 + 43.4 = 55 MB/s, inside the single-direction ceiling.

## 4. Host-side changes, by crate (question 4)

### 4.1 `isdb-frontend` (CPU reference and boundary; the RX counterpart lives here)

- `txformat.rs`, the label words:
  - `pub const LABELS_PER_WORD: usize = 5`, `pub const VALID: u16 = 0x8000`;
  - `LabelPacker { pending: [u8; 4], n: usize }` with `push(&[u8], &mut Vec<u16>)` and
    `finish(&mut Vec<u16>)` (pads with 000);
  - `unpack(&[u16]) -> Vec<u8>` (idle words skipped), the model of the HDL's unpacker;
  - unit tests (§6).
- `txparams.rs`: `TxDesign { sample_rate, symbol_rate, roll_off, taps, backoff_db }` →
  `TxParams`, the integer registers:
  - `step`: symbol phase per output sample, like `Params::nco_step`/`period_nominal`;
  - `gain`, `taps`, the polyphase RRC bank, and the 8-entry mapper table if loadable;
  - `validate()`.
  - Design it from the rate **read back** from `tx_path_rates`, as the RX `Params` is
    designed from the rate the device runs at.
- `txfixed.rs`: the **bit-exact fixed-point model** of the block (labels → 12-bit DAC
  words), CPU reference first per plan §7.2.
  - Today `shape::Shaper` is f32 (`isdb-frontend/README.md:203-216`), so a fixed variant
    has to be written and frozen before any HDL. It is checked against `Shaper`
    (MER ≥ 40 dB through an ideal matched filter, like `shaped_signal_is_isi_free`).
  - It doubles as the `--offload cpu` path for files (`--iq`, labels → IQ through the same
    arithmetic).
- `control.rs`, a `tx` submodule:
  - `TX_ID_MAGIC`: the value is the HDL agent's call; a magic distinct from `0x15DB`
    guards against a mis-wired readback;
  - `TxIdentity { version, taps }` with `parse`, `word` and `supports(&TxParams)`;
  - `tx::reg::{STEP, GAIN, TAPS…, STATUS, CONSUMED, MISSED, FIRST_MISS_AT, H_BASE, MAP_BASE}`;
  - `tx::ctrl::{RUN, START, SELECT}` (SELECT = the block drives the DAC instead of the DMA IQ);
  - `Encoder::configure_tx(&TxParams)`.
  - `Encoder`, `Op` and the toggle/HI/WRITE_S scheme are reused unchanged
    (`control.rs:88-241`). The command word layout is the same; only the register pair
    differs:
    - `TX_GPIO_OUT_REG` = 0x800000BC and `TX_GPIO_IN_REG` = 0x800000B8 **on
      `cf-ad9361-dds-core-lpc`**;
    - derivation: `up_dac_common` COMMON_ID 6'h10 (`up_dac_common.v:49`, not overridden in
      `axi_ad9361_tx.v:368-378`), with words 0x2E/0x2F = gpio_in/out (`up_dac_common.v:486-487`);
    - the DDS core is mapped at its own base (`cf-ad9361-dds-core-lpc@79024000`,
      `reg = <0x79024000 0x1000>`, Tezuka dts), so the debugfs offsets are ×4 from that
      base, with bit 31 selecting the HDL core;
    - consistent with the existing `R1_MODE_REG` 0x80000044 read on `TX_DEV`
      (`pluto.rs:168, 900-907`) and Tezuka's 0x80000088 TX-underflow read;
    - **[confirm]** with one read on the board.
- `vectors.rs`: TX vectors (labels in, words in, every output sample out) for `fpga/`.

### 4.2 `isdb-radio`

- `Offload` (`lib.rs:363-374`, `#[non_exhaustive]`): add `TxSymbols { version: u16, taps: usize }`.
- Discovery. `offloads()` (`lib.rs:458-461`; Pluto `pluto.rs:1006-1016`) parses **capture**
  arguments (`rx_settings_for(cfg, Dir::Capture)`). A TX config with `loopback=on` or
  `att=` would be refused by `device_args` (`pluto.rs:374-385`) and `check_gain_names`.
  Add one of:
  - `fn tx_offloads(&self, cfg) -> Result<Vec<Offload>>` (default empty), which on the
    Pluto parses `tx_settings` and reads `TX_GPIO_IN_REG` on `TX_DEV` (one register read,
    nothing written, like `probe_front_end` at `pluto.rs:1193-1199`); or
  - a `Direction` parameter on `offloads`.
- Sink trait:
  ```rust
  pub trait SymbolSink {
      fn write(&mut self, labels: &[u8], stop: &AtomicBool) -> Result<()>;
      fn status(&mut self) -> String { String::new() }
      fn finish(&mut self, aborted: bool) -> Result<String>;
      fn realtime(&self) -> bool;
  }
  ```
  It is a separate trait rather than an enum: `IqSink` (`lib.rs:131-145`) stays untouched
  for files, HackRF and Maia.
- Backend methods, with defaults returning `Unsupported`:
  - `fn describe_tx_symbols(&self, cfg, opts, p: &TxParams) -> Result<String>`;
  - `fn open_tx_symbols(&self, cfg, opts, p: &TxParams) -> Result<(Box<dyn SymbolSink>, String)>`.
  - A missing or wrong block is an error like `RadioError::FrontEnd { found, need }`
    (`pluto.rs:1031-1044`); add a `Modulator` variant, or a `Direction` field on that error.
- `QueuedSink` reuse (`queued.rs:78-212`). Today it hard-codes `Quantizer<R::Format>`
  converting `C32`. Generalise it over a converter:
  ```rust
  pub trait Encode { type In; type Component: Copy + Send + 'static;
      fn encode(&mut self, x: &[Self::In], out: &mut Vec<Self::Component>); fn describe(&self) -> String; }
  ```
  - `Quantizer<F>` implements it for `C32`; `LabelPacker` implements it for `u8 → i16`
    (bit-cast `u16`).
  - `QueuedSink<R, E>` implements `IqSink` when `E::In = C32` and `SymbolSink` when
    `E::In = u8`.
  - Prefill, back-pressure, stall detection, drain, the stop flag and underrun counting
    (`queued.rs:131-199, 233-285`) are reused unchanged, as are their mock tests
    (`crate::mock::MockRadio`).
  - `RadioTx::Format: IntFormat` becomes `type Component`.
  - The packer must flush its partial word in `finish` before the queue is dropped.
- Pluto backend (`pluto.rs`):
  - **`PlutoTxSymbols`** (or a mode of `PlutoTx`, `pluto.rs:1632-1751`): `channels =
    ["voltage0"]` (TX1 only; `channel=2` refused unless the HDL wires TX2), and `configure`
    as today. Keep its order:
    1. maximum attenuation;
    2. LO powerdown with `loopback`, DDS off (`:1655-1659`), rate, bandwidth, LO, loopback;
    3. probe `TxIdentity` and check `supports(p)`; refuse r1_mode (as `:900-912`) and DAC
       gpio bit 0 set (§2.6);
    4. `Encoder::new(read TX_GPIO_OUT)` → `configure_tx` → read back one register (as kp
       at `:1062-1071`) → CTRL START → CTRL RUN|SELECT;
    5. **read STATUS and require "selected"**: label words sent to a plain DAC would go
       out as I samples;
    6. attenuation last.
  - `stop`, `safe_state` and `Drop` (`:1693-1715`) add CTRL 0 (back to DMA IQ) before the
    attenuation and LO steps. RX does the same with "back to IQ samples, whatever
    happened" (`:1104-1108`).
  - `status()` appends the block's `MISSED`, read with READ about once per status line.
    Each READ costs 2–3 register accesses at 0.62 ms; do not do it per buffer.
  - The summary gives `CONSUMED`, `MISSED` and `FIRST_MISS_AT`.
  - Configuration time: if the RRC bank is host-loaded, 64 phases × 32 taps ≈ 2 048
    writes ≈ 1.3 s at 0.62 ms (the measured cost, `fpga/README.md:119-130`). A ROM in the
    bitstream makes it near zero.
  - `kbuffers` accepted for TX (`Dir::Tx` keys, `pluto.rs:374-376`).
- README and docs: the TX boundary section next to "The `Symbols` boundary".

### 4.3 `isdb-iio`

- No API change is needed. `start_tx(TX_DEV, &["voltage0"], buffer, kbuffers, depth)`
  passes the 2-byte check (`lib.rs:536`).
- Optional: a `u16` sender alias.
- **[confirm on the board]** the driver accepts the single-channel scan mask (§2.1).
- `tx_loop`'s zero padding and drain (`:919-945`) give V = 0 idle words, which is what the
  format wants.

### 4.4 Configuration plane

- Reuse `control::Encoder` and its command words on the DAC core's GPIO pair. This needs no
  kernel or device-tree change, as on RX (`fpga/README.md:119-136`).
- RX and TX blocks have separate GPIO pairs, so two processes can drive one each without
  interfering.

### 4.5 `isdbs-phy`

- `Transmitter::superframe_labels` / `null_superframe_labels` returning a
  `SuperframeLabels { labels: Vec<u8>, placed }`. Labels are produced natively:
  - `frame_symbols` → `frame_labels` (BPSK bit b → `b << 2`);
  - `encode_frame_with_state` → a label variant (`qpsk_label`, TC8PSK label, the `Mapping`
    LUTs of §1.2–1.3).
- Keep the `Cplx` API as `psk8_point(label)` over the labels, so both paths share one code
  path and stay bit-identical by construction.
- Refuse the Dummy-with-slots edge case (§1.3).
- Test: `psk8_point(labels[k]) ≈ symbols[k]` (1e-12) on every combination mix.

### 4.6 `isdb-cli` (`txs.rs`, `tx.rs`)

- **Option.** `--offload auto|fpga|cpu` on `tx` with `--system isdbs`; reuse
  `symbols::OffloadArg` (`crates/isdb-cli/src/symbols.rs:40-48`).
  - Default `auto`: FPGA when `tx_offloads` reports a compatible block, else today's IQ
    path with its underrun warning.
  - `fpga` without a block is an error, like `fpga_taps` (`symstream.rs:418-441`).
  - Add `"offload"` to `TX_ISDBS_ONLY` (`txs.rs:143`).
- **Where the paths diverge.** `run` builds `Shaper` (`txs.rs:990`); `spawn` (`:637-778`)
  pushes symbols through it:
  - pre-roll `:678-684`, lead `:685-712`, data `:714-748`, flush `:750-773` with
    `shaper.finish`;
  - change: `spawn` takes a `Modulate::{Shaped(Shaper), Labels}`. With `Labels` it calls
    `superframe_labels` and puts the `Vec<u8>` in `ProducedSf` (a `Payload::{Samples, Labels}`
    field);
  - the pre-roll (last frame of a null superframe), lead and flush null frame stay
    identical; `shaper.finish` is dropped (the block's filter tail drains on the idle words);
  - `to_c32` (`:624-632`) is not used on this path.
- **Driving.** `drive` (`:818-938`) takes `&mut dyn IqSink`. Make it generic over a small
  `Out` trait (or enum) implemented by both sinks. `rep.samples` becomes "units", and
  on-air seconds = units / fs or symbols / R.
- **The radio warning.** `:1031-1043` stays only for the IQ path. The FPGA path logs the
  link rate (11.54 MB/s) instead.
- **Sink opening.** In `tx.rs` `open_sink` (`:446-503`), after `radio(spec)`, decide the
  offload. FPGA path: `TxParams::design` → `describe_tx_symbols` printed → **the same gate**
  (`:473-478`, `--rf-enable`/`--dry-run` unchanged) → `open_tx_symbols`. Return an enum
  `TxSink { Iq(Box<dyn IqSink>), Symbols(Box<dyn SymbolSink>) }`. ISDB-T and `--iq` keep
  `Iq`.
- **Dry run and probing.** `--dry-run` promises not to open the device (`tx.rs:105-109`),
  but probing opens it.
  - `--offload fpga --dry-run`: describe the FPGA path without probing.
  - `--offload auto --dry-run`: describe both and state that the choice is made at open.
  - With `--rf-enable`: probe (read-only), then describe and open.
- **`describe_tx` output for the symbol path:** device and tuning as today; `TX path FPGA
  modulator format N, T taps, RRC α 0.35 at fs (x.xxx samples/symbol)`; `link labels 5/word,
  11.54 MB/s, buffers …, kbuffers …`; `level backoff X dB (block gain register G)`;
  `loopback …` as today.
- **Pacing.** For radio sinks the producer is not paced (`default_pace`). Back-pressure on
  the queued sink paces it, at 5.77 Mwords/s instead of fs samples/s.
  - Margin today: 9.8 ms per 11.07 ms superframe = 1.13× with the shaper. The shaper alone
    runs at 1.7× real time (≈ 6.5 ms), which leaves the transmitter about 3.3 ms
    (VERIFICATION.md "Speed" ≈ line 1167; plan §0.22).
  - Without the shaper the label path should run at **≈ 3.3× real time**. It also skips the
    f64 → f32 symbol copy, and packing (63 898 words) is well under a millisecond.
    **[measure]** before claiming it.
  - Enough with a release build. A dev build was 0.8× with the shaper, so check it too.
  - Ways to raise it: generate labels instead of `Cplx` (no 16-byte complex per symbol);
    run superframe n+1's transmitter while n is packed; keep `par::map` over the frames
    (`tx.rs:322-325`, already parallel).
  - `--prefill-frames 2` (22 ms) and `--queue-frames 4` stay; the device holds another
    180–360 ms.
- **Safety.** Gate, `--freq-unrestricted` for the BS IF, and TX attenuation 89.75 dB by
  default: all unchanged. With `loopback=on` nothing radiates (LO powered down,
  `pluto.rs:23-27, 1653`).

## 5. On-board self-test (question 5)

- **Two processes, nothing new needed.** `isdb tx --system isdbs --radio
  pluto:uri=…,loopback=on --offload fpga --rf-enable --rate 40M …` and `isdb rx --system
  isdbs --radio pluto:uri=… --offload fpga --channel BS1 …`.
  - Each process has its own libiio context and iiod connection.
  - Each block has its own GPIO pair (DAC and ADC cores).
  - `set_rate` writes `sampling_frequency` only when it differs by more than 1 Hz
    (`pluto.rs:1502-1505`, with a comment about glitching the other direction), so the RX
    does not disturb the running TX when both ask for 40 Msps. The AD9361 couples the TX and
    RX clock chains, so **both must request the same rate**.
  - The RX acquires on a short IQ capture first (trim; `symstream` acquisition), which also
    works in loopback.
  - `configure_rx` already reports "loopback ON" (`pluto.rs:1625-1627`).
- **One process (a later `isdb loopback --radio`)** can share one `Context`, which supports
  concurrent streams: `streams: Vec<Stream>` (`isdb-iio/src/lib.rs:380-384`) and
  `add_stream` (`:733-760`), each thread using only its own buffer (`:33-40`). The
  problems:
  - `Context::stop()` stops **all** streams (`:824-834`), and `stream_words` calls
    `ctx.stop()` at its end (`pluto.rs:1310`);
  - `is_streaming()` is "any" (`:763-767`);
  - `Context` is `!Sync`, so attribute calls (status READs on both cores) must come from one
    thread.
  - So either open two contexts (simplest), or add per-stream stop handles and stream ids to
    `isdb-iio`. Recommendation: two contexts.
- **Link load.** 11.54 MB/s host → board plus 43.4 MB/s board → host. Gigabit Ethernet is
  full duplex, so the wire is not the limit; the board's CPU is.
  - iiod took ≈ 72 % of a core at 50.8 MB/s RX (VERIFICATION.md §0.18 notes).
  - TX at 11.5 MB/s adds a fraction of a second core of the dual Cortex-A9.
    **[measure]** iiod CPU with both streams running (`tools/pluto-sample.sh`).
- **Loopback fidelity [confirm].** It is not yet known whether the AD9361 BIST loopback
  passes the TX/RX digital filters, so do not assume sample-transparent loops. The test
  below checks at the TS level, plus hard-decision symbol errors.

## 6. Tests and verification ladder (question 6)

1. **Host unit tests (`isdb-frontend::txformat`):**
   - pack/unpack round trip for any length and any chunking (proptest, as
     `prop_chunking_is_invisible`);
   - bit layout spec test (`[0,1,2,3,4]` → `0x8000|4<<12|3<<9|2<<6|1<<3|0`);
   - idle words (0x0000) skipped;
   - a partial final word padded with 000 and V = 1;
   - rate constant: 11.544 MB/s at 28.86 Mbaud.
2. **Label generation (`isdbs-phy`):**
   - `psk8_point(label) ≈ symbol` on every frame of the five combination mixes and the
     `Mapping` alternatives;
   - the QPSK closed form of §1.2;
   - the conjugate LUT equals `z̄`;
   - the dummy-with-slots refusal;
   - `superframe_labels` → points → the existing symbol-domain loopback decodes byte-exact.
3. **Sink tests (`isdb-radio`, `MockRadio`):** `QueuedSink<_, LabelPacker>` prefill,
   back-pressure, drain and underrun counting (the existing `queued.rs` tests made generic);
   Pluto argument tests (`loopback`/`kbuffers` on TX, `channel=2` refused for symbols).
4. **Fixed model (`txfixed`):**
   - against the float `Shaper` (MER ≥ 40 dB after an ideal matched filter);
   - through `FastFrontend` at 40 Msps (≥ 25 dB, as the shaper's test);
   - the sample-domain loopback (`isdb loopback --system isdbs`) with the fixed modulator:
     byte-exact.
5. **Vectors for `fpga/`** (`cargo run -p isdb-frontend --example vectors`, a TX mode):
   - TxParams and register words (also as `gpio_out` command words, like `--via-gpio`), the
     word stream including idle words and a starvation pattern;
   - every output (I, Q) per clock, plus `CONSUMED`/`MISSED`/`FIRST_MISS_AT`;
   - the HDL simulation must match bit for bit; one LSB off in a tap must fail (as for RX).
6. **CLI tests (`crates/isdb-cli/tests/cli.rs`):**
   - `tx --system isdbs --offload fpga --dry-run` prints the FPGA description;
   - `--offload fpga` with `--iq` is refused, or means the fixed model (decide which);
   - `--offload` refused with `--system isdbt`.
7. **Opt-in hardware test.** Use a **new** variable, e.g.
   `ISDB_RS_PLUTO_TX_LOOPBACK_URI`: `ISDB_RS_PLUTO_URI` is documented "receive only"
   (`docs/test-data.md:31`), and this test transmits, albeit into `loopback=on` with the LO
   off. It runs `isdb tx --system isdbs --radio pluto:uri=$U,loopback=on --offload fpga
   --rf-enable --ts 0x4010:24=a.ts --ts 0x4011:24=b.ts --seconds 5` alongside `isdb rx
   --system isdbs --radio pluto:uri=$U --offload fpga --out 'o-{ts_id}.ts' --strip-nulls
   --json`, and asserts:
   - every output TS equals its input file, byte-exact, aligned at the first non-null
     packet as `VerifierS` does;
   - 0 uncorrectable RS words;
   - TX summary: 0 host underruns, `MISSED` = 0;
   - RX: 0 losses;
   - also hard-decision label errors = 0 on the decoded frames, which is stronger than
     RS-clean.
   A variant starves the producer on purpose (e.g. `--realtime` with a throttled input) and
   checks that `FIRST_MISS_AT` points into the superframe the receiver reports as lost.
8. **Board measurements to record in VERIFICATION.md:**
   - the TX ceiling (§3);
   - single-channel TX scan mask;
   - DAC REG 0x22 bit position;
   - `up_dac_gpio_in` reading 0 on stock Tezuka;
   - 60 s TX+RX loopback load.

## 7. Open items, and what settles each

| Item | Settled by |
|---|---|
| `voltage0`-only TX buffer accepted by `cf_axi_dds` | one `start_tx` with `["voltage0"]` on the board (else the 2-channel, 32-bit-per-read fallback) |
| DAC debug addresses 0x800000B8/BC, 0x80000088 bit 0 vs Tezuka's `&4` | register reads on `cf-ad9361-dds-core-lpc` (stock image: gpio_in = 0, gpio_out bit 0 = TX FIR) |
| block placement and `fifo_rd_en` ownership vs `tx_fir_interpolator`; upack data on underflow | HDL agent (`txfir.tcl`, `util_upack2`) |
| TX-direction throughput ceiling | cs12 TX rate steps into `loopback=on` |
| AD9361 BIST loopback transparency (filters in or out of the loop) | ADI ad9361 driver / register 0x3F5 docs; or compare RX IQ to the TX model |
| label producer speed (≈ 3.3× real time claimed) | `isdb tx --system isdbs --offload fpga` against a mock or `--iq` labels timing |
| block configuration time (ROM vs host-loaded RRC bank) | HDL agent's choice; host cost 0.62 ms per write |
