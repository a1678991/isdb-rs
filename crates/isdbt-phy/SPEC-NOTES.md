# isdbt-phy — spec interpretation notes

Spec: ARIB STD-B31 v2.2 (E1), Part 2 (the Attachment, "Operational
Guidelines"), Chapter 5 "Signal Transmission Scheme to STL/TTL", §5.5
"Additional information necessary for re-multiplexing TS transmission",
Tables 5-5 .. 5-13 (printed pp. 116–128), and Chapter 6 where it points
back to §5.5.  Quotes are from `specs/6-STD-B31v2_2-E1.txt`.  All entries
concern `src/broadcast_ts.rs`.  The pipeline composition of the crate
(`Transmitter`, `Receiver`) has its interpretations in the `isdbt-fec` and
`isdbt-ofdm` notes and the workspace cross-crate entry.

Status words: *confirmed* = confirmed by a second statement of the spec
(asserted in a test); *interpretation* / *convention* / *choice* = no
external confirmation possible from the local text; *pending* = needs a
real studio broadcasting-TS feed (interface point I/F (2) of Fig. 5-1),
which has not been available.

## broadcast_ts: which slot carries the IIP
- Spec: §5.5.3 (1) "Data of the IIP is inserted in the 188 bytes of the
  packet information part in which the layer_indicator in Table 5-7
  becomes '1000,' and only this one packet is multiplexed in one multiplex
  frame."  Table 5-7: '1000' = "TSP which transmits the IIP but not
  transmitted by any of hierarchical layers A, B, or C."
- Interpretation: the IIP occupies a null slot of the §3.2.1 multiplex
  frame (a slot the model receiver assigns to no layer), so it never goes
  on air and costs no layer capacity.  `BroadcastTsWriter` uses the
  **first null slot** of `MultiplexPattern` (asserted to exist for every
  mode / guard interval / modulation in `every_configuration_has_an_iip_slot`).
  Which null slot a studio multiplexer uses is not stated; the reader
  accepts the IIP in any slot (it keys on `layer_indicator` '1000').
- Status: operational choice (TR-B14 Part 7 may fix it; not available).

## broadcast_ts: Table 5-7 layer_indicator '0010' printed twice
- Spec: Table 5-7 lists "'0010': TSP transmitted by hierarchical layer B"
  and, two rows later, "'0010': TSP which transmits AC data but not
  transmitted by any of hierarchical layers A, B, or C".  The codes around
  it are '0011' (layer C) and '0101'–'0111' (provider data).
- Interpretation: the AC row is '0100', the only code left between '0011'
  and '0101' (`LayerIndicator::Ac`).  Layer B keeps '0010'.
- Status: interpretation (typographical error in the English edition;
  `spec_table_5_7_layer_indicator` asserts the adopted table).

## broadcast_ts: the IIP PID
- Spec: Table 5-9, PID: "Should be service provider defined.  However,
  the same PID is used for digital terrestrial television broadcasting."
  No value is printed.
- Interpretation: `IIP_PID` = 0x1FF0, the PID digital terrestrial
  television uses in practice.  **[from knowledge]**: the value is not in
  the local specifications (it would be in TR-B14).  The writer sends it
  by default (`with_iip_pid` changes it); the reader does not use the PID
  to find the IIP, only `layer_indicator` '1000'.
- Status: pending (a studio feed or TR-B14 would confirm it).

## broadcast_ts: which frame the IIP describes
- Spec: §5.5.3 (1) "Information carried by IIP indicates the information
  of the multiplex frame after the multiplex frame to which the packet
  belongs."  Table 5-11 `TMCC_synchronization_word`: "w0 … = '0', w1 … =
  '1'"; Table 5-7 `frame_indicator`: "During even frame (w0) … '0'.
  During odd frame (w1) … '1'."
- Interpretation: the IIP in multiplex frame *k* carries the sync-word
  parity, mode / guard interval and TMCC information of frame *k* + 1.
  `BroadcastTsWriter` sets `tmcc_sync_odd` to the parity of the next
  frame (the complement of the frame's own `frame_indicator`) and, as it
  only transmits steady parameters, the same mode / GI / TMCC as the
  current frame.  `BroadcastTsReader` attaches the IIP carried by frame
  *k* to frame *k* + 1 (`BroadcastFrame::iip`); for the first frame read,
  `BroadcastFrame::params` falls back to the IIP that frame carries
  (steady state assumed).  A parameter switch (countdown) through this
  writer therefore needs the caller to drive `set_tmcc` one frame ahead
  of the switch on both the IIP and the dummy-byte `count_down_index`,
  which the writer does not separate; the TMCC/`Transmitter` side has the
  same limitation (README "Known limitations").
- Status: interpretation, consistent with the text; asserted between the
  writer and the reader (`reader_recovers_frames`); pending a studio feed.

## broadcast_ts: IIP_packet_pointer counting
- Spec: Table 5-9 "Indicates the number of packet from the multiplex
  position to the next multiplex frame head in the multiplex frame of the
  ISDB-T_information_packet.  The value of the last TPS [sic] of the
  multiplex frame should be 0 and should be counted from the multiplex
  position of the ISDB-T_information_packet to the next multiplex frame
  head in TSP unit."
- Interpretation: pointer = number of TSPs that follow the IIP in its
  frame = n − 1 − slot (n = Table 3-5 TSPs per frame).  An IIP in the last
  slot gives 0, as the second sentence requires.
- Status: confirmed by the "last TSP = 0" statement
  (`spec_5_5_writer_frame_structure`).

## broadcast_ts: the optional (204,196) parity
- Spec: §5.5.2 (1) "It should be possible to add the following parity when
  required.  The parity should apply the shortened (204,196) Reed-Solomon
  code generated by adding 00HEX of 51 bytes in front of the input data
  bytes in the case of (255,247) Reed-Solomon code and deleting the top 51
  bytes.  … p(x) = x8 + x4 + x3 + x2 + 1 … g(x) = (x−λ0)(x−λ1)…(x−λ7)
  where, λ=02HEX".  Fig. 5-10: 188 bytes information, 8 bytes multiplex
  position, 8 bytes "Parity (option)".
- Interpretation: the same field as the §3.3 RS(204,188) code
  (`isdbt_fec::rs`), generator roots λ⁰..λ⁷, systematic: parity = d(x)·x⁸
  mod g(x) over the 196 bytes (information part + ISDB-T_information).
  The writer always adds it (adding is allowed; "when required" is the
  receiver's side).  The reader reports per-frame `parity_errors` and
  rejects nothing: a sender that adds no parity leaves arbitrary bytes
  there, and the dummy-byte information is still usable.
- Status: confirmed (the eight syndromes of every written TSP are zero,
  checked by an independent Horner evaluation; single-byte corruption
  anywhere in the 204 bytes is detected; `spec_5_5_2_rs_204_196_parity`).
  No external test vector exists in the local text.

## broadcast_ts: stuffing of the AC bytes
- Spec: Table 5-6 "if(AC_data_invalid_flag==1) stuffing_bit 32 bslbf";
  Table 5-7 bytes 4–7 "AC_data … (Is not transmitted in the case of
  AC_data_invalid_flag ='1')"; the IIP uses "stuffing_byte(0xFF)".
- Interpretation: the value of the 32 stuffing bits is not fixed; the
  writer sends 0xFF (like the IIP stuffing bytes), the reader keeps the
  bytes as received (lossless).  `AC_data_effective_bytes` is '11' when
  no AC data is carried, as Table 5-7 says ("including the case in which
  AC data is not added").
- Status: convention.

## broadcast_ts: "no network synchronisation information" — length 0 or id 0xFF
- Spec: Table 5-9 `network_synchronization_information_length` "should be
  159 or less"; Table 5-13 `synchronization_id` "0xFF:
  SFN_synchronization_information is not added", followed by N stuffing
  bytes (N unspecified).
- Interpretation: both a zero length (`NetworkSync::Absent`) and the
  id 0xFF form (`NetworkSync::Stuffing(n)`, length n + 1) are accepted.
  The writer sends the one-byte 0xFF form, the only "not added" encoding
  the tables spell out.
- Status: interpretation.

## broadcast_ts: reserved bits of Table 5-10
- Spec: Table 5-10 has a 2-bit `reserved` after `AC_data_effective_position`
  that Table 5-11 does not describe; `reserved_future_use` (10 bits) "All
  of them should be '1'"; `TMCC_reserved_future_use` (12 bits) "Same as
  the reserved bit (B110 to B121)" (all 1 in §3.15).
- Interpretation: the writer sets the undescribed 2 bits to '11' too; the
  reader keeps whatever it receives.
- Status: convention.

## broadcast_ts: CRC_32 ranges
- Spec: Table 5-11 CRC_32 "Ranges are to all the
  modulation_control_configuration_information from
  TMCC_synchronization_word to reserved_future_use"; Table 5-13 CRC_32
  "Ranges to all the SFN_synchronization_information from the
  (synchronization_time_stamp) which is the head of
  SFN_synchronization_information_header to time_offset" (the polynomial
  of ISO/IEC 13818-1 in both cases).
- Interpretation: the 128 content bits (16 bytes) of Table 5-10, and the
  bytes from `synchronization_time_stamp` through the last `time_offset`
  of Table 5-12, which include `equipment_loop_length` in between; the
  `synchronization_id` byte is outside.  `isdb_ts::crc::crc32` (H.222.0
  Annex A) computes both.
- Status: confirmed as far as the text allows (a decoder over range +
  CRC yields 0 in the tests); pending a studio feed for the SFN range.

## broadcast_ts: not implemented
- Sub-IIP packets (Fig. 5-11, `IIP_branch_number` / `last_IIP_branch_number`
  > 0): the writer always fits its network synchronisation information
  in one packet (≤ 159 bytes, enforced), the reader parses each IIP on
  its own and does not reassemble `net_sync_info_A / B` across frames.
- AC data in the dummy bytes or in invalid-hierarchy TSPs (Chapter 6):
  the writer sets `AC_data_invalid_flag` '1' and
  `AC_data_effective_position` '1'; the reader decodes the flags and keeps
  the bytes but nothing maps them onto the AC carriers.
- Chapter 4 SFN synchronisation (the delay control itself, STL/TTL
  timing): only the Table 5-12 syntax is encoded / decoded.
- Service-provider TSPs (`layer_indicator` '0101'–'0111', '1001'–'1111')
  are decoded as `LayerIndicator::Provider` and dropped by the divider;
  the writer never emits them.
