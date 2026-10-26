#!/usr/bin/env python3
"""List the losses in a `Symbols` words file (gaps in the block sequence
numbers), streaming it in 64 MiB pieces: where each loss is, in blocks,
symbols and seconds, and how many blocks it dropped (plan §0.18).

Usage: tools/symbols-losses.py FILE [SYMBOL_RATE]
  SYMBOL_RATE  for the times (default 33.7561e6, ISDB-S3; 28.86e6 ISDB-S)
"""

import struct
import sys

SYNC = (0x1ACF, 0xFC1D)
PIECE = 64 << 20


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    rs = float(sys.argv[2]) if len(sys.argv) > 2 else 33.7561e6
    f = open(sys.argv[1], "rb")
    carry = b""
    prev = None
    blocks = symbols = lost = losses = 0
    times = []
    while True:
        piece = f.read(PIECE)
        if not piece and not carry:
            break
        data = carry + piece
        n = len(data) // 2
        w = struct.unpack("<%dH" % n, data[: n * 2])
        i = 0
        while i + 9 <= n:
            if (w[i], w[i + 1]) != SYNC:
                i += 1
                continue
            w2 = w[i + 2]
            version, bits, log2 = w2 & 0xF, ((w2 >> 4) & 0xF) + 1, (w2 >> 8) & 0xF
            if not (1 <= version <= 4 and 2 <= bits <= 8 and 8 <= log2 <= 15):
                i += 1
                continue
            bw = 9 + (1 << log2) * 2 * bits // 16 + (2 if version >= 3 else 1)
            if i + bw > n:
                break
            seq = w[i + 3] | w[i + 4] << 16
            if prev is not None and seq != (prev + 1) & 0xFFFFFFFF:
                gap = (seq - prev - 1) & 0xFFFFFFFF
                losses += 1
                lost += gap
                t = symbols / rs
                times.append(t)
                print(f"loss {losses}: after block {blocks}, symbol {symbols}, t = {t:.3f} s: {gap} blocks missing")
            prev = seq
            blocks += 1
            symbols += 1 << log2
            i += bw
        carry = data[i * 2 :]
        if not piece:
            break
    gaps = [b - a for a, b in zip(times, times[1:])]
    print(
        f"{blocks} blocks, {symbols / rs:.2f} s; {losses} losses, {lost} blocks missing"
        + (f"; spacing {', '.join(f'{g:.2f}' for g in gaps)} s" if gaps else "")
    )


if __name__ == "__main__":
    main()
