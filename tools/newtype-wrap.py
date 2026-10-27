#!/usr/bin/env python3
"""Mechanical step 4 of docs/plans/type-safety.md §1.2 for a value newtype:
wrap the integer literals rustc reports as "expected `<Type>`, found
integer" in the newtype's constructor, so the remaining errors are the
non-literal sites that need a hand edit.

    cargo build -p isdb-ts --all-targets --message-format=json 2>/dev/null \
      | tools/newtype-wrap.py Pid 'Pid::new({}).unwrap()'

Only a bare literal (decimal or hex, optionally `&`-borrowed) at the
primary span of an E0308 whose expected type is `<Type>` is rewritten; a
variable or expression is reported on stderr and left alone. Run it,
rebuild, repeat until it rewrites nothing, then fix the rest by hand.
"""

import collections
import json
import re
import sys


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    type_name, ctor = sys.argv[1], sys.argv[2]
    expected = f"expected `{type_name}`"
    expected_ref = f"expected `&{type_name}`"
    literal = re.compile(r"&?(0x[0-9A-Fa-f_]+|[0-9][0-9_]*)")
    edits: dict[str, set[tuple[int, int, int, str]]] = collections.defaultdict(set)
    for raw in sys.stdin:
        try:
            m = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if m.get("reason") != "compiler-message":
            continue
        msg = m["message"]
        code = msg.get("code") or {}
        if msg.get("level") != "error" or code.get("code") != "E0308":
            continue
        for sp in msg["spans"]:
            if not sp.get("is_primary"):
                continue
            label = sp.get("label") or ""
            if expected not in label and expected_ref not in label:
                continue
            if "found integer" not in label and "{integer}" not in label:
                continue
            text = sp["text"][0]["text"] if sp["text"] else ""
            lo, hi = sp["column_start"], sp["column_end"]
            frag = text[lo - 1 : hi - 1]
            # A literal in a `match` pattern cannot become a call; the arm
            # needs a named constant or a guard instead.
            if not literal.fullmatch(frag) or "=>" in text[hi - 1 :]:
                print(
                    f"skip {sp['file_name']}:{sp['line_start']}:{lo} {frag!r}: {label}",
                    file=sys.stderr,
                )
                continue
            edits[sp["file_name"]].add((sp["line_start"], lo, hi, frag))
    n = 0
    for path, sites in edits.items():
        with open(path, encoding="utf-8") as fh:
            lines = fh.read().split("\n")
        # Right-to-left within a line keeps earlier columns valid.
        for line, lo, hi, frag in sorted(sites, key=lambda e: (-e[0], -e[1])):
            s = lines[line - 1]
            assert s[lo - 1 : hi - 1] == frag, (path, line, s, frag)
            amp = frag.startswith("&")
            rep = ("&" if amp else "") + ctor.format(frag.lstrip("&"))
            lines[line - 1] = s[: lo - 1] + rep + s[hi - 1 :]
            n += 1
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines))
    print(f"{n} literals wrapped in {len(edits)} files", file=sys.stderr)


if __name__ == "__main__":
    main()
