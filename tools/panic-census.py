#!/usr/bin/env python3
"""Census of panic sites in library code: unwrap / expect / panic! /
unreachable! / assert! per crate, counting only the part of each src file
before its `#[cfg(test)]` module and skipping comment lines. The numbers
drive docs/plans/errors-and-logging.md item E4; run it before and after a
change to see what the change retired. Read-only.

Usage: tools/panic-census.py [crate ...]   (default: every crate)
"""
import re,glob,sys,collections
tot=collections.Counter(); per=collections.defaultdict(collections.Counter)
crates=sys.argv[1:]
for f in glob.glob('crates/*/src/**/*.rs', recursive=True):
    if crates and f.split('/')[1] not in crates: continue
    crate=f.split('/')[1]
    src=open(f).read()
    cut=src.find('#[cfg(test)]')
    lib=src if cut<0 else src[:cut]
    # strip comments and doc lines
    lib='\n'.join(l for l in lib.split('\n') if not l.strip().startswith('//'))
    for k,pat in [('unwrap',r'\.unwrap\(\)'),('expect',r'\.expect\('),('panic!',r'\bpanic!\('),('unreachable!',r'\bunreachable!\('),('assert!',r'\bassert(_eq|_ne)?!\(')]:
        n=len(re.findall(pat,lib)); per[crate][k]+=n; tot[k]+=n
print(f"{'crate':14s} {'unwrap':>7s} {'expect':>7s} {'panic!':>7s} {'unreach':>7s} {'assert':>7s}")
for c in sorted(per): p=per[c]; print(f"{c:14s} {p['unwrap']:7d} {p['expect']:7d} {p['panic!']:7d} {p['unreachable!']:7d} {p['assert!']:7d}")
print(f"{'TOTAL':14s} {tot['unwrap']:7d} {tot['expect']:7d} {tot['panic!']:7d} {tot['unreachable!']:7d} {tot['assert!']:7d}")
