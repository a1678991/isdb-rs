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
