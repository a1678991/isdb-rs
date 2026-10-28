#!/usr/bin/env python3
"""List every assert! / assert_eq! / assert_ne! in library code with its
enclosing function and whether that function's doc comment has a
`# Panics` section (docs/plans/errors-and-logging.md §2.3: an assert on a
public function's argument contract stays, documented). Library code is
each src file up to its `#[cfg(test)] mod` (not the first `#[cfg(test)]`,
which may sit on a field). Comment lines are skipped. Read-only.

Usage: tools/panic-docs.py [crate ...]   (default: every crate)
Output: path:line: pub=<bool> panics_doc=<bool> :: <fn signature>
"""
import glob
import re
import sys

crates = sys.argv[1:]
for f in sorted(glob.glob('crates/*/src/**/*.rs', recursive=True)):
    if crates and f.split('/')[1] not in crates:
        continue
    src = open(f).read()
    m = re.search(r'#\[cfg\(test\)\]\s*(pub(\(crate\))?\s+)?mod\b', src)
    lines = (src if not m else src[:m.start()]).split('\n')
    for i, l in enumerate(lines):
        if l.strip().startswith('//') or not re.search(r'\bassert(_eq|_ne)?!\(', l):
            continue
        j = i
        while j > 0 and not re.search(r'\bfn \w+', lines[j]):
            j -= 1
        fn = lines[j].strip()
        k, doc = j - 1, []
        while k > 0 and lines[k].strip().startswith(('///', '#[', '//')):
            doc.append(lines[k])
            k -= 1
        public = bool(re.search(r'\bpub (const )?fn\b', fn))
        print(f"{f}:{i + 1}: pub={public} panics_doc={'# Panics' in chr(10).join(doc)} :: {fn[:70]}")
