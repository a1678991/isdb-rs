# isdb-mkv

A Matroska writer (RFC 9559 over EBML, RFC 8794) for what an ISDB-S3
decoder delivers, with no dependencies:

| Track | Codec ID | CodecPrivate | Block |
|---|---|---|---|
| HEVC | `V_MPEGH/ISO/HEVC` | `HEVCDecoderConfigurationRecord` (`hevc::hvcc`) | 4-byte length-prefixed NAL units (`hevc::annex_b_to_length`) |
| AAC | `A_AAC` | `AudioSpecificConfig` | one raw AAC frame (`isdb_mmt::es::latm_frame` takes it out of a LATM AudioMuxElement) |
| text | `S_TEXT/UTF8` | — | a cue's text, in a BlockGroup with its BlockDuration |

and attachments (`Writer::attach`).

The writer is two-pass: `Writer::frame` spills each frame to a scratch
(`Read + Write + Seek`) and indexes it; `Writer::finish` writes the header
with every track added by then, the frames in decode order across the
tracks (a video's DTS orders, its PTS stamps the block), a cluster at each
video keyframe (at the latest every 5 s), Cues for the keyframes, the
attachments, and seeks back to fill in the Segment size, the SeekHead and
the Duration. Tracks — a codec configuration learnt from the stream, text
cues known only at the end — can be added up to `finish`; `rebase` and
`renumber` convert frames given in another time unit or track numbering.

`hevc::parse_sps` reads the SPS fields a track and an `hvcC` need (profile
/ tier / level, chroma format, bit depths, the cropped picture size).
Checked against BS17's stream: its `hvcC` header equals ffmpeg's byte for
byte.
