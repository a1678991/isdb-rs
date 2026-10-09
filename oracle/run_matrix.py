#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Differential PHY test matrix: Rust isdbt-phy <-> gr-isdbt oracle.

For every configuration:
  gr   : synthetic per-layer TS -> grisdbt_tx.py -> gr_<cfg>.cf32 -> rx_file
         (Rust RX) -> ts_compare per layer
  rust : same TS -> tx_file / rust_tx3 (Rust TX) -> rust_<cfg>.cf32 ->
         grisdbt_rx.py per layer (gr RX) -> ts_compare per layer
  loop : rust_<cfg>.cf32 -> rx_file (Rust loopback, sanity)
  iq   : iq_compare.py rust_<cfg>.cf32 gr_<cfg>.cf32 (carrier-level diff of the
         two modulators' outputs for identical input)

Run on the host from the repository root (gr-isdbt runs via `docker exec`
in a container built from oracle/Dockerfile, default name `isdb-oracle`
(env ISDB_RS_ORACLE_CONTAINER), with this repository mounted at /work):

  python3 oracle/run_matrix.py [CFG ...]          # default: all configs

Data goes to target/oracle-matrix/ (env ISDB_RS_ORACLE_OUT, which must lie
inside the repository so that the container sees it).  The Rust binaries
must be built first:
  cargo build --release -p isdbt-phy --examples
  cargo build --release --manifest-path oracle/rust_tx3/Cargo.toml --target-dir target/oracle
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
OUT = os.path.abspath(os.environ.get("ISDB_RS_ORACLE_OUT", os.path.join(REPO, "target", "oracle-matrix")))
CONTAINER = os.environ.get("ISDB_RS_ORACLE_CONTAINER", "isdb-oracle")
CONT = "/work/" + os.path.relpath(OUT, REPO)
ORC = "/work/" + os.path.relpath(HERE, REPO)
TX_FILE = os.path.join(REPO, "target", "release", "examples", "tx_file")
RX_FILE = os.path.join(REPO, "target", "release", "examples", "rx_file")
TX3 = os.path.join(REPO, "target", "oracle", "release", "rust_tx3")

TI_LEN = {1: [0, 4, 8, 16], 2: [0, 2, 4, 8], 3: [0, 1, 2, 4]}
BITS = {"qpsk": 2, "16qam": 4, "64qam": 6}
GR_CONST = {"qpsk": 4, "16qam": 16, "64qam": 64}
RATES = ["1/2", "2/3", "3/4", "5/6", "7/8"]

# name: (mode, gi, partial, [layer specs "segs,mod,rate,ti_index"], frames)
CONFIGS = {
    "m3g8p_q23_64q34": (3, 8, True, ["1,qpsk,2/3,2", "12,64qam,3/4,2"], 16),
    # = the gr-isdbt bench chain a consumer USB tuner (MyGica S270) decoded
    # with PER 0
    "m3g4p_bench": (3, 4, True, ["1,qpsk,1/2,3", "12,64qam,3/4,2"], 16),
    "m3g4p_q12_16q34": (3, 4, True, ["1,qpsk,1/2,3", "12,16qam,3/4,1"], 16),
    "m3g16_64q78_i0": (3, 16, False, ["13,64qam,7/8,0"], 10),
    "m2g8p_q56_64q12": (2, 8, True, ["1,qpsk,5/6,3", "12,64qam,1/2,0"], 24),
    "m1g32p_16q23_q78": (1, 32, True, ["1,16qam,2/3,3", "12,qpsk,7/8,2"], 40),
    "m3g8_3lay": (3, 8, False, ["3,qpsk,1/2,3", "5,16qam,2/3,2", "5,64qam,5/6,1"], 16),
    "m2g16p_3lay": (2, 16, True, ["1,qpsk,3/4,1", "4,16qam,1/2,3", "8,64qam,2/3,2"], 24),
}


def tsp_per_frame(mode, spec):
    segs, mod, rate, _ = spec.split(",")
    n, d = (int(x) for x in rate.split("/"))
    # Table 3-3: Mode 1 QPSK 1/2 = 12 TSPs per segment per frame.
    v = 12 * int(segs) * (BITS[mod] // 2) * 2 * n * 2 ** (mode - 1)
    assert v % d == 0
    return v // d


def gr_layer(mode, spec):
    segs, mod, rate, ti = spec.split(",")
    return f"{segs},{GR_CONST[mod]},{RATES.index(rate)},{TI_LEN[mode][int(ti)]}"


def make_ts(path, layer, count):
    """Synthetic TS: unique packets (PID 0x100+layer, packet index in payload)."""
    import random
    rnd = random.Random(1000 + layer)
    with open(path, "wb") as f:
        for k in range(count):
            hdr = bytes([0x47, 0x01, 0x00 + layer, 0x10 | (k & 15)])
            body = k.to_bytes(4, "big") + bytes(rnd.getrandbits(8) for _ in range(180))
            f.write(hdr + body)


def sh(cmd, log=None):
    r = subprocess.run(cmd, capture_output=True, text=True)
    if log:
        with open(log, "w") as f:
            f.write(r.stdout + r.stderr)
    if r.returncode not in (0, 1):
        print(r.stdout[-2000:], r.stderr[-2000:])
        raise SystemExit(f"command failed: {' '.join(cmd)}")
    return r.stdout + r.stderr


def docker(*args):
    return sh(["docker", "exec", "-w", CONT, CONTAINER, "python3", *args])


def compare(src, dec):
    return sh(["python3", os.path.join(HERE, "ts_compare.py"), src, dec]).strip()


def rx_summary(log):
    """valid frames, parity of valid frames, RS (clean, corr, uncorr) totals, MER min."""
    txt = open(log).read()
    lines = [l for l in txt.splitlines() if l.startswith("frame ") and "valid=true" in l]
    par = sorted({re.search(r"parity=(\w+)", l).group(1) for l in lines})
    tot = [[0, 0, 0] for _ in range(3)]
    mers = [[] for _ in range(3)]
    for l in lines:
        rs = re.search(r"rs=\[(.*)\]$", l).group(1)
        for i, m in enumerate(re.findall(r"Some\(\((\d+), (\d+), (\d+)\)\)|None", rs)):
            if m and m[0]:
                for j in range(3):
                    tot[i][j] += int(m[j])
        mer = re.search(r"mer=\[(.*?)\] cfo", l).group(1)
        for i, m in enumerate(re.findall(r"Some\(([\d.]+)\)|None", mer)):
            if m:
                mers[i].append(float(m))
    last = txt.strip().splitlines()[-1]
    return {"valid": len(lines), "parity": par, "rs": tot,
            "mer_min": [round(min(m), 1) if m else None for m in mers], "params": last}


def run(name):
    mode, gi, partial, specs, frames = CONFIGS[name]
    d = os.path.join(OUT, name)
    os.makedirs(d, exist_ok=True)
    print(f"== {name}: mode {mode} GI 1/{gi} partial={partial} layers {specs} frames {frames}")
    ts = []
    for i, s in enumerate(specs):
        p = os.path.join(d, f"in_{'abc'[i]}.ts")
        make_ts(p, i, tsp_per_frame(mode, s) * frames)
        ts.append(p)
    rel = lambda p: os.path.basename(p)
    # gr TX -> Rust RX
    gargs = [f"{ORC}/grisdbt_tx.py", "--out", f"{name}/gr.cf32", "--mode", str(mode), "--guard", str(gi)]
    if partial:
        gargs.append("--oneseg")
    for i, s in enumerate(specs):
        l = "abc"[i]
        gargs += [f"--{l}", gr_layer(mode, s), f"--{l}-ts", f"{name}/{rel(ts[i])}"]
    docker(*gargs)
    sh([RX_FILE, os.path.join(d, "gr.cf32"), os.path.join(d, "rrx_gr")], log=os.path.join(d, "rrx_gr.log"))
    res = {"gr->rust": rx_summary(os.path.join(d, "rrx_gr.log"))}
    res["gr->rust"]["cmp"] = [compare(ts[i], os.path.join(d, f"rrx_gr.{'abc'[i]}.ts")) for i in range(len(specs))]
    # Rust TX -> gr RX
    targs = [TX3, os.path.join(d, "rust.cf32"), str(frames), str(mode), str(gi), "1" if partial else "0"]
    for i, s in enumerate(specs):
        targs += [s, ts[i]]
    sh(targs)
    res["rust->gr"] = {"cmp": []}
    for i, s in enumerate(specs):
        l = "abc"[i]
        rargs = [f"{ORC}/grisdbt_rx.py", "--iq", f"{name}/rust.cf32", "--layer", l, "--out", f"{name}/grrx_rust.{l}.ts",
                 "--mode", str(mode), "--guard", str(gi)]
        if partial:
            rargs.append("--oneseg")
        for j, s2 in enumerate(specs):
            rargs += [f"--{'abc'[j]}", gr_layer(mode, s2)]
        out = docker(*rargs)
        res["rust->gr"].setdefault("tmcc", []).append(
            f"OK {out.count('TMCC OK')} / NOT OK {out.count('TMCC NOT OK')}")
        res["rust->gr"]["cmp"].append(compare(ts[i], os.path.join(d, f"grrx_rust.{l}.ts")))
    # Rust loopback
    sh([RX_FILE, os.path.join(d, "rust.cf32"), os.path.join(d, "rrx_rust")], log=os.path.join(d, "rrx_rust.log"))
    res["rust->rust"] = rx_summary(os.path.join(d, "rrx_rust.log"))
    res["rust->rust"]["cmp"] = [compare(ts[i], os.path.join(d, f"rrx_rust.{'abc'[i]}.ts")) for i in range(len(specs))]
    # carrier-level comparison of the two modulators
    segoff = []
    for s in specs:
        segs, _, _, ti = s.split(",")
        segoff += [0 if int(ti) == 0 else 1] * int(segs)
    skip = {1: 14, 2: 8, 3: 6}[mode]
    if mode == 3 and frames <= 10:
        skip = 3
    res["iq rust vs gr"] = {"report": sh(["python3", os.path.join(HERE, "iq_compare.py"), os.path.join(d, "rust.cf32"),
                                          os.path.join(d, "gr.cf32"), str(mode), str(gi), "--skip", str(skip),
                                          "--seg-offsets", ",".join(map(str, segoff))]).strip()}
    for k, v in res.items():
        print(f"  [{k}]")
        for kk, vv in v.items():
            if kk == "report":
                for c in vv.splitlines():
                    print(f"     {c}")
            elif kk == "cmp":
                for c in vv:
                    print(f"     {os.path.basename(c)}")
            else:
                print(f"     {kk}: {vv}")
    return res


def main():
    names = sys.argv[1:] or list(CONFIGS)
    for n in names:
        run(n)


if __name__ == "__main__":
    main()
