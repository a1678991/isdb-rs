# isdb-tlv

TLV streams of ISDB-S3 / advanced BS (ARIB STD-B32 v3.11 Part 3 §3.5,
§3.7, §3.9, §3.10; ARIB STD-B60 v1.14; ARIB STD-B44 v2.1 §3.2 (1)):

- `tlv`: TLV packets and the `Assembler` that rebuilds them from a
  stream's slots, using each slot header's head-TLV indication to gain
  and check packet sync and to recover after a lost slot;
- `ip`: IPv4, IPv6 and header-compressed IP (HCfB, header types 20h, 21h,
  60h, 61h) to the UDP flow and payload;
- `mmtp`: MMTP headers, control-message payloads, and the MMT-SI messages
  and tables they carry (PA message with MPT / PLT, M2 section and short
  section messages with MH-EIT, MH-SDT, MH-TOT, MH-AIT, ECM, EMM, …);
- `si`: TLV-SI sections (TLV-NIT, AMT) found by header and CRC-32 in
  transmission-control packets;
- `stats`: per-stream counters over all of the above.

ISO/IEC 23008-1 (MMT) is not available: only what B32 Part 3 and B60
define is parsed. Scrambled payloads are recognised, never descrambled.
Interpretations are in [SPEC-NOTES.md](SPEC-NOTES.md).
