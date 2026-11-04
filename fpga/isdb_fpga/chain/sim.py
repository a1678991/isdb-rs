"""Simulation of the chain's stages against the vectors of
`isdbs3-phy`'s `examples/chainvectors` (fpga/README.md "Chain vectors"),
bit for bit, the first mismatch reported with its symbol index.

python -m isdb_fpga.chain.sim VECTORS.json [--backend pysim|cxxrtl|verilator] [--symbols N]

The backend defaults to `$FPGA_SIM`, else pysim (`simx.py`: the compiled
backends, needed for whole sets in seconds); the real-decoder sets
(chain-7-hdl, -8-loss, -9-fade, -10-bch) to the compiled backend
(`simx.compiled_backend`).

The stage is taken from the file's name (chain-1-sync, chain-2-phase, ...,
chain-rec-*). `--symbols N` simulates a prefix of the input (stage 1: the
first N input symbols; what is compared is then everything the model gives
for that prefix).
"""

import json
import sys
import time

from .common import model_constants
from .simx import engine


def load(path):
    with open(path) as f:
        return json.load(f)


def check_constants(doc):
    """Mismatches between the vectors' "chain" constants and ours."""
    ours = model_constants()
    bad = []
    for k, v in doc["chain"].items():
        if k == "pilot_signs":
            from .tables import pilot_signs

            if v != pilot_signs():
                bad.append("constant pilot_signs differs")
            continue
        if k in ours and ours[k] != v:
            bad.append(f"constant {k}: ours {ours[k]!r:.60} != {v!r:.60}")
    return bad


def first_mismatch(name, got, exp, key=lambda i, x: f"#{i}", limit=1):
    """Compare two record lists; return messages (empty when equal)."""
    out = []
    for i, (a, b) in enumerate(zip(got, exp)):
        if tuple(a) != tuple(b):
            out.append(f"{name} {key(i, b)}: got {list(a)}, expected {list(b)}")
            if len(out) >= limit:
                return out
    if len(got) != len(exp):
        out.append(f"{name}: {len(got)} records, {len(exp)} expected")
    return out


# --- stage 1 (with 2a, 2b) ---------------------------------------------------

def stage1_ports(d):
    s, n = d.sync, d.nco
    p = {
        "i_valid": d.i_valid, "i_ready": d.i_ready, "x0": d.i_x[0], "x1": d.i_x[1],
        "i_loss": d.i_loss, "i_lost": d.i_lost,
        "rv": n.o_valid, "r0": n.o_r[0], "r1": n.o_r[1], "ph": n.o_phase,
        "wv": s.o_w_valid, "cf0": s.o_w_cf[0], "cf1": s.o_w_cf[1],
        "cs0": s.o_w_cs[0], "cs1": s.o_w_cs[1], "a": s.o_w_a,
        "pv": s.o_per_valid, "pat": s.o_per_at, "ppk": s.o_per_peak, "ppos": s.o_per_pos,
        "psum": s.o_per_sum, "plk": s.o_per_locked, "poff": s.o_per_offset,
        "ppar": s.o_per_parity,
        "hv": s.o_head_valid, "hat": s.o_head_at, "hw": s.o_head_word,
        "hc0": s.o_head_corr[0], "hc1": s.o_head_corr[1], "hth": s.o_head_theta,
        "hamp": s.o_head_amp, "hnd": s.o_head_numbered, "hnum": s.o_head_number,
        "hfr": s.o_head_frame,
        "fv": n.o_f_valid, "f": n.f, "fat": d.o_f_at,
        "rst": d.o_restart,
    }
    return p


def simulate_stage1(doc, backend=None, symbols=None, in_bits=6):
    from .sync import SyncFront

    d = SyncFront(in_bits, detail=True)
    e = engine(d, stage1_ports(d), backend, tag="chain1")
    gr = e.group("rv", ["r0", "r1", "ph"])
    gw = e.group("wv", ["cf0", "cf1", "cs0", "cs1", "a"])
    gp = e.group("pv", ["pat", "ppk", "ppos", "psum", "plk", "poff", "ppar"])
    gh = e.group("hv", ["hat", "hw", "hc0", "hc1", "hth", "hamp", "hnd", "hnum", "hfr"])
    gf = e.group("fv", ["fat", "f"])
    gx = e.group("rst", ["rst"])
    x = doc["input"]
    n = len(x) // 2 if symbols is None else min(symbols, len(x) // 2)
    t0 = time.time()
    e.run("i_valid", "i_ready", ["x0", "x1"], x[: 2 * n], tail=400)
    res = {
        "r": e.read(gr), "w": e.read(gw), "periods": e.read(gp), "heads": e.read(gh),
        "carrier": e.read(gf), "restarts": e.read(gx), "symbols": n, "cycles": e.cycles(),
    }
    res["seconds"] = time.time() - t0
    e.close()
    return res


def compare_stage1(doc, res):
    n = res["symbols"]
    bad = []
    x = doc["r"]
    exp_r = [(x[2 * i], x[2 * i + 1]) for i in range(n)]
    got_r = [(a, b) for a, b, _ in res["r"]]
    bad += first_mismatch("r", got_r, exp_r, key=lambda i, _: f"symbol {i}")
    phases = {at: ph for at, ph in doc["nco_phase"]}
    for i, (_, _, ph) in enumerate(res["r"]):
        if i in phases and phases[i] != ph:
            bad.append(f"nco_phase symbol {i}: got {ph}, expected {phases[i]}")
            break
    nw = max(0, n - 23)
    exp_w = [tuple(v[1:]) for v in doc["per_symbol"] if v[0] < nw]
    bad += first_mismatch("per_symbol", res["w"][: len(exp_w)], exp_w,
                          key=lambda i, v: f"position {i}")
    exp_p = [(p[0], p[1], p[2], p[3] | p[4] << 32, p[5], p[6], p[7])
             for p in doc["periods"] if p[0] + 23 < n]
    bad += first_mismatch("period", res["periods"], exp_p, key=lambda i, v: f"at {v[0]}")
    exp_h = [tuple(h) for h in doc["heads"] if h[0] + 24 <= n]
    got_h = [(h[0], h[1], h[2], h[3], h[4], h[5], h[7] if h[6] else -1, h[8] if h[6] else -1)
             for h in res["heads"]]
    # the model's last head of the prefix may still be held (head 0)
    bad += first_mismatch("head", got_h, exp_h, key=lambda i, v: f"at {v[0]}")
    exp_c = [tuple(c) for c in doc["carrier"] if c[0] <= n]
    bad += first_mismatch("carrier", res["carrier"], exp_c, key=lambda i, v: f"at {v[0]}")
    if res["restarts"]:
        bad.append(f"{len(res['restarts'])} restarts; the vectors have none")
    return bad


def stage_of(path):
    import os

    b = os.path.basename(path)
    for k in ("11-taps", "12-fail", "13-modes", "1-sync", "2-phase", "3-eq", "4-tmcc", "5-demap",
              "6-chain", "7-hdl", "8-loss", "9-fade", "10-bch"):
        if k in b:
            return k
    raise ValueError(f"{path}: not a chain vectors file")


def main(argv):
    path = argv[1]
    backend = argv[argv.index("--backend") + 1] if "--backend" in argv else None
    symbols = int(argv[argv.index("--symbols") + 1]) if "--symbols" in argv else None
    doc = load(path)
    doc["_path"] = path
    st = stage_of(path)
    bad = check_constants(doc)
    if st == "1-sync":
        res = simulate_stage1(doc, backend, symbols)
        bad += compare_stage1(doc, res)
        what = (f"{res['symbols']} symbols, {len(res['heads'])} heads, {len(res['periods'])} periods, "
                f"{len(res['carrier'])} carrier updates; {res['cycles']} cycles in "
                f"{res['seconds']:.1f} s ({res['cycles'] / max(res['seconds'], 1e-9):.0f} cycles/s)")
    else:
        from . import stages

        what, more = stages.run(st, doc, backend, symbols)
        bad += more
    print(f"{path}: {what}; " + ("bit-exact" if not bad else f"{len(bad)} mismatches"))
    for b in bad[:20]:
        print("  " + b)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
