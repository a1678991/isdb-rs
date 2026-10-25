#!/usr/bin/env python3
"""Repeat a SigMF recording N times into one longer recording, every copy
of every segment its own capture marked `isdb:gap_before` (the blocks are
discontinuous, as `isdb capture --segments` writes them): a long
multi-block input for throughput measurements of `isdb rx` from the
recordings at hand (plan §0.10, §0.11). The signal repeats, so decoded
streams repeat too; use it for speed, not for content.

Usage: tools/sigmf-repeat.py IN_BASE OUT_BASE N
  IN_BASE / OUT_BASE  recording base names (NAME for NAME.sigmf-data/-meta)
"""

import json
import shutil
import sys

ITEM_BYTES = {"ci8": 2, "ci16_le": 4, "cf32_le": 8}


def main() -> None:
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    src, dst, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
    if n < 1:
        sys.exit("N must be at least 1")
    meta = json.load(open(src + ".sigmf-meta"))
    item = ITEM_BYTES[meta["global"]["core:datatype"]]
    data = src + ".sigmf-data"
    with open(data, "rb") as f:
        f.seek(0, 2)
        samples = f.tell() // item
    caps = meta["captures"]
    out_caps = []
    for k in range(n):
        for c in caps:
            c = dict(c)
            c["core:sample_start"] = c["core:sample_start"] + k * samples
            if c["core:sample_start"] > 0:
                c["isdb:gap_before"] = True
            out_caps.append(c)
    meta["captures"] = out_caps
    meta["global"]["core:description"] = (
        meta["global"].get("core:description", "") + f" (repeated {n}x by tools/sigmf-repeat.py)"
    )
    with open(dst + ".sigmf-data", "wb") as out:
        for _ in range(n):
            with open(data, "rb") as f:
                shutil.copyfileobj(f, out, 1 << 24)
    with open(dst + ".sigmf-meta", "w") as f:
        json.dump(meta, f, indent=2, sort_keys=True)
    print(f"{dst}: {n} x {samples} samples, {len(out_caps)} captures")


if __name__ == "__main__":
    main()
