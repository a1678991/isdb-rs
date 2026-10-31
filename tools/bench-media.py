#!/usr/bin/env -S python3 -I
"""The BS bench service with media (docs/plans/isdbs-next/media-and-margin.md
§2.1), shared by tools/isdbs-bench-tx.sh --media and tools/isdbs-pacing-e2e.sh
MEDIA=DIR. Run with python3 -I.

Usage: python3 -I tools/bench-media.py SRC.toml DST.toml MEDIA_DIR

Writes DST.toml: SRC.toml (crates/isdbt-mux/examples/bs-bench.toml) with its
two declared streams made media streams of MEDIA_DIR's files (absolute
paths): PID 0x0111 `mpeg2-video` of video.m2v, PID 0x0112 `aac-adts` of
audio.m2a with adts_compliance = "warn" (FFmpeg's ADTS has no crc_check and
a variable buffer fullness, which the default "conform" refuses;
docs/mux-config.md "adts_compliance").

Prints one line, `FRAMES NOTE`: the whole number of OFDM frames (mode 3,
GI 1/8: 231.336 ms, the layout `isdb mux` uses by default) nearest to the
media loop, and a note on the lengths. isdbt-mux loops the media at the
longest stream's length (pictures × frame period, ADTS frames × 1024 / fs):
a TS of that many frames, played by `isdb tx --feed loop`, restarts the
media where the multiplexer would have, give or take less than half a frame.
Exit status: 0, or 2 with a message on stderr.
"""

import re
import sys
from fractions import Fraction as F

FRAME = F(231336, 1000000)  # mode 3, GI 1/8 (B31 Table 3-1)
RATES = {1: F(24000, 1001), 2: F(24), 3: F(25), 4: F(30000, 1001), 5: F(30),
         6: F(50), 7: F(60000, 1001), 8: F(60)}  # H.262 Table 6-4
FS = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000,
      11025, 8000, 7350]  # 13818-7 Table 35


def die(msg):
    print(f"bench-media: {msg}", file=sys.stderr)
    sys.exit(2)


def toml_string(p):
    return '"' + p.replace("\\", "\\\\").replace('"', '\\"') + '"'


def media_toml(src, dst, media):
    s = open(src).read()
    for pid, st, kind, extra in (
        ("0x0111", "0x02", "mpeg2-video", f"source = {toml_string(media + '/video.m2v')}"),
        ("0x0112", "0x0F", "aac-adts",
         f"source = {toml_string(media + '/audio.m2a')}\n  adts_compliance = \"warn\""),
    ):
        s, n = re.subn(
            rf'kind = "declared"\n(\s*pid = {pid}\n)\s*stream_type = {st}\b[^\n]*\n',
            lambda m, kind=kind, extra=extra: f'kind = "{kind}"\n{m.group(1)}  {extra}\n', s)
        if n != 1:
            die(f"{src}: no declared stream {pid} (stream_type {st}) to give media")
    open(dst, "w").write(s)


def video_length(path):
    """(pictures, frame rate): the first sequence header's frame_rate_code
    (H.262 §6.2.2.1); pictures = picture start codes, two fields each."""
    v = open(path, "rb").read()
    i = v.find(b"\x00\x00\x01\xb3")
    if i < 0 or i + 8 > len(v) or (v[i + 7] & 0x0F) not in RATES:
        die(f"{path}: no MPEG-2 sequence header with a frame rate")
    return v.count(b"\x00\x00\x01\x00"), RATES[v[i + 7] & 0x0F]


def audio_length(path):
    """(frames, samples, fs) of an ADTS file (13818-7 §6.2): each frame
    1024 × (number_of_raw_data_blocks_in_frame + 1) samples."""
    a = open(path, "rb").read()
    pos = frames = samples = 0
    fs = None
    while pos + 7 <= len(a):
        if a[pos] != 0xFF or a[pos + 1] & 0xF0 != 0xF0:
            die(f"{path}: no ADTS sync at byte {pos}")
        idx = (a[pos + 2] >> 2) & 0x0F
        length = ((a[pos + 3] & 3) << 11) | (a[pos + 4] << 3) | (a[pos + 5] >> 5)
        if idx >= len(FS) or length < 7:
            die(f"{path}: bad ADTS header at byte {pos}")
        fs = fs or FS[idx]
        frames += 1
        samples += 1024 * ((a[pos + 6] & 3) + 1)
        pos += length
    return frames, samples, fs


def main():
    if len(sys.argv) != 4:
        die("usage: bench-media.py SRC.toml DST.toml MEDIA_DIR")
    src, dst, media = sys.argv[1:4]
    pictures, fps = video_length(f"{media}/video.m2v")
    frames, samples, fs = audio_length(f"{media}/audio.m2a")
    if not pictures or not frames:
        die(f"{media}: the media hold no picture or no audio frame")
    media_toml(src, dst, media)
    tv = pictures / fps
    ta = F(samples, fs)
    period = max(tv, ta)
    n = max(1, round(period / FRAME))
    d = float(n * FRAME - period)
    print(n, f"video {pictures} pictures at {float(fps):.3f} Hz = {float(tv):.3f} s, "
          f"audio {frames} frames at {fs} Hz = {float(ta):.3f} s: media loop "
          f"{float(period):.3f} s -> TS of {n} OFDM frames = {float(n * FRAME):.3f} s "
          f"(each pass {'repeats' if d > 0 else 'drops'} {abs(d) * 1000:.0f} ms of media)")


if __name__ == "__main__":
    main()
