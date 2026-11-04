"""Run a whole-chain set on the real-decoder chain (`stages.run_chain_real`)
with some of its injected decoder-output errors left out, to find which
one a failure follows. The comparison with the model then differs where
the left-out errors were (expected); the monitors, the decoder's and
bridge's health counters and whether the stream parses are what to read.

python tools/real_variant.py build/chain-12-fail.json [--symbols N]
    [--no-bch] [--no-tmcc FRAME[,FRAME]] [--tmcc-keep FRAME:K]
    [--tmcc-pos FRAME:P1,P2,…] [--backend verilator|cxxrtl]
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import stages  # noqa: E402


def main(argv):
    path = argv[1]
    with open(path) as f:
        doc = json.load(f)

    def opt(name, default=None):
        return argv[argv.index(name) + 1] if name in argv else default

    n = int(opt("--symbols", 0)) or None
    if "--no-bch" in argv:
        doc["bch_errors"] = []
    if "--no-tmcc" in argv:
        drop = {int(x) for x in opt("--no-tmcc").split(",")}
        doc["tmcc_errors"] = [e for e in doc.get("tmcc_errors", []) if e[0] not in drop]
    if "--tmcc-keep" in argv:
        # FRAME:K keeps only the first K positions of that frame's TMCC errors
        fr, k = (int(x) for x in opt("--tmcc-keep").split(":"))
        doc["tmcc_errors"] = [[f, p[:k] if f == fr else p] for f, p in doc.get("tmcc_errors", [])]
    if "--tmcc-pos" in argv:
        # FRAME:P1,P2,… sets that frame's TMCC errors to these positions
        fr, ps = opt("--tmcc-pos").split(":")
        doc["tmcc_errors"] = [e for e in doc.get("tmcc_errors", []) if e[0] != int(fr)]
        doc["tmcc_errors"] = sorted(doc["tmcc_errors"] + [[int(fr), [int(x) for x in ps.split(",")]]])
    print(f"bch_errors {[(e[0], e[1], len(e[2])) for e in doc.get('bch_errors', [])]}, "
          f"tmcc_errors {[(e[0], len(e[1])) for e in doc.get('tmcc_errors', [])]}")
    what, bad = stages.run_chain_real(doc, None, opt("--backend"), n)
    print(what)
    print("\n".join(bad[:20]) or "no problems")


if __name__ == "__main__":
    main(sys.argv)
