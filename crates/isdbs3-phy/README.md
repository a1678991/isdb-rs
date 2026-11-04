# isdbs3-phy

ISDB-S3 receive chain after the symbol front end (`isdb-frontend`, the CPU
model of the FPGA stage): slot and frame synchronisation on the
π/2-BPSK sync words, carrier phase per slot, TMCC decoding (π/2-BPSK →
shortened LDPC 61/120 → energy dispersal → BCH), the pilot-learned
constellation, and the main signal of each frame (max-log demapping, bit
de-interleaving, LDPC, energy dispersal, BCH) to each slot's header and
data. Frame layout and codes are in `isdbs3-core`.

Examples (development tools): `tmccprobe` (TMCC of every frame of a
recording), `tmccbch` (the energy-dispersal readings tried before BCH
succeeded), `mainprobe` (main-signal LDPC/BCH per frame, nominal vs
learned constellation).

Diagnostics are `tracing` events, never printed; the crate installs no
subscriber (the `isdb` binary does). `info`: the acquisition (carrier,
trim, sync Es/N0), slot sync found or not found (`offset`,
`sync_snr_db`), the first TMCC after a search and every change of the
slot assignment (the modes in use, order of change). `debug`: the
carrier candidates, a reused or rejected prior acquisition, a sync
confirmed at the known offset, the equaliser, each frame's TMCC and its
LDPC / BCH failure counts (`ldpc_failed`, `bch_failed`, `mer_db`) and
the block's totals. Nothing is emitted per symbol or per slot.

## Speed (32-thread host, one 0.826 s block at 40 Msps)

`rx::receive` takes 0.60 s per 0.826 s block (was 5.85 s): every frame
is prepared in parallel (TMCC, main symbols, pilots), all codewords of
the block are demapped and decoded in one pool
(`main_signal::decode_frames`), each frame's dispersal and BCH run in
parallel, then the slots are assembled in order, stopping at the first
incomplete frame as a sequential loop would; the slot search runs once
(`sync::synchronise_at` after the equaliser, whose output is aligned
with its input), the equaliser in chunks on every core. Each value is
computed as before: decoded TLV streams are byte-identical.

`frontend::symbols_with_prior` lets a block reuse an earlier block's
carrier and trim: the front end runs over the block's first 0.05 s with
them and, if the sync-symbol Es/N0 is within 3 dB of the earlier
acquisition's, the survey, the carrier choice (0.11 s) and the trim
search (0.72 s) are skipped. The front end itself (`isdb-frontend`
`FastFrontend`, 1.13 s per block) is one sequential stream per block.

More development tools: `chaintime`, `rxtime`, `rxparts`, `fetime`
(where the time goes), `blockacq` (each segment's acquisition, fresh and
with the first segment's).

## The fixed-point chain

`chain` is the streaming fixed-point model of what the FPGA will run
after its symbol front end (docs/plans/isdbs3-fpga-rx.md, piece 3): one
6-bit symbol in per `Chain::push`, every stage in integers whose widths,
roundings and thresholds the module documentation states (it is the
specification the HDL follows), and frames out with their slots, TMCC
and status, which `chain::out::blocks` turns into the `Slots` boundary
format (`isdb_frontend::slots`). Stages: NCO and carrier loop (gain ½
per frame from the SSync phase steps), slot sync (±I±Q correlation,
max + ½min magnitude, a 9 296-position accumulator with a ⅛ leak, lock
after 4 periods and loss after 8, frame heads by a streaming
sign-minority rule), slot phase and amplitude (16-step CORDIC per head,
linear interpolation, the cosine table, one reciprocal per head),
11-tap Q2.14 equaliser with 48-bit training sums solved by the host
(`eq::solve`), TMCC soft bits (45 per unit amplitude, known zeros +127),
the max-log demapper with |y|² cancelled against the previous frame's
learned points and n0, de-interleaving by address, then the slot
dispersal and BCH as `main_signal`. The LDPC is a `chain::ldpc::LdpcDecoder`
given to the chain (default `isdb_fec::ldpc::fixed`, 50 iterations,
offset 1). The layout decoded from frame N's TMCC is used for frame
N + 2 (B44 §3.11, SPEC-NOTES "tmcc-timing"; lag 1 selectable), so the
first two frames after a (re)start are not decoded.

Measured against the floating-point receiver (2026-11-01):

| Check | Tool | Result |
|---|---|---|
| Whole chain vs `rx::receive` as `isdb rx --symbols` streams it, 40 s FPGA recording of BS7 (−989 … 0 Hz drift, two losses) | `chaincheck` | 1 207 frames on both; 115 296 slots compared, all byte-identical (through the `Slots` words); 0 LDPC / BCH failures on either side; mean 2.095 / 2.081 LDPC iterations; the 6 frames not decoded are the 2 after each of the 3 (re)starts (lag 2; lag 1: 3 frames, 115 584 slots identical) |
| Stage 1 on six IQ recordings (61.44 and 40 Msps, through the bit-exact front end) | `chainiq` | the slot offset and SSync parity of `s3sync::measure`; every chain slot head and frame head is one of `sync::synchronise`'s; 10 272 slots byte-identical with `rx::receive` |
| Stages 1–3 on the FPGA recording | `chainsync` | 1 796 heads and 15 frame heads equal; head carrier phase within 3.4 mrad (1.7 rms), amplitude within 0.13 % |
| Stage 4 | `chainstages` | integer sums = the f64 sums of the same symbols exactly; Q2.14 rounding ≤ 0.68 LSB; within 6.3 LSB (2.3 rms) of `eq::train` normalised by the centre tap (the chain equalises after the slot correction) |
| Stage 5 | `chainstages` | TMCC soft-bit signs 100 % equal to `tmcc::llrs`; decoded TMCC identical on 85 of 85 frames |
| Stage 6 | `chainstages` | LLRs 99.996 % equal to `main_signal::demap` + `fixed::quantise(1)`, never more than 1 apart |
| CPU (one thread, LDPC apart) | `chaincheck`, `chainprof` | 4.9 s per s of signal on the recording (2.7 s per s on 5 synthetic frames, 0.96 of it the slot sync): a reference model, not a fast path |

`chainvectors` writes the HDL's test vectors (fpga/README.md "Chain
vectors"); the chain's tests decode synthetic streams (`chain::synth`:
mixed modulations with dummies, a drift, a loss, any push split) back to
the data sent. `chain::synth::iq` gives the same frames as IQ samples at
a radio's rate (a symbol-spaced channel, the §3.10 root-raised cosine
through `isdb_frontend::shape`, the carrier offset and noise): the
acquisition's and the front end's synthetic input.

`chain::probe::probe_taps` solves the equaliser's taps from a short IQ
capture before a radio's chain runs: the front end's model with the
parameters the FPGA's front end gets, this chain's model (the TMCC
decoded, the main signal not), the first frame's training sums solved as
the host's slow loop solves the FPGA's. The sums are taken at the
equaliser's input, so they do not depend on the taps in use. On the
acquisition's 0.2 s probe at 40 Msps the first sums come from the third
frame after the frame sync (≈ 0.13 s of signal), in 0.3 s on the CPU
(the front end and the chain on two threads); on a synthetic 16APSK
signal through an echo (Es/N0 15 dB) the taps are within 0.017
(`‖Δw‖ / ‖w‖`) of the chain's own solves of every later frame and give
14.5 dB of training MER on them against 12.9 dB for the identity
(`isdb-cli` example `s3open`). `isdb rx` starts a radio's chain on them.
