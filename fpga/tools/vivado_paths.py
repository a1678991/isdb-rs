"""The paths of a Vivado timing report (`report_timing_summary` /
`report_timing` text, e.g. tezuka/timing.tcl's output) in one compact
block each: slack, source and destination (the hierarchy prefix dropped),
clocks, logic levels, data path delay and the cells along the data path.
For finding which logic a failing path crosses before pipelining it.

python tools/vivado_paths.py REPORT [--match SUBSTRING ...] [--strip PREFIX] [--max N] [--summary]

`--match`: only paths whose source or destination contains every
substring; `--summary`: one line per path; without `--max` every path.
"""

import re
import sys


def paths(text):
    """Yield a dict per path of the report."""
    for block in re.split(r"\n(?=Slack \()", text)[1:]:
        p = {"slack": float(re.match(r"Slack \(\w+\) :\s+(-?[\d.]+)ns", block).group(1))}
        for key, pat in (("src", r"Source:\s+(\S+)"), ("dst", r"Destination:\s+(\S+)"),
                         ("levels", r"Logic Levels:\s+(.*)"),
                         ("delay", r"Data Path Delay:\s+(.*)"),
                         ("req", r"Requirement:\s+(.*)")):
            m = re.search(pat, block)
            p[key] = m.group(1).strip() if m else ""
        clocks = re.findall(r"clocked by (\S+)", block)
        p["clocks"] = " -> ".join(clocks[:2])
        # the data path: between the source clock's arrival and the
        # destination's clock (the second dashed rule .. the third)
        rules = [m.start() for m in re.finditer(r"\n\s+-{40,}", block)]
        cells = []
        if len(rules) >= 3:
            for line in block[rules[1]:rules[2]].splitlines():
                m = re.search(r"\s(\w+) \((Prop_\w+|Setup_\w+)\)\s+([\d.]+)\s+([\d.]+)\s+\w\s+(\S+)", line)
                if m:
                    cells.append((m.group(1), float(m.group(3)), m.group(5)))
                m = re.search(r"net \(fo=(\d+)[^)]*\)\s+([\d.]+)\s+([\d.]+)\s+(\S+)", line)
                if m and cells:
                    cells[-1] = cells[-1] + (int(m.group(1)), float(m.group(2)))
        p["cells"] = cells
        yield p


def main(argv):
    text = open(argv[1]).read()
    match = [argv[i + 1] for i, a in enumerate(argv) if a == "--match"]
    strip = argv[argv.index("--strip") + 1] if "--strip" in argv else "i_system_wrapper/system_i/"
    nmax = int(argv[argv.index("--max") + 1]) if "--max" in argv else None
    summary = "--summary" in argv
    seen = set()
    shown = 0
    for p in paths(text):
        key = (p["src"], p["dst"], p["slack"])
        if key in seen or not all(s in p["src"] + " " + p["dst"] for s in match):
            continue
        seen.add(key)
        src, dst = p["src"].replace(strip, ""), p["dst"].replace(strip, "")
        if summary:
            print(f"{p['slack']:8.3f}  {src} -> {dst}  [{p['levels']}]")
        else:
            print(f"slack {p['slack']} ns  {p['clocks']}  req {p['req']}")
            print(f"  {src}\n  -> {dst}\n  {p['levels']}; {p['delay']}")
            for c in p["cells"]:
                name = c[2].replace(strip, "")
                fo = f" fo={c[3]} net {c[4]:.3f}" if len(c) > 3 else ""
                print(f"    {c[0]:8s} {c[1]:.3f}{fo}  {name}")
        shown += 1
        if nmax and shown >= nmax:
            break
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
