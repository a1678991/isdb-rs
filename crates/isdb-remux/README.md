# isdb-remux — playable files from a clear ISDB-S3 TLV stream

A clear TLV stream of ISDB-S3 / advanced BS in (a file or stdin), out

- a timed **MPEG-TS** of a service (`--ts`, to `-o` or stdout): the HEVC
  video, every AAC audio, the data broadcasting assets as private data,
  with the PTS/DTS of the MPTs' timestamp descriptors, and the captions
  as SubRip / WebVTT next to it, or
- a **Matroska** file of every track of the service (`--mkv -o OUT.mkv`):
  the video, every audio as proper AAC, the captions as text tracks, a
  chapter per programme of the MH-EIT, the data broadcasting packages as
  attachments.

The input's payloads are in the clear: a stream never scrambled, or the
output of a cardholder's own descrambler (which keeps the scramble
extension with its control bits cleared — `isdb-tlv` reads that as
clear). A payload still scrambled is skipped and counted; nothing is
descrambled here.

```sh
cargo build --release -p isdb-remux
isdb-remux -i dec.tlv --mkv -o out.mkv                 # every track, one file
isdb-remux -i dec.tlv --mkv --split-events             # one file per programme, next to the input
isdb-remux -i dec.tlv --service 0x0097 --ts -o out.ts  # + out.ja.srt / .vtt
DESCRAMBLER < rec.tlv | isdb-remux --ts > out.ts      # straight from a descrambler
```

The TS goes to stdout when no `-o` is given, but never onto an
interactive terminal: as curl does, `isdb-remux` refuses, naming the ways
out (`-o FILE`, a redirect or pipe); `-o -` writes it to stdout anyway.
`--mkv` needs a real file (`-o -` is refused: it seeks back into it).
Progress and the report always go to stderr.

Options: `--service` (default: the first service whose MPT arrives),
`--captions DIR` (where `--ts`'s `.srt` / `.vtt` go; needed for a TS to
stdout), `-v` logs verbosely, `-q` reports only errors.

## End to end: antenna → playable file

```sh
cargo build --release -p isdb-cli -p isdb-remux

# 1. capture BS7's TLV streams (one file per TLV stream id; BS朝日 4K,
#    service 0x0097, rides 0xb070)
target/release/isdb rx --radio pluto --channel BS7 --system isdbs3 \
    --seconds 60 --out 'bs7-{tlv_id}.tlv'

# 2. what the stream carries: services, the MPT's assets
target/release/isdb tlv inspect bs7-b070.tlv

# 3. descramble with your own card and descrambler (outside isdb-rs)
#    into bs7-b070-dec.tlv

# 4. a timed MPEG-TS of the service (or --mkv -o bs7-0097.mkv)
target/release/isdb-remux -i bs7-b070-dec.tlv --service 0x0097 --ts -o bs7-0097.ts

# 3+4 in one go, through a pipe:
DESCRAMBLER < bs7-b070.tlv | target/release/isdb-remux --service 0x0097 --ts -o bs7-0097.ts

# 5. remux or transcode
ffmpeg -i bs7-0097.ts -c copy bs7-0097.mkv                     # lossless
ffmpeg -i bs7-0097.ts -c:v copy -tag:v hvc1 -c:a aac -b:a 256k \
    -movflags +faststart bs7-0097.mp4                          # MP4: LATM audio re-encoded
ffmpeg -i bs7-0097.ts -vf scale=1920:1080 -c:v libx265 -crf 22 \
    -c:a aac -b:a 192k bs7-0097-1080p.mkv                      # 4K → 1080p
mpv bs7-0097.mkv
```

What the TS carries — one program, a stream per asset of the service's
MPT, at pid `0x0100 + component tag` (`0x100` the video, `0x110`/`0x111`
the audio, `0x140…` the data):

| Asset | In the TS | ffmpeg / mpv |
|---|---|---|
| `hev1` (first video) | HEVC, PTS+DTS, the PCR | plays |
| every `mp4a` | LATM (type 11h), PTS | every track; `--aid=2` etc. |
| `aapp` and other untimed assets | private data (type 06h): one `private_stream_2` PES per item, prefixed by the MPU sequence number (4 bytes), the item id (4) and a flags byte (`01h` on the continuation of an item split over PESs) | `bin_data`; mpv ignores it |
| `stpp` captions | SRT / WebVTT sidecars (below) | sidecars |

`ffmpeg -i OUT.ts -c copy out.mkv` takes the video and the first audio.
For every audio: `-map 0 -dn` (Matroska has no codec for the data
streams, so a plain `-map 0` fails there); into another TS, `-map 0 -c
copy` keeps everything. `tools/remux-ts-data.py OUT.ts --out DIR` writes the
data items back as files (`DIR/PID/MPU/ITEM`), `--concat DIR` as the
`.mfu` of the asset's MFUs back to back — byte-identical on the BS17
recording.

## `--mkv`: every track in one file

`--mkv -o OUT.mkv` writes the same streams as `--ts`, as Matroska and
without ffmpeg:

| Asset | Track |
|---|---|
| `hev1` (first video) | `V_MPEGH/ISO/HEVC`, its `hvcC` from the stream's VPS/SPS/PPS, the IRAP pictures keyframes (Cues) |
| every `mp4a` | `A_AAC`: the raw AAC frame of each LATM AudioMuxElement, the `AudioSpecificConfig` its `CodecPrivate` — not re-encoded (ffmpeg's `-c copy` of the TS stores LATM as `A_MS/ACM`, which few players take) |
| `stpp` captions | `S_TEXT/UTF8`, a block per cue, the same times as the TS form's sidecars |
| `aapp` and other untimed assets | attachments, one per asset and package version: `PID-MPU.mmtitems`, each item `[item id u32][length u32][bytes]` in arrival order (the carousel's repeats kept) |

Chapters: one per event of the service's MH-EIT in the recording, named by
its MH-short event descriptor (the broadcast's name as sent, symbols such as
🆞 included) in the descriptor's language, and clipped to the recording: an event
already running starts at 0, and the last ends with the file. A chapter starts
where the stream really switches, not at the schedule, which the picture follows
by about 2.7 s on the BS17 recordings: at a new coded video sequence in the 10 s
after the scheduled time, else at the first MPU sent wholly under a new MPT version.
Either way the mark is that MPU's key picture, so a seek lands in the chapter, up
to 117 ms after the cut. With neither seen, the mark is the scheduled time, and the
report counts it. No MH-EIT seen means no chapters; a time no event covers has none.
The evidence and how tables that disagree are merged:
[SPEC-NOTES.md](SPEC-NOTES.md#switch-point-where-a-programme-really-switches-in-the-stream).
The TS form has no chapters.

**A stream's structure can change mid-recording, and one file cannot follow it.**
On one BS17 recording, at a programme change 5.1 audio on `0xf310` becomes stereo,
the stereo asset `0xf311` ends, and the video goes from HLG to BT.2020 with a new
SPS. A track keeps its first configuration, so ffmpeg and mpv cannot decode the stereo
frames in the 5.1 track, and that file's last 12 minutes have no audio they can play
(the run warns). `--split-events` is the way out when the change comes with a
programme, as it did here; a change inside a programme still warns.

## `--split-events`: one file per programme

`--mkv --split-events` writes one Matroska file per MH-EIT event instead of one
with chapters, from the same switch points (above):

```sh
isdb-remux -i /path/to/dec.tlv --mkv --split-events               # files next to the input
isdb-remux -i dec.tlv --mkv --split-events -o DIR                 # in DIR (made if missing)
DESCRAMBLER < rec.tlv | isdb-remux --mkv --split-events -o DIR    # stdin needs -o DIR
isdb-remux -i dec.tlv --mkv --split-events \
    --split-name '${EVENT_NAME}_${START_TIME}.mkv'                 # another name
```

The name is `--split-name`'s template, by default
`${START_TIME}-${EVENT_NAME}-${CHANNEL_NAME}.mkv`, for example
`2026-11-05_02-24-02.698-番組名-ＮＨＫ　ＢＳＰ４Ｋ.mkv`:

| Field | Value |
|---|---|
| `${START_TIME}` | the presentation time of the file's first picture, from the MPT's clock, in JST to the millisecond |
| `${EVENT_NAME}` | the MH-short event descriptor's name; `unknown-event` for a stretch no event covers or an event with no name. Shortened to fit a file name |
| `${CHANNEL_NAME}` | the MH-SDT service name (else its provider name, else `service-0065`) |

Characters a file system could refuse or read as a path become `_`. An existing
file is never overwritten: the name takes a `-2`, `-3` ….

Each file stands alone:

- **Video** starts on the switch's key picture, with the parameter sets in force
  there in its `hvcC`. The RASL pictures after a CRA are left out, since they need
  pictures from the previous file: 7 per cut on the BS17 recordings (117 ms, the first
  moments of the new programme), none at an IDR.
- **Audio** cuts at the file's first picture, except that a stream's configuration
  change within 10 s of it is that stream's own cut, so a track holds one
  configuration. Tracks are named from their channels, and a stream with no frames
  in the span has no track. The old file's audio can run past its last picture.
- **Captions** shown across a cut are clipped to each file, and **data broadcasting**
  items go to the file they arrived in, so a package can appear in several.
- A stretch no event covers gets a file too, so no media is left out. The event's
  name is the Segment title; the files have no chapters.

The files appear at the end of the run, from one scratch in the directory
(`isdb-remux-PID.part`, about the media's size, removed at the end), so the input must
end, and live or very long recordings need that room. If a run fails, the files
finished by then stay and the one being written is removed.

Each track carries the MPT's language (the video / audio component
descriptors) and the audio its layout (`5.1ch`, `stereo`); the video and the
first audio are the defaults. The file is written in two passes: the media
wait in `OUT.mkv.part` next to it (about the file's size) until the end,
then the file is written in decode order and the scratch removed. A run
that fails removes both; `-o` is required (no stdout).

On the BS17 recordings: the 14 min capture gives 3 tracks (HEVC, AAC 5.1ch
and stereo) and 15 attachments in 6 s; the audio decodes with no error,
the video with exactly the errors of the TS form (the capture's lost
packets: its first 100 s lose 575 video packets in 198 gaps and 19
audio frames; resuming at the next MPU after each wrote 4450 pictures,
ffmpeg logging 177 RPS errors; stepping over the gaps writes 5896 with
47, each after one of the 12 pictures lost whole); the lossless recordings decode with none, mpv plays, seeks and
switches audio, and its caption times equal the sidecars'. The 1449 data
transmissions of the BS17 reference recording equal the TS form's data
streams item for item.

Captions: with `--ts -o OUT.ts`, each caption asset is also written as
`OUT.LANG.srt` and `OUT.LANG.vtt` (`--captions DIR` puts them elsewhere,
and is how a `--ts` run to stdout gets them). They are timed on the TS's
own timeline, so mpv picks them up next to the `.ts` and they mux in:

```sh
ffmpeg -i bs7-0097.ts -i bs7-0097.ja.srt -map 0 -map 1 -c copy \
    -disposition:s:0 default -metadata:s:s:0 language=jpn bs7-0097.mkv
```

The TTML of the captions is not put in the TS itself: neither ffmpeg nor
mpv decodes TTML (`crates/isdb-caption` does the conversion). The `.vtt`
keeps the broadcast's layout and styling as far as WebVTT can (each line
at its region, colours, translucent background, outline, font and size):
browsers and AVFoundation render it so; mpv and ffmpeg read it as plain
text, the same as the `.srt`.

MP4 has no mapping for LATM-framed AAC, so `-c copy` into `.mp4` fails
("Could not find tag for codec aac_latm"); the audio needs re-encoding
there, while MKV and TS take it as is. Raw `.hevc` / `.latm` elementary
streams (`isdb tlv extract`) have no timestamps (see below).

## Timestamps: `.hevc` vs `--ts`

The raw `.hevc` / `.latm` files carry no timestamps. With B-pictures in the
stream, `ffmpeg -i x.hevc -i x.latm -c copy out.ts` (or `.mkv`) fails with
"first pts and dts value must be set" — any raw Annex B stream with
B-frames does, a libx265 one included; and a guessed rate would drift from
the audio anyway. `isdb-remux` writes the stream's own clock instead (B60
§7.4.3.5/§7.4.3.35; `src/timing.rs`): for the *i*-th access unit of an
MPU in decode order,

    DTS(i) = PT − decoding_time_offset + Σ_{k<i} pts_offset(k)
    PTS(i) = DTS(i) + dts_pts_offset(i)

with `PT` the MPU timestamp descriptor's time of the MPU's first picture in
presentation order. On the BS7 recording the resulting display order equals
the HEVC slices' picture order count picture for picture. Points that the
recordings forced:

- the MPT keeps its version for a whole recording while its timestamp
  descriptors move on: they are read from every copy;
- the MFU header's `sample_number` is 0 throughout, so the pictures split
  at the HEVC access unit boundaries (AUD), and a stream starts at the
  next MPU boundary;
- lost packets (MMTP sequence gaps) cost what they took, not the rest of
  the MPU (half a second of video, four audio frames). Each video MFU is
  one NAL unit, a picture is AUD+PPS+SEI then one slice in many packets,
  so a gap of a few packets almost always lands inside one slice: that
  slice is written cut at the gap (decodable up to there), and the next
  picture is placed in its MPU by its slice's picture order count
  against the last one's, so its descriptor timestamps stay exact (the
  POC of every later picture is checked against its decode index; the
  report counts disagreements, 0 on the recordings). A picture lost
  whole leaves its slot empty; the RASL pictures (NAL 8/9) of the next
  CRA are then not written — they reference pictures before the CRA the
  file lacks (H.265's NoRaslOutputFlag, as at a BLA) — nor of the CRA a
  stream starts at; a lost CRA drops its MPU up to the next one. Audio
  packets each carry one whole frame: the stream steps over the lost
  frames, so the next keeps its own timestamp;
- an audio MFU is one LATM `AudioMuxElement`: it goes out as LOAS
  (stream type 11h), not ADTS.
