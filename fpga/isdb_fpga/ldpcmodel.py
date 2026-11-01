"""A plain Python model of the ISDB-S3 LDPC decoder's contract
(docs/plans/isdbs3-fpga-rx.md, "The LDPC schedule contract", with the
delta write-back of 2026-11-01), the IRA encoder, and a writer of test
vectors in the format the Rust model writes (`isdbs3-phy`
`examples/ldpcvectors`), so the HDL simulation runs on either. The
reference is the Rust `isdb_fec::ldpc::fixed`; this model reproduces its
vectors exactly (`--check`, and the tests' LdpcModelTest).

The arithmetic, per check (i8, saturating): t_e = sat(L_v - r_e);
|t| saturating (-128 reads 127); the sign parity; min1 <= min2 with the
first edge on ties; r_e,new = +-max(m - offset, 0), m = min2 on the min1
edge and min1 elsewhere, negative when parity xor (t_e < 0). (Which of
two tied edges is "first" never shows: a tie makes min2 = min1, so every
edge gets the same magnitude. The edge order inside a check is therefore
free, which the HDL uses.) Schedules: `{"kind": "sequential"}` (checks
0 .. P-1, each writing L_v = sat(t_e + r_e) at once) and `{"kind":
"layered", "step": S}` (layer l = the checks c = l (mod q) in increasing
c, cut into steps of S checks; within a step every check reads L and r as
at the step's start; at the step's end a variable with edges e1 .. em in
the step, in check order, gets sat(t_e1 + r_e1,new + sum over i >= 2 of
(r_ei,new - r_ei,old)), the sum exact and saturated once). Stopping: the
hard decisions (L < 0) are checked before the first iteration and after
each; a codeword stops at the first whose decisions satisfy every check,
else at max_iter.

python -m isdb_fpga.ldpcmodel --all BUILD_DIR [--toys]   # the presets as ldpcvec-py-*.json
python -m isdb_fpga.ldpcmodel OUT.json --preset NAME [--seed S]
python -m isdb_fpga.ldpcmodel --check VECTORS.json ... [--max N]   # the model against files

Presets (PRESETS): `toy-*` are small codes of the B44 form (n = 6 * 374,
the address table carried in the file as "rows", "rate" = its row count)
with conflicting pairs, triples, a quadruple and chains that wrap round
the group: the HDL tests' fast sets. `93-17` (93/120) and `tmcc-17` (the
shortened 61/120 TMCC code, the known zeros at +127) are B44 codes at
step 17. The format: crates/isdbs3-phy/examples/ldpcvectors.rs; these
files add "label" and "model" (and "rows" for the toy codes).
"""

import json
import math
import os
import re
import sys

GROUP = 374
N = 44_880
TMCC_SHORT = (1870, 9614)  # info_start, info_len of the TMCC's shortened 61/120 code
RATES = (41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109)
_HERE = os.path.dirname(os.path.abspath(__file__))
TABLES_RS = os.path.join(_HERE, "..", "..", "crates", "isdbs3-core", "src", "ldpc_tables.rs")
TABLES_JSON = os.path.join(_HERE, "..", "build", "ldpc-tables.json")


def sat(x):
    return -128 if x < -128 else 127 if x > 127 else x


def parse_tables_rs(path=TABLES_RS):
    """{rate: rows} from crates/isdbs3-core/src/ldpc_tables.rs (as
    tools/ldpc-check-degree.py reads it)."""
    text = open(path).read()
    out = {}
    for m in re.finditer(r"\(\s*(\d+),\s*&\[(.*?)\]\s*,\s*\)", text, re.S):
        out[int(m.group(1))] = [[int(v) for v in r.split(",") if v.strip()]
                                for r in re.findall(r"&\[([\d,\s]+)\]", m.group(2))]
    return out


def load_tables(path=None):
    """{rate: rows}: from `ldpc-tables.json` (the Rust model's) when it
    exists, else parsed from the .rs source (the tests check they agree)."""
    path = path or TABLES_JSON
    if os.path.exists(path):
        doc = json.load(open(path))
        assert doc["format"] == "isdb-rs ldpc tables" and doc["version"] == 1
        assert doc["n"] == N and doc["group"] == GROUP
        return {t["rate"]: t["rows"] for t in doc["tables"]}
    return parse_tables_rs()


def tables_doc(tables):
    return {"format": "isdb-rs ldpc tables", "version": 1, "n": N, "group": GROUP,
            "tables": [{"rate": r, "rows": tables[r]} for r in sorted(tables)]}


class Code:
    """A code of the B44 form (`isdb_fec::ldpc::Code::new`): bit j of
    group g is in the checks (x + j*q) mod P for each x of row g; check c
    also has the parity bits k + c - 1 (c > 0) and k + c, in that order
    after its information bits (increasing)."""

    def __init__(self, n, rows):
        self.n, self.rows = n, [list(r) for r in rows]
        self.k = GROUP * len(rows)
        self.p = n - self.k
        assert self.k < n and self.p % GROUP == 0
        self.q = self.p // GROUP
        checks = [[] for _ in range(self.p)]
        adj = []
        for g, row in enumerate(rows):
            for j in range(GROUP):
                m = g * GROUP + j
                cs = [(x + j * self.q) % self.p for x in row]
                adj.append(cs)
                for c in cs:
                    checks[c].append(m)
        for c in range(self.p):
            if c > 0:
                checks[c].append(self.k + c - 1)
            checks[c].append(self.k + c)
        self.checks, self.adj = checks, adj

    def encode(self, info):
        """`Code::encode`: info, then the accumulated parity."""
        p = [0] * self.p
        for m, b in enumerate(info):
            if b & 1:
                for c in self.adj[m]:
                    p[c] ^= 1
        for i in range(1, self.p):
            p[i] ^= p[i - 1]
        return list(info) + p

    def syndrome(self, hard):
        return [sum(hard[v] for v in vs) & 1 for vs in self.checks]

    def unsatisfied(self, hard):
        return sum(self.syndrome(hard))

    def layer_order(self):
        return [c for layer in range(self.q) for c in range(layer, self.p, self.q)]

    def steps(self, schedule):
        """The schedule as a list of steps (lists of checks)."""
        if schedule["kind"] == "sequential":
            return [[c] for c in range(self.p)]
        s = schedule["step"]
        out = []
        for layer in range(self.q):
            cs = list(range(layer, self.p, self.q))
            out.extend(cs[i:i + s] for i in range(0, len(cs), s))
        return out


def check_node(t, offset):
    """New messages of one check from its t values (`simd::check_node`)."""
    min1 = min2 = 127
    imin = par = 0
    for e, x in enumerate(t):
        a = 127 if x == -128 else abs(x)
        par ^= x < 0
        if a < min1:
            min2, min1, imin = min1, a, e
        elif a < min2:
            min2 = a
    m1, m2 = max(min1 - offset, 0), max(min2 - offset, 0)
    out = []
    for e, x in enumerate(t):
        mag = m2 if e == imin else m1
        out.append(-mag if par ^ (x < 0) else mag)
    return out


def pack(bits):
    """n bits, bit i at byte i // 8 bit i % 8, lowercase hex."""
    out = bytearray((len(bits) + 7) // 8)
    for i, b in enumerate(bits):
        out[i // 8] |= (b & 1) << (i % 8)
    return out.hex()


def unpack(hexs, n):
    raw = bytes.fromhex(hexs)
    return [(raw[i // 8] >> (i % 8)) & 1 for i in range(n)]


def decode(code, llr, schedule, max_iter, offset, trace=False):
    """The contract: {"bits", "iterations", "converged", "unsatisfied",
    "trace" (with `trace`: the hard decisions after each iteration,
    packed)}."""
    steps = code.steps(schedule)
    checks = code.checks
    lv = list(llr)
    r = [[0] * len(vs) for vs in checks]
    tr = []

    def result(it, u):
        out = {"bits": [int(x < 0) for x in lv], "iterations": it, "converged": u == 0,
               "unsatisfied": u}
        if trace:
            out["trace"] = tr
        return out

    u = code.unsatisfied([int(x < 0) for x in lv])
    if u == 0 or max_iter == 0:
        return result(0, u)
    for it in range(1, max_iter + 1):
        for st in steps:
            acc = {}
            new = []
            for c in st:
                vs, rc = checks[c], r[c]
                t = [sat(lv[v] - rc[e]) for e, v in enumerate(vs)]
                rn = check_node(t, offset)
                for e, v in enumerate(vs):
                    if v in acc:
                        acc[v] += rn[e] - rc[e]
                    else:
                        acc[v] = t[e] + rn[e]
                new.append((c, rn))
            for c, rn in new:
                r[c] = rn
            for v, x in acc.items():
                lv[v] = sat(x)
        hard = [int(x < 0) for x in lv]
        if trace:
            tr.append(pack(hard))
        u = code.unsatisfied(hard)
        if u == 0:
            return result(it, 0)
    return result(max_iter, u)


def decode_inplace(code, llr, order, max_iter, offset):
    """A decoder over the checks in `order`, each check's writes applied
    at once (`Schedule::Sequential` when `order` is 0 .. P-1): (bits,
    iterations, unsatisfied)."""
    checks = code.checks
    lv = list(llr)
    r = [[0] * len(vs) for vs in checks]
    u = code.unsatisfied([int(x < 0) for x in lv])
    if u == 0 or max_iter == 0:
        return [int(x < 0) for x in lv], 0, u
    for it in range(1, max_iter + 1):
        for c in order:
            vs = checks[c]
            t = [sat(lv[v] - r[c][e]) for e, v in enumerate(vs)]
            r[c] = check_node(t, offset)
            for e, v in enumerate(vs):
                lv[v] = sat(t[e] + r[c][e])
        hard = [int(x < 0) for x in lv]
        u = code.unsatisfied(hard)
        if u == 0:
            return hard, it, 0
    return [int(x < 0) for x in lv], max_iter, u


def self_check(code, llr, max_iter=6, offset=1):
    """The two schedules against the plain in-place decoder: Sequential
    is it in natural order, Layered{step: 1} is it in layer order (one
    check per step: the delta write-back never applies). Mismatch
    messages (empty when they agree)."""
    bad = []
    for sched, order in (({"kind": "layered", "step": 1}, code.layer_order()),
                         ({"kind": "sequential"}, range(code.p))):
        a = decode(code, llr, sched, max_iter, offset)
        b = decode_inplace(code, llr, order, max_iter, offset)
        if (a["bits"], a["iterations"], a["unsatisfied"]) != b:
            bad.append(f"{sched} != the in-place decoder in its order")
    return bad


# --- vectors ---------------------------------------------------------------

# Toy codes of the B44 form, n = 6 * 374 (k = 374 * rows, q = P / 374).
# toy-a (q = 4): pairs at shift differences 1 and 175 (layers 3 and 0), a
# triple at shifts {0, 370, 373} (a chain that wraps round the group) and
# {0, 100, 360}, {0, 1, 2}; toy-b (q = 3): a triple {0, 1, 373}, a
# quadruple {0, 1, 2, 373}, a pair {1, 2}, a triple {0, 11, 233}.
TOYS = {
    "toy-a": [[1, 3, 7, 300, 1000, 1481, 1493], [0, 2, 6, 10, 400, 901, 1440, 1495]],
    "toy-b": [[0, 3, 1119, 500, 4, 7], [2, 5, 8, 1121, 600], [1, 34, 700, 1001]],
}


class Rng:
    """xorshift64 with a Box-Muller normal (the Rust generator's shape)."""

    def __init__(self, seed):
        self.s = seed or 1

    def bits(self):
        s = self.s
        s ^= (s << 13) & 0xFFFFFFFFFFFFFFFF
        s ^= s >> 7
        s ^= (s << 17) & 0xFFFFFFFFFFFFFFFF
        self.s = s
        return s

    def uniform(self):
        return (self.bits() >> 11) / float(1 << 53)

    def gauss(self):
        u1, u2 = self.uniform() + 1e-300, self.uniform()
        return math.sqrt(-2 * math.log(u1)) * math.cos(2 * math.pi * u2)


def bpsk(bits, esn0_db, rng):
    """BPSK (0 -> +1) at Es/N0: i8 LLRs round(2y/var * 2), saturated to
    +-127 (the Rust sets' quantisation)."""
    var = 1.0 / (2 * 10 ** (esn0_db / 10))
    sd = math.sqrt(var)
    out = []
    for b in bits:
        y = (1.0 if b == 0 else -1.0) + sd * rng.gauss()
        x = 2 * y / var * 2
        out.append(max(-127, min(127, int(math.copysign(math.floor(abs(x) + 0.5), x)))))
    return out


def known(code, shortened):
    """The known-zero positions of a shortened code: the first k outside
    [info_start, info_start + info_len)."""
    s, n_ = shortened
    return [i for i in range(code.k) if not s <= i < s + n_]


def noisy(code, esn0s, rng, shortened=None):
    """One noisy codeword per Es/N0 (the known zeros at +127 when
    shortened)."""
    out = []
    for es in esn0s:
        info = [rng.bits() & 1 for _ in range(code.k)]
        if shortened:
            s, n_ = shortened
            info = [b if s <= i < s + n_ else 0 for i, b in enumerate(info)]
        llr = bpsk(code.encode(info), es, rng)
        if shortened:
            for i in known(code, shortened):
                llr[i] = 127
        out.append(llr)
    return out


def stress(code, rng):
    """Inputs that reach the corners: a valid word (iteration 0), the
    same saturated with strong flips, -128s in a noisy word, all zero."""
    info = [rng.bits() & 1 for _ in range(code.k)]
    cw = code.encode(info)
    clean = [127 if b == 0 else -127 for b in cw]
    flipped = list(clean)
    for _ in range(12):
        i = rng.bits() % code.n
        flipped[i] = -flipped[i]
    m128 = [x if rng.bits() % 7 else -128 for x in bpsk(cw, 3.0, rng)]
    return [clean, flipped, m128, [0] * code.n]


PRESETS = {
    # label: (code, step, max_iter, Es/N0 list, shortened, with the stress words)
    "toy-a": ("toy-a", 17, 12, (3.0, 0.0, -2.0, -3.0, -3.5, -4.5), None, True),
    "toy-a-short": ("toy-a", 17, 12, (-1.0, -3.0, -4.0), (100, 500), False),
    "toy-a-11": ("toy-a", 11, 8, (0.0, -2.5, -3.5), None, False),
    "toy-a-22": ("toy-a", 22, 8, (0.0, -2.5, -3.5), None, False),
    "toy-a-34": ("toy-a", 34, 8, (0.0, -2.5, -3.5), None, False),
    "toy-a-187": ("toy-a", 187, 8, (0.0, -2.5, -3.5), None, False),
    "toy-b": ("toy-b", 17, 12, (3.0, 1.0, 0.0, -1.0, -2.0), None, True),
    "toy-b-34": ("toy-b", 34, 10, (1.0, -0.5, -1.5), None, False),
    "93-17": (93, 17, 12, (3.6, 3.3, 3.1), None, False),
    "tmcc-17": (61, 17, 12, (1.5, 0.9), TMCC_SHORT, False),
}


def vectors(preset, seed=1, tables=None, trace=True):
    name, step, max_iter, esn0s, short, st = PRESETS[preset]
    rng = Rng((seed * 0x9E3779B97F4A7C15) % (1 << 64) or 1)
    if isinstance(name, str):
        rows, n, rate = TOYS[name], 6 * GROUP, len(TOYS[name])
    else:
        rows, n, rate = (tables or load_tables())[name], N, name
    code = Code(n, rows)
    llrs = noisy(code, esn0s, rng, short) + (stress(code, rng) if st else [])
    sched = {"kind": "layered", "step": step}
    doc = {"format": "isdb-rs ldpc vectors", "version": 1, "rate": rate, "n": n, "k": code.k,
           "q": code.q, "p": code.p,
           "shortened": {"info_start": short[0], "info_len": short[1]} if short else None,
           "schedule": sched, "max_iter": max_iter, "offset": 1, "codewords": [],
           "label": preset, "model": "fpga/isdb_fpga/ldpcmodel.py"}
    if isinstance(name, str):
        doc["rows"] = rows
    for llr in llrs:
        res = decode(code, llr, sched, max_iter, 1, trace=trace)
        cw = {"llr": llr, "bits_hex": pack(res["bits"]), "iterations": res["iterations"],
              "converged": res["converged"], "unsatisfied": res["unsatisfied"]}
        if trace:
            cw["trace_hex"] = res["trace"]
        doc["codewords"].append(cw)
    return doc


def code_of(doc, tables=None):
    """The code of a vectors file: its own "rows" (toy sets) or the
    tables'."""
    if "rows" in doc:
        return Code(doc["n"], doc["rows"])
    return Code(doc["n"], (tables or load_tables())[doc["rate"]])


def check_doc(doc, tables=None, limit=None, log=None):
    """The model against a vectors file: mismatch messages."""
    code = code_of(doc, tables)
    bad = []
    assert (code.k, code.p, code.q) == (doc["k"], doc["p"], doc["q"])
    for i, cw in enumerate(doc["codewords"][:limit]):
        if doc["shortened"]:
            sh = doc["shortened"]
            assert all(cw["llr"][j] == 127
                       for j in known(code, (sh["info_start"], sh["info_len"]))), \
                f"codeword {i}: known zeros not at +127"
        res = decode(code, cw["llr"], doc["schedule"], doc["max_iter"], doc["offset"],
                     trace="trace_hex" in cw)
        got = (pack(res["bits"]), res["iterations"], res["converged"], res["unsatisfied"])
        want = (cw["bits_hex"], cw["iterations"], cw["converged"], cw["unsatisfied"])
        ok = got == want and ("trace_hex" not in cw or res["trace"] == cw["trace_hex"])
        if not ok:
            bad.append(f"codeword {i}: model {got[1:]}, file {want[1:]}")
        if log:
            log(f"  codeword {i}: iterations {got[1]} converged {got[2]}: "
                + ("ok" if ok else "MISMATCH"))
    return bad


def main(argv):
    if "--check" in argv:
        limit = int(argv[argv.index("--max") + 1]) if "--max" in argv else None
        tables = load_tables()
        assert tables == parse_tables_rs(), "ldpc-tables.json differs from ldpc_tables.rs"
        bad = 0
        for path in argv[argv.index("--check") + 1:]:
            if path.startswith("--"):
                break
            doc = json.load(open(path))
            print(f"{path}: rate {doc['rate']}, {doc['schedule']}, limit {doc['max_iter']}",
                  flush=True)
            msgs = check_doc(doc, tables, limit, log=lambda s: print(s, flush=True))
            bad += len(msgs)
        return 1 if bad else 0
    if "--all" in argv:
        out = argv[argv.index("--all") + 1]
        os.makedirs(out, exist_ok=True)
        tables = load_tables()
        tpath = os.path.join(out, "ldpc-tables.json")
        if not os.path.exists(tpath):
            with open(tpath, "w") as f:
                json.dump(tables_doc(tables), f)
        names = [p for p in PRESETS if "--toys" not in argv or p.startswith("toy")]
        for i, p in enumerate(names):
            path = os.path.join(out, f"ldpcvec-py-{i + 1}-{p}.json")
            doc = vectors(p, tables=tables)
            with open(path, "w") as f:
                json.dump(doc, f)
            print(f"{path}: {len(doc['codewords'])} codewords, iterations "
                  + " ".join(("" if c["converged"] else "F") + str(c["iterations"])
                             for c in doc["codewords"]), flush=True)
        return 0
    path = argv[1]
    preset = argv[argv.index("--preset") + 1]
    seed = int(argv[argv.index("--seed") + 1]) if "--seed" in argv else 1
    doc = vectors(preset, seed)
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as f:
        json.dump(doc, f)
    print(f"{path}: {preset}, {len(doc['codewords'])} codewords")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
