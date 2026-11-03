# isdbs-next items 5 and 6: HDL follow-ups and housekeeping (investigation, 2026-10-30)

Read-only investigation of [isdbs-next.md](../isdbs-next.md) "Work" 5 (HDL
follow-ups in `fpga/isdb_fpga/tx.py`) and 6 (housekeeping). Nothing was
built, deployed or deleted; the board was not touched. Run: the FPGA
suite (simulation only), a mise templating check in the scratchpad, and
`uv run --locked` without the index override (read-only, it refuses).
Line numbers are at `main` = `371e72b`.

## Summary

| Item | Finding | Proposal |
|---|---|---|
| 5a start miss | The block requests nothing until RUN; its first two requests go out at RUN+4 and RUN+5 clocks; an underflow answer becomes 8 zero symbols in MISSED and starts the datapath; the third request waits for the unpack register to drain (≈ 11 clocks), so a start-only miss is 1 or 2 words, 8 or 16 symbols | prefetch while selected: poll the upack from SELECT on, drop underflow answers before RUN, hold up to 2 words; STATUS bit 3 PRIMED; clear held words on deselect. Model: `prefetch` (stalls before word k are not seen). Test: MISSED 0 when the block reports the feature |
| 5b bank reload | "bank" = banks a/b of coefficients in the 12 RAMB18 (BRAM init = default bank). The host always writes both (13 315 `gpio_out` commands, one libiio register write over iiod each): measured 3.9 s (VERIFICATION.md), docs say ≈ 8 s (0.62 ms estimate) | BANK_TAG register: init = tag of the default bank, cleared by any bank write, written by the host after a load, `reset_less`; host skips the bank when the tag matches (≈ 10 commands). Faster full reload: DMA load mode (optional) |
| 5c CONSUMED | 32 bits; wraps every 148.8 s at 28.86 Mbaud (SAMPLES 107.4 s at 40 Msps). Host shows it in the summary only; FIRST_MISS has the same wrap | host-only 64-bit extension works on today's image (needs a read ≤ ~100 s apart); add 64-bit counters + one HI register in the rebuild that 5a needs anyway |
| 5d cost / ID | +≈ 150–250 LUT, +≈ 200–260 FF, 0 DSP, 0 BRAM; no timing risk in the block's domain (divclk worst +2.325 ns at 16 ns, an RX path); image WNS +0.100 is the RGMII input, WHS +0.033 is `rx_clk` | keep the ID word 0xB5D11918 (arithmetic unchanged); add a FEATURES register (0x07) that the old image reads as 0 |
| 5e suite | 24/24 OK in 224 s (`UV_DEFAULT_INDEX=https://pypi.org/simple mise run fpga:test`, 230 s with the vectors); `uv.lock` untouched | — |
| 6f uv index | confirmed: without the override `uv run --locked` fails ("lockfile … needs to be updated"); build/apply scripts set it, mise tasks do not | per-task `env` on the six uv tasks (exact text below) |
| 6g branches | all 7 `worktree-agent-*` are ancestors of `main`, 0 commits ahead, no worktree registered, `.claude/worktrees/` empty | safe to `git branch -d` all 7 |
| 6h fpga/README.md | 10 stale passages (RX-only image as deployed, "not yet built/run", `--frozen`, per-clock slacks of the RX-only build, "starts without lead-in misses") | replacements below |
| 6i isdb-s.md §2 | L910 "directly, no attenuator" | replacement below; also §0.23 "Open" L876–879 |
| 6j other stale | README.md L423–424, receiving.md L115, transmitting.md L603–607 / L637–639, test-data.md L32, CHANGELOG L94–95, isdb-radio README L172, pluto.rs L99 / L106, cli.rs L1129, isdbs-fpga-tx.md L151 / L194–196 / L270–276 | listed below |

## 5a. The start miss

### Mechanism, from the HDL

`fpga/isdb_fpga/tx.py`:

- `active = run & selected` (L236–237). The DMA side runs only while
  active: `arm` counts from 0 when `active` rises (L252), and
  `rd_en = active & armed & (held < 2)` (L253), `ARM = 4` (L103). So
  **selected but not running, the block requests nothing**; the first
  request is RUN + 4 clocks (after the `gpio_out` synchroniser's
  2 clocks, L529–536).
- `held = (ucnt != 0) + svalid + pend` (L250): with nothing held, the
  block sends two requests back to back (RUN+4, RUN+5). util_upack2
  answers each one clock later (registered, `util_upack2_impl.v`
  `fifo_rd_valid <= s_axis_valid & ~reset_data`, `fifo_rd_underflow <=
  ~(…)`); it consumes nothing on an underflow (`s_axis_ready = ready &
  s_axis_valid & fifo_rd_en`), so an underflow is a non-destructive
  "no word yet".
- An underflow answer is taken as a *missing word*: `rmiss = ~rd_valid`
  (L257), `ureg ← 0, ucnt ← 8, umiss ← 1` (L310–311). `ready = primed |
  (ucnt != 0)` (L262) is then true, so the datapath starts on the miss,
  and each of its 8 zero symbols enters as MISSED with FIRST_MISS ←
  CONSUMED = 0 (L294–302).
- The third request goes out only when the first word's 8 codes have
  entered and the staging word moves in (`take`, L276–313): ≈ 8 × 1.386
  ≈ 11 enabled clocks later. So **a start-only miss is bounded by the
  block at two words: 8 or 16 symbols, FIRST_MISS 0** — exactly what the
  board shows (VERIFICATION.md L1700 "MISSED 8, FIRST_MISS 0", L1750
  "8–16 symbols at start"), and what `tx_symbols_loopback_on_a_pluto`
  tolerates (`crates/isdb-cli/tests/cli.rs` L1248–1262: `missed <= 16 &&
  (missed == 0 || FIRST_MISS 0)`).

So the miss means `s_axis_valid` (axi_dmac `m_axis_valid`) was low at
RUN+4 / RUN+5. What the HDL does **not** explain is why: in this design
axi_dmac presents its first beat without a read
(`axi_dmac_burst_memory.v`: `dest_ready = ~dest_mem_data_valid | …`
prefetches beat 0; `dest_axi_stream.v` L106 `m_axis_valid = fifo_valid &
active & has_sync`, `SYNC_TRANSFER_START` 0 in `xilinx_ad9361.tcl`
L249–256), the upack's `enable` is static during a stream
(`axi_ad9361_tx_channel.v` L272: `dac_data_sel == DMA`), and the host
waits for 8 pushed buffers (580 ms) before RUN (`pluto.rs`
`wait_buffers`, L2136–2172). A DMA that started ms earlier should be
valid at RUN+4; a DMA that started late would miss far more than two
words. The measured 1–2 words (and 0 in the 5 s synthetic run, L1686)
suggest a short read-related window, not a late DMA. **Open**: the root
cause; the design below removes the miss either way, and a diagnostic
counter (below) would name it on the next image.

### Design: prefetch while selected

Poll from SELECT, not from RUN; before RUN keep only real words.

| Signal / place | Today | Proposed |
|---|---|---|
| L252 `arm` | counts while `active` | counts while `selected` (still ARM = 4 clocks after SELECT, so a pass-through answer in flight is never taken) |
| L253 `rd_en` | `active & armed & (held < 2)` | `selected & armed & (held < 2)` |
| L255 `resp` | `(rd_valid \| rd_underflow) & (pend != 0)` | `… & selected` (answers after a deselect are the pass-through's) |
| new | — | `drop = resp & ~rd_valid & ~run`: an underflow before RUN stores nothing and counts nothing (`pend` still decrements); `keep = resp & ~drop` |
| L310–313 | loads on `resp` | loads `ureg` / `stg` on `keep` |
| new, on `~selected` | held words survive CTRL 0 (a later START\|SELECT keeps a full stale word: L465 keeps `ucnt == 8`) | `ucnt, umiss, svalid, pend, primed ← 0` (a new stream never plays the last one's words) |
| STATUS (L582) | `Cat(run, selected, underrun)` | bit 3 PRIMED = `(ucnt != 0) & ~umiss` (a delivered word held) |
| optional diag | — | 16-bit PRE_UNF: underflow answers dropped before RUN (sticky, START clears); read once on the new image to settle the root cause |

Behaviour: START|SELECT at open polls one request per clock (two in
flight) while the DMA is idle; once the stream's first beats arrive the
block holds word 0 in `ureg` and word 1 in `stg` and stops (held = 2).
RUN then starts the datapath on the first `req` with a word in hand:
MISSED 0. If RUN comes before any word (DMA late), the post-RUN
underflow answers are kept as missing words as today, so a real late
start is still counted. START keeps full words (L465, unchanged), so
START|SELECT|RUN sequences and START-while-running keep their meaning.
The pre-RUN underflows still reach the DAC core's underflow flag through
`din_unf`; the host clears it after RUN as today (`TxBlock::run`).

### Bit-exact model

The counters are "a function of the sample count alone" (tx.py L76–77)
only after RUN. The model has no RUN; its stall `(word_index, n)` means n
requests for word i find nothing (`shape/fixed.rs` L72–84,
`Interpolator::request` L815–850). With a pre-RUN poll, stalls on the
words the block holds at RUN are not seen. Minimal change:

- `crates/isdb-frontend/src/shape/fixed.rs`: doc of "# Input" (L72–84):
  "Selected and not running the block requests words and drops requests
  that find none; at RUN it holds up to two. `Interpolator::prefetched(k)`
  (k ≤ 2, after `start`, before the first sample) drops the stalls due
  before word k." Implementation: `self.stalls.retain(|&(i, _)| i >= k)`
  limited to indices < k (stalls are sorted by index).
- `crates/isdb-frontend/src/vectors.rs` (L93–161): a `prefetch` field
  (0 for the existing five sets, so their JSON and the 24 tests stay as
  they are); `examples/txvectors.rs`: a sixth set `tx-6-prefetch` with
  stalls at words 0 and 1 and one later (prefetch 2: MISSED only from
  the later one).
- `fpga/isdb_fpga/txmodel.py` (`Dma` L97–118, `run` L121–): the same
  `prefetch` argument.
- `crates/isdb-frontend/src/control.rs` `tx` (L278–400): `reg::FEATURES
  = 0x07`, `reg::BANK_TAG = 0x08`, `reg::HI = 0x09`; `status::PRIMED =
  8`; `features::{PREFETCH = 1, BANK_TAG = 2, COUNTERS64 = 4}`; the
  start-sequence doc on `configure_tx` (L393–402): "once STATUS reads
  PRIMED, CTRL RUN|SELECT".

### Tests

FPGA (`fpga/tests/test_isdb_fpga.py`, `fpga/isdb_fpga/sim.py`; today 24):

1. prefetch before RUN: START|SELECT, the bench's DMA dry for N polls
   (a bench-level "not before clock T", outside the vectors' stall list),
   then words, RUN after PRIMED: bit-exact on `tx-1-continuous`, MISSED 0.
2. `tx-6-prefetch` with `prefetch: 2` (the polls consume the stalls on
   words 0 and 1; `simulate_tx` gains a pre-RUN phase when the doc says so).
3. DMA later than RUN: the post-RUN underflows counted, equal to the
   Python model with `stalls = [(0, n)]`, n the underflow answers the
   bench saw after RUN.
4. CTRL 0 mid-stream, then START|SELECT: the first codes are the DMA's
   next words, none of the old ones.
5. STATUS PRIMED and FEATURES through `gpio_in` (`simulate_tx_top`).

The existing vectors set RUN, SELECT and START on one clock
(`simulate_tx` L466–470) and the gpio list ends CTRL START, CTRL
RUN|SELECT, so there is no pre-RUN phase and they keep passing.

Host (`crates/isdb-radio/src/backends/pluto.rs`): `TxBlock::open` reads
FEATURES (L1977–2031); `TxBlock::run` (L2063–2082) waits, when PREFETCH,
for STATUS & PRIMED (poll, timeout) after the half-queue wait, before
CTRL RUN|SELECT; the open info line names the features. Mock
(`MockDac`, L3197–3300): FEATURES / PRIMED / BANK_TAG / HI, and a
FEATURES-0 old image.

Hardware test (`crates/isdb-cli/tests/cli.rs` L1248–1262):

```rust
let prefetch = tx_log.contains("prefetch"); // the open line's feature list
assert!(
    if prefetch { missed == 0 } else { missed <= 16 && (missed == 0 || tx_log.contains("FIRST_MISS 0,")) },
    "the block missed symbols:\n{tx_log}"
);
```

and its doc (L1120–1129) back to "MISSED 0" for an image with the
feature.

## 5b. The bank reload

- **What it is.** Two coefficient banks, a (even k) and b = a/√2 (odd
  k), P = 512 rows × 2S = 24 taps, signed 18-bit; one true-dual-port
  RAMB18 (1024 × 18) per tap pair holds a and b for both mirrored taps:
  12 RAMB18 (tx.py L26–31, L336–364). The BRAM init is the default bank
  (ISDB-S, 40 Msps, 10 dB), taken from the Rust model's
  `tx-1-continuous.json` by `mise run fpga:tx-verilog` (mise.toml L149–153).
- **How the host loads it.** `Encoder::configure_tx` (control.rs
  L404–415): ADDR + STEP + SHIFT, then for each of 2 × 512 rows one ADDR
  and 12 WRITE_S (the near half; the far taps are the mirror) = 13 315
  words. `TxBlock::configure` (pluto.rs L2006–2009) writes each as one
  `reg_write` of `cf-ad9361-dds-core-lpc` 0x800000BC, i.e. one libiio
  network round trip to iiod and a debugfs `direct_reg_access` write —
  not `/dev/mem`. It runs on every open, default bank or not.
- **How long.** VERIFICATION.md L1685: "configured in 3.9 s (13 315
  commands)" = 0.29 ms each. The "≈ 8 s" in isdbs-next.md, pluto.rs L99,
  isdb-radio README L172, transmitting.md L603 and test-data.md L32 is
  the 0.62 ms/command estimate of the RX front end; the self-test with a
  receiver probing the same iiod may be slower. Either way 4–8 s per open.
- **Why the host cannot skip it today.** The BRAM keeps whatever was
  last loaded until the FPGA is reconfigured; a previous session may
  have loaded a 61.44 Msps bank, and nothing reads the banks back
  (READ of a bank address returns 0: L591–592).

### Design: BANK_TAG

- Register 0x08, 32 bits, **`reset_less`** (only the bitstream sets its
  initial value: the block's `rst` does not reset the BRAM, so it must
  not reset the tag either). Init = `bank_tag(default bank)`.
- Any bank write (`core.c_we`, L574) sets it to 0; WRITE to 0x08 (HI +
  WRITE) sets it; READ returns it.
- `bank_tag(p)` = CRC-32C (`isdb_frontend::format::crc32c_bytes`,
  format.rs L154) over the coefficients exactly as `configure_tx` writes
  them (a then b, row by row, taps 0..S, each as i32 LE), 0 mapped to 1.
  Python computes the same for the init (`synth/verilog.py` →
  `SymbolsTx`); the vectors carry it; the sim checks the readback.
- Host (FEATURES & BANK_TAG): read 0x08; equal to `bank_tag(p)` → write
  STEP, SHIFT only (≈ 10 commands, a few ms); else the full
  `configure_tx`, then write the tag. An interrupted load leaves 0, so
  the next open reloads. Old image: FEATURES 0 → always load (today).

### Faster full reload (only for other rates/backoffs)

| Option | Gain | Cost |
|---|---|---|
| drop the per-row ADDR (a load pointer that steps t = 0..S−1 then the next row) | −1 024 of 13 315 commands (−8 %) | ~20 LUT |
| two coefficients per command | none: the argument is 27 bits, a coefficient 18; b = a/√2 is rounded in f64 by the model, not derivable in the fabric | — |
| several contexts / threads | none: the toggle protocol needs order; iiod serialises register writes | — |
| **load through the TX DMA**: CTRL bit 3 LOAD; selected + LOAD, each DMA sample (32 bits) is one coefficient at the next near-half address; 12 288 samples = 48 KB, < 10 ms on the link | 4–8 s → ≈ 0.1 s (one buffer create/push/destroy) | ~50 LUT + 14-bit counter in HDL; the host prepends a raw-word buffer before the codes; verification by BANK_TAG written after, or a hardware CRC (~100 LUT) |

Recommendation: BANK_TAG in the next rebuild (it covers the bench's
only rate, 40 Msps); DMA load only if other rates become routine.

## 5c. CONSUMED

- Widths (tx.py L218–221): CONSUMED, MISSED, FIRST_MISS, SAMPLES are
  `Signal(32)`; the readback is 32 bits (L583–590).
- Wrap: 2³² / 28.86 × 10⁶ = **148.8 s** (CONSUMED, and FIRST_MISS,
  which is a CONSUMED snapshot); SAMPLES 2³² / 40 × 10⁶ = 107.4 s (69.9 s
  at 61.44 Msps). The 25-min bench run wrapped CONSUMED about 10 times.
- Host use (`pluto.rs`): `TxCounters` (L1920–1955) "low 32 bits", read
  once in `summary()` (L2404–2434) and printed; status lines read only
  MISSED and take `wrapping_sub` (L2085–2098, wrap-safe). Nothing
  compares CONSUMED with the host's count. The plan's starvation test
  (FIRST_MISS → superframe, 319 488 symbols each) needs FIRST_MISS
  absolute: ambiguous past 148.8 s (13 443 superframes).

Host-only wrap tolerance: in `TxBlock`, read CONSUMED with each MISSED
read (+3 commands, ≈ 1–2 ms per status line) and extend to u64 with
`wrapping_sub` deltas; FIRST_MISS64 = C64 − ((C32 − FM32) mod 2³²) at
the read where UNDERRUN first shows. Correct while reads are < 148.8 s
apart. Status lines are every 5 s by default but optional
(`--status-secs 0`, isdb-cli tx.rs L141–143; txs.rs L1035), so the sink
needs its own poll (≤ 100 s) through a `RadioTx` hook, not only
`status()`. **Sufficient**, and it works on today's image.

Hardware 64-bit, in the same rebuild as 5a: CONSUMED, FIRST_MISS and
SAMPLES 64-bit (MISSED stays 32: 148 s of missing symbols); one HI
register 0x09 latched by a READ of any of the three (`readback ← x[:32],
hi ← x[32:]`), so LO then HI is an atomic snapshot. Host: HI when
FEATURES & COUNTERS64, else the host-only extension.

## 5d. Cost, timing, register map and compatibility

Last build (VERIFICATION.md L1656–1657; `/path/to/sdr/isdb-work/out/isdb/
final_timing_isdb_util.rpt`): `isdb_symbols_tx` 2 331 LUT (core 2 150,
control 178), 2 672 FF, 12 RAMB18, 0 DSP; image 23 470 LUT (44 %),
158 DSP.

| Change | LUT | FF |
|---|---|---|
| prefetch (arm on selected, drop, deselect clear, PRIMED) | ~15–25 | ~2 |
| PRE_UNF diag (optional) | ~10 | 16 |
| FEATURES (constant) + readback mux inputs | ~10–20 | 0 |
| BANK_TAG | ~35–45 | 32 |
| 64-bit CONSUMED / FIRST_MISS / SAMPLES + HI | ~80–130 | ~128 |
| **total** | **≈ 150–250** | **≈ 180–200** |

0 DSP, 0 BRAM: 158 DSP unchanged, LUT ≈ 23.6–23.7k (45 %).

Timing (`final_timing.rpt`, the post-route-physopt checkpoint): the
block's domain `clk_div_sel_0_s` is constrained at 16 ns (62.5 MHz) and
its worst path is +2.325 ns inside the **RX** block
(`isdb_symbols/inst/core/q_rp__addr_reg` → `t_acc_reg`, rpt L12266);
no TX path in the 10 worst. A 64-bit counter is a 16-CARRY4 chain,
≈ 3 ns. The image's WNS +0.100 ns is `system_sys_rgmii_0_rgmii_rx_clk →
RGMII_rxc` (I/O, Inter Clock Table rpt L225) and its WHS +0.033 ns is
`rx_clk` (axi_ad9361): neither involves the TX block, but every build
re-places them, so read the final checkpoint with
`fpga/tezuka/timing.tcl` as before (isdbs-next.md "Pitfalls").

Register map / ID: the ID word 0xB5D11918 (`id_word`, tx.py L121–124;
`TxIdentity`, control.rs L338–399) is magic 0xB5D1, format version 1
("the arithmetic and the code format"), log2 P 9, 24 taps; every bit is
used. `TxIdentity::supports` requires the exact version, so a bump
would make the current host refuse the new image. None of 5a–5c
changes the samples, so **no bump**. Compatibility through a FEATURES
register (0x07, read-only) instead: the old image's READ of an unknown
address returns 0 (`m.Default()`, L591–592), so

| | old image | new image |
|---|---|---|
| old host | today | prefetch makes MISSED 0 (its sequence START\|SELECT → stream → RUN\|SELECT is what the prefetch wants); it reloads the bank, the tag goes 0; counters' low words as today |
| new host | FEATURES 0: full load, half-queue wait, ≤ 16-symbol tolerance, host-side counter extension | PRIMED wait, bank skipped on a matching tag, HI words |

Card: deploy as a new file (e.g. `isdb_txrx2_top.bin`), keep
`isdb_txrx_top.bin` and a `uEnv.txt.prev-txrx` for rollback.

## 5e. Running the suite and the build

```sh
UV_DEFAULT_INDEX=https://pypi.org/simple mise run fpga:test
```

Result 2026-10-30: vectors regenerated (`fpga:vectors`, `fpga:tx-vectors`),
**Ran 24 tests in 223.8 s, OK**, mise total 229.8 s; `git status` clean
(`uv.lock` not rewritten). Directly: `cd fpga && UV_DEFAULT_INDEX=…
uv run --locked python -m unittest discover -s tests -v` (needs the
vectors in `fpga/build`).

Rebuild (not run): `VIVADO_SETTINGS=/path/to/Xilinx/2026.1/…/settings64.sh
fpga/tezuka/build.sh /path/to/sdr/isdb-work isdb` (both blocks, the deployed
kind; `isdb-tx` is the TX-only image), ≈ 25 min; then `fpga/tezuka/
sdbin.sh WORK/out/isdb/plutoskyr2.xsa NAME.bin`, `vivado -mode batch
-source fpga/tezuka/timing.tcl -tclargs …/system_top_postroute_physopt.dcp`.
`build.sh` (L59–60) and `apply.sh` (L43–46) already set
`UV_DEFAULT_INDEX`.

## 6f. `UV_DEFAULT_INDEX` in the mise tasks

Verified: without it, `cd fpga && uv run --locked …` exits with "The
lockfile at `uv.lock` needs to be updated, but `--locked` was provided"
(the user's `~/.config/uv/uv.toml` makes the private `takumi-guard`
index the default; `fpga/uv.lock` records `registry =
"https://pypi.org/simple"`). The six tasks that call uv (mise.toml
L129–165) do not set it. mise task `env` is not passed to `depends`
tasks (mise docs, "task-configuration: env"), so each uv task carries
its own. Checked with mise 2026.10.21 in a scratch project: the
`get_env` default applies when unset and an exported value wins.

Exact change: add this line to each of `[tasks."fpga:tx-vectors"]`
(L129), `[tasks."fpga:test"]` (L137), `[tasks."fpga:verilog"]` (L143),
`[tasks."fpga:tx-verilog"]` (L149), `[tasks."fpga:tx-estimate"]` (L155),
`[tasks."fpga:estimate"]` (L161), after `dir = "fpga"`:

```toml
env = { UV_DEFAULT_INDEX = "{{ get_env(name='UV_DEFAULT_INDEX', default='https://pypi.org/simple') }}" }
```

and extend the section comment (L115):

```toml
# --- FPGA (fpga/: Amaranth HDL, built outside cargo; needs uv) ------------
# uv tasks resolve against PyPI, which fpga/uv.lock records, unless
# UV_DEFAULT_INDEX is set (a user-level default index would otherwise make
# `uv run --locked` ask for a relock); fpga/tezuka/*.sh do the same.
```

(A top-level `[env]` would also work but exports the variable to every
command run under mise in the repository.) The pre-commit hook runs
`mise lock` on a `mise.toml` change. Then fpga/README.md L83–84
"(… On a host where `uv run --locked` asks for a relock, run the steps
with `uv run --frozen`.)" becomes "(… The tasks set `UV_DEFAULT_INDEX`
to PyPI, the index `uv.lock` records, unless it is already set.)".

## 6g. `worktree-agent-*` branches

`git worktree list`: only `/path/to/isdb-rs 371e72b [main]`;
`git worktree prune --dry-run -v`: nothing; `.claude/worktrees/` empty.
All seven are in `git branch --merged main` and `git log main..BRANCH`
is empty:

| Branch | Tip | Merged by |
|---|---|---|
| `worktree-agent-a0e9ac5233fe191dd` | `46ee395` chore: FPGA transmit interpolator study, "throwaway (not for merge)" | `3b75a64` (it was merged anyway; isdbs-fpga-tx.md cites its tools) |
| `worktree-agent-a53b7c5c3b78b5adc` | `9def892` fix(isdb-radio): pluto IQ open sends CTRL 0 … | `fc57057` |
| `worktree-agent-a58f1097116386769` | `eb85197` fix(isdb-cli): tx --system isdbs creates the data file first | ancestor of main |
| `worktree-agent-a91235ebd260d09db` | `b816702` feat(isdb-frontend): shape::Shaper | ancestor of main |
| `worktree-agent-aa37575e5b774c600` | `70a44e9` feat(isdbs-phy): tx | ancestor of main |
| `worktree-agent-af3d1a043ceda798d` | `e1ca69a` feat(fpga): SymbolsTx follows the Rust model's decisions | `2924566` |
| `worktree-agent-af8ecd1f1051d1287` | `b85bcb3` feat(isdbs-phy): transmitter codes from the bits | ancestor of main |

None has an upstream. **All seven are safe to delete**: `git branch -d
worktree-agent-a0e9ac5233fe191dd …` (`-d` refuses an unmerged branch).
Not in scope, left alone: `feat/pluto-backend` (merged, tracks origin,
1 ahead of it) and `backup/pluto-backend-before-scrub` (164 commits not
in main: keep).

## 6h. fpga/README.md

| Lines | Now | Replace with |
|---|---|---|
| 83–84 | "On a host where `uv run --locked` asks for a relock, run the steps with `uv run --frozen`." | see 6f |
| 215–227 | "…so a configuration is STEP, SHIFT and CTRL; another rate or backoff reloads the bank (… 12 288 writes, 7.6 s through iiod). `configure_tx` ends with CTRL START, CTRL RUN\|SELECT. … then CTRL RUN\|SELECT starts without lead-in misses." | "The default bank (…) is the memories' initial content (…). The host writes both banks on every open (`Encoder::configure_tx`: STEP, SHIFT, the near half of each row, 13 315 commands, 3.9 s through iiod on the bench), since the memories keep the last bank loaded until the FPGA is reconfigured. The Python model's encoder (`txmodel.py`) ends its list with CTRL START, CTRL RUN\|SELECT; the host instead sends CTRL START\|SELECT (selected, not running: the block requests nothing and sends zeros), pushes half its kernel queue, then CTRL RUN\|SELECT. The first two requests go out 4 clocks after RUN; on the board one or both find no word (8–16 symbols in MISSED at FIRST_MISS 0, in the lead-in; VERIFICATION.md "on the board"). CTRL 0 makes the block transparent again." (and after the rebuild: the prefetch, PRIMED, BANK_TAG) |
| 306–309 | `sdbin.sh WORK/out/isdb/plutoskyr2.xsa isdb_top.bin` | `… plutoskyr2.xsa isdb_txrx_top.bin   # both blocks (the deployed image)` |
| 345–355 | "**Status (2026-10-26): the format-4 image … `isdb_top.bin` is ready to deploy. Not yet run on the board.** The image with the transmit block (2026-10-29) is not built yet (plan step 6). Expected, …" | "**Status (2026-10-29): the image with both blocks, `isdb_txrx_top.bin`, is built, meets timing and runs on the bench board** (`/boot/uEnv.txt` `bitstream_image=isdb_txrx_top.bin`; VERIFICATION.md "ISDB-S transmitter in the FPGA": the image, on the board, into the PIX-DT295). The receive-only format-4 image `isdb_top.bin` (2026-10-26) ran on the board before it (isdb-s.md §0.16) and stays on the card. Expected before the build, from Tezuka's TX FIR figures and the Yosys estimate: 158 DSP, ≈ 23.8k LUT, ≈ 30.0k FF, 68 BRAM36; measured: 158, 23 470, 29 994, 68." |
| 366–375 | per-clock slacks of the RX-only build (`clk_div_sel_0_s` +1.523 … `rx_clk` +0.445), reports `v4b_timing_summary.rpt`, `v4b_utilization*.rpt` | "Per clock in the final checkpoint of the image with both blocks (all met): `clk_div_sel_0_s` (the blocks' clock, constrained at 62.5 MHz; it runs at 40 MHz) +2.325 ns (an RX-block path); `clk_div_sel_1_s` (125 MHz, r1_mode only; the blocks' paths there are false) +0.821 ns; `clk_fpga_0` +0.970 ns; maia-sdr's `clk_out3` +0.779 ns; `rx_clk` +0.542 ns. The overall WNS +0.100 ns is the RGMII input (`RGMII_rxc`), the same path class as the stock build. Reports: `WORK/out/isdb/final_timing.rpt`, `final_timing_isdb_util.rpt` (from `timing.tcl`). The RX-only build's figures were `clk_div_sel_0_s` +1.523 … (`v4b_*.rpt`, no longer in `out/`)." |
| 394 | "…resources (the table above). Not yet run on the board." | "…resources (the table above). Deployed on the bench board 2026-10-29 (VERIFICATION.md "on the board")." |
| 423–434 | Deploying: copy `isdb_top.bin`, `bitstream_image=isdb_top.bin`, rollback `system_top.bin` | "1. copy `isdb_txrx_top.bin` (both blocks; or `isdb_top.bin`, receive only) … 2. … set `bitstream_image=isdb_txrx_top.bin` (keep the previous `uEnv.txt` as `uEnv.txt.prev-*`) …" Rollback: "`bitstream_image=isdb_top.bin` (receive only) or `system_top.bin` (stock), or the original card." |
| 439 | "**Verifying after deployment** (receive only):" | "**Verifying the receive block after deployment** (either image):" |
| 453–459 | "(plan §4.3; … the host side is plan step 4): 1. … 0xB5D11918 (the stock and the receive-only bitstreams read 0)" | "(plan §4.3, done 2026-10-29, VERIFICATION.md "on the board"; …): 1. … 0xB5D11918 (the stock bitstream and `isdb_top.bin` read 0)" |
| 463–464 | "0 host underruns, MISSED = 0, 0 RX losses" | "0 host underruns, MISSED ≤ 16 at FIRST_MISS 0 (the start-only miss; 0 once the prefetch is in the image), 0 RX losses" |

## 6i. docs/plans/isdb-s.md

§2 (L906–911):

```text
| TX1 | **PIX-DT295 (ISDB-T/S USB tuner), directly, no attenuator** |
```

→

```text
| TX1 | PIX-DT295 (ISDB-T/S USB tuner, `06b8:103b`) through a **30 dB fixed attenuator and a DC block** (since 2026-10-29; VERIFICATION.md "ISDB-S transmitter in the FPGA: into the PIX-DT295"). Transmit at attenuation ≥ 45 dB unless measuring the margin |
```

and L913 "**Before any TX test on TX1** (all three are required):" →
"**Before any TX test on TX1** (all three are required; 1 and 2 are in
place on the bench since 2026-10-29, keep them):". The address line
L928 (`ip:<PLUTO_IP>`) is the documentation placeholder for the bench
board's address, not to be committed as a real
identity — leave it.

§0.23 (L862–879): L865 "2.7k LUT" (Yosys) → "2 331 LUT placed"; L868
"Image expectation 158 DSP." → "Image: 158 DSP, timing met
(VERIFICATION.md "the image")."; L876–879 "**Open:** build the image
(Vivado, not on this host), deploy on a card copy, run
`tx_symbols_loopback_on_a_pluto`; skip the 8 s bank reload …; the
starvation variant of the test; CONSUMED wraps after 149 s." →
"**Done 2026-10-29:** image built (158 DSP, WNS +0.100 / WHS +0.033 ns),
deployed (`isdb_txrx_top.bin`), the loopback self-test passes byte-exact
(MISSED 8 at FIRST_MISS 0), the PIX-DT295 locks on the conducted bench
broadcast. **Open** (isdbs-next.md 5): the start-only miss (prefetch),
the bank reload on every open (BANK_TAG), CONSUMED wraps after 149 s;
the starvation variant of the test."

## 6j. Other stale text (relative to isdbs-next.md "State")

| File:line | Stale | Fix |
|---|---|---|
| README.md L423–424 | "radio transmit block … designed, modelled and simulated bit-exact (bitstream and on-board test next)" | "… built into the bitstream and verified on the board (loopback, conducted into a PIX-DT295)" |
| docs/receiving.md L115 | "a PlutoSky R2 with the `isdb_top.bin` bitstream" | "with `isdb_top.bin` or `isdb_txrx_top.bin`" |
| docs/transmitting.md L603–607 | "(13 315 commands, about 8 s through iiod) … so a clean run misses no symbol" | "(13 315 commands, 4–8 s through iiod) … so the run misses at most the block's first one or two words (8–16 zero symbols in the lead-in, FIRST_MISS 0)" |
| docs/transmitting.md L637–639, docs/test-data.md L32 | "0 host underruns and MISSED 0 in the transmitter's summary"; "≈ 8 s of it the transmit block's configuration" | "MISSED at most 16, at FIRST_MISS 0"; "4–8 s" |
| crates/isdb-cli/tests/cli.rs L1129 | doc "MISSED 0" | "MISSED ≤ 16 at FIRST_MISS 0" (the assertion, L1248–1262, already says so) |
| crates/isdb-radio/src/backends/pluto.rs L99, L106 | "≈ 8 s through iiod"; "so a clean run misses no symbol" | "4–8 s"; "a clean run misses at most the first one or two words" |
| crates/isdb-radio/README.md L172 | "≈ 8 s through iiod" | "4–8 s" |
| CHANGELOG.md L94–95 ([Unreleased]) | "MISSED 0, 0 RX losses (not yet run: it needs the image with the transmit block)" | "MISSED ≤ 16 at the start, 0 RX losses; passes on the PlutoSky R2 (VERIFICATION.md)" |
| docs/plans/isdbs-fpga-tx.md L151 | "the on-board self-test is written … and waits for the image" | "… passes on the board (§4)" |
| docs/plans/isdbs-fpga-tx.md L194–196 | step 6 "Image: … (expect 178 DSP, ≈ 23.7k LUT, +6 BRAM36)" not marked done | "(done 2026-10-29: 158 DSP, 23 470 LUT, 68 BRAM36, timing met; deployed)" |
| docs/plans/isdbs-fpga-tx.md L270–276 | §6 risks "Timing at 40 MHz is unverified", "loopback's transparency decides…", "link ceiling has not been measured" | settled: divclk +2.325 ns at 62.5 MHz; sample-exact; clean to 64 MB/s (VERIFICATION.md board checks) |
| docs/plans/isdbs-next.md L12 | "`main` at `082ae96`" | `371e72b` is the plan's own commit; harmless |

## Open questions

1. Root cause of `s_axis_valid` low at RUN+4/+5 despite 8 buffers
   pushed (the HDL says the DMA's first beat is presented without a
   read). The PRE_UNF counter on the next image would show whether the
   DMA is idle until RUN or the first reads fail. The prefetch design
   does not depend on the answer.
2. Whether the self-test's configuration takes 4 s or 8 s under a
   receiver's load (VERIFICATION.md has 3.9 s for a lone transmitter);
   one timed run would settle the docs' figure.
3. DMA bank load: only if rates other than 40 Msps matter.
4. Whether to keep the plain host-side counter extension once HI exists
   (needed for the deployed image only).
