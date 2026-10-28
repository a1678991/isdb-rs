#!/usr/bin/env python3
"""Census of panic sites in library code: unwrap / expect / panic! /
unreachable! / assert! per crate, counting only the part of each src file
before its `#[cfg(test)] mod … {` and skipping comment lines and files
a parent declares `#[cfg(test)] mod name;`. The numbers
drive docs/plans/errors-and-logging.md item E4; run it before and after a
change to see what the change retired. Read-only.

Usage: tools/panic-census.py [crate ...]   (default: every crate)
"""
import re,glob,sys,collections
tot=collections.Counter(); per=collections.defaultdict(collections.Counter)
crates=sys.argv[1:]
# `#[cfg(test)]` directly followed by a module: the test module that ends
# the library part of a file. (An attribute on a field or an item is not
# a cut: format.rs has one on a struct field.)
TEST_MOD=re.compile(r'#\[cfg\(test\)\]\s*\n\s*(pub(\([a-z]+\))?\s+)?mod\s+([a-z_0-9]+)\s*([{;])')
# files that a lib.rs / mod.rs declares as `#[cfg(test)] mod name;`
test_only=set()
for f in glob.glob('crates/*/src/**/*.rs', recursive=True):
    for m in TEST_MOD.finditer(open(f).read()):
        if m.group(4)==';':
            d=f.rsplit('/',1)[0]
            test_only.update({f"{d}/{m.group(3)}.rs", f"{d}/{m.group(3)}/mod.rs"})
for f in glob.glob('crates/*/src/**/*.rs', recursive=True):
    if crates and f.split('/')[1] not in crates: continue
    if f in test_only: continue
    crate=f.split('/')[1]
    src=open(f).read()
    m=next((m for m in TEST_MOD.finditer(src) if m.group(4)=='{'), None)
    lib=src if m is None else src[:m.start()]
    # strip comments and doc lines
    lib='\n'.join(l for l in lib.split('\n') if not l.strip().startswith('//'))
    for k,pat in [('unwrap',r'\.unwrap\(\)'),('expect',r'\.expect\('),('panic!',r'\bpanic!\('),('unreachable!',r'\bunreachable!\('),('assert!',r'\bassert(_eq|_ne)?!\(')]:
        n=len(re.findall(pat,lib)); per[crate][k]+=n; tot[k]+=n
print(f"{'crate':14s} {'unwrap':>7s} {'expect':>7s} {'panic!':>7s} {'unreach':>7s} {'assert':>7s}")
for c in sorted(per): p=per[c]; print(f"{c:14s} {p['unwrap']:7d} {p['expect']:7d} {p['panic!']:7d} {p['unreachable!']:7d} {p['assert!']:7d}")
print(f"{'TOTAL':14s} {tot['unwrap']:7d} {tot['expect']:7d} {tot['panic!']:7d} {tot['unreachable!']:7d} {tot['assert!']:7d}")
