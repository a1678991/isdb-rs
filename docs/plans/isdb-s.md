# Plan: ISDB-S and ISDB-S3 (investigation and implementation)

Hand-off document for the next agent. Written 2026-10-20 at the end of the
ISDB-T / ADALM-Pluto work. Everything under "Established" was measured or
read in that session; everything else is a plan or an open question.
Follow the repository conventions in [CONTRIBUTING.md](../../CONTRIBUTING.md)
(spec-first, SPECS.md and SPEC-NOTES.md, the verification ladder of
[VERIFICATION.md](../../VERIFICATION.md), conventional commits, no real
station identities in fixtures or docs).

## 0. Starting state

- **Branch.** `feat/pluto-backend` carries three unmerged commits:
  `325616c` (ADALM-Pluto backend + `isdb-iio`), `f230034` and `ac093a7`
  (broadcast reception records, TMCC test vectors). Ask the user whether
  to merge them into `main` before starting.
- **ISDB-T is complete** for this purpose: modulator, demodulator, the
  Pluto backend (`--radio pluto:uri=ip:192.0.2.30`), and real-broadcast
  reception (seven stations, TMCC parity confirmed, one station
  RS-clean; see VERIFICATION.md "Broadcast reception").
- **Not started:** anything ISDB-S / ISDB-S3. The README roadmap lists
  them as "not started".

## 0.1 Phase 1 status (done 2026-10-20)

Commits `34fb488` (isdb-iq), `a6d94b4` (isdb-radio/isdb-iio captures),
`96e5123` (isdb capture, isdb iq survey, `bands` channel tables), `cfd891d`
(docs). Findings that change this plan:

- **Capture lengths.** One libiio buffer: max 16 515 072 samples (63 MiB;
  ENOMEM above, kernel-block count irrelevant) = 282 ms @ 57.72 Msps.
  Tezuka's maia-sdr recorder (HTTP API on port 80, `/api/recorder`,
  `/recording` tar of SigMF; 16-bit mode, 12 bits **left-aligned**, full
  scale 32768): 33 554 432 samples = 581 ms, ~5.5 s per capture.
  `isdb capture --radio pluto:uri=ip:192.0.2.30,recorder=maia`. ≈ 1 s at
  16 bits is not possible (IQ8bit/IQ12bit modes would be; not wired).
- **Contiguity** verified at 61.44 Msps via the |z|² line phase (max
  0.08 rad vs 2.95 rad per lost sample). Drops of exact multiples of 1024
  samples are invisible to that test: Phase 2's frame sync must check frame
  spacing (39 936 symbols) across each recording.
- **Carrier phase noise:** z⁸ line phase moves ~0.5 rad rms per 1 ms chunk
  (≈ 0.065 rad carrier) on this feed — plan carrier recovery accordingly.
- **Carrier offset** of the whole feed: −112…−116 kHz (not −107 as in
  §3.2); Pluto clock ≈ +0.6 ppm vs the BS symbol clock.
- **CS classification:** all 12 ND transponders ISDB-S, QPSK main signal
  (z⁴ strongest, +49…+57 dB); ND22/24 ~10 dB lower SNR. BS: 9 ISDB-S
  (TC8PSK, z⁸ strongest), BS7/11/17 ISDB-S3 (π/2-BPSK z² line at 2Δf+R/2).
  NDn IF = 1613 + 20·(n − 2) MHz confirmed (band centres within 1.3 MHz).
- **Datasets** (outside the repo), 2026-10-20:
  `bs{1,3,5,9,13,15,19,21,23}` @ 57.72 Msps, `bs{7,11,17}` @ 61.44,
  `nd{2,12,24}` @ 57.72, `nd{2..24}-61` @ 61.44 (all maia, 33.5 M samples,
  128 MiB each), `bs1-61-iio` (libiio, cs16/2048, 16.5 M). Each has a
  `.sigmf-meta`. Tests: `ISDB_RS_ISDBS_IQ=…/nd2-61 ISDB_RS_ISDBS3_IQ=…/bs7`.
- **Survey caveats** for Phase 2: at fs = 2R the |z|² line and its alias
  coincide (ppm sign ambiguous); z^k lines have copies at k·Δf ± R
  (search within ±R/2); ISDB-S3 −R/2 window has an unrelated line of
  equal strength (use a corroborated offset).
- **Recommendation (not done):** move RS(204,188), the K=7 convolutional
  code/Viterbi and the PRBS from `isdbt-fec` into a shared crate when the
  ISDB-S FEC (Phase 3) first uses them; nothing in Phase 1 needed them.

## 0.2 Capture layer: 8-bit and streaming (2026-10-20, after Phase 1)

Options 1 and 3 of the "FPGA for the capture layer?" question (no HDL).
Findings that change this plan:

- **maia-sdr recorder: 128 MiB in every mode** (33.5 M / 44.7 M /
  67.1 M samples in 16 / 12 / 8 bits; 1.68 s at 40 Msps CS8). The device
  tree reserves 256 MiB (sysfs `recording_size` 0x10000000), but Tezuka
  builds maia-hdl's `maia_iio_lite` config, whose recorder DMA range is
  hard-wired to 0x0600_0000–0x0E00_0000. `maximum_duration` in `/api` is
  the last *requested* duration, not a capacity. So option 2 (bigger DT
  reservation) alone does nothing: longer one-shot captures need a
  rebuilt bitstream (`maia_iio` config = 256 MiB, or larger). Modes:
  IQ16bit (12 bits left-aligned, FS 32768), IQ12bit (3 B in DDR,
  downloaded unpacked to ci16 sign-extended, FS 2048), IQ8bit (ci8,
  **truncated**: mean −0.47 LSB). `isdb capture … recorder=maia,bits=8`.
- **Tezuka CS8 through libiio**: enable `voltage0` alone on
  `cf-ad9361-lpc`; the HDL (`cs12_cs8mux.v`) packs `{Q8,I8}` per 16-bit
  word, 12-bit/16 rounded with 1st-order error feedback, FS 128, mean ≈ 0.
  CS12: `voltage1` alone, 12-bit packed + a sync burst (20-byte magic +
  u32 counter) every 262 144 samples, replacing 8 samples. No firmware
  setting involved.
- **40 Msps 8-bit keeps ISDB-S** (spectrum within 0.5 dB of the 57.72
  Msps one out to ±16 MHz; `isdb iq survey` finds 28.86002 Mbaud and the
  carrier at 40 Msps).
- **Streaming over GbE does NOT reach 40 Msps CS8 (80 MB/s).** iiod 0.26
  delivers at most ≈ 63–66 MB/s to `isdb-iio` (`iio_readdev` only ≈ 38).
  The Pluto's TCP stack does ≈ 46 MB/s through a pipe (dd|socat); the
  GEM has no TSO and **maxmtu 1518 (no jumbo frames)**, so a direct cable
  does not raise the ceiling. Loss-free for 5 min: CS8 20 Msps (40 MB/s).
  Lossy: 24 Msps 9 losses/5 min, 28 Msps 7, 32 Msps 14, 40 Msps
  continuous. CS16 at 512/63 MHz (ISDB-T) 60 s without loss.
- **Survey fix**: at 40 Msps the π/2-BPSK ±R/2 windows met across fs/2
  (BS7 offset +3.0 MHz instead of −112 kHz); windows now kept apart.
- **Datasets** (2026-10-20): `bs1-40-cs8`,
  `bs7-40-cs8` (maia IQ8bit, 67.1 M samples, 1.68 s), `bs1-40-iio-cs8`
  (libiio, 33.0 M samples, 0.83 s).
- **Drop detection**: ADC core `UI_STATUS` (0x80000088) bit 2, sticky, set
  whenever no DMA transfer runs; validated against the CS12 sync counters
  (410 losses, each flagged ≤ 12 buffers before its data arrived).
- **Consequence for §7**: the `Symbols` boundary (≈ 58 MB/s ISDB-S, 8-bit
  I/Q at 1 sps) is also above what the link sustains loss-free; live
  ISDB-S/S3 over GbE needs `SoftBits` packed tighter than 8 bits, or
  `Packets` (6.5 MB/s), or a non-CPU network path. Until then, offline
  work uses maia captures (≤ 1.68 s at 40 Msps CS8; more needs a
  rebuilt bitstream, see above).

## 0.3 FPGA symbol front end (2026-10-20, after 0.2; user asked for FPGA work now)

Board state: Tezuka's DATV / GPS / MQTT / NFS services stopped and the TX
LO powered down (TX attenuation 89.75 dB) at every boot by
`/mnt/jffs2/autorun.sh` (user-authorised; delete it to restore Tezuka's
defaults). Board ≈ 99 % idle.

- **Ceiling (B)** with the services stopped:
  - 8-bit IQ through iiod: 5 min loss-free at 20, 32 and 36 Msps (up to
    71.9 MB/s); 28 Msps lost twice while the host was busy; 40 Msps
    (80 MB/s) loses from the start;
  - 16-bit: loss-free to 16 Msps (64 MB/s);
  - raw TCP from the board (`dd | socat -b 65536`): 59.8 MB/s with one
    stream, 69.5 MB/s with two on two cores.

  The host NIC's MTU is 1460, so UDP datagrams over 1432 bytes are
  dropped. B ≈ 70 MB/s; design target ≤ 60 % of it.
- **Boundary chosen: `Symbols` at 1 sample per symbol, 6 bits per
  component** (`isdb-frontend`, format 2 since 2026-10-25, §0.6):
  - ISDB-S 43.4 MB/s, −0.07 dB MER;
  - ISDB-S3 51.0 MB/s, −0.04 dB 16APSK ring MER (was 5 bits / 42.4 MB/s
    under format 1, measured through the untrimmed filter).

  IQ at 36 Msps would fit but costs ISDB-S 0.9 dB (aliased roll-off),
  has no margin, and needs ≥ 39 Msps to be clean. 8-bit symbols are
  57.7 / 67.5 MB/s, too close to B. Soft bits or packets would need the
  TMCC and frame sync in the FPGA (later). A leaner on-board streamer
  alone cannot pass 78 MB/s: raw TCP tops out near 70.
- **Front-end parameters** (measured; `Design::for_system`):
  - 40 Msps with **56 MHz analog bandwidth**: the default 0.8·fs costs
    2 dB;
  - 32 taps, Kaiser β 5, 64 phases;
  - timing-loop bandwidth 2·10⁻⁴ R (ISDB-S) and 5·10⁻⁶ R (ISDB-S3,
    whose α = 0.03 weakens the NDA detector);
  - BS1 8PSK MER 14.9 dB, fixed = float within 0.03 dB.
- **Built:**
  - `isdb-frontend`: model, float reference, format, metrics, control
    map, vectors;
  - `fpga/` (Amaranth `SymbolsRx`): bit-exact in simulation, with the
    configuration through `gpio_out`; Yosys estimate 46 DSP48E1, 3.6k
    LUT, 4.5k FF, 9 RAMB18 for 32 taps;
  - `isdb-radio` `Offload` / `capture_symbols` (Pluto: discovery via
    `up_adc_gpio_in`, configuration via `up_adc_gpio_out`, 0.62 ms per
    register access, ≈ 2.6 s per configuration; format 2: ≈ 3.9 s);
  - `isdb symbols`, `isdb iq symbols`.
- **Open:**
  - the bitstream: Vivado 2023.1 or 2023.2 (ML Standard), an AMD
    account, tens of GB of disk; maia-hdl `projects/tezuka` for
    `plutoskyr2`; boot from a spare SD card;
  - timing closure at 160 MHz;
  - the hardware test;
  - the ISDB-S3 code rate (TMCC needed; the constellation is 16APSK,
    §0.6);
  - real-time CPU fallback (the scalar model runs 9–15 Msym/s, below
    28.86 / 33.76 Msym/s: SIMD and threads per §7.2).

## 0.4 Phase 2 status (done 2026-10-25)

Commits `2c52bb8` (channel names T<n> / BS<n> / ND<n>; `tx`/`rx` refuse
satellite channels), `40c1323` (`isdb-fec`: RS any length incl.
RS(64,48), K=7 code + Viterbi, PRBS15, burst PRBS9; `isdbt-fec`
delegates, ISDB-T tests and Viterbi speed unchanged), `69f9b51`
(`isdbs-core`, `isdbs-phy`, `isdb iq tmcc`, rx SigMF guard,
`tools/isdbs-tmcc-acceptance.sh`), `a68bca0` (docs). Findings:

- **The ordinance settles BO.1408's gaps, all confirmed on air:** frame =
  192-symbol header (W1 | 8 TMCC bytes | W2/W3, BPSK r=1/2, X(171) then
  Y(133)) + 192 × (203 main + 4 burst); burst = BPSK of PRBS9 alone
  (data all zero), restarted per frame, clocked on burst symbols only
  (30 720/30 720 symbols match); TMCC PRBS15 per superframe from the 1st
  TMCC byte, W words clocked but not XORed.
- **TMCC information layout** (Notification 304 unavailable) read as
  BO.1408 Fig. 7–10 MSB-first: order 5 | 4 × (word 4 + slots 6) | 48 × rel
  TS 3 | 8 × TS_ID 16 | emergency 1 | uplink 4 | reserved 62. Consistent
  on all 21 transponders (sums 48, unused TS_ID FFFFh, stable). Open:
  uplink = 0010 (BS) / 0100 (CS); first reserved bit is 0 everywhere
  (an undocumented flag?).
- **Acceptance met:** RS(64,48) ok on every superframe of BS1…BS23 (9)
  and ND2…ND24 (12), 0 slips, exact 39 936-symbol spacing (no capture lost
  samples). BS: TC8PSK 2/3 × 48, 2–4 TSs; CS: QPSK 3/4 × 48, 1 TS (36
  valid slots, every 4th a dummy if the dummy-slot reading holds).
- **Sync must use the whole frame signature** (808 known symbols): the
  W1 tail alone fails on the weak CS transponders.
- **Pilot MER** (burst symbols vs BPSK, a true SNR): BS 14.0–14.8 dB; CS
  ND2 13.0 dB falling to ND20 8.2 dB (cable tilt); ND22/ND24 ≈ 1 dB:
  TMCC still decodes, main signal (QPSK 3/4 needs ~4 dB) is not expected
  to. Re-capture ND22/24 with more gain, or accept them as out of reach.

**For Phase 3:** start from `isdbs_phy::rx::Frame::main` (38 976 derotated
unit-amplitude symbols per frame, `Frame::index` = position in the
superframe) and `isdbs_core::slots::runs(&TmccInfo)` (contiguous symbol
range per combination, puncturing restarts at each run start). Still to
build: TC8PSK / QPSK soft demapping (annexed table 46; TC8PSK = B1
uncoded on C2, B0 into the K=7 code), depuncturing with
`Combination::puncturing().pairs()` (P1, P0 per QPSK symbol), Viterbi
(`isdb_fec::ViterbiDecoder`), inter-frame byte de-interleaving (depth 8,
203 bytes, same slot number across frames, annexed table 48 別記 2),
PRBS15 removal per superframe from the 2nd byte (sync bytes skipped,
PRBS runs), sync-byte reinsertion (0x47), RS(204,188), slot → TS_ID
demux, and the first-12-slot header-byte replacement (table 45 note 4:
the first byte of slots 1–12 carries the header, i.e. the sync bytes
are not sent). Possibly a decision-directed carrier refinement for
TC8PSK. Then `isdb rx --system isdbs` with `--ts-id`.

## 0.5 Phase 3 status (done 2026-10-25)

Commits `c348567` (main-signal decoder: `isdb-fec` Viterbi additions,
`isdbs-core::mainsig`, `isdbs-phy::{demod, ts}`), `a907182` (`isdb rx
--system isdbs`, `isdb-analyze` fixes for scrambled payloads and short
captures), `a94d3fb` (docs, tests, tools; also carries two small code
changes: missing PMTs in captures < 2 s are Info in `isdb-analyze`, and
`rx` does not write the TS_ID FFFFh slots). Findings:

- **19 of 21 transponders give RS-clean TS**: 311 520 packets, 12
  corrected, 0 uncorrectable, 0 CC errors, 0 TEI; PAT tsid = TMCC TS_ID
  everywhere. ND22/ND24 (pilot MER ≈ 1 dB) fail as expected: re-capture
  with more gain (or another feed) if they matter.
- **Constellation readings settled by RS** (`examples/mapprobe`): TC8PSK
  C1 = 133₈ (Y), C0 = 171₈ (X) (BO.1408 Fig. 15 (W,Y,X)); QPSK P1 on I
  (table 46 text; figure -086 disagrees); no spectrum inversion; dummy
  slots after the valid ones in a unit; interleaver per Fig. 6.
- **Unassigned slots**: BS9/15/21/23 have slots whose relative TS ID
  names an FFFFh entry; they carry null packets; not written.
- **validate misfires fixed** (scrambled payloads parsed as sections/PES;
  "mandatory table missing" on captures shorter than the repetition
  interval). The CS TSs' PCR deviates 11–16 µs from a CBR fit:
  broadcaster multiplexing (rate and CC check out), left as a warning.
- **Throughput** (BS3, 32 cores): front end 0.36× real time (one
  thread, bit-exact model), frame sync/carrier/TMCC 0.61× (one thread),
  main signal → TS 3.7× (threaded per superframe; 7.5 Msym/s per core).
  Whole chain ≈ 0.2× real time. To reach real time on the CPU: SIMD
  front end or splitting the IQ stream at frame boundaries with overlap
  (breaks bit-exactness at the seams unless designed for), and a
  parallel carrier tracker; or the FPGA symbol front end.
- **Pre-existing flaky test**: `isdb-cli` `live_feed_with_stalling_input_stays_cbr_and_lossless`
  (ISDB-T `tx --feed live`, wall-clock pacing) fails under the parallel
  load of `cargo test --workspace` and passes alone; it fails the same
  way on `a68bca0` (before Phase 3). Not touched. Reproduced by the
  coordinator: passes alone (5.2 s); with 32 busy-loop threads it fails
  at `cli.rs:417` "11 frames for a 1.5 s stall" (needs ≥ 13): the paced
  producer, starved of CPU, emits fewer frames than the stall lasts. A
  timing test, not an ISDB-S regression; fixing it means a looser bound
  or a mock clock.
- **Live**: `isdb rx --radio pluto:…,recorder=iio,bits=8 --channel BS3
  --block-seconds 0.4 --seconds 0.8`: 2 blocks, 26 880 packets RS-clean,
  13.7 dB MER (receive only). Blocks are discontinuous by design.
- **For Phase 4**: `isdbs_phy::demod::encode_frame` and
  `isdbs_core::mainsig::frame_bytes` are the transmit direction of the
  main signal already (used in round-trip tests); the TMCC side is
  `isdbs_core::tmcc::headers`, the bursts `isdbs_core::burst`. A
  modulator needs the header/burst/main symbol assembly (the inverse of
  `rx::receive`, `testsig.rs` is a start), RRC shaping and a TS
  multiplexer that fills slots per TS_ID.

## 0.6 ISDB-S3 front end: timing trim and 6 bits (done 2026-10-25)

Asked: "investigate enough bits for ISDB-S3 and implement it". Commits
`4aedd3f` (model, format 2, trim, s3sync library, investigation
examples), `d37bbfd` (fpga format 2), `65b5b79` (CLI `--trim`,
`isdb iq s3sync`), `0dedbc6` (docs).

- **Loss budget** (BS7/11/17, 61.44 Msps, 56 MHz, sync-symbol Es/N0):
  8.8–9.5 dB through the format-1 front end against ≈ 17 dB in band
  (spectrum) and 14.7–15.9 dB with per-slot data-aided timing. The
  timing loop locks −0.15…−0.18 T from the best instant, steady over
  time, sample rate, bit width and analog bandwidth: 5.1–6.4 dB.
  Neighbours, filter length (0.4 dB at 32 taps), jitter and fixed point:
  under 0.5 dB together. The NDA detector is unbiased on synthetic
  signals (±0.05 T), so the received pulse is distorted upstream (cause
  not settled: satellite, cable feed or the radio's analog filter).
  ISDB-S has the same offset at −0.05…−0.10 T.
- **Fix: format 2.** Output bank `k` = matched filter a trim later in the
  same window; output and AGC on `k`, TED on `h`/`g` (a trim on the TED's
  instant would be followed by the loop). The host measures the trim on
  0.1–0.15 s (`s3sync::best_trim`, `isdbs_phy::frontend::measure_trim`);
  `--trim auto` default. ISDB-S3: 14.58–14.67 dB (+5.2…+5.8); ISDB-S
  burst MER +0.1…+2.3 dB on 21 transponders, TMCC/TS acceptance clean
  (corrected packets 85 → 33).
- **Bits:** 6 for ISDB-S3 (16APSK ring MER −0.04 dB, sync −0.07 dB; 5
  bits −0.16…−0.27 dB), 51 MB/s, 73 % of B.
- **Modulation of BS7/11/17:** 16APSK (two rings 0.26 / 0.74, 4 and 12
  angle clusters), ring ratio 2.2–2.3 (compressed by the transponder).
- **FPGA:** bit-exact in simulation (trimmed and untrimmed vectors, BS7
  and BS1 recordings, port load, gpio_out); 64 DSP / 3.9k LUT / 4.6k FF /
  1 RAMB18 + 8 RAMB36 at 32 taps (format 1: 46 DSP); 170 / 220 DSP with
  Tezuka's build.
- **Open:** an equaliser on the host for the remaining 0.7–1.2 dB of ISI
  (Phase 5, with the STD-B44 pilots); reloading only `k` on the FPGA for
  a new trim (`Encoder::output_bank` exists, the Pluto backend reloads
  everything); the source of the pulse distortion; the bitstream (Vivado
  2026.1 installed but missing Zynq-7000 devices and a licence).

## 0.7 Phase 5 status: ISDB-S3 receiver (done 2026-10-25)

Commits `d001f95` (`isdb-fec`: BCH GF(2¹⁶) t = 12, IRA LDPC encoder and
layered min-sum decoder, `B44Prbs`), `fe1a377` (`isdbs3-core`,
`isdbs3-phy`: sync, TMCC, main signal to slot data), `762c106`
(`isdb-tlv`: TLV, HCfB, MMTP, MMT-SI, TLV-SI; `rx::receive`), `840d2d7`
(`isdb rx --system isdbs3`, `isdb iq s3tmcc`, `isdb tlv inspect`; the
pilot-trained equaliser; B60 scramble-information extension), `bded7f2`
(docs, property tests), `71c6ab1` (dropped a fault-injection proptest
seed). Findings:

- **Everything decodes on BS7, BS11, BS17**: TMCC on every whole frame
  (16APSK, rate code 0111 = 93/120, 120 slots; three TLV relative
  streams of 40 slots, BS11's stream 1 unassigned with null packets); all
  4 512 main-signal codewords LDPC- and BCH-clean (0 BCH corrections);
  12 633 TLV packets in 8 streams with 0 head-indication mismatches and 0
  IP errors; every UDP payload is MMTP (HCfB context 1).
- **Readings settled on the signal** (`isdbs3-core` SPEC-NOTES): B44 PRBS
  output is the **last stage** (80 readings tried; only this one is
  BCH-clean); Table 3.11-2 "1010 = 101/120" is a typo for 105/120, and
  0111 = 93/120 (the amended MIC numbering) decodes; frame head = the odd
  slot with the opposite FSync sign; BCH word = header + data + parity,
  dispersal (incl. stuffing) removed first; 16APSK labels; forward
  interleaver at 93/120; pilot = labels in order with PRBS 0°/180°. LDPC
  tables transcribed by `tools/b44-ldpc-tables.py` (2.0-E1 = 2.1-E1).
- **Equaliser** (11 taps, LS on 6 720 sync + pilot symbols per frame):
  sync Es/N0 14.6 → 17.0–17.7 dB (= the spectrum's in-band SNR, so the
  "remaining ISI" hypothesis of §0.6 is confirmed), main MER +1.3…+1.7
  dB on untrained data symbols, LDPC iterations 3.0 → 2.0. The pilot-
  learned 16APSK has ring ratio 2.51 vs γ 2.87 (compression); demapping
  against it adds 0.7 dB.
- **MMT-SI**: PA (MPT, PLT; on air N = 0 with tables in the payload),
  MH-EIT p/f + schedule, MH-SDT, MH-TOT, MH-AIT, ECM, EMM, data
  transmission messages. **No TLV-SI** (TLV-NIT / AMT) in 0.55 s: needs a
  longer recording (maia 8-bit at 40 Msps = 1.68 s; loss-free streaming
  tops out near 36 Msps 8-bit, barely above ISDB-S3's 34.8 Msps minimum).
- **Scrambling**: 98.8–98.9 % of MPU payloads carry the B60 type-0001h
  scramble-information extension (STD-B61 not available); left as is.
- **Throughput**: LDPC 190 codewords/s/thread, 3 029/s on 32 threads vs
  2 905/s needed (1.04×, no margin). SIMD / early-termination of the
  syndrome pass would give headroom; the front end (one thread) is the
  bigger limit for live reception, as for ISDB-S.
- **ISDB-S unchanged**: 38 TSs, 319 584 packets, 0 uncorrectable.
- **Open**: TLV-SI on a longer recording (and the 0xFE packet layout
  around the section, `isdb-tlv` SPEC-NOTES pending); QPSK/8PSK/32APSK
  and mixed allocations only tested synthetically; live `--system isdbs3`
  from the radio not exercised in this phase (offline only); the FPGA
  front end sized at 32 taps — with the host equaliser, the bit-width
  study of §0.6 still holds (6 bits); real-time LDPC.

## 0.8 Live ISDB-S3 from the Pluto (coordinator, 2026-10-25)

Workspace clippy fixed: `isdb-iq` example `bandpower` (`as_chunks`),
committed `c5ac0c7` with the full hook. Live `isdb rx --system isdbs3
--radio pluto:uri=ip:192.0.2.30,recorder=iio,bits=8 --channel BSn
--seconds 1.2` (receive only; 40 Msps, 56 MHz, 0.826 s libiio blocks):

| | TMCC ok | slots | LDPC failed | sync Es/N0 → eq. | main MER |
|---|---|---|---|---|---|
| BS7 | 48/48 | 4 608 | 6 | 13.6 → 16.4 dB | 14.8 dB |
| BS11 | 48/48 | 4 608 | 0 | 13.5 → 16.5 dB | 15.0 dB |
| BS17 | 48/48 | 4 608 | 5 | 13.5 → 16.0 dB | 14.6 dB |

- TLV streams and signalling as offline (MPT, PLT, MH-EIT, MH-SDT,
  MH-AIT, MH-TOT, MH-BIT, ECM, EMM, EMT); "lost slots" 0–4 per stream.
- Live is ≈ 1 dB below the 61.44 Msps cs16 recordings (14.6 → 17.0–17.7
  dB). Not the 8-bit format: BS7 at `bits=16` (0.41 s blocks) gives the
  same (6 failed of 4 320, 13.6 → 16.5 dB). Failures fall in either
  block, not only at block starts. At 16APSK 7/9 that ≈ 1 dB leaves a
  few failed slots per thousand: needs either the lost dB found (40 vs
  61.44 Msps? the trim measured on 0.15 s per block?) or more LDPC
  iterations / a better decoder.
- **Bug**: one 8-bit BS7 run decoded no TMCC in block 1 (24 frames,
  "TMCC ok 24" of 48, 0 slots) while block 2 decoded normally; the first
  8-bit run of the same channel did not show it. Intermittent per-block
  acquisition failure.

## 0.9 SIMD LDPC and the carrier fix (2026-10-25, after §0.8)

Commits `f01d1cb` (carrier choice), `461f601` (`isdb-fec` fixed-point
SIMD LDPC), `5aaa08f` (receiver default, `--ldpc`, parallel trim and
carrier trials), plus docs.

- **§0.8 bug found and fixed**: the block that decoded no TMCC was a
  wrong carrier. The survey's +R/2 window held an unrelated line 0.3 dB
  stronger than the true −R/2 line; `best_carrier()` (strongest line)
  mixed 361 kHz off (sync Es/N0 5.5 dB). Now: lines grouped into
  candidates (within 2 kHz), the first four tried through the front end
  on 0.05 s, best sync Es/N0 wins. Reproduced on a recording
  (the 2026-10-25 8-bit BS7 recording, segment 1): TMCC 48/48
  frames after, 24/48 before.
- **Fixed-point LDPC**: i8 layered offset min-sum, input scale 1 (LLR
  units, ±127), offset 1. Frame-parallel lanes (one codeword per lane,
  same per-lane arithmetic as one lane ⇒ bit-exact by construction and
  by test); lanes refilled as each codeword stops (`decode_all`), which
  took 16-lane throughput on near-threshold data from 1 980 to 3 025
  cw/s. The QC row-parallel layout was not used: duplicate residues in
  table rows and the staircase make it awkward; frames have 96–120
  independent codewords, enough lanes.
- **Codegen**: the first portable kernel was mostly scalar (SSE2 lacks
  signed-byte min/max and abs; per-lane `if` selects against the scalar
  edge index became `pinsrb`/`cmov`). Rewritten with mask blends and
  min/max: 2 489 → 4 678 cw/s per thread, within 3 % of nightly
  `std::simd` (4 834).
- **Throughput** (BS11 codewords): float 166 / 2 742 cw/s (1 / 32
  threads), fixed SIMD stable 4 678 / 33 076 (11.4× real time), AVX2
  5 739 / 35 602, nightly AVX2 6 153 / 37 754. AVX-512 = AVX2 (memory
  bound: messages edges × W bytes). 16 lanes best at baseline.
- **Live failures (§0.8)**: decoder-independent. On 9 216 live-path
  codewords: fixed 5, float 6 failed; iteration limit 20 → 7, 50 → 5,
  100 → 5, 200 → 5 (float 6 → 5 at 200). Needs SNR (the ≈ 1 dB live
  deficit: 40 Msps libiio path vs 61.44 Msps recorder), not iterations.
- **Whole chain** (1.65 s live-path recording): 37.8 s → 19.9 s. Trim
  search and carrier trial on threads (10.6 → 1.3 s per block). Now:
  front end 2.5 s per 0.83 s block (single thread, 0.33× real time),
  `synchronise` 1.7 s per block, run twice (before/after equaliser);
  LDPC 1.1 s. Next: parallel/SIMD slot-sync correlation and front end
  (or the FPGA symbol path).
- Cross-checks: `isdb-fec` type-checks for aarch64 (stable, nightly) and
  wasm32 (stable); not run there. MSRV 1.88 passes.

## 0.10 Real-time push: front end, sync, pipeline (2026-10-25, after §0.9)

Commits `bfc4efd` (`FastFrontend`), `0735f18` (receive batch, sync once,
parallel eq), `b579215` (ISDB-S trim/sync/derotation parallel), `e8d79a2`
(carry-over, pipeline, carrier phase-step fix), `6dfd4cc` (ISDB-S
pipeline), `2b818ec` (`satpipe`, 3 front ends), `34a6d3a` (live-feed test
tolerance), plus docs.

- One 0.826 s ISDB-S3 block, 40 Msps: front end 2.57 → 1.13 s; slot sync
  1.74 s ×2 → 0.17 + 0.07 s; carrier+trim 1.26 s every block → first
  block only (later blocks verify in 0.05 s); receive 5.85 → 0.60 s.
- ISDB-S BS1 1.68 s: 16.5 → 2.9 s (trim candidates were sequential).
- Files: ISDB-S3 2-block 19.4 → 4.2 s; 12-block 9.9 s recording 10.9 s
  (0.91×). Live: decoder keeps up with libiio captures (2.03 s per
  0.826 s block, capture-bound).
- Bit-exact: FastFrontend == reference (proptests, recordings), FPGA
  vectors byte-identical, `fpga:test` passes; TLV/TS outputs identical
  on single-block recordings; ISDB-S acceptance 38 TSs / 0 uncorrectable.
- Bug fixed: carrier choice picked a candidate 48 kHz off on live BS11
  segment 2 (phase step +1.6 rad): phase-step stillness now ranks first.
- Remaining limit: one contiguous block's front end is one sequential
  stream (0.73× at 40 Msps baseline, ~0.9 s native); x86-64-v3 dispatch
  would need an `unsafe` exception (user decision). Carry-over costs ~1
  extra failed slot per block on the 2-block live recordings.

## 0.11 SIMD dispatch decision, front end and LDPC (2026-10-25, after §0.10)

Commits `089e785` (chunked front end), `4804088` (LDPC iteration out of
line), `1664f09` (front end reverted to the fused loop; adds
`tools/sigmf-repeat.py`), plus docs.

- **Dispatch without `unsafe`** (user question): not possible for these
  kernels (VERIFICATION.md "Satellite receive speed"). Facts:
  E0133 on 1.99 and nightly 1.101; a `#[target_feature]` caller may call
  `#[target_feature]` callees safely; pulp 0.22 `Arch::dispatch` runs a
  generic op inside its own `#[target_feature]` fn, safe for us, but (a)
  LLVM's SLP vectoriser leaves the 16-bit dot product scalar once
  unrolled into the inlined op body (front end 2.8–3.9 s vs 1.0 s), (b)
  an `#[inline]` kernel is not inlined into a `#[target_feature]` caller
  (inline compatibility), so it runs the baseline code, (c) a nested
  `Simd::vectorize` from a same-feature caller is inlined again, (d) our
  own `#[target_feature]` kernel called from pulp's generic op is E0133
  (lexical check). pulp `x86-v4` (AVX-512) needs Rust 1.89 > MSRV.
- **Front end**: per-stage profile (callgrind, BS7-like input): dot
  products ≈ 60 % of front-end instructions, the rest thin per-symbol
  integer work. Chunked mixer: faster through `process` (S3 0.99 → 0.88,
  S 1.88 → 1.68 s) but slower via `process_each` in the ISDB-S
  acquisition (1.90 → 2.03–2.17 s) and the 6-block ISDB-S pipeline
  (1.27× → 1.22×); not explained (not the i32 line writes, not the
  callback inside the loop); reverted. Batched dot products across the
  `LOOP_DELAY` look-ahead are exact (symbol k's instant depends on errors
  ≤ k − 7; the queue must be popped as if the planned symbols' errors
  were pushed) but measured slower (1.18 vs 1.07 s): bookkeeping, and the
  kernel no longer vectorised when unrolled ×6.
- **Single-stream limit stays the front end**: ISDB-S3 0.83×, ISDB-S
  0.89× real time on one core. Next options: (1) one `unsafe` dispatch
  entry (user decision) for ≈ +15 %; (2) splitting a stream across two
  cores with a deterministic hand-over — only bit-exact if the loop
  state is reproduced at the seam (the loop is a few i64 registers; a
  second front end started earlier on overlap converges, but equality
  must then be proven, not assumed); (3) the FPGA symbol path.
- **Whole pipeline, long files**: ISDB-S3 12 blocks 0.92× → 0.96× real
  time (LDPC change; output identical); ISDB-S 6 blocks 1.27×.
- **Found, not fixed (pre-existing)**: `isdb-frontend` `format` property
  test `prop_any_loss_is_reported_and_the_rest_kept` fails on seed
  `cc 0781ec8f… # bits = 5, cut_at = 208, cut_len = 282`: after a cut
  the reader accepts payload words as a block (a false resync passing
  the CRC-16). Reproduced on `7cdc4d2` (before this work). The seed was
  not committed (it would fail `mise run check`); fix the reader (e.g.
  require the next header to follow, or a longer check) and add it.

## 0.12 Symbols format 3: CRC-32C trailer (2026-10-26, after §0.11)

The coordinator fixed the v2 reader first (`6006cef`: framing check
against a CRC-16 splice, cost one block per loss); the user then chose
the 32-bit CRC. Commits `5e3e20a` (format, reader, tests, FPGA, CLI
gate), `b048adc` (docs), `c03a2aa` (sidecar records the words' version).

- **Definition:** trailer = 2 words, low first, the standard CRC-32C
  (CRC-32/ISCSI, check 0xE3069283) of seq (4 bytes LE) ‖ payload (LE
  words). CRC-32C over IEEE CRC-32: HD 4 at 49 152–524 288 payload bits,
  where 0x04C11DB7 is HD 3 beyond 91 607 bits (Castagnoli 1993, Koopman
  2002, cited not re-derived). FORMAT_VERSION 3; MIN_ARITH_VERSION 2
  (host drives v2 and v3 devices; same arithmetic).
- **Reader:** v3 on the CRC alone (no framing cost, no lag); v1/v2 keep
  CRC-16 + framing. False accept ≈ 2⁻³² per header position tried after
  a loss: once in ~136 years at 1 loss/s.
- **Host speed:** CRC-32C slicing-by-8 2 831 MB/s vs bitwise CRC-16 173
  MB/s (link 50.6 MB/s); Reader 558 MB/s (v3) vs 137 MB/s (v2).
- **FPGA:** word-per-clock XOR network from the polynomial (11–22 inputs
  per bit, 560 total; two LUT6 levels), seq folded in during the header,
  2 trailer words; bit-exact on regenerated vectors; missing final XOR
  fails at word 169. Yosys +279 LUT / +17 FF (4 175 LUT, 4 583 FF, 64
  DSP). No timing report (no Vivado devices/licence yet).
- **Verification:** v3 loss property without the framing exception;
  v2 property with its seeds; the two found cuts on both; fault
  injection (CRC skipped: v3 property fails, 148/208 grid cuts caught);
  4 500-block v3 file 0 losses, v2 file still reads, 1 000-word cut
  loses exactly 2 blocks; ISDB-S3 BS7/11/17 and ISDB-S BS1/ND2 output
  byte-identical; ISDB-S acceptance 38 TSs / 0 uncorrectable; `mise run
  check` and `mise run fpga:test` pass.

## 0.13 FPGA image build for the PlutoSky R2 (2026-10-26, after §0.12)

Commits `56e1da3`, `ea9e44b`, `4207544` (fpga/tezuka build scripts,
dvb_fpga submodules, sdbin.sh) plus docs. Work tree
`WORK` (outside the repo).

- **Source of the shipped bitstream**: Tezuka v0.3.24's plutoskyr2
  `tezuka.xsa` (Vivado 2023.1, built 2026-10-06, `xc7z020clg484-2`, a
  deliberate "-2" hack per maia-sdr `108aa43`) comes from F5OEO/maia-sdr
  `refactor` at `f7092ba`. Pinned in `fpga/tezuka/build.sh`.
- **Vivado 2026.1 is not a blocker**: the stock design builds and closes
  timing (WNS +0.100 ns, WHS +0.024 ns after upstream's `hold_fix.tcl`),
  23 211 LUT / 32 304 FF / 55.5 BRAM / 106 DSP — within 1 % of Tezuka's
  2023.1 figures. Workarounds: `ADI_IGNORE_VERSION_CHECK`, amaranth-yosys
  for Yosys, the XSA target instead of `make all`, dvb_fpga's nested
  submodules.
- **With SymbolsRx it does not close timing** (after route WNS −3.28 ns,
  WHS −3.65 ns): (1) the block's clock is `util_ad9361_divclk`, 62.5 MHz
  or 125 MHz (÷2 when adc_r1_mode & dac_r1_mode), not the 160 MHz assumed;
  at 125 MHz a 5-DSP48E1 PCIN cascade (one dot product, unpipelined) takes
  9.2 ns > 8 ns (passes at 62.5 MHz, +5.4 ns). (2) `gpio_in` → axi_ad9361
  `up_rdata` crosses ADC clock → clk_fpga_0 without an exception (−4.06 ns
  as a 2 ns synchronous path); quasi-static, needs set_max_delay
  -datapath_only or a register. Next: pipeline the dot products in
  `frontend.py` bit-exactly, add the CDC constraint, rebuild.
- **Deployment path established** (no bitstream yet): the SD card's FAT
  partition holds several bitstreams and U-Boot loads `${bitstream_image}`
  set in `uEnv.txt`; `sdbin.sh` reproduces the card's `system_top.bin`
  byte for byte (sha256 4e9ea382…5c76, read from the board's /boot,
  read-only). The board's /boot is mounted rw: nothing was written.
- The coordinator's earlier "160 MHz" and the 64-DSP Yosys estimate were
  for an idealised block; Vivado's placement of the unpipelined
  multiply-adds is what fails.
- **Timing-fix attempt (2026-10-26): stopped — the block cannot work on
  this board as designed, independently of timing.** The board runs the
  AD9361 in LVDS 2R2T (device tree `adi,lvds-mode-enable`,
  `adi,2rx-2tx-mode-enable`; driver `RXSAMP:40000000` at 40 Msps), so
  `rx_clk` = 4·fs = 160 MHz and `util_ad9361_divclk` = rx_clk/4 = **40 MHz
  = fs**: one clock per sample, not four. (The "160 MHz" was `rx_clk`;
  the 125 MHz failure is the BUFGMUX's 1R1T input, never selected here.)
  `util_wfifo` also delivers samples in bursts of 8 on consecutive
  `dout_clk` cycles. The design assumes `CYCLES = 4` (time-multiplexed
  MAC) and a 16-cycle loop inside `LOOP_DELAY = 6` symbols, which the
  model itself documents as "24 clock cycles at 4 cycles per sample". At
  one sample per clock, 6 symbols can be 6 clocks; the loop path (dot
  product, ×G, TED/power products, loop terms, clamps) needs ~16–20 cycles
  pipelined. Timing at 40 MHz is not the issue (9.2 ns of 25 ns).
  **Model change needed (owned by `isdb-frontend`, not made):**
  `LOOP_DELAY` ≈ 24 symbols (check lock/MER on the recordings first; the
  loop bandwidths 5·10⁻⁶–2·10⁻⁴ R make the extra delay negligible in
  theory), and a MAC budget that fits: full rate is 32 × 6 = 192 DSPs
  (+16) vs 220 on the chip with Tezuka's 106 already used — so fewer
  multiplies (shorter `g` bank, `k` = `h` when untrimmed, fewer taps).
  Both change the bit-exact output and the FPGA vectors. Details in
  fpga/README.md "Status". No new Vivado run was started.

## 0.14 AVX2 / AVX-512 front end (2026-10-26, parallel to §0.13)

Commits `a0b46c2` (dispatch, kernels, tests), `6b7de7c` (helpers as
macros, AVX2 default, examples), plus docs. User-approved: runtime
dispatch with exactly one `unsafe` (`isdb-frontend` `simd::run`).

- **Design**: `simd::Isa` {Baseline, Avx2, Avx512}; `Isa::best()` = AVX2
  if detected else baseline (AVX-512 only via `ISDB_SIMD=avx512`);
  `FastFrontend::with_isa`. The per-sample work is split into `advance`
  (mixer, delay line) and `finish(dots)` (loop, decision), generic and
  inlined; per ISA only `process_*` loops (a `process_loop!` macro) and
  the 16-bit dot kernel differ. `process_each` on vector ISAs runs the
  `process` loop on 4 096-sample blocks into a buffer. AVX-512 needs Rust
  1.89 → `build.rs` probe sets `cfg(isdb_frontend_avx512)`; clippy msrv
  attribute on those items.
- **Measured traps**: (1) generic reduction loops inlined into
  `#[target_feature]` fns come out scalar (61 ns vs 6.7 ns per dot6 with
  intrinsics); (2) `#[target_feature]` helpers can't be
  `#[inline(always)]`, and LLVM left them out of line in one loop → AVX2
  process_each 2.8× slower than baseline; fixed by macros; (3) AVX-512
  slower than AVX2 on Zen 4.
- **Result** (one core, `feisa`, process_each): ISDB-S3 baseline 1.06×,
  AVX2 1.19×, AVX-512 1.07×; ISDB-S 1.25× / 1.40× / 1.28×. One
  contiguous ISDB-S3 stream's front end now ≥ 1× real time. Whole
  pipeline on long files, contended by the parallel Vivado build:
  ISDB-S3 0.95–0.96×, ISDB-S 1.30–1.33×, any ISA.
- **Bit-exact**: every ISA, both entry points, prop tests, all widths,
  recordings; ±1 lane fault fails 3–4 tests; rx-compare byte-identical
  vs `c03a2aa` (4 ISDB-S3, 5 ISDB-S recordings); ISDB-S acceptance 38 TSs
  / 0 uncorrectable.
- **Not done**: LDPC dispatch through the same mechanism (earlier +23 %
  with AVX2 by build flag; LDPC is 12× real time already, not on the
  critical path).

## 0.15 FPGA front end redesigned for one sample per clock (2026-10-26)

Commits `390bbea` (model format 4, r1_mode refusal), `0c242b3` (full-rate
HDL, XDC, image without DVB-S2 TX), `d3fda62` (adders in fabric, XDC at
implementation), plus docs.

- **Clock, measured on the board (read-only):** l_clk 160.0 MHz
  (`adc_clk_count` 0x1999A at reg 0x80000054, debugfs with the
  0x80000000 prefix; version reg reads 0xA0300), r1_mode 0 on both cores,
  LVDS divider 4 ⇒ block clock = 40 MHz = fs, one sample per clock,
  util_wfifo bursts of 8. §0.3/§0.13's "160 MHz" was l_clk. (An earlier
  read of the debugfs registers without the prefix returned 0 for
  everything — misaddressed, not real.)
- **Model format 4:** LOOP_DELAY 24 (free: 14.69/14.71/14.58 vs 14.65/
  14.67/14.58 dB), detector banks on the centre 8 taps (free or better:
  14.89/14.91/14.78 dB). Detector decimation (time-shared h/g) tried and
  rejected: cycle slips on ISDB-S3. Receivers: ISDB-S 38 TSs / 0
  uncorrectable; ISDB-S3 LDPC/BCH-clean, eq. Es/N0 18.0/18.0/17.3 dB.
- **HDL:** fully pipelined, 112 DSP48E1 (64 k + 32 h,g + 16 misc),
  bit-exact (every clock, wfifo bursts, jitter, load port, gpio, live
  BS7/BS1 vectors; LSB faults caught).
- **Image:** without DVB-S2 TX; WNS +0.100, WHS +0.022 (= stock margin),
  178/220 DSP, ADI timing check passes. `isdb_top.bin` 2 785 408 B,
  sha256 479141e0…1fd3 at WORK/out/isdb/. First build
  failed (Vivado mapped adders into DSPs → 220/220; XDC read at synthesis
  before generated clocks existed): fixed with use_dsp="no" and an
  implementation-only, LATE XDC.
- **Open:** deploy on a card copy (user), then the on-board checks in
  fpga/README.md (ID word 0x15DB4020, 0 losses at 51 MB/s, words
  identical to the CPU model on the same capture). Trim reload without a
  full reconfiguration (Encoder::output_bank) still not wired.

## 0.16 Continuous reception through the FPGA, carrier fix (2026-10-26)

The format-4 bitstream (`isdb_top.bin`) runs on the PlutoSky R2 (booted
from SD by the user; ID word 0x15DB4020). Commits `a9c28da` (carrier),
`3a28487` (receiver hint, frame starts, ISDB-S acquire), `314694f`
(`isdb rx` streaming), plus docs.

- **`isdb rx` uses the FPGA front end** (`--offload auto|fpga|cpu`):
  0.2 s IQ probe → acquisition (carrier, trim; up to 3 probes if the lock
  is below 4 dB) → `capture_symbols` on a thread into a channel →
  `symstream`: words parsed on their own thread (drains the capture
  whatever the decoder does), receiver on chunks of 24 frames / 24
  superframes, each call keeps what starts at its last whole frame
  (`Rx::frame_starts`; ISDB-S superframe starts), so the stream is cut on
  its own boundaries and TLV assemblers persist. Loss / lost lock (3
  chunks without a cut) = reported discontinuity. Decode calls paced on a
  chunk of new symbols; Ctrl-C stops at once. `--symbols FILE` decodes an
  `isdb symbols --out` words file.
- **Live 30 s** (RX only): ISDB-S BS1 — 2 716 superframes, 3 TS ×
  347 648 packets RS-clean, 0 CC errors, 0 TEI, 0 losses, receiver 2.5×
  real time: the first gapless TS from the Pluto. ISDB-S3 BS7 — 873
  frames, TMCC 872, 1 LDPC failure / 83 712 slots, 1 loss (1 DMA overflow,
  40 blocks), receiver 1.6×.
- **A first live ISDB-S3 run hung** (decode called on every 4 096-symbol
  block over a growing buffer after frames failed; 3 000 s of CPU, the
  user SIGKILLed it at ~527 s). Fixed by the pacing and the bound
  (`symstream` test `a_decoder_that_finds_nothing_is_paced_and_bounded`,
  fails with the old rule: 192 calls for 200 blocks) and the immediate
  stop. Board checked afterwards: reachable, TX LO off, front end
  answers, IQ capture works.
- **Carrier bug** (task 2): `isdb symbols` took the survey's strongest
  line; on 2 of 7 live 40 Msps BS recordings it was an alias (BS7
  −1001 kHz at −3 dB, BS11 +1052 kHz at −8 dB; carrier ≈ −155 kHz at
  +9 dB). `pick_carrier` was right on all 7 but tried only 4 candidates
  (the carrier was 4th of 5 on BS11). Now every candidate (≤ 8), `isdb
  symbols` uses `pick_carrier` for ISDB-S3; opt-in test
  `isdbs3_carrier_beats_the_survey_aliases` (fails with the
  strongest-line rule). ISDB-S is safe: its z⁸/z⁴ line is corroborated by
  z² on every recording (examples/carrierlines). A live `isdb rx` run
  locked on −760 kHz (1.46 dB) once — reproduced alias on a later probe
  (−759.5 kHz at 1.43 dB, still phase) but the trial picked right there;
  the lock floor + re-probe guards it.
- **Frame markers** (task 3): no problem — the count was "FSync 302 /
  !FSync 17854" (17 854 truncated to "178" in an earlier display); 302 =
  36 312 / 120, and the receiver decodes the same frames. Seen instead:
  +306 Hz residual carrier over 10 s (1.06 rad per SSync pair), followed
  by the receiver's per-slot phase (no FPGA carrier loop).
- **Open**: one live ISDB-S3 run lost lock for ≈ 2 s after a loss (64
  frames without TMCC, 1 restart; AGC 33 → 34 dB); the same cut offline
  recovers at once. Losses log their stream position now. DMA overflows
  come from the libiio stream stalling on a loaded host (≤ 1.1 s queued
  behind the receiver), not the decoder. ISDB-S `slips` counted only on
  final calls. `isdb ts validate` on 10 s satellite TS: CAT missing
  (broadcaster) and the ISDB-T-only `cross.nit` warning.

## 0.17 Coordinator's live check of §0.16 (2026-10-26)

`isdb rx … --radio pluto:uri=ip:192.0.2.30 --seconds 30`, FPGA path,
RX only (board TX LO off before and after):

- **ISDB-S, BS1** (load avg ≈ 3): 30.07 s in 32.2 s wall incl. setup;
  2 715/2 715 superframes; 211 891 symbol blocks, 0 losses; 3 TS ×
  347 520 packets, each **0 CC errors, 0 TEI**, PCR CBR fit ≤ 128 ns over
  30 s, PAT tsid = TMCC TS_ID. First gapless TS from the Pluto: confirmed.
- **ISDB-S3, BS7** (load avg ≈ 7): 1 loss at symbol 342 704 128 (37
  blocks ≈ 4.5 ms, a host-side DMA overflow), then **TMCC on 657 of 836
  frames** — ≈ 179 frames (≈ 6 s) undecoded after the loss before the
  receiver recovered; 9 LDPC failures in 63 072 slots, BCH 0, 0
  head-indication mismatches. Same failure as §0.16's "lost lock after a
  loss" (there ≈ 2 s), worse here. Open, highest priority for ISDB-S3:
  a 4.5 ms gap must not cost seconds. Losses now log their position, so
  the words around it can be captured (`isdb symbols --out`) and the
  recovery reproduced offline with `isdb rx --symbols`.

## 0.18 ISDB-S3 recovery and host-busy handling (2026-10-26)

Commits `e00a873` (Part A), `bba7736` (coordinator: s3drift streams the
file; `*.sym`/`*.tlv`/`data/` ignored), `6556b50` (Part B), plus docs.

- **Part A — the "lost lock after a loss" was carrier drift, not the
  loss.** Live words recorded with a CPU burn (40 s, 2 losses) decode
  offline to 904/957 frames with 3 resyncs, every frame lost from 30 s on
  while the losses themselves recovered at once. `examples/s3drift`: the
  residual carrier drifts −97 → −830 Hz in 30 s (FPGA NCO fixed at the
  acquisition, no carrier loop; drift sign and rate vary run to run, up
  to +1 537 Hz/min). The frame marker's sign is read against the SSync
  slots one slot either side; their sum is 2cos(2πΔf·9296/Rs), zero at
  908 Hz → random frame heads, no TMCC. (Further out the flip is common
  to every slot and the minority rule works again — the synthetic test
  shows heads found at 1 400 Hz — but a drifting stream passes 908 Hz.)
  Fix: `RxOptions::carrier_hz` derotates before sync, `Rx::residual_hz`
  (mean phase step between SSync slots, ±908 Hz unambiguous), tracked in
  `rxs3::run_stream` across calls and losses when the call's sync Es/N0
  > 3 dB. Recording: 1 207/1 207 frames, 0 resyncs. Live 60 s: 1 816/1 816.
  Synthetic: a 4.5 ms cut costs 1 frame (301 → 300); ISDB-S cuts cost the
  touched superframes + ≈ 1 (902 → 899 for two cuts), CC events only at
  the gaps.
- **Part B — the losses are device-side, periodic.** No decoder, words to
  disk: losses every 10.19 s (64–106 blocks) at 50.8 MB/s; none at
  43.4 MB/s. iiod ≈ 72 % of a Pluto core; nothing periodic visible on the
  board (`/proc/<pid>/stat`, interrupts); cause of the ≈ 10 ms stall not
  identified (kernel/DDR/network?). Fix: `kbuffers` (default 16 × 1 Mi
  words); 8 and 16 lossless over 45 s. Host: parser→decoder queue bounded
  to 4 chunks, whole-block drops reported (`blocks_dropped`).
- **Live 60 s** (VERIFICATION.md table): normal load 0 losses both
  systems (ISDB-S 3 TS × 694 144 packets 0 CC); 48 busy threads for 20 s:
  0 radio overflows (2 with kbuffers=4), host drops 11 (S3) / 37 (S)
  reported, 0 resyncs.
- **Open**: the device's 10.2 s stall itself (worth a look at iiod's
  socket/zero-copy options, or Tezuka's network driver); under heavy host
  load the decoder still falls behind (≈ 0.9× real time with 48 busy
  threads) — fewer threads for other work, or a lighter receiver.

## 0.19 Coordinator's live check of §0.18, and the OOM incident (2026-10-26)

- **OOM incident**: Claude Code was killed twice (14:09, 16:14 JST) by
  the kernel OOM killer (61 GB, no swap) while `s3drift` read a 2.3 GB
  .sym whole into Complex<f64> (≈ 27 GB); OOMPolicy=stop then stopped the
  terminal scope. tmpfs also held ≈ 23.8 GB of captures.
  Fixed: `s3drift` streams (bba7736; 45 s file in 13.7 s under a 4 GB
  cap). Data moved to `data/2026-10-26/` (git-ignored, on disk). Rules
  since: no captures/outputs in tmpfs; heavy commands under
  `systemd-run --user --scope -q -p MemoryMax=16G -p MemorySwapMax=0`;
  tools that read captures must stream.
- **Live check** (capped, RX only, load avg ≈ 5): `isdb rx --system
  isdbs3 … --channel BS7 --seconds 60` → 1 816/1 816 frames with TMCC,
  494 867 symbol blocks, 0 losses, 0 host drops, 0 resyncs; 174 336
  slots, LDPC failed 2, BCH 0; sync Es/N0 14.0 dB (17.3 dB equalised).
  Residual carrier drifted +2 → −1 522 Hz during the run — past the
  ±908 Hz point where the receiver used to lose every frame — and was
  tracked. Board TX LO powered down before and after.

## 0.21 ISDB-S3 decoder side: MMT-SI, TLV-SI and media (2026-10-26)

Commits `4d31ca8` (new `isdb-mmt`: descriptors, tables, message and MFU
reassembly, ES framing, decoder; `isdb-tlv` FileReader), `6b89838` (`isdb
tlv inspect / extract / validate`, fixes, tests), plus docs.

- 40 s live BS7/11/17 (8 TLV streams, `data/mmt/live/`): every table
  CRC-clean; MPT (one service per TLV stream), PLT, MH-EIT (97–117
  sections), MH-SDT (ten services), MH-BIT, MH-TOT, NTP, TLV-NIT (network
  000Bh, frequencies), AMT. MMT-SI and TLV-SI text is UTF-8.
- All media scrambled (hev1, mp4a, stpp, aapp): only the 8-byte MPU
  payload header is clear; MPU continuity of timed media 0 jumps. No
  unscrambled ES on air; the media path is checked synthetically.
- Found: control-message fragment_counter is 0 on air (requiring it lost
  141 MH-EIT fragments / 40 s); repeat suppression skipped the CRC of
  repeats (fixed: every copy checked); aapp MPUs jump by design.
- Open: captions / data that are not scrambled would exercise the ES
  path on air (none seen in 40 s); MH-AIT, MH-CDT, ECM/EMM contents not
  decoded (CA / application, out of scope); MPU timestamps are decoded
  but not yet attached to access units in the extracted streams.

## 1. Goals and non-goals

Goals, in order:

1. **ISDB-S receiver** (BS / 110° CS, ITU-R BO.1408 "System" = ARIB
   STD-B20): IQ capture → symbols → frame and TMCC → main signal → MPEG-2
   TS, verified on the live satellite IF of the lab's cable feed.
2. **ISDB-S modulator** (for TX → RX loopback tests, and for a conducted
   test against a real receiver, the PIX-DT295, once it is safe; §2).
3. **ISDB-S3 receiver** (ITU-R BO.2098 "System F" = ARIB STD-B44): IQ →
   symbols → TMCC → LDPC/BCH → TLV packets → MMT/TLV signalling.
4. **FPGA implementations of the same stages** on the PlutoSky R2
   (Zynq-7020), selected at run time like SIMD kernels, so that reception
   can run live over Gigabit Ethernet while every other SDR keeps using
   the CPU chain (§7).

Non-goals (project policy): no descrambling (B-CAS / ACAS); the payload
of BS / CS / 4K services is expected to be scrambled, while PSI/SI (and
TLV/MMT signalling) are in the clear. No radiating: any transmission is
conducted, through attenuators, into a receiver on the bench.

## 2. Lab set-up and safety (read first)

Connections reported by the user on 2026-10-20:

| Port | Connected to |
|---|---|
| RX1 | cable (CATV) feed: terrestrial ISDB-T recast on its UHF channels, and the satellite IF (BS / CS) |
| RX2 | 510 mm 144 / 430 MHz antenna |
| TX1 | **PIX-DT295 (ISDB-T/S USB tuner), directly, no attenuator** |
| TX2 | open, no load |

**Before any TX test on TX1** (all three are required):

1. Insert 30–60 dB of fixed attenuation. A Pluto at low attenuation can
   exceed what a tuner input tolerates (TV tuners overload around
   −20 dBm).
2. Insert a DC block, and/or switch the tuner's LNB power off. A
   satellite tuner input can put 11–15 V DC on the cable for an LNB,
   which would sit on the Pluto's TX port.
3. Transmit with `--rf-enable` only, start at the maximum attenuation
   (`att=89.75`, the backend default) and lower it step by step.

Never transmit on TX2 (open) or RX2's antenna. The project's legal notice
applies (radiating needs a licence; conducted bench only).

**The radio.** PlutoSky R2 (Zynq XC7Z020, AD9361), Ethernet
`ip:192.0.2.30` (static, in the U-Boot environment in flash), booting
**Tezuka v0.3.24 from the SD card** (DIP switches both at 3.3 V; the
factory firmware stays in flash: SW1 3.3 V / SW2 GND boots it). Serial
console on the debug port at 115200 baud (the FTDI vanishes
while the board is off). SSH on the same address.

- **Use Tezuka ≥ v0.3.22.** The PlutoSky's 40 MHz VCTCXO is tuned only
  by an ADF4001 charge pump; the factory firmware leaves it active with
  no 10 MHz reference, which put ≈ 1 rad of phase jitter between ISDB-T
  symbols at UHF. Tezuka three-states it (`/etc/init.d/S22refclk
  status`). With Tezuka the unit is ≈ 0.6 ppm off; a 10 MHz OCXO is
  planned (SMA → MMCX adapter to be bought) and Tezuka locks to it
  automatically. The 10 MHz input level is undocumented: start with a
  10 dB attenuator in line.
- **Boot calibration failure (2 boots in 3).** `dmesg` shows "Tuning TX
  FAILED!" and there is no `cf-ad9361-lpc` (RX) IIO device. Recovery
  without a reboot (on the Pluto):

  ```sh
  D=/sys/bus/platform/drivers
  echo 79024000.cf-ad9361-dds-core-lpc > $D/cf_axi_dds/unbind
  echo spi0.0 > /sys/bus/spi/drivers/ad9361/unbind
  echo spi0.0 > /sys/bus/spi/drivers/ad9361/bind
  echo 79020000.cf-ad9361-lpc > $D/cf_axi_adc/bind
  /etc/init.d/S23udc restart        # iiod re-reads the device list
  ```

  Worth reporting upstream (Tezuka issue tracker; PlutoSky R2 bitstream
  timing margin was +0.100 ns in their build notes, issue #494).
- The board also rebooted once on its own during the session (cause
  unknown).

## 3. Established facts (measured 2026-10-20)

### 3.1 What the feed carries (RX1)

Sweep at 61.44 Msps, 56 MHz analog bandwidth, manual gain 30 dB:

| Band | IF | Content |
|---|---|---|
| BS right-hand | 1032–1488 MHz | 12 transponders, BSn centre = 1049.48 + 19.18·(n − 1) MHz, n odd 1…23 |
| 110° CS | 1592–2068 MHz | 12 transponders, 40 MHz spacing (ND2 … ND24, ≈ 1613–2053 MHz); about 10 dB weaker than BS (cable tilt) |
| BS/CS left-hand | 2.2–3.2 GHz | **nothing** (no left-hand IF on this feed) |

Classification of the BS transponders (|z|² cyclic line, band-edge
steepness):

| Transponders | System | Evidence |
|---|---|---|
| BS1, 3, 5, 9, 13, 15, 19, 21, 23 | ISDB-S | |z|² line at 28.86 MHz, +19…+23 dB; RRC α = 0.35 edges (−3→−20 dB in ≈ 17 MHz) |
| BS7, 11, 17 | ISDB-S3 | |z|² line at 33.756 MHz (+5…+6 dB), none at 28.86; steep edges (−3→−20 dB in 2.5 MHz, α = 0.03) |

The CS transponders were not classified yet (expected ISDB-S; verify).

### 3.2 First ISDB-S capture (BS1, 1049.48 MHz)

One contiguous IIO buffer, 4 194 304 samples at 57.72 Msps (2 samples per
symbol; the AD9361 programs exactly 57 719 999 Hz) = 72.7 ms:

- symbol rate line at 28.8591 MHz (FFT resolution 0.9 kHz): 28.86 Mbaud;
- z⁸ line 26 dB above the median (8PSK main signal), z² and z⁴ lines
  weaker (BPSK TMCC / burst): all three give a carrier offset of
  **−107.2 kHz** (mostly the head-end conversion);
- −3 dB width 28.15 MHz.

Capture recipe (RX only, safe):

```sh
U=ip:192.0.2.30
iio_attr -u $U -i -c ad9361-phy voltage0 sampling_frequency 57720000
iio_attr -u $U -i -c ad9361-phy voltage0 rf_bandwidth 44000000
iio_attr -u $U -i -c ad9361-phy voltage0 gain_control_mode slow_attack
iio_attr -u $U -c -o ad9361-phy altvoltage0 frequency 1049480000   # RX LO
iio_readdev -u $U -b 4194304 -s 4194304 cf-ad9361-lpc voltage0 voltage1 > bs1.cs16
```

The output is interleaved little-endian int16 I/Q, 12-bit values
sign-extended (scale 1/2048). Only **one buffer is contiguous**: across
buffers the network cannot keep up at these rates (see §7), so capture
single large buffers (4 Mi samples worked; try larger, and Tezuka's
maia-sdr recorder, which reserves 256 MB of DDR, for ≈ 1 s captures). A
superframe (8 frames × 39 936 symbols) is ≈ 11.1 ms, so one 73 ms buffer
holds ≈ 6 superframes. ISDB-S3 at 33.756 Mbaud needs > 61.44 Msps for 2
samples/symbol; use 61.44 Msps (1.82 samples/symbol) and resample.

The session's captures and scripts lived in a temporary scratchpad and
will not be there for you: re-capture.

### 3.3 Hardware limits

- AD9361 RX 70 MHz – 6 GHz (BS/CS IF fine), sample rate ≤ 61.44 Msps,
  analog RX bandwidth ≤ 56 MHz.
- Gigabit Ethernet through iiod: 512/63 MHz cs16 (32.5 MB/s) streams in
  real time in one direction; TX + RX together dropped RX samples on the
  factory firmware (one CPU; Tezuka enables two, not retested).
- `isdb-iio` accepts only 16-bit channels; Tezuka has 8-bit (CS8) and
  CS12 sample modes with "DDC routing" (Tezuka README: "Complex 8-bit
  mode … 45 MHz over GbE"; details in Tezuka issue #494 and
  maia-sdr's `doc/cs12-stream.md`). Supporting them needs an `isdb-iio`
  change (sample size check) and a format in `isdb-radio`.
- Zynq XC7Z020 PL: 53 200 LUTs, 106 400 flip-flops, 140 BRAM36 (4.9 Mbit),
  220 DSP48E1. Tezuka's PL already holds maia-sdr (spectrometer,
  recorder, DDC) and, on this board, a DATV path.

## 4. Specification sources

| Document | Status | Use |
|---|---|---|
| **ITU-R BO.1408-1** (04/2002), "Transmission system for advanced multimedia services provided by ISDB in a broadcasting-satellite channel" | free: <https://www.itu.int/rec/R-REC-BO.1408-1-200204-I/en>; English PDF 606 342 bytes, sha256 `8c5744c06bb4c2bc70211b528a884b1cd85e31af6aef4b8404f50fce76c3a39f` | ISDB-S (the system of STD-B20) |
| **ARIB STD-B20** v3.x | **Japanese only, sold** to non-members ([ARIB page](https://www.arib.or.jp/english/std_tr/broadcasting/std-b20.html)); no English edition (the `6-STD-B20v3_*-E1.pdf` URLs are 404) | would close BO.1408's gaps (below); ask the user whether to buy it |
| **ITU-R BO.2098-0** (12/2016), "Transmission system for UHDTV satellite broadcasting" | free: `https://www.itu.int/dms_pubrec/itu-r/rec/bo/R-REC-BO.2098-0-201612-I!!PDF-E.pdf`; 1 121 743 bytes, sha256 `f73ff79a27a6c414147d5143fd107245785095d47e43378384aef7e044cfa3ab` | ISDB-S3 summary (Annex 1, "System F") |
| **ARIB STD-B44 v2.0, English** | free: <https://www.arib.or.jp/english/html/overview/doc/6-STD-B44v2_0-E1.pdf> (cited by BO.2098; the URL answered with a PDF) | ISDB-S3 in full: LDPC parity-check tables, BCH, TMCC, pilot, interleaver |
| ARIB STD-B32 Part 3 (TLV multiplexing) | listed in SPECS.md, **not in `specs/` on this machine** | TLV packet format of ISDB-S3 |
| ARIB STD-B60 (MMT-SI) and the MMT standard (ISO/IEC 23008-1) | not local; availability to check | MMT/TLV signalling of 4K/8K services |
| ARIB STD-B10 | local | SI of BS/CS (network_id, BIT, SDTT …) |

Add every document the way SPECS.md does (row with version, URL, sha256,
local PDF and text extraction in the git-ignored `specs/`, `pdftotext
-layout`), and record each reading that is not settled by the text in the
crate's SPEC-NOTES.md, with its status.

### 4.1 What BO.1408-1 gives exactly, and its gaps

Exact (Annex 1 and Appendix 1, the Japanese system):

- outer code RS(204,188) shortened from RS(255,239), g(x) = Π(x + λⁱ),
  i = 0…15, λ = 02h, p(x) = x⁸ + x⁴ + x³ + x² + 1 (§2);
- framing: data frame of N = 48 slots of 204 bytes (203 on air: MPEG
  sync bytes not sent), superframe of M = 8 frames, slot assignment from
  the most to the least efficient modulation-code combination with dummy
  slots, equations (1)–(4) (§3);
- energy dispersal PRBS 1 + x¹⁴ + x¹⁵, initial state `100101010000000`,
  loaded at the second byte of every superframe, sync bytes skipped
  (§4);
- inter-frame block interleaving of depth M (§5, Fig. 6);
- TMCC information format (Fig. 7–10, Table 1): order of change,
  modulation-code combinations (word per combination: 0001 BPSK 1/2 …
  0111 TC8PSK 2/3, 1111 dummy) with slot counts, relative TS ID per
  slot, table relative TS ID → MPEG TS_ID (16 bits), other information;
  Appendix 1: CM = 4 combinations, TM = 8 TSs, RS(64,48) outer code, no
  interleaving, other information = order of change 5 bits, emergency
  alert flag 1 bit, uplink information 4 bits, reserved 62 bits; 384 bits
  in all;
- TMCC timing references: W1 = 1B95h at the start of each of the M
  blocks, W2 = A340h at the end of the block of frame 1, W3 = 5CBFh
  (= ¬W2) at the end of the others; TMCC randomised with the main PRBS
  starting at the 3rd byte of the first block (§6.3, §6.4);
- burst: 4 symbols every 203 symbols of the coded main signal (§7);
- inner code: K = 7, 171/133 octal, puncturing of Table 2 (QPSK 1/2,
  2/3, 3/4, 5/6, 7/8, BPSK 1/2, TC8PSK 2/3), P1/P0 order, BPSK sends P1
  then P0, TC8PSK takes B1 uncoded and B0 coded (§9, Figs. 13–15),
  constellations of Fig. 15;
- RRC roll-off 0.35; Table 3: 38 976 main-signal symbols and 39 936
  symbols per frame (TMCC and bursts included).

**Update 2026-10-20 (Phase 0):** MIC Ordinance No. 87 of 2011, annexed
tables 45–51 (free, e-Gov; SPECS.md §1.4) settle the burst PRBS (X⁹+X⁴+1,
init 111101101, per frame, burst symbols only), the frame layout (192-symbol
header W1 / TMCC 8 bytes / W2|W3, then 192 × (203 + 4)), BPSK for TMCC and
burst, the bit mappings and puncturing phase. STD-B20 will not be bought.
Still open: the TMCC information bit layout (MIC Notification 304/2011,
not online in full; use BO.1408 Figs. 7–10), and the burst's data before
randomisation (assume all zeros; verify on the signal).

Left to "the system" by BO.1408 (= in STD-B20 only), to be resolved from
the live signal and recorded as interpretations (mostly superseded by the
update above):

- the random sequence of the burst symbols (§7: "an appropriate random
  sequence");
- the exact placement of TMCC and burst symbols in the 39 936-symbol
  frame, and the TMCC modulation (BPSK r = 1/2 is implied by "most
  robust"; verify);
- bit order of the TMCC fields beyond Fig. 7's field order;
- whether the TMCC is interleaved (Appendix 1: "not applied").

### 4.2 Reuse from the ISDB-T crates (checked)

| Need | Existing code |
|---|---|
| RS(204,188) | `isdbt-fec::rs` (same field and generator family; check the generator roots match BO.1408 §2) |
| K = 7, 171/133 convolutional code, puncturing, Viterbi | `isdbt-fec::conv` (G1 = 0o171, G2 = 0o133); check the puncturing serialisation (P1/P0) and add BPSK and TC8PSK branch metrics |
| PRBS 1 + x¹⁴ + x¹⁵, init `100101010000000` | `isdbt-fec::energy` (same polynomial and initial state; ISDB-S resets per superframe, not per OFDM frame) |
| TS reader, PSI/SI, analysis | `isdb-ts`, `isdb-si`, `isdb-analyze` (BS/CS network_id and BIT/SDTT usage) |
| IQ capture and streaming | `isdb-radio` (`pluto` backend), `isdb-iio` |

Consider moving the system-independent pieces (RS, convolutional code,
PRBS) into a shared crate once two systems use them.

## 5. Proposed architecture

New crates, mirroring the `isdbt-*` split:

- `isdbs-core`: ISDB-S parameters (N, M, B, symbol counts, combinations,
  slot assignment from equations (1)–(4)), TMCC information codec.
- `isdbs-modem` (or `isdbs-phy`): RRC filtering, symbol timing
  (Gardner / Mueller–Müller on 2 sps), carrier recovery (burst- and
  TMCC-aided, then decision-directed on 8PSK; BO.1408 §7 warns about
  false PLL lock and suggests counting cycles over the TMCC), frame
  sync on W1/W2/W3, slot demultiplexing, TC8PSK / QPSK / BPSK soft
  demapping, Viterbi, deinterleaving, derandomising, RS, TS output per
  relative TS ID. Also the modulator (for loopback).
- `isdbs3-core`, `isdbs3-fec` (LDPC 44 880 and (31 680, 9 614) TMCC
  code, BCH (65 535, 65 343, t = 12) and (9 614, 9 422) shortened, bit
  interleaver, π/2-BPSK … 32APSK mapping), `isdbs3-modem` (roll-off
  0.03, pilot-aided equalisation, 120-slot frame, sync signals),
  `isdb-tlv` (TLV packets) and later `isdb-mmt` (MMT-SI).
- CLI: a generic `isdb capture` (radio → IQ file at any rate, for
  datasets) and `isdb srx` / `isdb s3rx` (or `isdb rx --system …`);
  satellite channel tables (BSn, NDn) like the UHF table of ISDB-T. The
  current `isdb rx` is ISDB-T specific (512/63 MHz, UHF band).

**The receiver is a chain of stages with defined boundaries**, so that a
stage can run on the CPU or in a radio's FPGA (§7) without the rest of
the chain noticing:

| Boundary | Content |
|---|---|
| `Iq` | complex samples at the radio's rate (cs16, cs12, cs8 or cf32) |
| `Symbols` | matched-filtered, timing-recovered samples at 1 sample/symbol, 8-bit fixed-point I/Q, with frame-position and carrier-phase metadata |
| `SoftBits` | demapped soft bits (LLRs, fixed width) per slot, with slot / modulation metadata from the TMCC |
| `Packets` | TS packets (ISDB-S) or TLV packets (ISDB-S3), with error flags |

Each stage is a type implementing one trait per boundary pair (for
example `Stage<In = Symbols, Out = SoftBits>`), and every boundary format
is defined bit for bit (fixed-point widths, scaling, metadata layout) in
the crate docs, because the FPGA has to produce exactly the same bits.

## 6. Phases

Each phase ends with tests, docs (crate README, SPEC-NOTES, SPECS.md
coverage rows, VERIFICATION.md) and a commit.

**Phase 0 – Housekeeping.** Merge decision for `feat/pluto-backend`. Add
BO.1408-1, BO.2098-0 and STD-B44 v2.0 to `specs/` and SPECS.md. Ask the
user about buying STD-B20.

**Phase 1 – Datasets and capture tooling.**
- `isdb capture --radio pluto:… --freq … --rate … --samples …` (one
  contiguous buffer per capture; refuse more than the backend can stream
  contiguously, or chain captures with an explicit gap marker).
- Capture every ISDB-S BS transponder, a few CS transponders and the
  three ISDB-S3 transponders; store outside the repo and expose them to
  tests through `ISDB_RS_*` variables as [docs/test-data.md](../test-data.md)
  describes (tests print `SKIP` without them).
- Classify the CS transponders.

**Phase 2 – ISDB-S front end and TMCC (offline).**
- Matched filter, timing and carrier recovery; symbol-rate and offset
  estimates must reproduce §3.2.
- Frame sync on the TMCC timing references; superframe sync on W2/W3.
- TMCC decode: derandomise, RS(64,48), parse Fig. 7–10.
- Resolve and document the BO.1408 gaps (§4.1) from the signal.
- **Acceptance:** TMCC decoded with RS success on every ISDB-S
  transponder; the relative-TS-ID table gives TS_IDs that later match the
  NIT / SDT; spec vectors from Table 3 (N, M, B, ID, symbols per frame)
  asserted.

**Phase 3 – ISDB-S main signal to TS.**
- TC8PSK 2/3 (and the QPSK/BPSK combinations present), Viterbi,
  inter-frame deinterleave, derandomise, RS(204,188), sync-byte
  reinsertion, per-TS output.
- **Acceptance:** RS-clean TS from at least three BS and two CS
  transponders; 0 continuity errors; PCR CBR fit; PSI/SI parse
  (`isdb ts inspect` / `validate`); scrambled payloads recognised as
  such.

**Phase 4 – ISDB-S modulator and loopback.**
- Modulator from the same crates; software loopback with impairments
  (AWGN, CFO, timing offset), spec vectors, property tests, as the
  ISDB-T crates do.
- Conducted test into the PIX-DT295 **only after §2's prerequisites**,
  and only if the tuner is usable from Linux (§8).

**Phase 5 – ISDB-S3 receiver.**
- STD-B44 front end at 61.44 Msps (1.82 samples/symbol, resample),
  roll-off 0.03, sync signals, pilot-aided equalisation.
- TMCC: BCH (9 614, 9 422) + LDPC (31 680, 9 614).
- Main signal: LDPC 44 880 (normalised min-sum, layered), BCH,
  descramble, TLV packets out; then MMT-SI.
- **Acceptance:** TMCC decoded on BS7, 11, 17; LDPC/BCH-clean frames;
  TLV packets parse; MMT signalling tables decoded.
- Performance risk: LDPC at ≈ 100 Mbit/s on the host needs SIMD and
  threads; plan offline first (scalar reference, then `CpuSimd` kernels
  dispatched at run time, §7.2).
- For Phases 2–5: the stages that will move to the FPGA (§7.3) get a
  frozen fixed-point variant, with their boundary formats (§5) documented
  bit for bit.

**Phase 6 – FPGA implementations of the CPU stages** (§7): only after
the CPU chain of Phases 2–5 is complete and its fixed-point stages are
frozen as the reference.

**Phase 7 – Documentation and verification.** README feature list and
roadmap, VERIFICATION.md rungs for ISDB-S / ISDB-S3, CHANGELOG.

## 7. CPU first, then the same stages in the FPGA

**Principle.** Every receiver stage is implemented on the CPU in this
repository first, and that implementation is the reference. The FPGA
implementations come afterwards and are *alternative implementations of
the same stages*, selected at run time the way a SIMD kernel is: the
receiver works with any SDR that delivers IQ (HackRF, a plain ADALM-Pluto,
an IQ file), and when the radio can run some stages itself (a PlutoSky R2
with the offload image) those stages move into its FPGA and the output is
the same, bit for bit.

### 7.1 Why the FPGA path is needed at all

At 2 samples/symbol and 16-bit I/Q, ISDB-S needs 231 MB/s and ISDB-S3 (at
61.44 Msps) 246 MB/s, against ≈ 110 MB/s of usable Gigabit Ethernet, and
the Zynq's CPUs cannot copy and scan much over ≈ 100 MB/s (Tezuka issue
#494). With a CPU-only chain, live reception from a Pluto over Ethernet is
limited to captures; with stages in the FPGA the data crossing the link
shrinks:

| Stages in the FPGA (boundary leaving the radio) | ISDB-S | ISDB-S3 |
|---|---|---|
| none, 16-bit `Iq` at 2 sps | 231 MB/s | 246 MB/s (61.44 Msps) |
| none, 8-bit `Iq` (Tezuka CS8) at ≈ 1.25 sps | 72 MB/s | 84 MB/s |
| front end → `Symbols` (8-bit I/Q, 1 sps) | 58 MB/s | 68 MB/s |
| front end → `Symbols` (6-bit I/Q, 1 sps; **chosen**, §0.3, §0.6) | 43.4 MB/s | 51.0 MB/s |
| … → `SoftBits` (5-bit LLRs) | 54 MB/s (TC8PSK: 3 per symbol) | 63–105 MB/s (8PSK … 32APSK): no gain over `Symbols` |
| … → `Packets` (all TSs / one TS) | 6.5 MB/s / ≤ 3 MB/s | ≈ 12.5 MB/s |

**Measured (§0.3):** the Pluto's usable link is ≈ 70 MB/s (iiod and raw
TCP alike; the CPU, not the cable, limits it), so 8-bit IQ for ISDB-S
(≥ 78 MB/s at (1 + α)·R) does not fit, and the first FPGA stage is the
front end with 5- or 6-bit symbols, about 60 % of the ceiling. The order
of §7.3 was changed at the user's request: the front end went to the
FPGA before Phases 2–5, with its CPU fixed-point model first, as §7.2
requires.

### 7.2 Dispatch, as for SIMD

- **One trait per boundary pair**, several implementations:
  `CpuScalar` (the reference, portable), `CpuSimd` (x86-64 AVX2 /
  AVX-512, aarch64 NEON, chosen with `is_x86_feature_detected!` /
  `is_aarch64_feature_detected!`; for the hot kernels: RRC filter,
  Viterbi add-compare-select, LDPC check-node updates, demapping), and
  `Fpga` (the stage runs in the radio; the host stage is a pass-through
  that reads the radio's output boundary).
- **Capability discovery.** A radio backend reports what it can do
  (`Backend::stages()` or similar in `isdb-radio`): the boundaries it can
  emit and, for each, the system and parameters it supports and a format
  version. The PlutoSky offload image announces them through IIO (a
  dedicated IIO device per output boundary, e.g. `isdbs-symbols`, with an
  attribute carrying the format version), so discovery is a libiio lookup
  and an old or missing image simply offers `Iq` only.
- **Split selection.** The receiver builds the chain from the deepest
  boundary the radio offers and the CPU stages after it; it falls back to
  a shallower boundary (ultimately `Iq`) when the radio's version is
  unknown, the stage is busy, or the user forces it
  (`--offload none|auto|<boundary>`), exactly like forcing the scalar path
  in a SIMD library.
- **Same results.** A stage's implementations must agree **bit for bit**
  at its output boundary (fixed-point stages), or within a documented
  tolerance where the CPU reference is floating point (then the FPGA
  matches the fixed-point model, which is part of the CPU code). This is
  why the fixed-point model is written first and frozen before any HDL.
- **Status (2026-10-25, §0.9):** first `CpuScalar` / `CpuSimd` pair
  done: ISDB-S3 LDPC (`isdb-fec` `ldpc::fixed`), bit-exact by test, the
  float decoder kept as the floating-point reference (`isdb rx --ldpc
  fixed|scalar|float`). Back ends: portable auto-vectorised arrays
  (stable) or `std::simd` (nightly, detected by the build script). No
  runtime ISA dispatch: `#[target_feature]` calls need `unsafe`, denied
  workspace-wide; AVX2 via `-C target-cpu` gives +23 % (4 678 → 5 739
  cw/s per thread). Decide whether a documented `unsafe` exception for
  dispatch is wanted.
- **Status (2026-10-25, §0.11):** runtime dispatch without `unsafe`
  measured and not adopted. E0133 holds on stable and nightly; `pulp`
  (safe token) works mechanically but the kernels either inline and go
  scalar (front end) or gain ≤ 6 % only when forced inline while the
  baseline loses (LDPC). The only remaining route to runtime AVX2 is one
  documented `unsafe` call at a dispatch entry with safe
  `#[target_feature]` kernels below it (callable from a caller with the
  same features); worth at most +15–23 % on these kernels — a user
  decision. `-C target-cpu=native` stays the documented way.

### 7.3 Order of work

1. **CPU reference chain** (Phases 2–5): floating-point where convenient,
   then a **fixed-point variant of every stage planned for the FPGA**
   (front end: AGC, RRC, timing and carrier loops; ISDB-S: frame sync,
   TMCC, Viterbi, deinterleave, RS), with the boundary formats of §5
   frozen and documented. Tests: fixed-point vs floating-point on the
   captures (MER / BER within a stated margin), bit-exactness between
   `CpuScalar` and `CpuSimd`.
2. **8-bit IQ** (no HDL): Tezuka's CS8 / CS12 modes behind the existing
   `Iq` boundary; extend `isdb-iio` (8-bit and 12-bit packed channels) and
   `isdb-radio` (formats), measure sustained throughput and losses.
3. **FPGA front end → `Symbols`** (useful to both systems), then
   **ISDB-S → `Packets`** (a 64-state K = 7 Viterbi at 28.86 Msym/s fits
   the PL comfortably). Each FPGA block is verified against the CPU
   fixed-point stage: the CPU stage writes test vectors (input and
   expected output at the boundary) that the HDL simulation must
   reproduce exactly, and a hardware-in-the-loop test (opt-in through an
   `ISDB_RS_*` variable, like the other hardware tests) feeds the same
   capture through both paths and compares the boundary bit for bit.
4. **ISDB-S3 LDPC in the FPGA** only if a resource estimate shows it fits:
   a 44 880-bit decoder at ≈ 100 Mbit/s is unlikely to fit next to
   maia-sdr in an XC7Z020; the realistic FPGA boundary for ISDB-S3 is
   `Symbols` or `SoftBits`, with LDPC on the host (`CpuSimd`).

### 7.4 FPGA practicalities

- Tezuka builds its bitstreams from maia-sdr (Amaranth HDL in Python,
  Rust firmware, Vivado Tcl; Tezuka issue #494 used Vivado 2023.2). Add
  the blocks there or as a separate PL design for an SD-card image;
  never flash (Tezuka: "always boot via SD card"). Decide early whether
  the HDL lives in this repository (e.g. `fpga/`, built outside cargo,
  like `oracle/`) or in a fork of maia-sdr / Tezuka; the CPU reference
  and the test vectors stay here either way.
- Zynq XC7Z020 PL: 53 200 LUTs, 106 400 flip-flops, 140 BRAM36
  (4.9 Mbit), 220 DSP48E1, shared with maia-sdr (spectrometer, recorder,
  DDC) and, on this board, a DATV path. The PlutoSky R2 build had the
  smallest timing margin of Tezuka's 12 boards (+0.100 ns): budget for
  timing closure, or drop unused blocks (DATV) in a dedicated image.
- Expose each output boundary as an IIO buffer so `isdb-iio` reads it
  like any other device (no new transport), with the boundary's format
  version as an attribute.

## 8. Verification plan

Same ladder as ISDB-T (VERIFICATION.md), plus one rule for the
interchangeable implementations of §7: every implementation of a stage
(`CpuScalar`, each `CpuSimd` variant, `Fpga`) is tested against the
reference at its output boundary. CI runs scalar vs SIMD on generated
and captured inputs. The FPGA runs in HDL simulation against the CPU's
test vectors, and in an opt-in hardware-in-the-loop test against the
same capture.

1. Spec vectors: BO.1408 Table 3 numbers, Table 1 words, Table 2
   puncturing, W1/W2/W3, PRBS state; STD-B44 tables.
2. Lossless round trips and property tests (TMCC codec, slot assignment).
3. TX → channel → RX software loopback, byte-exact.
4. Independent oracle: none known for ISDB-S / ISDB-S3 (gr-isdbt is
   ISDB-T only). Candidate: the **PIX-DT295** as a reference receiver
   (TS from the same transponder, compared byte for byte). Its Linux
   support is unknown: no driver is loaded for USB 06b8:103b, `px4_drv`
   was not confirmed to support it, and its Windows driver is named
   `pxmb86m0x_usb.inf`; check whether it can output a raw TS at all.
5. Real broadcasts: RS-clean TS (ISDB-S) and LDPC-clean frames (ISDB-S3)
   from the feed, with PSI/SI or MMT-SI parsing, recorded without naming
   the stations.
6. Real receiver: the modulator into the PIX-DT295, conducted, after §2.

## 9. Risks and open questions

- BO.1408 gaps (§4.1) without STD-B20; interpretations must be checked
  on several transponders.
- Contiguous capture length: a 4 Mi-sample buffer works; larger buffers,
  the maia-sdr recorder and CS8 streaming are untested.
- Carrier recovery: −107 kHz offset and 8PSK false-lock risk; the cable
  feed's conversion may add phase noise (check the TMCC/burst phase
  stability as was done for ISDB-T).
- Pluto boot calibration failures (§2) and an unexplained reboot.
- Host performance for ISDB-S3 LDPC.
- MMT-SI / TLV specification availability (B32 Part 3, B60).
- PIX-DT295 Linux usability.
