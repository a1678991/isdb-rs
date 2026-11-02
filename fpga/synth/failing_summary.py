"""Summarise the failing setup endpoints `fpga/tezuka/failing.tcl` lists
(its FAIL lines): per register name with the bit index removed, the
endpoint count, the worst slack and the worst path's start point, worst
first; optionally only the endpoints under a hierarchy prefix.

python synth/failing_summary.py FAILING.log [PREFIX] [--top N]
"""

import re
import sys


def main(argv):
    path = argv[1]
    prefix = argv[2] if len(argv) > 2 and not argv[2].startswith("--") else ""
    top = int(argv[argv.index("--top") + 1]) if "--top" in argv else 60
    groups = {}
    pat = re.compile(r"^FAIL \S+ -> \S+ : (\S+)\s+(\d+) endpoints, worst\s+(-?[\d.]+) ns\s+\((\S+) -> (\S+)\)")
    for line in open(path):
        mm = pat.match(line)
        if not mm:
            continue
        ep, n, slack, start, _ = mm.groups()
        if prefix not in ep:
            continue
        key = re.sub(r"\[\d+\]", "[*]", ep)
        skey = re.sub(r"\[\d+\]", "[*]", start.replace("i_system_wrapper/system_i/", ""))
        g = groups.setdefault(key, [0, 0.0, ""])
        g[0] += int(n)
        if float(slack) < g[1]:
            g[1], g[2] = float(slack), skey
    rows = sorted(groups.items(), key=lambda kv: kv[1][1])
    print(f"{sum(g[0] for g in groups.values())} endpoints in {len(groups)} registers"
          + (f" under {prefix}" if prefix else ""))
    for key, (n, slack, start) in rows[:top]:
        print(f"{slack:8.3f} ns {n:5d}  {key}  <- {start}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
