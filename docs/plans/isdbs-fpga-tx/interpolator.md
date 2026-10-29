# FPGA transmit interpolator for ISDB-S on the PlutoSky R2 (XC7Z020): design and sizing

Investigation, 2026-10-29. Throwaway tooling, committed on the worktree branch
`worktree-agent-a0e9ac5233fe191dd` only (commit `46ee395`, not for merge; the
branch was fast-forwarded to `main` first, because `shape.rs` was not on it yet):

- `crates/isdb-frontend/examples/txinterp.rs`: the interpolator model (phases,
  span, phase selection, arithmetic, accumulator bits) plus the ISI MER, the
  digital loopback into `fast::FastFrontend`, and underrun gaps;
- `fpga/synth/tx_sketch.py`: Amaranth datapath sketch (mux and multiply versions)
  and the Yosys estimate;
- `fpga/synth/tx_sketch_sim.py`: the mux sketch simulated against a Python model,
  bit for bit.

## 0. Summary and recommendation

| Choice | Value | Why (measured below) |
|---|---|---|
| Phases P | **512** (256 also fine), nearest phase as "floor with half-phase rows" | ISI MER 54.4 dB (P 256: 52.3 dB) with 18-bit coefficients and 12-bit output; in digital loopback the RX's 6-bit output reads the same as with the f32 shaper or better. P 512 costs the same BRAM as P 256 |
| Span | **12** (24 taps) | 55.5 dB truncation ceiling; out-of-band peak −49 dB; in loopback span 12 reads better than span 16 (36.7 against 36.2 dB) |
| Point product | **mux, no multiplier**: one ROM per tap gives A (even k) or B = A/√2 (odd k); I and Q are ±value or 0 | 0 DSP48E1 instead of 48; costs about 820 LUT and 1 050 FF more than the multiply version |
| ROM | the pulse's symmetry `c[q][t] = c[P−1−q][2S−1−t]`: one true-dual-port RAMB18 (1024 × 18) per tap pair | 12 RAMB18 |
| Phase accumulator | Q0.24, `step = round(2²⁴·R/fs)` | −0.028 ppm at 40 Msps; exact at 57.72 and 61.44 Msps |
| Backoff | folded into the coefficients, `A = round(G·p·D·2^S)`, D = 2048·10^(−b/20), S = 7 at 10 dB | 0 DSP; peak at most 1 047 of 2048 LSB, so it cannot saturate at backoffs of 4.2 dB or more |
| Underrun | send the **zero symbol**, set a sticky flag, count the missing symbols | it is the same path as the lead-in and the tail; the receiver re-syncs within 0.1–0.35 ms |
| Yosys estimate | **0 DSP, ≈2.0k LUT, 2.3k FF, 12 RAMB18** | images: TX-only 66 DSP, TX+RX 178 DSP of 220 |
| Loopback acceptance | 8PSK MER ≥ **36.0 dB** at the RX block's 6-bit output (measured 36.7 dB), and the 12-bit IQ bit-exact against the model | §6 |

With every option ≥ 42 dB, the TX is never the limit on a real 10–15 dB link: a
43 dB TX costs 10·log10(1 + 10^(−2.8)) = 0.007 dB at 15 dB. The TX stops being
the limit **against the loopback's 36 dB** (the RX's 6-bit output) at
**P ≥ 256 nearest, or P ≥ 64 with linear interpolation**. Both are within
0.04 dB of the f32 shaper (P 1024, span 16).

## 1. MER against phases and taps

Method (`txinterp grid|arith`): 20 000 random 8PSK symbols at 1.386 sps
(40 Msps, 28.86 Mbaud, α 0.35, the RRC truncated at ±span as `Shaper`), each
followed by an ideal f64 matched filter at the exact symbol instants of the
*realised* rate, and the best complex gain (the method of `tests/shape.rs`).
The model reproduces `Shaper`: P 1024 / span 16 nearest 57.5 dB (shape.rs
docs: 57.6), P 256 53.3 dB (docs: 53.3).

**f64 arithmetic, exact 63-bit phase** (`round` = q = round(f·P) on P + 1 rows,
as `Shaper`; `floor½` = q = floor(f·P) on rows at (q + ½)/P; `lin` = linear
between rows q and q + 1, μ exact or quantised to 8 or 4 bits):

| P | span | round | floor½ | lin | lin μ8 | lin μ4 |
|---|---|---|---|---|---|---|
| 64 | 8 | 42.02 | 42.09 | 47.93 | 47.93 | 47.77 |
| 128 | 8 | 45.36 | 45.36 | 47.92 | 47.92 | 47.88 |
| 256 | 8 | 47.18 | 47.15 | 47.92 | 47.92 | 47.91 |
| 1024 | 8 | 47.87 | 47.87 | 47.92 | 47.92 | 47.92 |
| 64 | 12 | 43.15 | 43.16 | 56.01 | 56.00 | 55.06 |
| 128 | 12 | 48.08 | 48.13 | 56.00 | 55.99 | 55.74 |
| 256 | 12 | 52.58 | 52.47 | 55.99 | 55.99 | 55.92 |
| 1024 | 12 | 55.70 | 55.68 | 55.98 | 55.98 | 55.98 |
| 64 | 16 | 43.21 | 43.25 | 57.93 | 57.92 | 56.52 |
| 128 | 16 | 48.39 | 48.37 | 57.94 | 57.94 | 57.53 |
| 256 | 16 | 53.31 | 53.27 | 57.94 | 57.94 | 57.83 |
| 1024 | 16 | 57.49 | 57.47 | 57.94 | 57.94 | 57.94 |

(P 16 / 32: about 30.4 / 37.6 dB nearest and 47.6–55 dB linear.) The span sets a
ceiling (truncation: 47.9 / 56.0 / 57.9 dB). Nearest phase gains about 5 dB per
doubling of P, from 43 dB at P 64. Linear interpolation reaches the ceiling at
P ≥ 32–64, and μ needs 8 bits. **floor½ equals round** within ±0.1 dB, and it
needs no row P (see §4).

**FPGA arithmetic**: 18-bit coefficients, 12-bit output at 10 dB backoff,
Q0.24 phase. `mux`: A, B = round(K·c), round(K·c/√2), with the point product a
sign/zero mux. `mul`: points as integers ±16384, ±11585, multiplied. `bankgain`:
RX-style `bank_scale` coefficients (row Σ|c| ≤ 2¹⁷ − taps/2), then a 16-bit
gain multiplier with a 20-bit shift. No sample clipped in any configuration.

| P | span | sel | f64 | mux | mul | bankgain |
|---|---|---|---|---|---|---|
| 64 | 12 | floor½ | 43.16 | 43.14 | 43.14 | 43.14 |
| 64 | 12 | lin μ8 | 56.00 | 55.52 | 55.51 | 55.52 |
| 128 | 12 | floor½ | 48.15 | 48.07 | 48.07 | 48.07 |
| 256 | 12 | floor½ | 52.47 | 52.25 | 52.25 | 52.26 |
| 512 | 12 | floor½ | 54.74 | **54.38** | 54.38 | 54.38 |
| 1024 | 12 | floor½ | 55.69 | 55.26 | 55.26 | 55.26 |
| 256 | 16 | floor½ | 53.26 | 53.02 | 53.02 | 53.02 |
| 512 | 16 | floor½ | 56.09 | 55.61 | 55.60 | 55.60 |
| 1024 | 16 | floor½ | 57.46 | 56.84 | 56.84 | 56.85 |
| any | 16 | lin μ8 | 57.94 | 57.22 | 57.22 | 57.23 |
| any | 8 | — | ≤ 47.9 | ≤ 47.84 | | |

- The 18-bit coefficients cost nothing measurable: mux, mul and bankgain agree
  to 0.01 dB. The 12-bit output does cost something: it puts a 65.4 dB floor
  in-band (57.94 → 57.22 dB; theory: SQNR (647.6²·6) = 64.0 dB over 40 MHz,
  +1.4 dB in the matched filter's band).
- Against the RX (digital loopback, §6, 200 000 symbols, `FastFrontend`,
  ISDB-S design at 40 Msps, trim 0, 8PSK MER at the 6-bit output q / before
  the quantiser zk):

| TX | q (6-bit) | zk |
|---|---|---|
| f32 `Shaper` P 1024 span 16 (12-bit, 10 dB backoff) | 36.19 | 41.24 |
| mux floor½ P 64 / 128 / 256 / 1024, span 16 | 35.34 / 36.02 / 36.15 / 36.18 | 39.17 / 40.56 / 41.08 / 41.24 |
| mux lin μ8 P 64, span 16 / 12 / 8 | 36.18 / 36.76 / 36.86 | 41.23 / 43.39 / 43.79 |
| mux floor½ P 256, span 12 / 8 | 36.72 / 36.81 | 43.15 / 43.53 |
| **mux floor½ P 512, span 12** / span 16 | **36.74** / 36.18 | **43.35** / 41.20 |

  Another seed reproduces these within 0.02 dB. Compared with the f32 shaper,
  P 64 nearest costs 0.85 dB, P 128 0.17 dB, P 256 0.04 dB.
- Unexpected, but reproducible: **spans 8 and 12 read 0.5–0.7 dB higher than
  span 16 through this RX** (zk 43.4 against 41.2 dB), although their ISI
  through an ideal matched filter is lower. The RX filter (32 taps = ±11.5
  symbols under Kaiser β 5) is not the TX pulse's matched filter. A TX pulse cut
  near the RX window seems to leave less residual ISI in the cascade. I did not
  analyse this further. Span 8 is not recommended despite it: its out-of-band
  peak is −41.5 dB, against −49.3 dB (span 12) and −52.1 dB (span 16)
  (`shapewin 0.35 8 12 16`, truncation, beyond band edge + R/20).

**Recommendation: P 512, span 12, floor½, mux.** TX ISI 54.4 dB. In loopback it
measures 36.74 dB at the 6-bit output (the f32 shaper: 36.19).

## 2. Resource budget

### DSP48E1

| Option | DSP | TX-only image (stock 106 − DVB-S2 40 + TX) | TX+RX image (66 + RX 112 + TX) |
|---|---|---|---|
| **mux, nearest** (any P, span) | **0** | 66 (30 %) | **178 (81 %)** |
| mux, linear between rows (2 dot products, μ·Δ at the end) | 2 | 68 | 180 |
| multiply, span 8 (16 taps × I, Q) | 32 | 98 | 210 (95 %, risky) |
| multiply, span 12 | 48 | 114 | 226 ✗ |
| multiply, span 16 | 64 | 130 | 242 ✗ |
| multiply + coefficient interpolation (3 per tap), span 12 | 72 | 138 | 250 ✗ |

The phase accumulator is a 24-bit adder in fabric (`use_dsp = "no"`, as
the RX's loop adds); it needs no DSP. The TX needs no mixer: the AD9361's LO
sets the carrier. Any digital offset would cost 4 DSP + 1 RAMB18, as the RX's.
The backoff is in the coefficients, so it costs 0 DSP. A separate gain (the
`bankgain` column) would cost 2 DSP for nothing measurable.

Every option fits the TX-only image. In the TX+RX image only the mux versions
fit with a margin. The first format-4 build showed what happens near 220: at
220 / 220 the placement pressure broke timing in Tezuka's own paths. If a
multiply version were wanted anyway, the cuts would be: the TX FIR interpolator
(20 DSP; not in the path when the block feeds the DAC at 40 Msps), giving
158 + 48 = 206 for span 12; then maia-sdr (18), giving 140 + 64 = 204 for
span 16.

**The 8PSK trick.** All ISDB-S points (TC8PSK, QPSK at ±45°/±135°, BPSK at
0°/180°: `isdbs_phy::demod::psk8_point`, `qpsk_point`, `tx::bpsk`) are
e^{jkπ/4}. A point with k even is (±1, 0) or (0, ±1); with k odd it is
(±c, ±c), c = 1/√2. So per tap only one magnitude is ever needed: A = coef for
even k, B = round(c·coef) for odd k. **One ROM read per tap, addressed by
{k & 1, q}, serves both I and Q.** Each component is then +v, −v or 0 by the
sign tables sI = [1,1,0,−1,−1,−1,0,1] and sQ = [0,1,1,1,0,−1,−1,−1] (the zero
code gives 0). No multiplier is needed, and B is rounded from the float, not
from A. The trick is specific to ISDB-S's 45° grid: ISDB-S3's 16/32APSK would
need per-ring ROMs (or multipliers), and α 0.03 needs span 32–64 anyway.

### BRAM, and how the RX's banks fit in one RAMB18

- **RX:** its one RAMB18 is the mixer's 1024 × 16 cosine table. The 48
  coefficient memories (k on 32 taps, h and g on 8 each, 64 × 18 bits each) and
  the error queue are **distributed RAM: 348 RAM64M** in the Yosys stat. That
  is about 1 392 LUTs, which `estimate.py`'s LUT figure leaves out:
  4 487 + 1 392 = 5 879, against Vivado's 5 825 for `isdb_symbols`. Yosys LUT
  plus 4·RAM64M is a good predictor of Vivado's total.
- **TX:** P × 2S taps × 18 bits × 2 (A, B). For P 512, span 12 that is
  442 368 bits; the symmetry halves it to 221 184 bits = 12 RAMB18. The
  symmetry is exact for floor½ rows: row q tap t is at x = (q + ½)/P + t − S,
  and row P−1−q tap 2S−1−t is at −x. So each tap pair (t, 2S−1−t) shares one
  1024 × 18 true-dual-port RAMB18: port A reads {k_t&1, q} (it is also the
  load port), port B reads {k_u&1, ~q}. The BRAM count is set by ports, not
  bits: without the symmetry the sketch needs 24 RAMB18, and P 256 also needs
  12 (half full). P ≤ 32 would fit RAM64M like the RX, but its 37.6 dB is too
  low.
- Device: 140 RAMB36. The isdb image uses 62; +6 RAMB36 gives 68 (49 %).
- Loading: 2·P·S = 12 288 coefficients (A, B, half bank). Through GPIO
  register writes (0.62 ms each through iiod) that takes 7.6 s, or 3.8 s for P
  256. Recommended: put the default bank (ISDB-S, 40 Msps, 10 dB) in the
  bitstream as BRAM init, from the model's vectors file as the RX does. Reload
  only for other rates, through the GPIO port or a load command in the TX DMA
  stream (well under 1 ms).

## 3. Yosys estimate (`synth_xilinx -family xc7`, YoWASP Yosys 0.70)

```sh
cd fpga && uv run --frozen python synth/tx_sketch.py MODE P SPAN [linear]
```

(`--locked` refused: uv picked CPython 3.14 and wanted to relock; `--frozen`
uses the pinned lock as is.)

| Sketch | DSP48E1 | LUT | FF | BRAM | CARRY4 |
|---|---|---|---|---|---|
| **mux P 512 span 12** | **0** | **1 962** | **2 266** | **12 RAMB18** | 393 |
| mux P 256 span 12 | 0 | 1 961 | 2 265 | 12 RAMB18 | 393 |
| mux P 64 / 128 span 12 | 0 | 1 959 / 1 960 | 2 263 / 2 264 | 12 RAMB18 | 393 |
| mux P 256 span 8 | 0 | 1 316 | 1 477 | 8 RAMB18 | 265 |
| mux P 256 span 16 | 0 | 2 604 | 2 957 | 16 RAMB18 | 521 |
| mux P 1024 span 16 | 0 | 2 606 | 2 959 | 16 RAMB36 | 521 |
| mux linear P 64 span 12 (36-bit rows q, q + 1) | 2 | 3 879 | 4 341 | 24 RAMB18 | 779 |
| mul P 256 span 8 | 32 | 762 | 737 | 16 RAMB18 | 155 |
| mul P 256 span 12 | 48 | 1 138 | 1 217 | 24 RAMB18 (one memory per tap; pairing → 12) | 231 |
| mul P 256 span 16 | 64 | 1 516 | 1 537 | 32 RAMB18 | 307 |

None of these use LUTRAM, so the LUT figure is the whole count. At span 12 the
mux saves 48 DSP for +824 LUT and +1 049 FF (the term registers and the
narrower tree, against the products the DSPs absorb). Device: 53 200 LUT and
106 400 FF. The isdb image (21 694 LUT, 28 421 FF) grows to about 23.7k LUT
(45 %). The first sketch, with separate load and read addresses, made Yosys
duplicate each pair memory (24 RAMB18). With the load sharing port A's address
it maps to 12.

**Bit-exact check of the sketch**
(`uv run --frozen python synth/tx_sketch_sim.py 256 12`, then `64 8`): 2 960
samples equal a plain Python model of the arithmetic, at latency 8 clocks
(span 12) and 7 (span 8). The run includes 91 underrun clocks (a 30-clock gap
and random 2 % gaps). This validates the parity addressing, the symmetric port
B at ~q, the sign tables and the zero insertion.

Core of the sketch (the mux version; full file: `fpga/synth/tx_sketch.py`):

```python
acc = Signal(FRAC + 1, attrs=NO_DSP)                 # D0: Q0.24 phase
line = [Signal(4, name=f"ln{t}", init=8) for t in range(w)]   # codes, 8 = zero
m.d.comb += nxt.eq(acc[:FRAC] + self.step)
m.d.sync += acc.eq(nxt)
wrap = nxt[FRAC]
m.d.comb += self.s_ready.eq(wrap & self.s_valid)      # pull one code per wrap
with m.If(wrap):
    m.d.sync += [line[t].eq(line[t - 1]) for t in range(1, w)]
    m.d.sync += line[0].eq(Mux(self.s_valid, self.s_code, 8))   # underrun: zero
    with m.If(~self.s_valid):
        m.d.sync += self.underrun.eq(1)
m.d.sync += [q.eq(acc[FRAC - pb:FRAC])] + [snap[t].eq(line[t]) for t in range(w)]  # D1
for t in range(span):                                 # one RAMB18 per tap pair
    u = w - 1 - t
    mem = Memory(shape=unsigned(18), depth=2 * P, init=A_rows_t + B_rows_t)
    rp0, rp1, wp = mem.read_port(), mem.read_port(), mem.write_port()
    pa = Mux(self.c_we, self.c_addr, Cat(q, snap[t][0]))     # port A = load port
    m.d.comb += [wp.addr.eq(pa), rp0.addr.eq(pa),
                 rp1.addr.eq(Cat(~q, snap[u][0]))]           # c[P-1-q][u] = c[q][t]
    for rp, tt in ((rp0, t), (rp1, u)):                      # D2 data, D3 terms
        ti, tq = term(rp.data.as_signed(), codes_d2[tt])     # ±v or 0 by sI/sQ[k]
        leaves_i.append(ti); leaves_q.append(tq)
si, _ = tree(leaves_i, "ti", 24)                      # D4..D8, use_dsp = "no"
sq, _ = tree(leaves_q, "tq", 24)
for s, o in ((si, self.o_i), (sq, self.o_q)):         # D9: round >> S, sat 12
    r = (s + (1 << (S - 1))) >> S
    m.d.sync += o.eq(Mux(r > 2047, 2047, Mux(r < -2048, -2048, r)))
```

## 4. Fixed-point model spec (`isdb_frontend::shape::fixed`, the TX twin of `fixed::Frontend`)

**Input codes** (4 bits; the DMA carries them): bits 2..0 are the angle index
k (point e^{jkπ/4}); bit 3 set means the zero symbol. I suggest angle indices
rather than TC8PSK labels, because the BPSK header/bursts and QPSK slots are on
the same grid. The label → k map (table 46: 000→0, 001→1, 011→2, 010→3, 100→4,
101→5, 111→6, 110→7) stays on the host; it could also be an 8-entry LUT in the
block. Packing: 8 codes per 32-bit DMA sample (the I/Q pair of the DAC DMA).
That is 14.43 MB/s at 28.86 Mbaud, against 160 MB/s for cs16 IQ at 40 Msps, so
it fits the board's measured 60–70 MB/s Ethernet ceiling. (That ceiling was
measured for RX; iiod's TX push rate is not measured.)

**Parameters** (`TxParams`, from `TxDesign {sample_rate, symbol_rate, roll_off,
span, backoff_db}`):
- `step: u32`, Q0.24 symbol periods per sample: `round(2²⁴·R/fs)`. Validate
  0 < step < 2²⁴, i.e. fs > R (at most one symbol per sample), and fs ≥ (1+α)R
  as `Shaper`.
- `shift: u8` = S = the largest s with max|A| ≤ 2¹⁷ − 1 (S = 7 at 10 dB:
  max|A| = 90 813).
- Banks `a[q][t]`, `b[q][t]`: `i32` within 18 bits, P = 512 rows of 2S taps.
  Row q is at x = (q + ½)/P + t − S, with a = round(G·p(x)·D·2^S) and
  b = round(G·p(x)·D·2^S/√2) (both from f64). p = `tables::rrc` truncated
  (no window, as `Shaper`). G = `Shaper`'s gain (mean row energy 1).
  D = 2048·10^(−backoff/20). Build one half and mirror it, so the symmetry is
  exact; `validate` checks `a[q][t] == a[P−1−q][2S−1−t]` (same for b),
  because the FPGA stores only one copy (as `validate` enforces the RX's TED
  zeros).
- Backoff → DAC scale: unit-RMS symbols give RMS |y| = D LSB of the 12-bit DAC
  (10 dB: D = 647.6, measured 647.6; per component 457.9). This is the CLI's
  convention (`input_sample` ×2048, `--backoff-db` 10 → RMS 0.316). The worst
  row has Σ|G·p| = 1.616 (span 16; 1.609 at 12), so |component| ≤ 1 047 LSB
  (measured peak 992 over 200 000 symbols). It cannot saturate for backoff
  ≥ 4.2 dB. Set the level with the AD9361's TX attenuation (0.25 dB steps),
  not the bank.

**State:** `frac: u32` (24 bits), the line of 2S codes (newest at t = 0),
counters (samples, consumed codes, underrun symbols, a sticky underrun flag).
**Start:** line all zero codes, frac = 0, then one forced wrap: the first code
enters before sample 0. This makes sample n the `Shaper` signal model's sample
n, up to the phase grid and the integer rounding.

**Per output sample n (one per clock):**
1. q = frac >> (24 − log2 P) (9 bits at P 512); the row is at (q + ½)/P, so the
   error is ≤ ½P, the same as nearest-of-P + 1, and no row P is needed;
2. for t in 0..2S with a non-zero code k_t: v = (k_t odd) ? b[q][t] : a[q][t];
   I += sI[k_t]·v, Q += sQ[k_t]·v (`i32`; |Σ| < 2¹⁸ for these banks, so 24-bit
   sums are plenty);
3. out = clamp(round_shift(sum, S), −2048, 2047), with `fixed::round_shift`
   (add half, arithmetic shift) and saturation as `sat16` but to 12 bits.
   The DAC word is out << 4 in axi_ad9361's 16-bit `dac_data` (MSB-aligned
   12-bit; to confirm in Tezuka's wiring);
4. frac += step; on carry (frac ≥ 2²⁴): frac −= 2²⁴, the line shifts by one,
   and the next code enters (consumed from the FIFO), or the zero code with
   the underrun counter incremented if the FIFO is empty. The symbol that
   enters is used from sample n + 1. Wrap and entry are the same clock, as in
   the sketch.

**Phase accumulator and rate error** (`txinterp rate|frac`):

| fs | step Q0.24 | symbol-rate error | RX period Q8.24 (nominal) |
|---|---|---|---|
| 40 Msps | 12 104 761 (0xB8B439) | −0.028 ppm | 23 253 245, +0.016 ppm |
| 57.72 Msps | 0x800000 | exact | exact |
| 61.44 Msps | 0x784000 (481/1024) | exact | +0.007 ppm |

Fewer bits for comparison: Q0.20 gives −0.55 ppm and Q0.16 +4.7 ppm; Q0.32
gives 0.00002 ppm for 8 more FFs. The ISI MER is 57.49 dB from 16 to 63 bits:
the rate error is a slow drift, not jitter. The f32 shaper's exact 63-bit step
exists to count samples per superframe exactly. In hardware the DAC clock is
the reference, so 0.028 ppm (0.8 baud) is a tiny constant rate offset.
**The receiver absorbs it.** In loopback (one AD9361 clock for both) the
relative offset is 0.044 ppm (TX −0.028, RX nominal +0.016). Over the air the
two boards' clocks dominate (≈ 0.6–1 ppm). The RX loop already locks the board's
0.68 ppm against the satellites and stays within 5 ppm on every capture, and its
integrator range is far wider.

**Test vectors** (the model writes them, the HDL reproduces them, as
`vectors.rs`):
- header: `step`, `shift`, P, span, banks a and b (full P × 2S), the code
  stream, and the per-clock FIFO-valid pattern;
- per output sample: n, frac, q, wrap, code consumed or underrun, the line
  (2S codes, newest first), sum_i / sum_q (pre-round `i32`), out_i / out_q,
  and the saturation flags;
- patterns: continuous; random gaps (underrun); one long gap; the bank written
  through the load port; a deliberately asymmetric bank refused by `validate`;
  one LSB changed in a, in b, and in the far tap of a pair must fail.

**Pipeline at one sample per clock** (40 MHz = the DAC rate; `frontend.py`
docstring style):

```
D0  frac += step (24-bit + carry); on carry: line shift, next code or zero
    (+ underrun), s_ready to the FIFO
D1  q = frac[23:15], snapshot of the 2S codes; BRAM addresses
    {k_t&1, q} (port A) and {k_u&1, ~q} (port B), u = 2S-1-t
D2  BRAM data (registered)
D3  terms: per tap and component +-v or 0 (19-bit)
D4..D8  adder tree 24 -> 1 in five levels (24-bit, use_dsp = "no")
D9  round >> S, saturate to 12 bits, register -> dac_data
```

The simulated latency is 8 clocks (span 12) or 7 (span 8) from the
accumulator to the output. It is constant and irrelevant to the signal; the
model need not represent it. Nothing feeds back, so the pipeline needs no loop
delay (unlike the RX's 24).

## 5. Underrun

The DAC keeps converting every clock, so there is no "stop": the block must
emit some sample. Measured (`txinterp gap`, P 256 span 12 → `FastFrontend`).
Symbols 100 000.. are replaced for G symbols, and the MER is taken at the 6-bit
output in 3 000-symbol windows after the gap:

| Fill | G | MER before | after +0 / +3k / +10k / +30k | RX AGC gain (2 496 before) | q at full scale, first 3k after |
|---|---|---|---|---|---|
| none | — | 36.90 | 36.94 / 36.89 / 36.96 / 36.89 | 2 495 | 0 |
| zero symbol | 2 000 | 36.90 | **15.84** / 36.92 / 36.96 / 36.89 | 10 307 at gap end | 140 |
| hold last | 2 000 | 36.90 | 36.94 / 36.92 / … | 2 489 | 0 |
| point 000 | 2 000 | 36.90 | 36.94 / 36.89 / … | 2 489 | 0 |
| zero symbol | 40 000 | 36.90 | **9.58** / 35.71 / 36.99 / 36.95 | 65 535 (clamped) | 721 |
| hold last | 40 000 | 36.90 | 34.04 / 35.71 / 36.88 / 36.95 | 2 488 | 0 |
| point 000 | 40 000 | 36.90 | 34.24 / 35.71 / 36.88 / 36.95 | 2 489 | 0 |

- **Zero symbol.** The output goes quiet. The RX timing loop coasts (e = 0)
  and the TX symbol clock never stops, so timing is kept. The RX AGC winds up
  during the gap, so the first ≈ 3 000 symbols after it clip. The RX front end
  recovers within 0.1 ms (2k gap) or 0.35 ms (40k gap).
- **Hold, or point 000.** A constant symbol is a CW line at the carrier. The
  AGC sees constant power, so nothing winds up. The timing detector has no
  transitions, so the loop coasts too.
- **For the receiver the data is lost in every case.** The gap delays the
  following symbols, so the frame grid shifts by G and that superframe is
  lost. The ISDB-S receiver re-syncs on W1 / TMCC (≥ one 1.38 ms frame), which
  is longer than any of the front-end recoveries above.
- **Recommendation: the zero symbol, a sticky `underrun` flag and a 32-bit
  missing-symbol counter** readable by the host. It is the same path as the
  lead-in and the `finish` tail (the zero code), it emits no spectral line,
  and the model already defines it. Two more things help. A **prefill** (start
  the output only when the FIFO holds N codes) avoids start-up underruns. The
  AXI DMA's FIFO read side should signal underflow; ADI's `axi_dmac` has an
  underflow output on its FIFO interface, to confirm. Hold-last is a one-line
  alternative if AGC wind-up ever matters, but the AGC is the RX's to fix (it
  could freeze on low power).

## 6. Digital loopback inside the AD9361 (acceptance of an on-board self-test)

The chain is: TX block → axi_ad9361 DAC data → AD9361 BIST data-port loopback
→ ADC data → RX block. The RX block then sees the TX block's 12-bit integers
unchanged (assuming the conditions below), with one clock for both, so there is
nothing analog in the path. Measured with the model chain (`txinterp loop`, TX
integers → `FastFrontend`, ISDB-S design at 40 Msps, trim 0, 200 000 random
8PSK symbols, MER over the second half):

| | 6-bit q | zk (before the quantiser) |
|---|---|---|
| recommended TX (P 512, span 12, mux, Q0.24, 10 dB) | **36.74 dB** | 43.35 dB |
| P 256, span 12 | 36.72 | 43.15 |
| f32 `Shaper` reference (P 1024, span 16) | 36.19 | 41.24 |

- **Acceptance figures.**
  - 8PSK MER **≥ 36.0 dB** at the RX block's 6-bit output (`metrics::psk_mer`,
    loop settled). The measurement is 36.7 dB; the margin covers the trim
    search and the symbol content.
  - Real ISDB-S frames (`isdbs_phy::tx::Transmitter` symbols as codes) must
    decode with every TS error-free.
  - The receiver's measured trim must be within ±0.05 T of 0.
- **A stronger, bit-exact test** for the same setup: a short buffered 12-bit IQ
  capture in loopback, into DDR rather than streamed (2²⁰ samples = 26 ms). It
  must equal the TX model's samples for the same codes after an offset search.
  The RX block's symbols on that capture must then equal `fixed::Frontend` run
  on it, which is the existing RX equality.
- **Conditions to check on the board** (not verified here):
  - the AD9361 driver's debugfs `loopback` = 1 is the digital TX → RX loopback
    at the AD9361's data port. Mode 2 is the FPGA-side RX → TX loopback. I am
    working from memory of ADI's documentation, so confirm whether mode 1 sits
    outside the AD9361's TX/RX FIR and half-band filters;
  - axi_ad9361's RX DC filter and IQ correction and TX IQ correction must be
    off or at unity;
  - Tezuka's decimators must be in bypass at 40 Msps;
  - the 12-bit alignment on `dac_data` / `adc_data` must be as assumed (§4,
    step 3);
  - TX and RX must run at the same 40 Msps.

## 7. Commands run

```sh
git merge --ff-only main                      # shape.rs was not on the branch yet
cargo run -q --release -p isdb-frontend --example txinterp -- rate
cargo run -q --release -p isdb-frontend --example txinterp -- grid     # §1 table 1
cargo run -q --release -p isdb-frontend --example txinterp -- arith    # §1 table 2
cargo run -q --release -p isdb-frontend --example txinterp -- frac
cargo run -q --release -p isdb-frontend --example txinterp -- peak
cargo run -q --release -p isdb-frontend --example txinterp -- loop [N SEED]   # §1/§6
cargo run -q --release -p isdb-frontend --example txinterp -- gap 0 2000      # §5
cargo run -q --release -p isdb-frontend --example txinterp -- gap 0 40000
cargo run -q --release -p isdb-frontend --example shapewin -- 0.35 8 12 16
cargo run -q --release --locked -p isdb-frontend --example vectors -- \
    fpga/build/vec-2-s3-t32.json - s3 32 3000 6 8
cd fpga && uv run --frozen python synth/estimate.py build/vec-2-s3-t32.json 32
#   -> 112 DSP48E1, 4487 LUT, 5426 FF, 1 RAMB18E1 (+348 RAM64M in the stat)
uv run --frozen python synth/tx_sketch.py mux 512 12      # and the other rows of §3
uv run --frozen python synth/tx_sketch_sim.py 256 12      # bit-exact, latency 8
uv run --frozen python synth/tx_sketch_sim.py 64 8        # bit-exact, latency 7
```

Not measured: Vivado place and route, so timing at 40 MHz is unverified (the
deepest stage is a 24-bit add, far below the RX's critical paths); iiod TX
throughput; and anything on the board.
