"""The chaos runs' results (build/chaos/<set>-s<seed>.json, written by
`tests/test_chaos.py` and `tools/chaos_run.py`) as a Markdown table: per
run the events, the health after each (banks held and the queue levels
in/tag/out/result FIFO, at each sample), the comparison with the model
and the problems.

python tools/chaos_table.py [build/chaos/*.json ...]     # from fpga/
"""

import glob
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import chaos  # noqa: E402

SHORT = {"long": "L", "short": "S", "start": "R", "loss": "X", "setloss": "x", "taps": "T"}


def event_text(ev):
    k = ev["kind"]
    s = SHORT[k]
    if k in ("loss", "setloss"):
        return f"{s}{ev['lost']}@{ev['at']}"
    if k == "taps":
        return f"{s}({ev.get('which', '')})@{ev['at']}"
    move = "" if ev["resume"] == ev["at"] else f"→{ev['resume']}"
    pause = f"/{ev['pause']}" if "pause" in ev else ""
    return f"{s}{pause}@{ev['at']}{move}"


def health_text(r):
    out = []
    for s in r["samples"]:
        st = s["state"]
        lab = {"stopped": "stop", "after START": "start", "at the event": "ev", "settled": "set",
               "end": "end"}[s["label"]]
        tag = f"{s['event']}" if s["event"] is not None else ""
        out.append(f"{tag}{lab}:{st['held']}/{st['in_level']}.{st['tag_level']}.{st['out_level']}.{st['rf_level']}"
                   + ("!" if s["problems"] else ""))
    return " ".join(out)


def main(argv):
    files = argv[1:] or sorted(glob.glob(os.path.join(chaos.BUILD, "chaos", "*-s*.json")))
    rows = []
    for p in files:
        with open(p) as f:
            r = json.load(f)
        if "samples" not in r:
            continue
        frames = ", ".join(f"{k}: {v}" for k, v in r["frames"].items())
        seed = (0, r["seed"], "") if isinstance(r["seed"], int) else (1, 0, str(r["seed"]))
        rows.append((chaos.SETS.index(r["set"]) if r["set"] in chaos.SETS else 9, seed,
                     f"| {r['set']} | {r['seed']} | {' '.join(event_text(e) for e in r['events'])} | "
                     f"{health_text(r)} | {frames} | "
                     f"{'; '.join(r['problems'][:4]) if r['problems'] else 'none'} | "
                     f"{r['timing']['total']:.0f} s |"))
    print("| set | seed | events | health (held/in.tag.out.rf) | frames vs model | problems | time |")
    print("|---|---|---|---|---|---|---|")
    for _a, _b, line in sorted(rows):
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
