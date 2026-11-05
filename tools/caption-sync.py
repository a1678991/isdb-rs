#!/usr/bin/env python3
"""Check a SubRip caption file against its recording: cues that overlap (programmes
laid over each other show up here), and how far the cues sit from the speech.

    tools/caption-sync.py CUES.srt [--audio MEDIA [--from S] [--to S]]

With --audio, the speech-band energy of MEDIA's audio stream (--stream) is taken around
the start of each cue that follows a pause; the lag at which it rises most is how late
the speech comes after the cues (positive: the cues lead), with how far that peak stands
out from the other lags in standard deviations. --from/--to restrict the cues to a span
of the timeline, one programme of a recording. Cues in parentheses or of music are left
out. Below 3 sigma or so there is no measure (music, continuous narration). Captions may
be authored to lead speech a little, so compare the same span between builds rather than
reading a lag as an absolute. MEDIA's timeline must start where the cues' does. Needs
ffmpeg and numpy.
"""

import argparse
import re
import subprocess
import sys

CUE = re.compile(
    r"(\d+):(\d\d):(\d\d)[,.](\d+) --> (\d+):(\d\d):(\d\d)[,.](\d+)\r?\n(.*?)(?:\r?\n\r?\n|\Z)",
    re.S,
)
HOP = 0.02
PAUSE = 1.0


def read_cues(path):
    text = open(path, encoding="utf-8-sig").read()
    cues = []
    for m in CUE.finditer(text):
        g = [int(x) for x in m.groups()[:8]]
        start = g[0] * 3600 + g[1] * 60 + g[2] + g[3] / 1000
        end = g[4] * 3600 + g[5] * 60 + g[6] + g[7] / 1000
        cues.append((start, end, m.group(9).strip()))
    return sorted(cues)


def overlaps(cues):
    return [(a, b) for a, b in zip(cues, cues[1:]) if b[0] < a[1]]


def is_speech(text):
    return not re.search(r"^[（(]|♬|♪", text)


def energy(media, stream):
    import numpy as np

    # Gaps are filled from the first timestamp: the energy stays on the timeline.
    command = [
        "ffmpeg", "-nostdin", "-v", "quiet", "-i", media, "-map", f"0:a:{stream}", "-ac", "1",
        "-af", "aresample=8000:async=1:first_pts=0,highpass=f=300,lowpass=f=3000",
        "-f", "s16le", "-",
    ]
    samples = np.frombuffer(
        subprocess.run(command, stdout=subprocess.PIPE, check=False).stdout, dtype=np.int16
    ).astype(np.float64)
    n = int(HOP * 8000)
    frames = samples[: len(samples) // n * n].reshape(-1, n)
    return np.log1p((frames**2).mean(axis=1))


def lag_of(cues, e, start, stop, reach=5.0, side=0.5):
    """The lag at which energy rises most across the starts of cues after a pause."""
    import numpy as np

    hops, width = int(reach / HOP), int(side / HOP)
    previous_end = float("-inf")
    onsets = []
    for s, t, text in cues:
        if is_speech(text) and start <= s and t <= stop and s - previous_end >= PAUSE:
            onsets.append(int(s / HOP))
        previous_end = max(previous_end, t)
    onsets = [o for o in onsets if o - hops - width >= 0 and o + hops + width < len(e)]
    if not onsets:
        return 0, 0.0, 0.0
    # Prefix sums: the mean of a window in constant time.
    total = np.concatenate(([0.0], np.cumsum(e)))
    rise = []
    for k in range(-hops, hops + 1):
        at = np.array(onsets) + k
        after = total[at + width] - total[at]
        before = total[at] - total[at - width]
        rise.append(float((after - before).mean()) / width)
    rise = np.array(rise)
    k = int(rise.argmax())
    return len(onsets), (k - hops) * HOP, float((rise[k] - rise.mean()) / (rise.std() or 1))


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("srt")
    p.add_argument("--audio")
    p.add_argument("--stream", type=int, default=0, help="which audio stream of MEDIA")
    p.add_argument("--from", dest="start", type=float, default=0)
    p.add_argument("--to", dest="stop", type=float, default=float("inf"))
    a = p.parse_args()
    cues = read_cues(a.srt)
    if not cues:
        sys.exit(f"{a.srt}: no cues")
    o = overlaps(cues)
    print(f"{a.srt}: {len(cues)} cues, {cues[0][0]:.3f}-{cues[-1][1]:.3f} s, {len(o)} overlap the next")
    for x, y in o[:3]:
        print(f"  {x[0]:.3f}-{x[1]:.3f} {x[2][:24]!r} / {y[0]:.3f}-{y[1]:.3f} {y[2][:24]!r}")
    if a.audio:
        e = energy(a.audio, a.stream)
        stop = min(a.stop, len(e) * HOP)
        used, lag, peak = lag_of(cues, e, a.start, stop)
        print(f"  {a.start:.0f}-{stop:.0f} s: {used} cue starts after a pause, speech {lag:+.2f} s after them ({peak:.1f} sigma)")


if __name__ == "__main__":
    main()
