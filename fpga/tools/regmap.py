"""Write (or check) the register tables generated from the chain's register
map (issue #30 item 5): `isdb_fpga/chain/registers.toml` is the one source;
`isdb_fpga/chain/top.py`'s docstring and fpga/README.md ("Register map")
carry a table rendered from it between the `regmap.BEGIN` / `regmap.END`
markers (`isdb_fpga.chain.regmap`).

    uv run --locked python tools/regmap.py          # rewrite both tables
    uv run --locked python tools/regmap.py --check  # exit 1 if either is stale
    uv run --locked python tools/regmap.py --print  # the README table on stdout

tests/test_registers.py runs the check.
"""

import argparse
import os
import sys

FPGA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
sys.path.insert(0, FPGA)

from isdb_fpga.chain import regmap  # noqa: E402

TARGETS = {
    os.path.join(FPGA, "isdb_fpga", "chain", "top.py"): regmap.render_docstring,
    os.path.join(FPGA, "README.md"): regmap.render_readme,
}


def stale(m=None):
    """The targets whose table is not the map's: [(path, current, generated)]."""
    m = m or regmap.load()
    out = []
    for path, render in TARGETS.items():
        with open(path, encoding="utf-8") as f:
            text = f.read()
        want = render(m)
        if regmap.section(text) != want:
            out.append((path, text, regmap.replace_section(text, want)))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="exit 1 if a table is not the map's")
    ap.add_argument("--print", action="store_true", help="print the README table")
    a = ap.parse_args()
    m = regmap.load()
    bad = regmap.problems(m)
    if bad:
        sys.exit("registers.toml: " + "; ".join(bad))
    if a.print:
        print(regmap.render_readme(m))
        return
    todo = stale(m)
    for path, _, new in todo:
        rel = os.path.relpath(path, FPGA)
        if a.check:
            print(f"{rel}: the register table is not the map's (run tools/regmap.py)")
        else:
            with open(path, "w", encoding="utf-8") as f:
                f.write(new)
            print(f"{rel}: written")
    if a.check and todo:
        sys.exit(1)


if __name__ == "__main__":
    main()
