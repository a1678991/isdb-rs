# Spec interpretation notes — index

Every place where the specification text is ambiguous, garbled by PDF
text extraction or seemingly inconsistent is recorded in the
`SPEC-NOTES.md` of the crate that implements it: spec + clause, quoted
text, the interpretation adopted, and how it is confirmed (status:
confirmed | pending real-world check). Quotes are from the editions
listed in [SPECS.md](SPECS.md) (extracted locally into the gitignored
`specs/` directory; the specifications are not redistributed).
[SPECS.md §3](SPECS.md#3-interpretations-and-deviations-index) lists
every entry with its status.

| Crate | Notes | Topics |
|---|---|---|
| `isdb-ts` | [crates/isdb-ts/SPEC-NOTES.md](crates/isdb-ts/SPEC-NOTES.md) | H.222.0 2006 table numbering, PES extension/stuffing, adaptation field edge cases, CC and duplicates, section assembly, PMT CRC "Annex B", reserved bits, PCR range, 192/204-byte framings |
| `isdb-text` | [crates/isdb-text/SPEC-NOTES.md](crates/isdb-text/SPEC-NOTES.md) | SI initial state (G3 = Katakana), character widths, Kanji / additional-symbol Unicode mapping, DRCS/macro, control codes |
| `isdb-si` | [crates/isdb-si/SPEC-NOTES.md](crates/isdb-si/SPEC-NOTES.md) | Annex C time formulas, section length limits, descriptor syntax vs semantics conflicts, descriptors defined outside B10, Table 6-1 mandatory descriptors |
| `isdb-analyze` | [crates/isdb-analyze/SPEC-NOTES.md](crates/isdb-analyze/SPEC-NOTES.md) | severity classification, Table 5-2 transmission level vs frequency, PCR interval/accuracy, repetition time base, table_id per PID, EIT rules, cross-table identities, PES timing |
| `isdbt-mux` | [crates/isdbt-mux/SPEC-NOTES.md](crates/isdbt-mux/SPEC-NOTES.md) | timing model, PCR placement, repetition intervals, section packing, NIT/SDT/EIT/BIT/TOT/CAT content, media packetisation, AAC ADTS header rules (the raw_data_block is not parsed), recorded-program relay (`ts-program`) and its copy-control and conditional-access signalling, T-STD pacing, **receiver compatibility notes**; list of **operational choices** (not dictated by the local specs, ARIB TR-B14 unavailable) to be tested on receivers |
| `isdbs-core` | [crates/isdbs-core/SPEC-NOTES.md](crates/isdbs-core/SPEC-NOTES.md) | ISDB-S (BO.1408-1, MIC Ordinance No. 87 annexed tables 45–51): frame layout, burst data, BPSK bit order of the inner code, TMCC energy dispersal, TMCC information bit layout (Notification 304/2011 unavailable), "other information" values, dummy-slot placement, unused TS_IDs; main signal: energy dispersal, interleaver order, QPSK axes, TC8PSK C1/C0 and its decoding, the inner code across a frame, unassigned slots |
| `isdbs3-core` | [crates/isdbs3-core/SPEC-NOTES.md](crates/isdbs3-core/SPEC-NOTES.md) | ISDB-S3 (ARIB STD-B44 v2.1): LDPC table transcription, the TMCC code-rate table (101/120 printed twice; 0111 = 93/120), energy-dispersal output stage, TMCC bit order, frame marker, interleaver rows and direction, codeword placement, BCH vs dispersal order, constellation labels, pilot content |
| `isdb-tlv` | [crates/isdb-tlv/SPEC-NOTES.md](crates/isdb-tlv/SPEC-NOTES.md) | TLV streams (B32 Part 3, B60, B44 slot header): head-TLV indication, header-compressed IP layout, PA message table list (N = 0 on air), control-message header, TLV-SI in control packets |
| `isdbt-fec` | [crates/isdbt-fec/SPEC-NOTES.md](crates/isdbt-fec/SPEC-NOTES.md) | remux model receiver, PRBS bit order and frame alignment, delay adjustment, byte-interleave alignment, convolutional taps / puncturing reset, one-frame latency contract |
| `isdbt-ofdm` | [crates/isdbt-ofdm/SPEC-NOTES.md](crates/isdbt-ofdm/SPEC-NOTES.md) | mapping bit order and delays, DQPSK reference, time/frequency interleave indexing and carrier rotation, pilots, TMCC/AC, difference-set code bit order, frame alignment (`latency`), gr-isdbt deviations |
| `isdbt-phy` | [crates/isdbt-phy/SPEC-NOTES.md](crates/isdbt-phy/SPEC-NOTES.md) | broadcasting TS (B31 Part 2 §5.5): the IIP's slot and PID, the Table 5-7 layer_indicator typo, which frame the IIP describes, IIP_packet_pointer, the optional (204,196) parity, stuffing / reserved conventions, CRC_32 ranges, what is not implemented (sub-IIP, AC data, SFN delay control) |

`isdbt-core`, `isdb-radio`, `isdb-hackrf`, `isdb-iio`, `isdb-iq`,
`isdb-frontend`, `isdbs3-phy` and `isdb-cli` have no notes of their own: they take
parameters from the spec tables, connect the baseband to a radio, record
and survey IQ or turn it into symbols with this project's own format
(`isdb-frontend` defines format version 1 itself; `isdb-iq`
quotes two symbol rates; the satellite channel plans of `isdb-cli` are
listed in [SPECS.md §1.2](SPECS.md#12-referenced-but-not-available-locally)),
or wire the crates above together (`isdbt-phy`'s
pipeline composition has none either; its notes cover only the
broadcasting TS).

Format of an entry:

```
## <crate>: <topic>
- Spec: ARIB STD-B31 v2.2 §x.y, Table/Fig …
- Text: "…"
- Interpretation: …
- Confirmation: … (status: confirmed | pending real-world check)
```

## Cross-crate decision: OFDM frame alignment (B31 Part 2 Figs. 3-8/3-9)

- Spec: ARIB STD-B31 v2.2 Part 2, Fig. 3-8 (transmitting side) and Fig. 3-9
  (receiving side). Fig. 3-8: "(Two-symbol delay is caused by bit
  interleaving. Therefore, frame synchronization is delayed by two
  symbols.)"; the OFDM frame is formed "by 1-frame buffer".
- Decision: the modulator advances each layer's bit-interleaved
  carrier-symbol stream by 2 OFDM symbols through a one-frame buffer, so
  data-frame symbols 1–2 go on air as the last two symbols of the previous
  OFDM frame and, after the receiver's bit deinterleaver, data-frame symbol
  `s` is OFDM-frame symbol `s`. Energy dispersal and puncturing reset at
  the OFDM frame start; byte (de)interleaving plus delay adjustment add
  exactly one frame.
- Where: `isdbt-ofdm` "latency" (`src/latency.rs`, the TX first output is
  on-air frame −1; receiver per-layer alignment FIFO) and `isdbt-fec`
  "energy: frame alignment", "conv: puncturing … reset point", "layer:
  latency / alignment contract".
- Confirmation: gr-isdbt's TX applies the same 2-symbol advance
  (`skiphead(2)` after its frequency interleaver); its output was decoded
  by a MyGica S270 USB tuner with PER 0, the Rust TX/RX cross-decode
  byte-exactly with gr-isdbt in both directions in 8 configurations
  ([VERIFICATION.md](VERIFICATION.md)), and the Rust TX itself was
  received by the same tuner and a Sony BDZ-series recorder over a
  conducted link. Status: confirmed via the oracle and real receivers, and
  by a broadcast decoded RS-clean in both layers
  ([VERIFICATION.md](VERIFICATION.md#broadcast-reception)).
