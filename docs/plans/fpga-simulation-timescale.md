# FPGA simulation turnaround

**A dated measurement, 2026-11-02, with a note of 2026-11-04. Read this
first; what follows it describes the code as it was measured.**

What has changed since the measurements of 2026-11-02:

- **Verilator is the compiled default**
  (`fpga/isdb_fpga/chain/simv.py`; [`fpga/README.md`, "Simulation
  backends"](../../fpga/README.md#simulation-backends)). The measurements of
  that date are CXXRTL's and describe only CXXRTL, which remains a backend
  (`FPGA_SIM=cxxrtl`) for where Verilator is not installed.
- **The "hour-scale" runs and the projection of 73-85 minutes per scenario
  no longer apply.** A real-decoder set takes about 2-3.5 minutes on
  Verilator: 1.7-3.9 minutes per set in the run of 2026-11-04 below, with
  every core busy.
- **The cache key** is the design, `harness.cc` and the build flags (and
  Verilator's version, without the RTLIL's `src` attributes), no longer the
  Python around them.
- **The compiler level was not adopted.** `simx.CXXFLAGS` is still `-O1`;
  the `-O2` rows are a measurement, not the default. Verilator's models build
  at `-O2` (`simv.VFLAGS`). The figures of 2026-11-04 below say what the
  choice is worth today.
- **The design and the harness have changed:** the monitors, the sums block
  and a second CXXRTL step per clock edge. The 2026-11-02 rates cannot be
  compared with today's to a few percent.

Figures describe this host (Ryzen 9 7950X) and these designs, not a promised
CI runtime.

## Measured 2026-11-04

GCC 16.2.1, Verilator 5.054, Amaranth 0.5.10. `synth/chain_simspeed.py`,
`RealChainBench` on the input of `chain-7-hdl`, one core (`taskset`), load
about 2. The prefix ends before the set's first frame does, so it measures
acquisition; `Slots` words, counters and monitors matched between the
repeats and between the two compiler levels (no output yet at this length,
so that says little).

| Library | Prefix | Cycles | Cycles/s | Build |
|---|---:|---:|---:|---:|
| Verilator | 100 000 symbols + 1 000 | 120 181 | 73 900-75 900 | 22 s (8 make jobs, elaboration included) |
| Verilator, `FPGA_SIM_PROGRESS` chunks of 1 000 | the same | 120 181 | 75 800-76 100 | |
| Verilator, idle clocks | 101 000 cycles | 101 000 | 77 100-77 300 | |
| Verilator | 2 600 000 symbols + 1 000 | 3 108 723 | 75 200 | |
| CXXRTL, `-O1` (the default) | 10 000 symbols + 1 000 | 12 879 | 1 395-1 405 | 321 s |
| CXXRTL, `-O2` | the same | 12 879 | 1 495-1 502 | 133 s |

So Verilator is about 54 times CXXRTL `-O1` here. The chunks cost nothing
measurable. On this model `-O2` compiles in 41 % of `-O1`'s time and runs 7 %
faster. CXXRTL builds are the C++ compile alone, from the same generated
file (11.9 MB), on one core; with `write_cxxrtl` and the elaboration on a
loaded host the first `-O1` build took 500 s. The 2 600 000-symbol prefix
reaches frame 2's slot blocks (38 414 words, two TMCC words decoded).

The suites at `PYTHONHASHSEED=0` (32 threads, `pytest -n auto --dist
worksteal tests`, Verilator, no recordings):

| Run | Result | Wall |
|---|---|---:|
| Every compiled model built (one test fewer than the others) | 196 passed, 20 skipped | 154 s |
| Warm, twice | 197 passed, 20 skipped | 105-107 s |
| Warm, `FPGA_SIM_PROGRESS=1 mise run fpga:test -- -s` | 197 passed, 20 skipped | 104 s and the vectors |
| Warm, `FPGA_CHAIN_REAL=1` | 211 passed, 6 skipped | 563 s |

In that last run the real-decoder sets took 104-237 s each under
`RealDecoderChainTest` (`test_stop_and_start_free_the_decoder`, four passes
and the whole input: 372 s).

### The hash seed

`tools/sim_hashseed.py`, a new process per seed (keys as in
`<tag>-<key>.so`):

| Design | Domains | Seed 0, twice | Seed 1 | Seed 2 |
|---|---:|---|---|---|
| `ThreeDomains`, Verilator key | 3 | `76a8024ab0784f8d` both | `6b329370a329c34f` | `2682590c42d76682` |
| `ChainBench`, Verilator key | 2 | `cfd4aba018732958` both | `89fe00765aca08b8` | `89fe00765aca08b8` |
| `RealChainBench`, Verilator key | 3 | `ea75009031dc0e57` both | `6201f7e4a03be9e4` | `feb40f7e1b7cacee` |
| `LdpcDecoder` S = 17, Verilator key | 1 | `4c87d8bccb21c975` both | the same | the same |

The CXXRTL keys behave alike. Without the variable each xdist worker has a
hash seed of its own (checked with a test printing `hash("x")`: differing
across workers without it, one value with `PYTHONHASHSEED=0`).

The effect on the suite, with the models already built at seed 0: warm at
seed 0, 48 libraries before and after; a run with no seed added 13, a second
4; seed 0 again added none. The wall time did not change on this host
(104-109 s against 105-107 s): the designs that vary are small, and 32
threads hide their builds. Where it should matter is four vCPUs and a
restored cache, or a large design: the CI's Verilator build of the whole
chain takes about 25 s and each process with a new seed may repeat it. That
was not measured.

A seed does not make CXXRTL's key independent of the entry point: its RTLIL
keeps `src` attributes (`tools/..` against an absolute path, at seed 0 two
keys for `RealChainBench`, `b5a03bfb4e48d046` and `816f0dfeb7b42276`).
Verilator's key drops them, so `synth/chain_simspeed.py` and the tests share
one library there. `--library` opens a CXXRTL library that exists.

## Measured 2026-11-02 (CXXRTL)

GCC 16.2.1, Clang 23.1.4, Amaranth 0.5.10, YoWASP Yosys 0.70.0.0.post1259.
Verilator was not part of this measurement.

| Experiment | Result |
| --- | --- |
| Full `RealChainBench` RTLIL, seed 1 vs seed 2 | Different hashes for the same design |
| Full `RealChainBench` RTLIL, seed 0 twice | Identical hashes; about 16 s elaboration each |
| Small three-domain fixture, seed 0 in three independent processes | One compiled library reused, mtime unchanged; warm setup about 4 ms |
| GCC `-O1` full-chain library, idle, three repeats | Median 1,544 base cycles/s |
| GCC `-O2` full-chain library, idle, three repeats | Median 1,892 base cycles/s |
| GCC `-O2`, compile existing generated C++ | 125.4 s |
| GCC `-O1`, compile the same generated C++ | Exceeded 180 s; stopped there |
| Clang `-O1`, compile the same generated C++ | Exceeded 180 s; stopped there |
| 10,000-symbol acquisition prefix + 1,000 drain cycles, GCC `-O1`, three repeats | Median 1,515 base cycles/s |
| Same prefix, GCC `-O2`, three repeats | Median 1,851 base cycles/s: about 22% faster / 18% less simulation time |
| Same prefix, GCC `-O1`, split into 1,000-record calls | Median 1,509 base cycles/s: about 0.4% slower than one call |
| 60,000-symbol acquisition prefix + 1,000 drain cycles | `-O1`: 47.8 s; `-O2`: 39.2 s; identical two head records, six period records, output and diagnostics |

Compile timings include C++ compilation and linking of the model, C API and
harness, **not** Amaranth elaboration or `write_cxxrtl`. Each attempt had a
180-second wall budget.

The longer prefix captures nonempty synchronization traces but precedes
frame decoding and emits no `Slots` words. It does not establish complete
real-decoder equivalence.

## Why those runs took so long

The full scenarios were 5.6 to 6.7 million symbols then (`chain-7-hdl`'s
input is 5,755,224 today). With input pacing and the fixed 1,500,000-cycle
drain that is 8 to 9.5 million base cycles (`chain-7-hdl`: 8,179,491).

The harness schedules the 4x and 5x clocks at distinct times: seven interior
events and one common edge per base cycle. Each interior event takes two
evaluations of the whole design, the settle one and the common edge one, so
16 per base cycle on Verilator and 17 on CXXRTL, whose edge takes a second
step (`chain/harness.cc`, `edge`). The cost per evaluation is what Verilator
removes.

On 2026-11-02 the improved CXXRTL acquisition rate projected to 73-85
minutes per scenario: an extrapolation, not a completed run, and
superseded (see the note above).

`--symbols N` in `isdb_fpga.chain.sim` still uses the 1,500,000-cycle drain
with the real decoder: about 20 s on Verilator, about 18 minutes on CXXRTL.
For a short measurement use the benchmark below.

## Development workflow

From the repository root:

- `mise run fpga:test-quick`: seconds, no vectors (about 35 s the first
  time, which builds its small libraries). The stream driver on every
  compiled backend, the LDPC bridge's clock crossing, the CRC. A smoke test,
  not the regression suite.
- `mise run fpga:test`: the regression suite, with the stubbed chain and the
  model comparisons. It writes the vectors first.
- `FPGA_CHAIN_REAL=1 mise run fpga:test`: the same with the real decoder in
  the chain, every set, on every core: what `fpga/README.md` and `AGENTS.md`
  ask for after a real decoder or chain change; about 10 minutes warm on 32
  threads. Check that `RealDecoderChainTest` ran rather than skipped.

From `fpga/`, after `mise run fpga:vectors`, one affected class:

```sh
PYTHONHASHSEED=0 uv run --locked pytest -q tests/test_chain.py::PhaseTest
```

The `fpga:test*` tasks set `PYTHONHASHSEED=0` **before** Python starts; set
the same variable on direct commands, or the first run under a new seed
builds its own library for each design with more than one clock domain.
Setting it inside a running interpreter cannot change hash ordering. (The
tasks also set `UV_DEFAULT_INDEX` and `UV_EXCLUDE_NEWER`: if a direct
`uv run --locked` asks for a relock, set them as `mise.toml` does.)

`FPGA_SIM_PROGRESS=1` reports elaboration, the cache's answer and a run's
rate on stderr, for the compiled backends, and splits the run into chunks
of 10,000 records or drain cycles without changing its clock sequence
(`tests/test_simx.py`, `ProgressTest`). Under pytest it needs `-s`, which
also shows the xdist workers' reports:

```sh
PYTHONHASHSEED=0 FPGA_SIM_PROGRESS=1 uv run --locked \
  python -m isdb_fpga.chain.sim build/chain-7-hdl.json --symbols 10000
FPGA_SIM_PROGRESS=1 mise run fpga:test -- -s
```

It is no wall timeout: a blocked record can still spend its whole stall
limit in one native call.

## Reproducing

From `fpga/`, after `mise run fpga:vectors`:

```sh
# the keys per design and seed
uv run --locked python tools/sim_hashseed.py

# the chain with the real decoder on a short prefix, the compiled default
PYTHONHASHSEED=0 uv run --locked python synth/chain_simspeed.py \
  --vectors build/chain-7-hdl.json --symbols 100000 --tail 1000

# what FPGA_SIM_PROGRESS's chunks cost
PYTHONHASHSEED=0 uv run --locked python synth/chain_simspeed.py \
  --vectors build/chain-7-hdl.json --symbols 100000 --tail 1000 --chunk 1000

# on CXXRTL (the first run builds its library, minutes), and again at -O2
PYTHONHASHSEED=0 uv run --locked python synth/chain_simspeed.py --backend cxxrtl \
  --vectors build/chain-7-hdl.json --symbols 10000 --tail 1000 \
  --compiler g++ --opt=-O2 --build-seconds 900

# the stub chain and the decoder alone, build times included
PYTHONHASHSEED=0 uv run --locked python synth/sim_speed.py chain verilator
```

Omit `--vectors` for idle clocks; pin with `taskset -c N` for per-core
figures. Each step prints a JSON line: rates, cycles, `Slots` words, the
chain's counters, the monitors' violations; the exit status is 1 if those
differ between repeats or compiler levels. A compile writes its own library
and log beside the original and is bounded by `--build-seconds`, killing only
its own process group. The deadline does not bound the simulation, so keep
the counts small on CXXRTL.

The benchmark feeds what `run_chain_real` feeds (`stages.chain_real_records`:
the taps at open, the host's writes and the set's losses inside the prefix)
to the bench of the set's own bit errors, but compares nothing with the
model: it measures timing and what a compiler changes, and the integration
tests are what validate against the reference model.
