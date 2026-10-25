# isdbs3-core

ISDB-S3 transmission parameters from ARIB STD-B44 v2.1 (ITU-R BO.2098-0
"System F"; MIC Ordinance No. 87 of 2011, Chapter 5 §3): frame and slot
geometry, the eleven inner code rates and their LDPC codes (address tables
transcribed from the standard by `tools/b44-ldpc-tables.py`), the
π/2-BPSK / QPSK / 8PSK / 16APSK / 32APSK constellations with their bit
labels, the bit interleaver, the slot / TMCC / pilot energy dispersals, the
TMCC information codec and signal coding, and the slot allocation.

The BCH and LDPC machinery is in `isdb-fec`; the receiver is
`isdbs3-phy`. Interpretations of the standard, and how each was confirmed
on live signals, are in [SPEC-NOTES.md](SPEC-NOTES.md).

| Module | B44 clause | Contents |
|---|---|---|
| `params` | §3.2–§3.4, Tables 3.2-1, 3.4-2, 3.8-2/3 | geometry, modulations, code rates, γ |
| `ldpc`, `ldpc_tables` | §3.4 (2) Appendix 1 | the LDPC codes |
| `constellation` | §3.8, Fig. 3.8-1 | signal points and labels |
| `interleave` | §3.7 | bit interleaver |
| `dispersal` | §3.6 | energy dispersal |
| `tmcc` | §3.2 (3), §3.5, §3.11 | TMCC information and signal |
| `slots` | §3.2 (2) | slot allocation, codeword placement |
