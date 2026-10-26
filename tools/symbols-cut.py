#!/usr/bin/env python3
"""Cut a `Symbols` words file the way a stream loss does, for reproducing
how a receiver recovers (`isdb rx --symbols OUT`; plan §0.18).

Each cut drops whole blocks (a DMA overflow) or, with `+W`, also W more
words so the stream resumes mid-block (the reader skips to the next
header). The sidecar (`IN.json`) is copied to `OUT.json`.

Usage: tools/symbols-cut.py IN OUT AT:BLOCKS[+WORDS] [AT:BLOCKS[+WORDS] ...]
  AT      block index (counted in IN) where the cut starts
  BLOCKS  whole blocks dropped
  WORDS   extra 16-bit words dropped after them (a mid-block resume)
"""

import shutil
import struct
import sys

SYNC = (0x1ACF, 0xFC1D)


def block_words(w2: int) -> int:
    version = w2 & 0xF
    bits = ((w2 >> 4) & 0xF) + 1
    log2 = (w2 >> 8) & 0xF
    trailer = 2 if version >= 3 else 1
    return 9 + (1 << log2) * 2 * bits // 16 + trailer


def main() -> None:
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    cuts = []
    for c in sys.argv[3:]:
        at, rest = c.split(":")
        blocks, _, words = rest.partition("+")
        cuts.append((int(at), int(blocks), int(words or 0)))
    data = open(src, "rb").read()
    n = len(data) // 2
    w = struct.unpack("<%dH" % n, data[: n * 2])
    # first header: the file may start mid-block
    start = next(i for i in range(n - 1) if (w[i], w[i + 1]) == SYNC)
    bw = block_words(w[start + 2])
    drop = []
    for at, blocks, words in cuts:
        a = start + at * bw
        drop.append((a, a + blocks * bw + words))
    drop.sort()
    out = bytearray()
    pos = 0
    for a, b in drop:
        out += data[pos * 2 : a * 2]
        pos = b
    out += data[pos * 2 :]
    open(dst, "wb").write(out)
    try:
        shutil.copyfile(src + ".json", dst + ".json")
    except FileNotFoundError:
        pass
    print(f"{dst}: {len(drop)} cut(s), block = {bw} words, {len(out) // 2} words")


if __name__ == "__main__":
    main()
