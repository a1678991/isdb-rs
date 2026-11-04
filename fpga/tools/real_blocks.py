"""Compare the blocks of a real-decoder chain run (`stages.run_chain_real`
writes the chain's words to build/chain-real-got-N.json) with a whole-chain
set's, block by block, and print the first words of the blocks that differ
(decoded: a slot block's w0 fields, iterations, BCH), to see what a
failing run got wrong.

python tools/real_blocks.py build/chain-13-modes.json build/chain-real-got-2450000.json
    [--frames 2,3] [--max 12] [--order]

--order prints the blocks the run got in stream order (frame, kind, slot,
first words) instead: which result came where, e.g. a TMCC block in a
slot result's place (a tag out of step with the decoder's commits).
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import slots as S  # noqa: E402
from isdb_fpga.chain.stages import blocks_by_frame  # noqa: E402


def slot_fields(p):
    w0, w1, w2 = p[:3]
    return (f"slot {w0 & 0xFF} mode {w0 >> 8 & 7} dummy {w0 >> 11 & 1} conv {w0 >> 12 & 1} "
            f"bch_ok {w0 >> 13 & 1} mod {w1 & 0xF} rate {w1 >> 4 & 0xF} it {w1 >> 8} bch {w2:#x}")


def main(argv):
    with open(argv[1]) as f:
        doc = json.load(f)
    with open(argv[2]) as f:
        got = json.load(f)

    def opt(name, default=None):
        return argv[argv.index(name) + 1] if name in argv else default

    frames = {int(x) for x in opt("--frames").split(",")} if opt("--frames") else None
    most = int(opt("--max", 12))
    if "--order" in argv:
        for kind, _seq, frame, pay in S.blocks(got):
            k = (frame, kind, pay[0] & 0x7F if kind == S.SLOT else 0)
            if frames is None or frame in frames:
                print(k, slot_fields(pay) if kind == S.SLOT else f"{len(pay)} words, first {pay[:6]}")
        return
    gb, _, _ = blocks_by_frame(got)
    eb, _, _ = blocks_by_frame(doc["slots_words"])
    shown = 0
    for k in sorted(set(gb) | set(eb)):
        if frames is not None and k[0] not in frames:
            continue
        g, e = gb.get(k), eb.get(k)
        if g == e:
            continue
        print(f"block {k}:")
        for name, p in (("got", g), ("expected", e)):
            if p is None:
                print(f"  {name}: absent")
            elif k[1] == S.SLOT:
                print(f"  {name}: {slot_fields(p)}, {len(p)} words")
            else:
                print(f"  {name}: {len(p)} words, first {p[:6]}")
        shown += 1
        if shown >= most:
            break


if __name__ == "__main__":
    main(sys.argv)
