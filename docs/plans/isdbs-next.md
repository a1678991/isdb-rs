# Plan: ISDB-S after the FPGA transmitter (hand-off, 2026-10-29)

Written at the end of the session that took ISDB-S from "received only"
to a transmitter running in the PlutoSky R2's FPGA and locked by a real
tuner. Everything below "State" is measured or committed; everything
under "Work" is open. Conventions: [CONTRIBUTING.md](../../CONTRIBUTING.md)
(spec-first, SPEC-NOTES, conventional commits, no real station
identities, nothing radiated without `--rf-enable` on a conducted bench).

## State

**Repository** (`main` at `082ae96`, clean, workspace clippy / tests /
rustdoc clean, FPGA suite 24/24):

- ISDB-S transmit on the CPU: `isdb tx --system isdbs` (files + SigMF
  sidecar, `isdb rx` reads them back byte-exact), `isdb loopback
  --system isdbs` ([isdbs-loopback.md](isdbs-loopback.md); isdb-s.md §0.22).
- ISDB-S transmit in the FPGA ([isdbs-fpga-tx.md](isdbs-fpga-tx.md), the
  three reports under `isdbs-fpga-tx/`; isdb-s.md §0.23): 4-bit symbol
  codes over the DMA (14.4 MB/s), `SymbolsTx` in `fpga/isdb_fpga/tx.py`
  (0 DSP48E1, 2 331 LUT, 12 RAMB18), bit-exact model
  `isdb_frontend::shape::fixed`, `control::tx`, host path
  `isdb-radio` (`Offload::TxSymbols`, `SymbolSink`, Pluto symbols mode),
  `isdb tx --offload auto|fpga|cpu`.
- Verified (VERIFICATION.md, the four "ISDB-S transmitter in the FPGA"
  sections): board checks; the image (158 DSP, WNS +0.100 / WHS +0.033 ns
  in the final checkpoint); on the board through the AD9361 digital
  loopback (two TSs byte-exact, 0 losses, MER 34.1 dB; the opt-in test
  `tx_symbols_loopback_on_a_pluto` under `ISDB_RS_PLUTO_TX_LOOPBACK_URI`
  passes); conducted into the PIX-DT295 (locks at the first tune, clean
  TS with the multiplexer's SI).
- Tools: `tools/isdbs-bench-tx.sh` (the test broadcast),
  `crates/isdb-radio/examples/txrate.rs` (TX link probe),
  `tools/loopback-tone-check.py`, `fpga/tezuka/timing.tcl` (a
  checkpoint's real slack), `fpga/tezuka/build.sh WORK isdb|isdb-tx`.

**Bench** (as left):

- PlutoSky R2 at `ip:10.10.0.30` (the docs' 192.0.2.30 is a placeholder),
  booted from SD with `/boot/uEnv.txt` `bitstream_image=isdb_txrx2_top.bin`
  (since 2026-10-31: both blocks plus the transmit block's prefetch /
  BANK_TAG / 64-bit counters; `uEnv.txt.prev-txrx` = the `isdb_txrx_top.bin`
  entry, `uEnv.txt.prev-isdb` = the RX-only entry; `isdb_txrx_top.bin`,
  `isdb_top.bin` and `system_top.bin` still on the card). TX ID word
  0xB5D11918 on `cf-ad9361-dds-core-lpc` 0x800000B8, RX 0x15DB4020 on
  `cf-ad9361-lpc`. Vivado 2026.1 at `/opt/Xilinx/2026.1`; the build work
  tree is `~/dev/sdr/isdb-work` (`out/isdb/plutoskyr2.xsa`,
  `isdb_txrx_top.bin`, reports); upstream sources at `~/dev/sdr/`.
- **2026-10-31: TX1 was disconnected from the PIX-DT295** (the user's
  word) and reconnected the same day for the bench acceptance of items 1
  and 2 (VERIFICATION.md "ISDB-S PCR-paced placement and BS SI into the
  PIX-DT295"). Unconnected, `dt295 tune` reads 0xC3 = 50, `Searching`.
- The DT295's raw output (`stream --raw-config`) is not the TS on air: its
  PID filter drops the CAT (0x0001) without a null, and the PMT/PCR PIDs
  until it has learnt them from the PAT/PMT, and it rewrites the CC of
  adaptation-only packets. `tools/isdbs-bench-check.sh --placed` accounts
  for it (`--receiver dt295`, the default).
- TX1 → 30 dB attenuator → DC block → PIX-DT295 (USB `06b8:103b`, hub
  port `1-2.1`), driven by `~/dev/PIX-DT295` (`target/release/dt295`:
  `init [--auto]`, `tune --isdbs bs1 --tsid 4017`, `status --isdbs
  --watch N`, `stream --isdbs bs1 --tsid 4017 --raw-config --out F
  --seconds N`; STATE 0xFFFF needs `tools/power-cycle.sh 1-2 1 3` then a
  cold `init`). TS_ID 4017 is the BS bench TS of `isdbs-bench-tx.sh`
  since bs-si.md piece D; its `--terrestrial-si` run (the 2026-10-29
  broadcast) is TS_ID 4010.
- **The test broadcast may still be on air** when the next session
  starts: `pgrep -af "isdb tx --system isdbs"`; stop with `kill PID`
  (the FIFO writer `pv` ends with it; the Pluto closes to attenuation
  89.75 dB, TX LO down). Restart: `tools/isdbs-bench-tx.sh WORKDIR 45
  ip:10.10.0.30` (WORKDIR holds `bs-bench.toml`, `bs60.ts`, `ts.fifo`;
  a WORKDIR of an earlier run also has the terrestrial `bench60.ts`,
  which the script now sends only with `--terrestrial-si`).
- Board hygiene: `tools/pluto-autorun.sh` kills Tezuka's daemons at boot
  (their `/dev/mem` writes would flip the command protocol's toggle bit;
  one takes a bus fault on the DVB registers the isdb image lacks). The
  boot's "Tuning TX FAILED" (no `cf-ad9361-lpc`) happens 2 boots in 3;
  the unbind/rebind recovery of isdb-s.md §2 works after a few attempts.
- ssh: `sshpass -e ssh -o UserKnownHostsFile=/dev/null
  -o StrictHostKeyChecking=no root@10.10.0.30` (`SSHPASS=analog`; the
  user's known_hosts has a stale key for this address); `scp -O` (no
  SFTP server on the board).

## Work, in priority order

1. **PCR-paced placement in the ISDB-S producer** (`crates/isdb-cli/src/
   txs.rs`). **Status 2026-10-30: implemented and verified offline,
   bench acceptance 2026-10-31 (below).** P1–P6 of
   [pcr-pacing.md](isdbs-next/pcr-pacing.md) §9 are on `main`
   (`fad5559..4d105ea`): `--ts-pacing auto|pcr|arrival`, `--placed-ts`,
   loop seams joined (PCR/PTS/DTS shift, CC renumbering), live inputs
   paced with a deadline, `isdb ts validate --expect-rate`. Through
   `--iq` → `isdb rx` (`tools/isdbs-pacing-e2e.sh`, FEED=file|loop|live):
   52.170000 Mbit/s, CBR-fit max 19 ns / rms 11 ns, 0 CC errors and 0 PCR
   discontinuities across loop seams, received TS byte-exact with
   `--placed-ts`, no live deadline miss. **Bench acceptance done
   2026-10-31** (VERIFICATION.md "ISDB-S PCR-paced placement and BS SI
   into the PIX-DT295"): `tools/isdbs-bench-tx.sh --feed loop --placed …`
   at 45 dB into the PIX-DT295, locked at the first tune; all 344 PCR
   values of a 20 s recording are in the placed TS, which the recording
   equals byte for byte; PCR rate 52.170000 Mbit/s, CBR fit max 19 ns /
   rms 11 ns with the receiver's drops re-inserted. Caveat: the DT295's
   raw output drops the CAT (and PMT/PCR until it has learnt their PIDs)
   without a null and rewrites the CC of adaptation-only packets, so its
   index-based fit reads ±14.4 µs and validate a missing CAT;
   `tools/isdbs-bench-check.sh --placed` (default `--receiver dt295`)
   accounts for that. Original problem: a live feed's packets are placed in arrival order,
   one superframe at a time from the reader queue: the PIX-DT295
   recording shows PAT/PMT intervals of 0.17–0.21 s and 12–30 ms rms PCR
   jitter against H.222.0's ±500 ns (VERIFICATION.md "into the
   PIX-DT295"). Reuse the `Pacer` of the ISDB-T `--mux-ts` path
   (`input.rs`): each packet goes into the slot whose on-air time (the
   superframe's 8 × 48 slots at 52.17 Mbit/s, 11.07 ms per superframe)
   matches its PCR schedule, nulls elsewhere; file inputs with a PCR get
   it by default (`--feed file|loop`), stdin/FIFO too when PCRs are
   present; handle the loop seam (`--feed loop`: shift PCR/PTS/DTS by a
   constant as the mux's `ts-program` relay does, or re-stamp the
   continuity counters). Acceptance: `dt295 stream` recording
   `isdb ts validate` clean of timing findings, PCR CBR-fit residual
   ≤ a few µs; the CPU (`--iq`) and FPGA paths identical.
2. **Satellite-shaped SI in the multiplexer** (`isdbt-mux`, `isdb-si`).
   Live check 2026-10-31 (BS1 on RX1, 10 s): NIT actual network_id
   0x0004, system_management_id 0x0201, 26 TSs, each TS loop 0x41 then
   0x43 and no 0xCD; BS1's 0x43 `011727481100e802886008` is what
   `tools/bs-nit-section.py` writes; the placeholders 0x4017 / service
   0x03E7 are not on air. Design: [bs-si.md](isdbs-next/bs-si.md).
   `[network] kind = "bs"` (or a satellite section): NIT with the
   satellite delivery system descriptor (B10 §6.2.6; `isdb-si`
   `descriptor/dvb.rs` has it), BS `network_id` 0x0004 /
   `original_network_id`, TS information descriptor as BS uses it, the
   service list, a BS-style TS_ID (0x40xx) and service_ids; SDT/BIT
   details per B10 Part 2 for BS; keep terrestrial behaviour unchanged.
   Acceptance: `isdb ts validate` clean; the PIX-DT295's software (or
   `dt295`'s SI readout) lists the bench service as a BS channel.
   Status: pieces A–D merged; offline end to end clean (VERIFICATION.md
   "ISDB-S BS-shaped SI, offline"). **Bench acceptance done 2026-10-31**
   (bs-si.md §6.4 item 7, with item 1's run): the PIX-DT295 tuned with
   `--tsid 4017` locked at the first tune and received NIT network_id
   0x0004 "ベンチ" with TS 0x4017 / onid 0x0004 / satellite 11727.48 MHz
   110.0°E circular-right, SDT service 0x03E7 "ベンチHD", no `expect` or
   `cross.*` finding (VERIFICATION.md "ISDB-S PCR-paced placement and BS
   SI into the PIX-DT295"). The CAT is sent but the DT295's raw output
   filters it out (same caveat as item 1). Left: mirakc / the tuner
   software's channel list.
3. **Media in the bench service and the tuner's decoder**: with
   `ISDB_RS_MEDIA_DIR` (docs/test-data.md) run `tools/isdbs-bench-tx.sh`
   with `bench.toml`'s media streams (the script uses `bench-minimal.toml`
   with declared streams; add a media mode), record with `dt295 stream
   --raw-config`, decode with ffmpeg; needs 1 first. Also an in-process
   multiplexer for ISDB-S (`tx --system isdbs --mux-config`), which would
   place packets on the superframe timeline directly, once the mux's
   slot model is generalised from the OFDM frame.
   Status 2026-10-31: pieces A and B of
   [media-and-margin.md](isdbs-next/media-and-margin.md) §7 done and
   verified offline (VERIFICATION.md "ISDB-S bench service with media,
   offline"): `tools/isdbs-bench-tx.sh --media[=DIR]` (loop by default, a
   TS one media loop long; `--feed fifo` a live `isdb mux --out-ts -`),
   `tools/media-decode-check.py`, `MEDIA=DIR tools/isdbs-pacing-e2e.sh`.
   A looped media TS is not seamless in its media (a gap every pass:
   video 137.6 ms, audio 603 ms); the live multiplexer is. Piece C, the
   bench run, waits for TX1 to be reconnected; its commands are in that
   VERIFICATION.md section ("Pending: the bench run").
   In-process multiplexer (pieces G–J of media-and-margin.md §7): done
   offline. `isdb tx --system isdbs --mux-config FILE` (repeatable, one
   TS each, `kind = "bs"`, services on layer "A"), `isdb loopback
   --system isdbs --mux-config`, `isdb mux --system isdbs --out-ts`, and
   `--mux-pcr-grid uniform|slot` (default `uniform`, the receiver's
   constant-rate grid, as BS stamps; `slot` deviates by up to 6.6 µs at
   48 slots and ±0.45 ms for a 16-slot TS). Offline (VERIFICATION.md
   "In-process ISDB-S multiplexer, offline"): `tx --iq` → `rx` gives the
   placed TS byte for byte, 52.170000 Mbit/s, CBR fit 18 ns, PAT/PMT
   0.090 s, `validate --expect-rate 52170000:1` clean with the BS
   expectations, the media decoded (`MUX=1 MEDIA=DIR
   tools/isdbs-pacing-e2e.sh`). Bench run pending TX1: `isdb tx --system
   isdbs --channel BS1 --mux-config bs.toml --radio pluto … --offload fpga
   --rf-enable` (bs.toml = `bs-bench.toml` with `layer = "A"`), then
   `tools/isdbs-bench-check.sh --record 20` and the media check.
4. **Level margin**: a sweep of `att` (45 → 60 → …) with `dt295 tune`
   / `status --isdbs --watch` to find the lock threshold and the C/N
   slope (the driver reads C/N once while acquiring); record in
   VERIFICATION.md next to the ISDB-T figures.
   **Status 2026-10-31: tooling done (pieces D, E of
   [media-and-margin.md](isdbs-next/media-and-margin.md) §7), the run
   (piece F) waits for TX1 to be reconnected.** The driver reads C/N
   and the error counters in every locked round (it did already; at
   att 45 the raw 0xBC, 0x0B66–0x0B7A, is below the tc90522 formula's
   3000, so no dB). PIX-DT295 `d4ad6dc`: `status --json` (a JSON line
   per round: time, lock, raw C/N, dB where the formula covers it, error
   and packet counts, BER per layer, INFERRED from tc90522), the summary
   says "below the formula's range" instead of "not read"; rebuild
   `target/release/dt295`. isdb-rs: `tools/isdbs-level-sweep.sh` (+ `.py`
   for the CSV and the §6.3 table, `-test.sh` with stubs). Bench run:
   `tools/isdbs-bench-tx.sh WORK 45 ip:10.10.0.30` in its own terminal
   (its pid into a file), then `tools/isdbs-level-sweep.sh --conducted
   --tx-pid FILE --uri ip:10.10.0.30 ~/dev/sdr/isdbs-bench/sweep-1`
   (or `--start-tx WORK` to let the sweep start and stop it); the first
   step is also the unverified check that an `iio_attr` hardwaregain
   write while `isdb tx` streams takes effect without an underrun or
   MISSED (§5.2 **[confirm]**: the sweep stops on a read-back mismatch;
   `--restart` with `--start-tx` falls back to one broadcast per level).
   If the hold reaches att 80 still locked, a second pass with +20 dB of
   pad (`--pad 20`). Deviations from §6.2: `--margin` (as the bench
   script) instead of `--allow-below-45`; exit attenuation 89.75 dB
   (`--exit-att`) rather than the start level; a fresh tune and a
   `--stream-secs 5` recording at every level instead of every 5 dB; no
   quality poll inside `dt295 stream` (optional §4 item 4, not done).
5. **HDL follow-ups** (`fpga/isdb_fpga/tx.py`; each needs a rebuild,
   ≈ 25 min, and a card update): prefetch the first DMA word(s) while
   selected but not running (removes the 8–16-symbol start miss; then
   the hardware test can require MISSED 0 again); a "bank loaded" sticky
   flag or readback so the host can skip the 8 s bank reload when the
   BRAM default is in place; 64-bit (or wrap-tolerant) CONSUMED.
   **Status 2026-10-31: HDL, model, tests and host done; build and
   deploy pending.** As designed in
   [hdl-and-housekeeping.md](isdbs-next/hdl-and-housekeeping.md) §5:
   the block polls the DMA from SELECT (drops underflow answers before
   RUN, holds ≤ 2 words, drops them on deselect), STATUS bit 3 PRIMED,
   PRE_UNF 0x0A; BANK_TAG 0x08 (CRC-32C of the bank as written,
   bitstream init = the default bank's, any bank write → 0, no reset);
   64-bit CONSUMED / FIRST_MISS / SAMPLES with HI 0x09; FEATURES 0x07 = 7;
   ID word 0xB5D11918 unchanged. Model `Interpolator::prefetched`,
   vectors `tx-6-prefetch`; FPGA suite 33/33. The host reads FEATURES:
   with them it skips a tagged bank, waits for PRIMED and reads HI;
   at 0 (the deployed image) it behaves as before and extends the
   32-bit counters itself. `tx_symbols_loopback_on_a_pluto` requires
   MISSED 0 once the open line names the prefetch. **Built, deployed and
   verified 2026-10-31** (VERIFICATION.md "image 2"): 23 557 LUT / 158
   DSP, WNS +0.100 / WHS +0.022 ns, `isdb_txrx2_top.bin` on the card;
   loopback PASS with MISSED 0, the bank skipped on a tag match
   (configured in 0.0 s), 64-bit CONSUMED; PRE_UNF 7 545 871 shows the
   start miss was the DMA still empty between SELECT and the first word.
6. **Housekeeping**: `uv run --locked` in `fpga/` needs
   `UV_DEFAULT_INDEX=https://pypi.org/simple` on this host (a private
   index rewrites `uv.lock`; the build scripts already set it, the mise
   tasks do not); the merged agent branches `worktree-agent-*` can be
   deleted; `fpga/README.md` still describes the RX-only `isdb_top.bin`
   as the deployed image in places; isdb-s.md §2's bench table (TX1
   "directly, no attenuator") is outdated. **Done 2026-10-31**: the mise
   uv tasks default `UV_DEFAULT_INDEX` (`2741d4e`), the 7 branches are
   deleted, fpga/README.md names `isdb_txrx2_top.bin`, isdb-s.md §2 and
   §0.23, README.md, receiving.md, transmitting.md and test-data.md are
   current (hdl-and-housekeeping.md §6h–§6j).

## Investigation (2026-10-30)

Read-only investigation of the Work list, one report per cluster under
[isdbs-next/](isdbs-next/); nothing built, transmitted or deleted.

- [pcr-pacing.md](isdbs-next/pcr-pacing.md) (item 1). The ISDB-T `Pacer`
  paces per frame only (`input.rs:255-312`: packets due in frame k fill
  its first slots, nulls after, PCRs untouched); per superframe it would
  still leave 5.5 ms max PCR error. Slot placement alone leaves 14.3 µs
  max / 8.3 µs rms (one slot is 28.83 µs); slot placement plus PCR
  re-stamping (H.222.0 D.0.4) gives 18 ns max on the bench TS in the
  offline model (`tools/isdbs-pacing-sim.py`). Reuse `PcrClock`, not
  `Pacer`. `bench60.ts` breaks the continuity counters of 9 of 11 PIDs at
  the loop seam (`tools/ts-loop-seam.py`). Today's `--feed file` plays a
  29.96 Mbit/s TS 1.74× too fast and validates clean: `validate` needs a
  rate check. Split P1–P6 in §9.
- [bs-si.md](isdbs-next/bs-si.md) (item 2). `isdb-si` has every descriptor
  BS uses; the work is a `kind = "bs"` branch in `isdbt-mux` (NIT, BIT,
  config checks) and `isdb-analyze` accepting 0x43. 0x4010 is a real
  on-air TS_ID (the `txs.rs:85` "placeholder" comment was wrong, fixed
  in D); the
  report proposes 0x4017 / service 999. `dt295` has no SI readout.
  `tools/bs-nit-section.py` gives BS SI via `[nit] replace_sections`
  today. Split A–D.
- [media-and-margin.md](isdbs-next/media-and-margin.md) (items 3, 4). Media
  is in `crates/isdbt-mux/examples/media/` (git-ignored); the AAC needs
  `--adts-compliance warn`. **`tools/isdbs-bench-tx.sh` bug**: `exec`
  drops the EXIT trap, and `while :; do cat …; done | pv` spins for ever
  once `pv` exits; use `while cat …; do :; done`. The driver reads C/N on
  every poll; at att 45 the raw value is below the borrowed TC90522
  formula's floor, so no dB is printed. Attenuation is set once at open
  (`pluto.rs:2227-2242`). Splits A–K.
- [hdl-and-housekeeping.md](isdbs-next/hdl-and-housekeeping.md) (items 5,
  6). The start miss comes from requests gated on RUN (`tx.py:252-253`);
  the design polls from SELECT and adds a FEATURES register (0x07) instead
  of an ID bump. The bank reload measured 3.9 s, not 8 s; a BANK_TAG
  (CRC-32C) register lets the host skip it. The deployed image is
  `build.sh WORK isdb`, not `isdb-tx`. All 7 `worktree-agent-*` branches
  are merged; exact edits for mise.toml, fpga/README.md and isdb-s.md §2
  are listed. FPGA suite 24/24 (`mise run fpga:test`).

## Pitfalls met this session

- 2026-10-31: the session scratchpad (`/tmp/claude-…`) is tmpfs: TS, IQ
  and placed-TS files cost RAM there. Bench work trees go on disk
  (`~/dev/sdr/isdbs-bench`: `bench60.ts`, `bs-bench.toml`, recordings).
- 2026-10-31: the board booted without `cf-ad9361-lpc`; the §2 rebind
  recovery bound it on the third bind (the first two: "Tuning TX
  FAILED", probe error −5); afterwards `tx_symbols_loopback_on_a_pluto`
  passed as on 10-29 (MER 34.1 dB, byte-exact, 0 uncorrectable).

- A `pkill -f PATTERN` whose pattern appears in the calling shell's own
  command line kills that shell. Keep pids in a file.
- zsh: `$VAR` with spaces is not word-split (use `${=VAR}`); `=word` at
  the start of an argument is a command expansion (quote `====`).
- The Vivado project's `*_timing_summary_routed.rpt` is taken before
  upstream's `hold_fix.tcl` hook and the post-route physical
  optimisation; read the final checkpoint with `fpga/tezuka/timing.tcl`.
- libiio's `iio_buffer_push` returns when iiod has the buffer, not the
  DMA; a receive capture's 32 MB block starves iiod's TX thread for
  ≈ 400 ms. The symbol path buffers 1.16 s on the device for that.
- The DAC underflow flag (0x80000088 bit 0) is set whenever the DMA is
  idle: clear and read it while streaming.
- A background `reboot` inside an ssh command dies with the session; run
  `ssh … 'sync; reboot'` directly with a timeout.

## Bootstrap prompt for the next session

Copy from here to the end of the file into the new session's first message.

```text
Continue the isdb-rs ISDB-S work from docs/plans/isdbs-next.md (read it
first, then docs/plans/isdbs-fpga-tx.md and the four "ISDB-S transmitter
in the FPGA" sections of VERIFICATION.md). State: the FPGA transmitter is
built, deployed on the PlutoSky R2 (ip:10.10.0.30, isdb_txrx_top.bin) and
verified bit-exact in loopback and conducted into a PIX-DT295 tuner
(driver: ~/dev/PIX-DT295, `dt295`), which locks on the bench broadcast of
tools/isdbs-bench-tx.sh. The broadcast may still be running
(`pgrep -af "isdb tx --system isdbs"`); stop it before changing anything
on the board. The bench is conducted only: TX1 → 30 dB pad → DC block →
tuner; never transmit without that, never without --rf-enable, keep the
attenuation ≥ 45 dB unless measuring the margin.

Do the "Work" list of isdbs-next.md in order, starting with 1 (PCR-paced
placement in the ISDB-S producer: today a live feed's packets are placed
in arrival order and the tuner's recording shows 12–30 ms rms PCR jitter
and PAT/PMT intervals up to 0.21 s). Investigate the existing Pacer of
the ISDB-T --mux-ts path first and design before coding; use Opus
subagents in worktrees for implementation with one contract per piece,
merge and verify yourself (cargo clippy --workspace --all-targets --locked
-- -D warnings, cargo test --workspace --locked, rustdoc -D warnings; for
fpga/ use UV_DEFAULT_INDEX=https://pypi.org/simple with uv run --locked).
Commit at each checkpoint with conventional commits (commitlint's scope
list) and show the diff summary. Verify every change on the bench with
dt295 stream + isdb ts validate / inspect, record results in
VERIFICATION.md and the plan's status, keep investigation scripts in
tools/. Read "Pitfalls met this session" in isdbs-next.md before touching
the board or running pkill.
```
