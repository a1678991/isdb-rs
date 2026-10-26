# isdb-tlv — spec interpretation notes

Specs: ARIB STD-B32 v3.11-E1 Part 3 (§3.5 TLV packet, §3.7 compressed IP
packet, §3.9 MMTP packet, §3.10 transmission control signal); ARIB
STD-B60 v1.14-E1 (MMT-SI, TLV-SI); ARIB STD-B44 v2.1-E1 §3.2 (1) Fig.
3.2-1 Appendix 1 (slot header). Not available: ISO/IEC 23008-1, ARIB
STD-B63, and the MIC notification tables B32 refers to for the layouts
around sections (SPECS.md §1.2). "Lab recordings": BS7, BS11, BS17 of
2026-10-25 (VERIFICATION.md "ISDB-S3 receiver").

## head-tlv: the slot header's head-TLV indication
- Spec: B44 Fig. 3.2-1 Appendix 1: "Head TLV indication" (16 bits) then
  160 bits undefined (all '1'): "the head byte position of first TLV in
  the slot is indicated as the number of byte from slot head except slot
  header … if the head byte does not exist, the value is set to 0xFFFF".
- Interpretation: the 176-bit header begins with the 16-bit indication,
  MSB first; offsets count bytes of the slot's data (the D bits of Table
  3.2-1). A stream's bytes are the data of its slots in slot order, frame
  after frame.
- Confirmation: on the lab recordings, over 8 TLV streams and 12 633
  packets, every indication equals the offset at which the packet chain
  of the previous slots lands (0 mismatches, 0 bytes skipped); the 160
  undefined bits read all ones in all 4 512 slot headers (`isdbs3-phy`
  example `headercheck`). Status: confirmed (live data).

## hcfb: header-compressed IP layout
- Spec: B32 §3.7 (1) No. 1, No. 2 and notes: context id 12, sequence
  number 4, header type 8; 20h/60h carry a "partial" IPv4/IPv6 header and
  a partial UDP header (lengths and checksums removed).
- Interpretation: 20h = the IPv4 header's 20 bytes without total length
  and checksum (16 bytes, options excluded) followed by the UDP ports (4
  bytes); 60h = the IPv6 header's 40 bytes without payload length (38)
  followed by the ports; 21h = the 2-byte IPv4 identification only; 61h =
  no header. The payload follows.
- Confirmation: on the lab recordings every compressed packet is context
  1 and every one decodes to a UDP payload that parses as MMTP (version
  00, known payload types) — 0 IP errors, 0 non-MMTP payloads in 12 633
  packets. Only header types used on air are exercised. Status: confirmed
  (live data) for the types seen.

## pa-message: PA message table list
- Spec: B60 Table 7-1: message_id 16, version 8, length 32, extension
  {number_of_tables N, (table_id, table_version, table_length) × N},
  message_payload {table() × N}.
- Observation: on air the PA messages have N = 0 and still carry tables
  in the payload (e.g. `80 04 00 08 …`: PLT, version 4, 8 bytes;
  likewise MPT 20h).
- Interpretation: the payload is walked as a sequence of tables, each
  table_id 8, version 8, length 16, body, whatever N says; the extension
  entries, when present, are added to the list.
- Confirmation: walking the payload this way finds MPT and PLT in every
  PA message of the lab recordings with the lengths consistent to the
  byte. Status: observation (the reason N is 0 is not in B60).

## control-message: control-message payload header
- Spec: B32 §3.9 Figure No. 2: division index 2, reserved 4, length
  extension flag 1, aggregation flag 1, division number counter 8.
- Observation: the first byte is 3Ch on air (reserved bits ones).
  Status: confirmed (live data).

## control-packet-layout: sections in transmission-control TLV packets
- Spec: B32 §3.5: packet type FEh "transmission control signal packet";
  B60 §5: TLV-SI (TLV-NIT 40h/41h, AMT FEh) "transmitted by the section
  format". What precedes the section inside the packet is not in the local
  documents.
- Interpretation: `si::sections` finds sections at any offset by table
  id, section syntax indicator and CRC-32.
- Confirmation: in 40 s live recordings of BS7, BS11 and BS17 (8 TLV
  streams), every control packet holds one CRC-valid section starting at
  offset 0 of its data (TLV-NIT 40h and AMT FEh, four of each per stream
  in 40 s). Status: confirmed (live data): nothing precedes the section.
