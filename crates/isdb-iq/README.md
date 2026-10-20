# isdb-iq

System-independent IQ recordings for the `isdb-rs` receivers: the `Iq`
boundary of the receive chain (complex samples at the radio's rate) as
files on disk. It reads and writes the sample formats and the
[SigMF](https://sigmf.org) metadata of `isdb capture`, and surveys a
single-carrier recording (symbol rate, carrier offset, band edges, the
system whose symbol rate matches) without demodulating it. ISDB-S and
ISDB-S3 receivers will start from it; ISDB-T keeps its own 512/63 MHz
cf32 / cs8 file path (`isdbt-phy::iq`).

## Spec scope

No broadcast specification is implemented. The survey quotes two nominal
symbol rates to name a recording's system (`survey::KNOWN_SYSTEMS`):

| System | Symbol rate | Roll-off | Source |
|---|---|---|---|
| ISDB-S | 28.860 Mbaud | 0.35 | MIC Ordinance No. 87 of 2011 Art. 52(3) and annexed table 47; ITU-R BO.1408-1 Annex 1 Table 3 |
| ISDB-S3 | 33.7561 Mbaud (±20 ppm) | 0.03 | ARIB STD-B44 2.1 Annex B.3, B.4; ITU-R BO.2098-0 Annex 1 |

The SigMF fields used are those of the SigMF 1.2 core namespace
(`core:datatype`, `core:sample_rate`, `core:version`, `core:extensions`,
`core:description`, `core:recorder`, `core:hw`; per capture segment
`core:sample_start`, `core:frequency`, `core:datetime`). This project's
own fields form the optional `isdb` extension, listed in the `sigmf`
module documentation (`isdb:full_scale`, `isdb:lo_hz`,
`isdb:rf_bandwidth_hz`, `isdb:gain`, `isdb:channel`,
`isdb:channel_rf_hz`, `isdb:capture_method`, `isdb:gap_before`).

## Position in the stack

- Depends on: `num-complex`, `rustfft`, `serde_json`, `thiserror`.
- Used by: `isdb-cli` (`isdb capture` writes its metadata, `isdb iq
  survey` runs the survey). The planned ISDB-S / ISDB-S3 front ends will
  read their input through `Recording`.

## Key types and entry points

| Item | Role |
|---|---|
| `SampleFormat` | `Cs8`, `Cs16 { full_scale }` (2048 for libiio AD936x samples, 32768 for maia-sdr and most tools), `Cf32`; `decode` to `C32`, SigMF datatype, `FromStr` (`cs8`, `cs16`, `cs16/2048`, `cf32`) |
| `sigmf::Meta`, `sigmf::Segment` | SigMF metadata: typed core fields, every other field kept (so foreign recordings round-trip), `to_json` / `from_json`, `segment_ranges` |
| `Recording` | a data file opened by its data path, `.sigmf-meta` path or SigMF base name; format and rate from the metadata or given; `read(start, n)`, `segments()` |
| `meta_path_for` | `NAME.sigmf-data` → `NAME.sigmf-meta`, other names → name + `.sigmf-meta` |
| `survey::survey(x, fs, &Options) -> Survey` | power, `Band` (centre, −3 / −20 dB widths), the |z|² symbol line, the z², z⁴, z⁸ (and π/2-BPSK z² at ±R/2) `CarrierLine`s, the matched `KnownSystem` |
| `survey::match_system` | the known system for a |z|² line at a sample rate (tries the alias fs − line) |
| `iso8601_utc` | `core:datetime` text from Unix seconds |

```rust
use isdb_iq::survey::{Options, survey};
use isdb_iq::C32;

// 8PSK-like test signal: no known rate, but the survey still runs.
let x: Vec<C32> = (0..8192)
    .map(|i| C32::from_polar(1.0, std::f32::consts::FRAC_PI_4 * ((i * 7 / 3) % 8) as f32))
    .collect();
let s = survey(&x, 57.72e6, &Options::default());
println!("symbol line {:.0} Hz, {:+.1} dB", s.symbol_line.freq_hz, s.symbol_line.level_db);
assert_eq!(s.samples, 8192);
```

## Design notes

- **One segment = contiguous samples.** A SigMF capture segment covers
  the samples from its `core:sample_start` to the next one's; `isdb
  capture --segments N` writes one segment per capture and marks the
  later ones `isdb:gap_before`, so a reader never treats a gap as
  continuous signal.
- **Survey lines.** |z|² of a linearly modulated signal is
  cyclostationary at the symbol rate R; its line is searched from 1 MHz to
  fs/2, and a rate above fs/2 is found at fs − R. At fs ≈ 2R (57.72 Msps for
  ISDB-S) the line and its alias coincide: the rate is right but the sign
  of its ppm deviation is not determined; 61.44 Msps separates them. z^k
  carrier lines are searched within |f| < R/2 (z²: R/4) once R is known,
  because cyclostationarity also leaves copies at k·Δf ± R that can be as
  strong as the line (seen on BS IF recordings); π/2-shift BPSK (ISDB-S3
  TMCC) puts its z² line at 2Δf ± R/2, searched separately. On satellite
  IF, neighbouring transponders can leave strong unrelated lines in a
  window, so trust an offset that a second line kind confirms (as the
  opt-in tests require).

## Verification

`cargo test -p isdb-iq`: format decoding and names, SigMF round trips
(with extension fields, and a maia-sdr 0.6 recording's metadata), opening
recordings with and without metadata, the survey on synthetic 8PSK
(ISDB-S rate, 2.1 samples per symbol, −107 kHz), QPSK at the ISDB-S3 rate
above fs/2, π/2-BPSK (offset from the ±R/2 line) and band-limited noise
(band edges), rate matching.

Opt-in on real recordings (`docs/test-data.md`): `tests/captures.rs`
`isdb_s_recording` (`ISDB_RS_ISDBS_IQ`) and `isdb_s3_recording`
(`ISDB_RS_ISDBS3_IQ`) survey a SigMF recording, require the system's
rate within 20 ppm, a symbol line and a carrier line above 20 dB with the
offset confirmed by a second line kind, and, when fs is not near 2R, a
symbol clock without jumps across the whole first segment (a lost sample
would move it by 2π·R/fs). Passed on BS and 110° CS satellite IF
recordings from a cable feed (VERIFICATION.md "Satellite IF survey").

## Known limitations

- Survey only: no matched filter, timing or carrier recovery (Phase 2 of
  the ISDB-S work).
- `Recording::read` loads the requested samples into memory; there is no
  streaming reader yet.
- Only the `ci8`, `ci16_le` and `cf32_le` SigMF datatypes.
