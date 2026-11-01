#!/usr/bin/env python3
"""Check LDPC vectors files against an independent Python reading of the
decoder contract (docs/plans/isdbs3-fpga-rx.md, "The LDPC schedule
contract"; the format in crates/isdbs3-phy/examples/ldpcvectors.rs).

Builds the code from `ldpc-tables.json` alone (ARIB STD-B44 v2.1
§3.4 (2): bit j of group g is in the checks (x + j*q) mod P for each x of
table row g, then the staircase parity bits c-1 and c), decodes each
codeword's `llr` with the file's schedule, limit and offset in plain
integer arithmetic, and compares bits, iterations, convergence, the
unsatisfied count and (when present) every trace entry. The layered
schedule's step writes are the contract's delta write-back: a variable
with edges e1 ... em in a step gets sat(t_e1 + r_e1,new + sum over
i = 2..m of (r_ei,new - r_ei,old)).

Usage: tools/ldpc-vectors-check.py [--max N] TABLES.json VECTORS.json [VECTORS.json …]
(files in parallel; --max: the first N codewords of each)

Pure Python: about 0.1 s per iteration of a 93/120 codeword.
"""

import json
import sys


def sat(x):
    return max(-128, min(127, x))


def build(tables, rate, n, group):
    rows = next(t["rows"] for t in tables["tables"] if t["rate"] == rate)
    k = group * len(rows)
    p = n - k
    q = p // group
    checks = [[] for _ in range(p)]
    for g, row in enumerate(rows):
        for j in range(group):
            m = g * group + j
            for x in row:
                checks[(x + j * q) % p].append(m)
    for c in range(p):
        # information bits in increasing index (they are appended so),
        # then parity c-1, then parity c
        if c > 0:
            checks[c].append(k + c - 1)
        checks[c].append(k + c)
    return k, p, q, checks


def steps_of(sched, p, q, group):
    if sched["kind"] == "sequential":
        return [[c] for c in range(p)]
    s = sched["step"]
    out = []
    for layer in range(q):
        cs = list(range(layer, p, q))
        assert len(cs) == group
        out.extend(cs[i:i + s] for i in range(0, group, s))
    return out


def unsatisfied(checks, hard):
    return sum(1 for vs in checks if sum(hard[v] for v in vs) & 1)


def pack(bits):
    out = bytearray((len(bits) + 7) // 8)
    for i, b in enumerate(bits):
        out[i // 8] |= (b & 1) << (i % 8)
    return out.hex()


def decode(checks, steps, llr, max_iter, offset):
    l = list(llr)
    r = [[0] * len(vs) for vs in checks]
    trace = []
    hard = [int(x < 0) for x in l]
    u = unsatisfied(checks, hard)
    if u == 0 or max_iter == 0:
        return hard, 0, u == 0, u, trace
    for it in range(1, max_iter + 1):
        for st in steps:
            out = []
            for c in st:
                vs = checks[c]
                t = [sat(l[v] - r[c][e]) for e, v in enumerate(vs)]
                min1 = min2 = 127
                imin = 0
                par = 0
                for e, x in enumerate(t):
                    a = min(abs(x), 127)
                    par ^= x < 0
                    if a < min1:
                        min2, min1, imin = min1, a, e
                    elif a < min2:
                        min2 = a
                new = []
                for e, x in enumerate(t):
                    mag = max((min2 if e == imin else min1) - offset, 0)
                    new.append(-mag if par ^ (x < 0) else mag)
                out.append((c, t, new))
            # delta write-back: per variable, its edges in check order;
            # sat(t1 + r1_new + sum over the later edges of (r_new - r_old)),
            # summed exactly, saturated once
            acc = {}
            for c, t, new in out:
                for e, v in enumerate(checks[c]):
                    if v in acc:
                        acc[v] += new[e] - r[c][e]
                    else:
                        acc[v] = t[e] + new[e]
                r[c] = new
            for v, x in acc.items():
                l[v] = sat(x)
        hard = [int(x < 0) for x in l]
        trace.append(pack(hard))
        u = unsatisfied(checks, hard)
        if u == 0:
            return hard, it, True, 0, trace
    return hard, max_iter, False, u, trace


def check_file(tables_path, vec_path, limit=None):
    """Check one file; returns the number of mismatching codewords."""
    tables = json.load(open(tables_path))
    vec = json.load(open(vec_path))
    limit = len(vec["codewords"]) if limit is None else limit
    assert tables["format"] == "isdb-rs ldpc tables" and tables["version"] == 1
    assert vec["format"] == "isdb-rs ldpc vectors" and vec["version"] == 1
    k, p, q, checks = build(tables, vec["rate"], vec["n"], tables["group"])
    assert (k, p, q) == (vec["k"], vec["p"], vec["q"]), (k, p, q)
    steps = steps_of(vec["schedule"], p, q, tables["group"])
    bad = 0
    for i, cw in enumerate(vec["codewords"][:limit]):
        assert len(cw["llr"]) == vec["n"]
        if vec["shortened"]:
            s, n_ = vec["shortened"]["info_start"], vec["shortened"]["info_len"]
            known = [cw["llr"][j] for j in range(k) if not s <= j < s + n_]
            assert all(x == 127 for x in known), "known zeros not at +127"
        hard, it, conv, u, trace = decode(checks, steps, cw["llr"], vec["max_iter"], vec["offset"])
        got = (pack(hard), it, conv, u)
        want = (cw["bits_hex"], cw["iterations"], cw["converged"], cw["unsatisfied"])
        ok = got == want and ("trace_hex" not in cw or cw["trace_hex"] == trace)
        bad += not ok
        print(f"{vec_path} codeword {i}: iterations {it} converged {conv} unsatisfied {u}: "
              + ("ok" if ok else f"MISMATCH (file: {want[1:]})"), flush=True)
    print(f"{vec_path}: {limit - bad} of {limit} codewords agree", flush=True)
    return bad


def main():
    args = sys.argv[1:]
    limit = None
    if "--max" in args:
        i = args.index("--max")
        limit = int(args[i + 1])
        del args[i:i + 2]
    tables, files = args[0], args[1:]
    from concurrent.futures import ProcessPoolExecutor
    with ProcessPoolExecutor() as ex:
        bad = sum(ex.map(check_file, [tables] * len(files), files, [limit] * len(files)))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
