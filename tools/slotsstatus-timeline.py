#!/usr/bin/env python3
"""Where in a run its failures and dropped slots fall, from
`examples/slotsstatus FILE.slots 1` output: per file, the frame (relative
to the run's first status block) at which the first host taps are in
force, the relative frames with dropped slots (not converged, 0
iterations), and how many frames had failures before and after the first
taps.

    slotsstatus-timeline.py RUN.frames...
"""

import sys


def main(argv):
    for path in argv[1:]:
        f0 = taps_at = None
        drops, fail_before, fail_after = [], 0, 0
        for line in open(path):
            w = line.split()
            if not w or not w[0].isdigit() or len(w) < 12:
                continue
            frame = int(w[0])
            f0 = frame if f0 is None else f0
            if w[1] != "-" and taps_at is None:
                taps_at = frame - f0
            failed, dropped = int(w[-3]), int(w[-1])
            if dropped:
                drops.append(frame - f0)
            if failed:
                if w[1] == "-":
                    fail_before += 1
                else:
                    fail_after += 1
        print(f"{path}: first taps at +{taps_at} frames; dropped slots at +{drops}; "
              f"frames with failures: {fail_before} before the first taps, {fail_after} after")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
