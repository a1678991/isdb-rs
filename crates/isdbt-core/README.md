# isdbt-core

Transmission parameters of the ISDB-T system (ARIB STD-B31 v2.2,
Chapter 3) and the integer quantities derived from them: modes, guard
intervals, per-layer modulation / code rate / time-interleave length, a
validated `Params` set (what TMCC signals), and the frame, rate and delay
figures that the FEC and OFDM crates need. It contains no signal
processing; it is the shared vocabulary of the PHY crates and of the
multiplexer's frame clock.

## Spec scope

ARIB STD-B31 v2.2, Chapter 3 (the 6 MHz, 13-segment system only):

| Item | Clause / table |
|---|---|
| `Mode`, `GuardInterval`, carriers per segment, FFT size, useful symbol length, frame length | §2.3, Table 3-1 |
| total carriers, time-interleave lengths per mode | Table 3-2 |
| `LayerConfig::tsp_per_frame` (TSPs per segment per frame) | Table 3-3 |
| `Params::multiplex_frame_tsps` (TSPs per multiplex frame, n) | Table 3-5 |
| bits per carrier symbol S_X | Table 3-6 |
| `delay_adjustment_tsps` (byte-interleave delay adjustment) | §3.6, Table 3-7 |
| code rates, `coded_bits_per_frame` / `info_bits_per_frame` | §3.8, Table 3-8, §3.2.2.2 |
| bit-interleave delays and their adjustment | §3.9.2–3.9.3, Figs. 3-12/3-14/3-16/3-18, Table 3-9 |
| layer ordering rules checked by `Params::new` | §3.11.2 |
| time-interleave branch factor m_i, delay adjustment, delayed frames | §3.11.1, Fig. 3-23, Table 3-12 |
| segment arrangement on the spectrum | Fig. 3-32 |
| centre carrier Kc | §3.14.1 |

Modules: `params` (re-exported at the crate root), `fec_params` (adds
`LayerConfig` methods used by `isdbt-fec`), `ofdm_params` (free functions
and `Params` methods used by `isdbt-ofdm`).

## Position in the stack

- Depends on: no workspace crate (only `thiserror`).
- Used by: `isdbt-fec`, `isdbt-ofdm`, `isdbt-phy`, `isdbt-mux` (frame
  clock: `Params::frame_ticks_27mhz`, per-layer TSP counts) and `isdb-cli`
  (parsing of `--mode`, `--gi`, `--partial`, `--layer-a/-b/-c` into `Params`).

## Key types and entry points

- `Params::new(mode, guard, partial_reception, [Option<LayerConfig>; 3])`
  validates a configuration and returns `ParamError` otherwise.
- `LayerConfig { segments, modulation, code_rate, time_interleave }`;
  `TimeInterleave(k)` is the TMCC index 0..=3, and `.length(mode)` gives I.
- `Params` figures: `multiplex_frame_tsps`, `samples_per_symbol`,
  `samples_per_frame`, `frame_seconds`, `frame_ticks_27mhz`,
  `layer_of_segment`, `differential_segments`, `symbol_domain_delay_symbols`.
- Constants: `TSP` (188), `TRANSMISSION_TSP` (204), `SYMBOLS_PER_FRAME`
  (204), `SEGMENTS` (13), `SAMPLE_RATE_NUM`/`SAMPLE_RATE_DEN` (512/63 MHz).

```rust
use isdbt_core::{CodeRate, GuardInterval, Layer, LayerConfig, Mode, Modulation, Params, TimeInterleave};

// Mode 3, GI 1/8 with partial reception: layer A = 1 segment QPSK 2/3,
// layer B = 12 segments 64QAM 3/4, time-interleave index 2 (I = 2 in Mode 3).
let ti = TimeInterleave(2);
let a = LayerConfig { segments: 1, modulation: Modulation::Qpsk, code_rate: CodeRate::R2_3, time_interleave: ti };
let b = LayerConfig { segments: 12, modulation: Modulation::Qam64, code_rate: CodeRate::R3_4, time_interleave: ti };
let p = Params::new(Mode::M3, GuardInterval::G1_8, true, [Some(a), Some(b), None]).unwrap();

assert_eq!(a.tsp_per_frame(p.mode), 64); // Table 3-3: 16 × 4 (Mode 3) × 1 segment
assert_eq!(b.tsp_per_frame(p.mode), 2592); // 54 × 4 × 12
assert_eq!(p.multiplex_frame_tsps(), 4608); // Table 3-5, Mode 3, GI 1/8
assert_eq!(ti.length(Mode::M3), Some(2));
assert_eq!(p.layer_of_segment(0), Layer::A);
assert!((p.frame_seconds() - 0.231336).abs() < 1e-12); // Table 3-1
assert_eq!(p.frame_ticks_27mhz(), 6_246_072); // PCR ticks per OFDM frame

// §3.11.2: partial reception requires a one-segment layer A.
let bad = LayerConfig { segments: 2, ..a };
let c = LayerConfig { segments: 11, ..b };
assert!(Params::new(Mode::M3, GuardInterval::G1_8, true, [Some(bad), Some(c), None]).is_err());
```

## Design notes

- Only quantities stated in the spec are encoded; derived values are exact
  integer functions (e.g. TSPs per frame = nc·S·R/8 per segment) that the
  tests compare against the tables as printed, not against each other.
- `frame_ticks_27mhz` is exact for every mode and GI (it asserts the
  division is exact); `isdbt-mux` uses it to tie PCR to the OFDM frame.
- `Params` is the unit of configuration everywhere: the modulator, the
  receiver's TMCC decoder and the multiplexer all agree through it.
  `Layer::ALL` / `layer.index()` define the A/B/C array order used by the
  other crates.

## Verification

All tests are in-source unit tests (`cargo test -p isdbt-core`); there is
no `tests/` directory and no data dependency.

| Test (module) | Proves |
|---|---|
| `params::tests::table_3_5` | `multiplex_frame_tsps` = Table 3-5 for all modes and GIs |
| `params::tests::table_3_3` | `tsp_per_frame` = Table 3-3 for every modulation × rate × mode |
| `params::tests::frame_lengths` | frame length = Table 3-1 (257.04 / 231.336 / 53.0145 ms) and 27 MHz ticks |
| `params::tests::table_3_2` | total carriers 1405/2809/5617 and the I values per TMCC index |
| `params::tests::segment_ordering_rules` | §3.11.2 ordering checks of `Params::new` |
| `fec_params::tests::spec_table_3_7_delay_adjustment` | Table 3-7 as printed (all three mode columns, N = 1..13) and adjustment + 11 = one frame |
| `fec_params::tests::coded_bits_consistent_with_rate` | coded bits = info bits / R for every layer configuration |
| `ofdm_params::tests::spec_*` | Kc (§3.14.1), Table 3-9 (total 2 symbols), Table 3-12 (frame multiple), Fig. 3-32 positions, Table 3-1 carrier counts |
| doctest (this README / `lib.rs`) | the example above |

## Known limitations

- Only the 6 MHz, 13-segment system with the fixed 512/63 MHz sample rate
  is modelled; ISDB-T<sub>SB</sub> and other channel bandwidths are not.
- `Params::new` checks segment sums, contiguity from A, the partial
  reception segment count, DQPSK-after-coherent ordering and the
  time-interleave index; it does not reject combinations that are legal by
  §3.11.2 but unusual for broadcasting.
- `Params::layer_of_segment` panics for a segment index ≥ 13.
- This crate has no `SPEC-NOTES.md` of its own: its values come directly
  from the tables. See the workspace index
  [`../../SPEC-NOTES.md`](../../SPEC-NOTES.md).

See also the workspace [README](../../README.md).
