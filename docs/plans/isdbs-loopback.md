# Plan: ISDB-S transmitter and loopback (TX ⇄ RX like ISDB-T)

Written 2026-10-29 as the design for Phase 4 of [isdb-s.md](isdb-s.md)
("ISDB-S modulator and loopback"). The receiver (`isdb rx --system
isdbs`) exists and is verified on 21 live transponders; this plan adds
the transmit direction so that, as for ISDB-T, `isdb tx` writes a signal
that `isdb rx` decodes byte-exact and `isdb loopback` checks the whole
chain under impairments in one process. Follow
[CONTRIBUTING.md](../../CONTRIBUTING.md) (spec-first, SPEC-NOTES,
conventional commits, no real station identities).

## 1. What exists and what is missing

Transmit-direction code already in the tree (all tested as the inverse
of the receiver):

| Stage | Where | Status |
|---|---|---|
| TMCC information → 8 header blocks (W1, TMCC bytes with RS(64,48) and dispersal, W2/W3) | `isdbs_core::tmcc::headers` | done |
| burst bits per frame (X⁹+X⁴+1) | `isdbs_core::burst::frame_bits` | done |
| TS packets (RS-coded, 204 B) of a superframe → dispersal, depth-8 interleave, sync bytes dropped → 203 B per sent slot per frame | `isdbs_core::mainsig::frame_bytes` | done (`prop_superframe_round_trip`) |
| frame bytes → 38 976 main symbols (BPSK, punctured QPSK, TC8PSK; encoder state after W2/W3) | `isdbs_phy::demod::encode_frame` | done (`prop_frame_round_trip`) |
| header (coded BPSK) + bursts + main symbols → 39 936-symbol frame | `isdbs_phy::testsig::Synth` (test-only) | to be promoted |
| RS(204,188) encoder | `isdb_fec::RS_204_188.encode_in_place` | done |
| RRC pulse (α = 0.35) and Kaiser window | `isdb_frontend::tables::{rrc, kaiser}` | done (RX side) |

Missing:

1. a **pulse shaper** symbols → IQ at any sample rate ≥ (1 + α)·R
   (fractional samples per symbol: 40 Msps / 28.86 Mbaud = 1.386), the
   inverse of `isdb-frontend`'s matched filter and timing recovery;
2. a **`TmccInfo` builder** from a slot layout (combinations and TS_IDs
   with slot counts), with validation;
3. an **ISDB-S `Transmitter`** (TS packets per TS → superframe of
   319 488 symbols), and `frame_symbols` promoted from `testsig`;
4. the **CLI**: `isdb tx --system isdbs`, `isdb loopback --system
   isdbs`, a SigMF sidecar on `--iq` output so `isdb rx --iq` reads it
   back, a per-TS_ID verifier, a channel model with the CFO in Hz;
5. tests, docs, VERIFICATION.md rung, CHANGELOG.

## 2. Hardware limit (stated, not solved here)

ISDB-S at its fixed symbol rate needs ≥ 38.96 Msps of samples. To a
Pluto that is 40 Msps × 4 B (cs12 in i16) = 160 MB/s, above Gigabit
Ethernet (≈ 65 MB/s measured, VERIFICATION.md "8-bit captures and
streaming"); a HackRF stops at 20 Msps. So:

- `isdb tx --system isdbs --iq FILE` and `isdb loopback --system isdbs`
  are the deliverable and must work completely;
- `isdb tx --system isdbs --radio …` is wired through the same `IqSink`
  (the gate, dry run and the backends' validation apply unchanged) and
  documented as "will underrun on a Gigabit Ethernet Pluto; HackRF
  refuses the rate". No special casing; the sink reports underruns as it
  does for ISDB-T. A modulator in the PlutoSky's FPGA (the mirror of the
  symbol front end) is future work.

## 3. Signal and timing

- Symbol rate R = 28.86 Mbaud, roll-off 0.35 (BO.1408-1 §9; MIC
  Ordinance 87 annexed table 47; `isdb_iq::survey::KNOWN_SYSTEMS[0]`).
- Frame 39 936 symbols = 1.3838 ms; superframe 8 frames = 319 488
  symbols = 11.07 ms. At 40 Msps a superframe is 442 810.81 samples:
  **not an integer**, so the shaper keeps its fractional phase across
  calls and the samples per superframe vary by one.
- Unit of the transmit API: the **superframe** (the TMCC, the
  dispersal and the interleaver all span one superframe; the receiver
  decodes superframes independently, each frame's Viterbi starting from
  its own W2/W3 state and ending on the known W1).
- Lead-in: 1 null superframe by default (the front end's timing loop
  and AGC settle in well under a frame; the trim search uses the first
  0.15 s or the whole signal). Flush: 1 null **frame** (39 936 symbols)
  after the last data superframe, so that the shaper's and the front
  end's filter tails do not cut the last superframe. There is no
  interleaver memory across superframes, so nothing else drains.
- Level: the shaper's output has RMS 1 for unit-modulus symbols. The
  CLI writes ISDB-S samples at `--backoff-db` below full scale in every
  format (default 10 dB → RMS 0.316), **cf32 included**, because the
  ISDB-S receiver reads samples as a fraction of full scale (its front
  end input is 12-bit, `isdbs_phy::frontend::to_input` clips at ±1.0),
  unlike the ISDB-T receiver, which is scale-free. The loopback's
  channel measures the signal power itself, so the SNR is unaffected.
- Mapping: `isdbs_phy::demod::Mapping::default()` (the readings settled
  on air: TC8PSK C1 = 133₈, QPSK P1 on I, no inversion).

## 4. Library design

### 4.1 `isdb-frontend::shape` (system-independent, Agent A)

```rust
pub struct Shaper { … }
impl Shaper {
    /// `sample_rate / symbol_rate` ≥ 1 + roll_off (else ShapeError::Rate);
    /// `span` symbols on each side of the pulse (default 16).
    pub fn new(sample_rate: f64, symbol_rate: f64, roll_off: f64) -> Result<Self, ShapeError>;
    pub fn with_span(self, span: usize) -> Result<Self, ShapeError>;
    pub fn sps(&self) -> f64;
    /// Shape `symbols` (any scale) into `out` (appended); the output
    /// samples of the interval covered by these symbols, fractional phase
    /// kept across calls. Output RMS = 1 for unit-modulus symbols.
    pub fn push(&mut self, symbols: &[Complex32], out: &mut Vec<Complex32>);
    /// The tail (the last `span` symbols' pulses) and reset.
    pub fn finish(&mut self, out: &mut Vec<Complex32>);
}
```

Implementation: polyphase RRC bank over `2·span` symbol taps, P phases
(P = 1024 or linear interpolation between fewer phases; the agent
chooses and proves it with the test below), the pulse from
`tables::rrc` (same definition as the matched filter so the cascade is a
raised cosine), windowed over ±span symbols (Kaiser as the RX uses or a
plain truncation; state the choice), scaled so that unit-modulus symbols
give RMS 1 at the output. Per output sample the symbol-time advances by
1/sps; the delay line holds the last 2·span symbols. f32 output, f64 or
f32 accumulation (the agent measures).

Tests (`shape.rs` unit tests and `tests/shape.rs`):
- `prop_chunking_is_invisible`: any chunking of the symbol stream gives
  the same samples (bit for bit) as one `push`.
- `shaped_signal_is_isi_free`: random 8PSK through the shaper at sps ∈
  {40/28.86, 2, 61.44/28.86, 57.72/28.86}, then an ideal matched filter
  (the same pulse, float) sampled at the exact symbol instants: MER ≥
  40 dB (print the figure in the test name or assert message).
- `output_rms_is_one`, `finish_emits_the_tail_and_resets`, `rate_below_nyquist_is_refused`.
- Through the real receive front end: `isdb_frontend::fast::FastFrontend`
  on the shaped signal at 40 Msps (`Design::for_system(ISDB-S, 40e6)`,
  trim 0) recovers symbols whose 8PSK MER (`metrics::psk_mer`) is ≥
  25 dB after the loop settles (the 6-bit output quantiser bounds it).

### 4.2 `isdbs-core`: the TMCC builder (Agent B)

```rust
impl TmccInfo {
    /// BO.1408-1 §3.2, §6.1, Figs. 7–10: `combinations` (word, slots), in
    /// any order, sorted by efficiency; `ts` = (TS_ID, sent slots per
    /// frame) for relative TS 0, 1, …; the sent slots are assigned to the
    /// TSs in slot order. Errors: the existing `TmccError`s plus
    /// `TsSlots { given, sent }` (the TS slot counts do not sum to the
    /// sent slots) and `TooManyTs(n)` (> TM = 8).
    pub fn assign(combinations: &[(Combination, u8)], ts: &[(u16, u8)]) -> Result<Self, TmccError>;
}
```

Unused combination entries are `CombinationEntry::DUMMY`; unused TS_ID
entries `UNUSED_TS_ID` (FFFFh); dummy slots take the relative TS of the
previous sent slot (the receiver ignores it); `order_of_change` 0,
`emergency` false, `uplink` and `reserved` as observed on the BS
transponders (SPEC-NOTES "tmcc-other": uplink 0010, reserved = first
bit 0 then 61 ones), documented as observed values the caller may
change. Tests: the BS layout (TC8PSK 2/3 × 48, three TSs of 16) decodes
to the slot tables the lab TMCCs show; the CS layout (QPSK 3/4 × 48,
one TS of 36); a mixed layout; every error variant; `validate` passes on
every built info; `encode`/`decode` round trip.

### 4.3 `isdbs-phy::tx` (Agent B)

```rust
pub type Packet = [u8; 188];
pub const SUPERFRAME_SYMBOLS: usize = FRAMES * FRAME_SYMBOLS;   // 319 488

/// A frame's 39 936 symbols from its 12 header bytes (inner-coded BPSK,
/// X then Y, table 45) and its 38 976 main symbols; bursts from
/// `burst::frame_bits`. (Promoted from `testsig`; `testsig::Synth` then
/// calls it.)
pub fn frame_symbols(header: &[u8; HEADER_BYTES], main: &[Cplx]) -> Vec<Cplx>;

pub struct Transmitter { … }
impl Transmitter {
    pub fn new(info: TmccInfo) -> Result<Self, TxError>;   // validate(); at least one sent slot
    pub fn with_mapping(self, m: Mapping) -> Self;
    pub fn info(&self) -> &TmccInfo;
    /// TSs in use (relative TS ids 0..n).
    pub fn ts_count(&self) -> usize;
    pub fn ts_id(&self, relative_ts: usize) -> u16;
    /// Sent slots of relative TS `r` per frame; × FRAMES per superframe.
    pub fn slots_per_frame(&self, r: usize) -> usize;
    pub fn packets_per_superframe(&self, r: usize) -> usize;
    /// `ts[r]`: exactly `packets_per_superframe(r)` packets of TS r in
    /// order (sync byte 0x47 first; checked). Returns the superframe's
    /// symbols (unit modulus, transmission order) and where each packet
    /// went.
    pub fn superframe(&mut self, ts: &[&[Packet]]) -> Result<Superframe, TxError>;
    pub fn null_superframe(&mut self) -> Superframe;
    pub fn superframes(&self) -> u64;   // calls so far
}
pub struct Superframe {
    pub symbols: Vec<Cplx>,                       // 319 488
    /// (frame, slot, ts_id) of each packet in transmission order,
    /// matching `isdbs_phy::ts::Packet`'s fields.
    pub placed: Vec<(usize, usize, u16)>,
}
```

Per superframe: for frame f and sent slot n (slot s, relative TS r =
`info.relative_ts[s]`) the next packet of `ts[r]` → RS(204,188) →
`mainsig::frame_bytes` → `demod::encode_frame(bytes[f], info, f == 0,
mapping)` → `frame_symbols(headers[f], main)`. Null packets for a TS the
caller gives too few of are an error (`TxError::PacketCount`), not
silent padding: the CLI pads.

Tests (`tests/loopback.rs` in `isdbs-phy`):
- **symbol domain, byte-exact**: for each of the BS layout, the CS
  layout, TC8PSK 44 + BPSK 4, QPSK 7/8 40 + QPSK 1/2 8, QPSK 5/6 30 +
  2/3 18: 3 superframes of distinct packets → `Transmitter` → carrier
  offset 2·10⁻⁴ cycle/symbol, phase, AWGN at 12 dB (TC8PSK) / 8 dB
  (QPSK) → `receive` + `ts::decode_all` → every packet back, per TS_ID,
  in order, `corrected == Some(0)` or small, no uncorrectable; the
  receiver's `TmccInfo` equals the sent one.
- `testsig` tests still pass through `frame_symbols`.
- **sample domain** (one layout, 40 Msps): `Shaper` → `frontend::symbols`
  (`Trim::Auto`) → `receive` → `decode_all`: byte-exact; the measured
  trim is within ±0.05 symbol of 0 (the detector is unbiased on a clean
  pulse, `isdb-frontend` README).

## 5. CLI design (Agent C, after A and B are merged)

### 5.1 `isdb tx --system isdbs`

`--system isdbt|isdbs` on `tx` and `loopback` (default `isdbt`; the
`SystemArg` of `rx` minus `auto`/`isdbs3`, or a new enum). With `isdbs`:

| Option | Default | Meaning |
|---|---|---|
| `--combination WORD:N` (repeatable, ≤ 4) | `tc8psk2/3:48` | slot layout: the Table 1 combination and its slots (incl. dummies), any order, summing to 48 in whole units (`bpsk1/2`, `qpsk1/2`, `qpsk2/3`, `qpsk3/4`, `qpsk5/6`, `qpsk7/8`, `tc8psk2/3`) |
| `--ts ID[:SLOTS][=PATH]` (repeatable, ≤ 8) | `0x4010` synthetic, all sent slots | a TS: TS_ID (`0x4010`, `4010h`, decimal), sent slots per frame (required with more than one `--ts`; must sum to the sent slots), its packet file (`-` stdin, `--feed` as for ISDB-T) or synthetic packets when absent |
| `--rate SPS` | 40M | output sample rate (≥ 1.35 × 28.86 M; the Pluto's limit is 61.44 M) |
| `--iq PATH`, `--iq-format cf32\|cs8` | — | the data file **and** its SigMF sidecar (`isdb_iq::meta_path_for`: `NAME.sigmf-meta`), `core:sample_rate`, `core:datatype`, `core:description "isdb tx ISDB-S …"`, `isdb:system "ISDB-S"`, one capture segment at 0 with `core:frequency` from `--freq`/`--channel` if given; `isdb rx --iq PATH` then reads it and its survey picks ISDB-S |
| `--backoff-db` | 10 | RMS below full scale, applied to cf32 too (§3) |
| `--lead-superframes` | 1 | null superframes before the data |
| `--no-flush` | — | omit the null flush frame |
| `--seconds`, `--realtime`, `--queue-frames`, `--prefill-frames`, `--status-secs`, `--feed`, `--radio`, `--gain`, `--rf-enable`, `--dry-run`, `--channel`, `--freq`, `--freq-unrestricted`, `--freq-corr-ppm`, `--bb-filter-hz` | as ISDB-T | `--channel BSn/NDn` is the IF (`Channel::tune_hz`), outside the UHF band so it needs `--freq-unrestricted` as the gate requires today (replace the "there is no satellite transmitter" message); `--bb-filter-hz` defaults to 40 MHz with `--system isdbs`; the radio sample rate is `--rate` (ppm-corrected), not 512/63 MHz |

ISDB-T-only options (`--mode`, `--gi`, `--layer-*`, `--a-ts/--b-ts/--c-ts`,
`--mux-ts`, `--mux-config`, `--relay`, `--iq-in`, `--lead-frames`) with
`--system isdbs`, and the ISDB-S ones with `isdbt`, are refused with a
message naming the system (as `rx` does for its satellite options).

Threads as ISDB-T (`producer` → bounded queue → `IqSink`): an ISDB-S
producer (`txs.rs`) assembles each superframe's packets per TS
(`source::PacketStream` per file TS; a synthetic generator per TS that
encodes (ts_id, superframe, index) in the payload like
`SyntheticInput::packet`; null padding when a file TS has no packet,
counted as for ISDB-T), calls `Transmitter::superframe` and the
`Shaper`, and queues `ProducedFrame`-like items (samples, kind
lead/data/flush, packets per TS for the loopback's verifier, input and
modulate times). `TxOptions.frame_seconds` = one superframe. Status and
summary lines as for ISDB-T (per TS instead of per layer: input
packets, null padding, null fill %). `check_not_scrambled` applies.

### 5.2 `isdb loopback --system isdbs`

| Option | Default | Meaning |
|---|---|---|
| `--superframes N` | 6 (until EOF with file inputs) | data superframes |
| `--snr-db`, `--seed`, `--sample-offset`, `--json`, `--no-flush` | as ISDB-T | |
| `--cfo-hz HZ` | 0 | carrier frequency offset (`--cfo-carriers` is ISDB-T's; each refused with the other system) |
| `--lead-superframes` | 1 | |
| the input and layout options of `tx --system isdbs` | | |

Pipeline: producer → `Channel` (generalised: `Channel::with_cfo_hz(snr,
cfo_hz, fs, sample_offset, seed)`, the existing constructor expressed
through it) → one buffer of samples (a loopback of N superframes is N ×
443 k samples; keep it simple, no streaming) → `rxs::front` (CPU front
end, `Trim::Auto`, carrier from the survey) → `isdbs_phy::receive` →
`ts::decode_all` → `VerifierS`.

`VerifierS` (mirrors `loopback::Verifier`): records each sent superframe
(its `placed` list and packets per TS); anchors the first decoded
superframe that carries a non-null packet to the earliest sent
superframe with the same content; numbers later decoded superframes by
their frame start (`r.frames[sf.first_frame].mark.start`, 319 488
symbols apart); compares packet by packet per TS_ID (pass, input_pass,
fail, lost as today) and the receiver's `TmccInfo` against the sent one;
PASS iff anchored, no fail, no lost input packet, every decoded
superframe's TMCC equals the sent one. Report lines per TS_ID with RS
corrected / uncorrectable and the main MER (`DecodedSuperframe.mer_db`);
`--json` with `verify` keyed by TS_ID, `rx` = `rxs::Stats::json()`.

### 5.3 Tests (`crates/isdb-cli/tests/cli.rs`)

- `isdbs_tx_then_rx_reproduces_every_ts_byte_exact`: two file TSs
  (`--ts 0x4010:24=a.ts --ts 0x4011:24=b.ts`, 3 superframes of distinct
  packets) → `tx --system isdbs --iq t.cf32` (sidecar written) →
  `rx --iq t.cf32 --out 'o-{ts_id}.ts' --strip-nulls` → each output
  equals its input (and `--json` reports 0 uncorrectable); `rx` must
  pick ISDB-S by itself (`--system auto`).
- `isdbs_loopback_passes_with_impairments`: `--superframes 4 --snr-db 12
  --cfo-hz 50000 --sample-offset -12345` PASS; also the CS layout
  (`--combination qpsk3/4:48`, `--snr-db 8`).
- `isdbs_loopback_detects_loss`: `--no-flush --json` → FAIL with
  `lost > 0`; a file input shorter than the superframes asked ends at
  EOF with the packets counted.
- Argument tests: ISDB-T options refused with `--system isdbs` and the
  reverse; `--ts` slot sums; `completions` still covers everything.
- Keep every test under a few seconds in `cargo test` (the front end
  runs ≈ 0.8× real time; 4–6 superframes ≈ 70 ms of signal).

### 5.4 Docs (Agent C)

README (features: "its transmitter is not written yet" → the new
state; crate table), `crates/isdb-cli/README.md` (commands, options,
test inventory), `crates/isdbs-phy/README.md` (TX module, boundaries),
`crates/isdbs-core/README.md` (`assign`), `crates/isdb-frontend/README.md`
(`shape`), `docs/transmitting.md` (an "ISDB-S" section: file output,
the SigMF sidecar, the radio limit of §2), `docs/receiving.md`
("Software loopback": the ISDB-S form), `docs/architecture.md` (data
flow and CLI lines), SPECS.md coverage rows for the TX clauses
(BO.1408-1 Fig. 1 chain, §9 roll-off), SPEC-NOTES only if a new
interpretation was needed (none expected), VERIFICATION.md rung "ISDB-S
transmitter and loopback" with the measured numbers (shaper MER, trim
found, loopback SNRs, speed), CHANGELOG "Added", and §0.22 of
[isdb-s.md](isdb-s.md) with the commits and findings.

## 6. Work split and order

| Agent | Scope | Depends on |
|---|---|---|
| A | §4.1 `isdb-frontend::shape` + tests + README | — |
| B | §4.2 `TmccInfo::assign`, §4.3 `isdbs_phy::tx`, `testsig` refactor, `tests/loopback.rs` (symbol domain; the sample-domain test is added by C once A is merged, or by B if A lands first) | — |
| C | §5 CLI, tests, docs, VERIFICATION, CHANGELOG, plan status | A and B merged |

A and B run in parallel in their own worktrees and branches; the
coordinator merges them into `main`, runs `mise run lint` and `mise run
test`, then starts C.

## 7. Acceptance

1. `cargo test --workspace` and `mise run lint` clean; `mise run doc`
   clean.
2. Symbol-domain loopback byte-exact on every combination mix; sample
   domain byte-exact at 40 Msps.
3. `isdb tx --system isdbs --iq` → `isdb rx --iq` (auto-detected
   ISDB-S) reproduces every TS byte-exact, 0 uncorrectable.
4. `isdb loopback --system isdbs` PASS at 12 dB (TC8PSK 2/3), 8 dB
   (QPSK 3/4), with CFO and a mid-stream start; FAIL with counted loss
   when the flush is omitted.
5. Docs and VERIFICATION.md updated; no real station identity anywhere.
