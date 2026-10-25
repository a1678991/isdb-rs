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
