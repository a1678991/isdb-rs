# isdb-ts: spec interpretation notes

Interpretations of ITU-T H.222.0 (05/2006) | ISO/IEC 13818-1:2007 made by
this crate, in the format of the workspace `SPEC-NOTES.md`. Clause, table
and equation numbers are those of the 05/2006 edition
(`specs/ITU-T-H.222.0-2006.txt`).

## isdb-ts: table numbering of the 2006 edition
- Spec: H.222.0 (05/2006) §2.4.3.6, §2.6.
- Text: "Table 2-21 – PES packet", "Table 2-22 – Stream_id assignments",
  "Table 2-45 – Program and program element descriptors".
- Interpretation: older editions number the PES syntax Table 2-17 and the
  descriptor tag table 2-39. The code cites the 2006 numbers: PES 2-21,
  stream_id 2-22, trick mode 2-24, stream_id_extension 2-27, PSI pointer
  2-29, PAT 2-30, table_id 2-31, CAT 2-32, PMT 2-33, stream_type 2-34,
  descriptor tags 2-45.
- Confirmation: the 2006 text itself (status: confirmed).

## isdb-ts: PES_extension_field_length covers the stream_id_extension byte
- Spec: §2.4.3.6 Table 2-21, §2.4.3.7.
- Text (syntax): "marker_bit 1 / PES_extension_field_length 7 /
  stream_id_extension_flag 1 / If ( stream_id_extension_flag == '0') {
  stream_id_extension 7 / for (i = 0; i < PES_extension_field_length; i++){
  reserved 8 } }". Text (semantics): "PES_extension_field_length – This is a
  7-bit field which specifies the length, in bytes, of the data following
  this field in the PES extension field up to and including any reserved
  bytes."
- Interpretation: read literally, the syntax loop would put
  `PES_extension_field_length` reserved bytes *after* the
  stream_id_extension byte, i.e. one byte more than the semantics say. The
  semantics ("data following this field … up to and including any reserved
  bytes") are taken as normative: exactly `PES_extension_field_length` bytes
  follow the length byte, the first of which carries
  stream_id_extension_flag/stream_id_extension. The bytes are kept raw
  (`PesExtension2::data`), so decoding is lossless under either reading as
  long as the outer PES_header_data_length is consistent;
  `stream_id_extension()` reads the first byte when its MSB is '0'. The
  syntax also leaves the flag = '1' case ("reserved") without fields.
- Confirmation: no PES in the live terrestrial capture uses
  PES_extension_flag_2 (status: pending real-world check).

## isdb-ts: adaptation_field_extension_length = 0
- Spec: §2.4.3.4 Table 2-6, §2.4.3.5.
- Text: "adaptation_field_extension_length – … It indicates the number of
  bytes of the extended adaptation field data immediately following this
  field, including reserved bytes if present." The syntax always follows the
  length with "ltw_flag 1 piecewise_rate_flag 1 seamless_splice_flag 1
  Reserved 5".
- Interpretation: a length of 0 cannot hold the mandatory flags byte; the
  decoder returns `Error::Invalid { what: "adaptation_field_extension_length" }`
  rather than invent flags. Any length ≥ 1 is decoded; bytes after the
  flagged fields are the trailing `reserved` bytes, kept verbatim.
- Confirmation: structural (status: confirmed by the syntax).

## isdb-ts: bytes not described by the packet syntax
- Spec: §2.4.3.2 Table 2-2, Table 2-5, §2.4.3.5.
- Text: Table 2-2 has data_bytes only "if(adaptation_field_control = = '01'
  || adaptation_field_control = = '11')"; Table 2-5 "00 Reserved for future
  use by ISO/IEC"; §2.4.3.5 "When the adaptation_field_control value is '10',
  the value of the adaptation_field_length shall be 183."
- Interpretation: to stay lossless, `Packet::payload` holds whatever bytes
  follow the header and adaptation field even when the syntax says there is
  no payload (AFC '00': all 184 bytes; AFC '10' with length < 183: the
  remainder). `Packet::data_bytes()` returns `None` for these, and
  `packet::check` reports them (Table 2-5 reserved value, §2.4.3.5 length,
  Table 2-2 undescribed bytes). Demultiplexers ignore them ("decoders shall
  discard Transport Stream packets with the adaptation_field_control field
  set to a value of '00'").
- Confirmation: structural (status: confirmed).

## isdb-ts: continuity_counter of adaptation-only packets
- Spec: §2.4.3.3.
- Text: "The continuity_counter shall not be incremented when the
  adaptation_field_control of the packet equals '00' or '10'." and "The
  continuity_counter in a particular Transport Stream packet is continuous
  when it differs by a positive value of one from the continuity_counter
  value in the previous Transport Stream packet of the same PID, or when
  either of the non-incrementing conditions (adaptation_field_control set to
  '00' or '10', or duplicate packets as described above) are met."
- Interpretation: an AFC '00'/'10' packet is always "continuous" and does
  not move the expectation; the next payload packet must be the last
  *payload* packet's counter + 1. "Shall not be incremented" is read as the
  AF-only packet carrying the same value as the previous packet; a different
  value is reported as `NoPayloadChanged` (a conformance finding, not a
  loss). If such a packet has discontinuity_indicator '1', its value becomes
  the new base (`Discontinuity`). The transmit-side `Counter::next(false)`
  returns the previous payload packet's value.
- Confirmation: Live terrestrial capture: 232 AF-only PCR packets (PIDs
  0x01FF, 0x03FF), zero continuity events apart from transport-error packets
  (status: confirmed on real data).

## isdb-ts: duplicate packets
- Spec: §2.4.3.3.
- Text: "duplicate packets may be sent as two, and only two, consecutive
  Transport Stream packets of the same PID. … In duplicate packets each byte
  of the original packet shall be duplicated, with the exception that in the
  program clock reference fields, if present, a valid value shall be
  encoded."
- Interpretation: a payload packet with the same counter as the previous
  payload packet is a duplicate; its payload is never used by the
  reassemblers. A third copy is `ExcessDuplicate` (also not used). The
  checker compares the duplicate with the original byte-for-byte except the
  6 PCR bytes (offsets 6..12 when PCR_flag is set) and reports
  `Duplicate { identical }`. A duplicate with different content is still
  treated as a repeat (not as a counter break).
- Confirmation: unit tests only; the live terrestrial capture has no
  duplicates (status: pending real-world check).

## isdb-ts: discontinuity_indicator in the reassemblers
- Spec: §2.4.3.5.
- Text: "When the discontinuity state is true in any Transport Stream packet
  of a PID not designated as a PCR_PID, the continuity_counter in that packet
  may be discontinuous with respect to the previous Transport Stream packet
  of the same PID." and "A Transport Stream shall not be constructed in such
  a way that discarding such a packet will cause the loss of PES packet
  payload data or PSI data."
- Interpretation: a counter jump signalled by discontinuity_indicator is
  not a loss, so the section/PES reassemblers keep the partial unit (the
  CRC_32 of the completed section still guards integrity); only unsignalled
  jumps (`Break`) and transport_error_indicator discard it.
- Confirmation: no discontinuity_indicator in the live terrestrial capture
  (status: pending real-world check).

## isdb-ts: pointer_field, partial sections and stuffing
- Spec: §2.4.4, §2.4.4.2, Table 2-31.
- Text: "pointer_field – … the number of bytes, immediately following the
  pointer_field until the first byte of the first section that is present in
  the payload"; "packet stuffing bytes of value 0xFF may be found in the
  payload … only after the last byte of a section. In this case all bytes
  until the end of the Transport Stream packet shall also be stuffing bytes
  of value 0xFF"; table_id "0xFF Forbidden".
- Interpretation:
  * The `pointer_field` bytes before the first new section must complete the
    section in progress exactly. If they do not reach its declared end the
    partial section is discarded (`PointerMismatch`); if the section ends
    earlier, it is emitted and the remaining bytes, unless all 0xFF, are
    reported as `UnexpectedBytes`.
  * A 0xFF where a table_id would start means stuffing to the end of the
    packet (non-0xFF bytes after it → `BadStuffing`).
  * A section may begin with fewer than 3 bytes at the end of a packet
    (the spec does not forbid splitting the section header); the assembler
    waits for section_length in the next packet.
  * Before the first PUSI packet nothing is assembled; the bytes before the
    pointed-to start of the first PUSI packet are dropped silently.
  * Packetiser (`Packing::Packed`): a new section can only start in a
    packet if at least one of its bytes fits after the pointer_field and the
    tail of the previous section; when the tail is exactly 183 bytes, the
    packet carries the tail plus one 0xFF stuffing byte and the next
    section starts in the next packet with pointer_field 0.
- Confirmation: Live terrestrial capture: 825 sections on 16 (PID, table_id)
  pairs, no assembler events, all 823 long sections CRC-valid (status:
  confirmed).

## isdb-ts: PMT CRC_32 "Annex B"
- Spec: §2.4.4.9.
- Text: "CRC_32 – … gives a zero output of the registers in the decoder
  defined in Annex B after processing the entire Transport Stream program
  map section." (PAT, CAT, private section and TSDT say "Annex A"; Annex B
  of this edition is DSM-CC.)
- Interpretation: editorial error; the Annex A CRC is used for PMT too.
- Confirmation: all 451 PMT sections of the live terrestrial capture have a
  zero Annex A residue (status: confirmed).

## isdb-ts: PES_packet_length 0 vs. an empty bounded PES packet
- Spec: §2.4.3.7.
- Text: "PES_packet_length – … A value of 0 indicates that the PES packet
  length is neither specified nor bounded and is allowed only in PES packets
  whose payload consists of bytes from a video elementary stream contained in
  Transport Stream packets."
- Interpretation: a bounded PES with zero bytes after the length field is
  not representable (it would read back as unbounded), so
  `PesPacket::encode` rejects it. `pes::check` flags length 0 on a
  non-video stream_id (Table 2-22 `1110 xxxx` only).
- Confirmation: structural (status: confirmed).

## isdb-ts: PES header stuffing and trick-mode byte kept raw
- Spec: §2.4.3.6 Table 2-21, §2.4.3.7, Table 2-24.
- Text: "stuffing_byte – This is a fixed 8-bit value equal to '1111 1111' …
  No more than 32 stuffing bytes shall be present in one PES packet header."
  Trick mode: freeze_frame is "field_id 2 uimsbf / Reserved 3", the others
  are field-specific 5-bit layouts.
- Interpretation: PES header stuffing bytes (the bytes counted by
  PES_header_data_length after the flagged fields) are stored verbatim, so
  values ≠ 0xFF and counts > 32 survive a round trip and are reported by
  `pes::check`. The DSM trick-mode field is stored as its raw byte and
  interpreted by `TrickMode::from_byte`. `pack_header()` (Table 2-39 of the
  Program Stream) is stored raw, delimited by pack_field_length.
- Confirmation: 820 PES packets of the live terrestrial capture (video 0x0100,
  audio 0x0110, captions/data PIDs) decode and re-encode byte-identically
  (status: confirmed).

## isdb-ts: reserved bits
- Spec: §2.1.64.
- Text: "Unless otherwise specified within this Recommendation | International
  Standard, all reserved bits shall be set to '1'."
- Interpretation: codecs keep reserved bits as decoded (fields named
  `reserved*`); constructors set them to '1'; the `check` functions report
  any '0'. The CAT's "reserved 18" bits occupy `LongHeader.table_id_extension`
  (16 bits) and `LongHeader.reserved2` (2 bits).
- Confirmation: Live-capture PAT/CAT/PMT: no reserved-bit violations (status:
  confirmed).

## isdb-ts: PCR extension range and allowed PCR PIDs
- Spec: §2.4.2.2 eq. 2-1..2-3, Table 2-3 NOTE, §2.4.4.9.
- Text: "PCR_ext(i) = ((system_clock_frequency × t(i)) DIV 1) % 300"; "The
  transport packets with PID values 0x0000, 0x0001, and 0x0010-0x1FFE are
  allowed to carry a PCR."; PCR_PID "If no PCR is associated with a program
  definition for private streams, then this field shall take the value of
  0x1FFF."
- Interpretation: the 9-bit extension field can hold 300..511; it is kept and
  flagged by `packet::check`. `Pcr::ticks()` is `base × 300 + ext` without
  reduction; `PCR_MODULUS = 2^33 × 300` is the wrap of valid values. PCR on
  PIDs 0x0002–0x000F or 0x1FFF is flagged; PCR_PID 0x1FFF is accepted in a
  PMT (no PCR).
- Confirmation: 232 PCRs in the live terrestrial capture, no violations
  (status: confirmed).

## isdb-ts: 192- and 204-byte framings
- Spec: §2.4.3 ("Transport Stream packets shall be 188 bytes long"),
  §2.4.3.3 sync_byte, Table 2-1.
- Text: Table 2-1 "do { transport_packet() } while (nextbits() = =
  sync_byte)".
- Interpretation: only the 188-byte packet is H.222.0 syntax. The reader
  additionally accepts 4-byte-prefixed (192) and 16-byte-trailer (204, e.g.
  RS parity of ARIB STD-B31) units and passes the extra bytes through
  opaquely. Sync is acquired where 0x47 appears at the unit period for 5
  consecutive units (fewer only when the input ends), trying the current
  format first; a missing sync byte drops sync and the reader searches
  forward byte by byte. The format choice is a heuristic, not a spec rule.
- Confirmation: Live terrestrial capture detected as 188-byte, one
  acquisition, no sync losses, 44 trailing bytes of a truncated last packet
  (status: confirmed).

## isdb-ts: packets with transport_error_indicator
- Spec: §2.4.3.3.
- Text: "When set to '1' it indicates that at least 1 uncorrectable bit
  error exists in the associated Transport Stream packet."
- Interpretation: such a packet's header may itself be corrupt (wrong PID,
  impossible adaptation_field_length). Reassemblers discard the partial
  unit of that PID and reset continuity; the CC checker reports
  `TransportError` without updating state; `Stats` counts TEI from the raw
  header even when the adaptation field is malformed.
- Confirmation: the live terrestrial capture has 5 TEI packets (3 of them
  structurally undecodable), all on otherwise unused PIDs (status: confirmed).
