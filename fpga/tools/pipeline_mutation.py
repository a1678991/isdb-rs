"""The pipeline-delay mutation run (issue #30, item 2; one-off, opt-in: not
part of `mise run fpga:test`).

Each mutation shifts one pipeline delay of the chain's blocks or of the
LDPC decoder by one cycle, +1 or -1 (a delay line's length, the stage a
side field is taken from, a latency constant the schedule assumes), on a
scratch copy of the module's source (`monitors.source_patch`), and runs
the fast compiled tests of the stages it can affect on their vectors. The
table lists, per mutation, the compared vector fields (and the invariant
monitors, `monitor:<check>`) that noticed it, and those that did not: a
mutation nothing notices is a coverage gap (or a delay the design does not
depend on, e.g. a scheduler's slack). Each mutation runs in its own
process (a patched design builds its own compiled model, cached in
fpga/build/ by its RTLIL).

python tools/pipeline_mutation.py [--jobs N] [--only SUBSTRING] [--json OUT]
python tools/pipeline_mutation.py --list

Stages (the fields their comparisons name):
  1-sync     stage 1 + 2 on chain-1-sync, the whole set (`sim.compare_stage1`)
  2-phase    stage 3 on chain-2-phase (`stages.run_phase`)
  3-eq       stage 4 on chain-3-eq's frame (`stages.run_eq`)
  4-tmcc     stage 5 on chain-4-tmcc (`stages.run_tmcc`)
  5-demap    stage 6 (the back end) on chain-3/-5, the next frame a cycle
             after the last symbol (`stages.run_back(..., next_start=True)`)
  status     the status block's learned points (`Finish` with a point RAM:
             tests/test_monitors.py's bench, the words against the RAM)
  6-chain    the whole chain with the LDPC stub, frame 0 (2 260 000 symbols)
  ldpc       the decoder on the toy sets at step 34 (toy-a-34, toy-b-34)
"""

import concurrent.futures
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))
VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(HERE, "..", "build"))
TIMEOUT = 1200  # seconds per mutation (its builds and runs)

# (name, module, stages, edits): edits as [(old, new)], `old` once in the source
MUTATIONS = []


def mut(name, module, stages, plus, minus):
    MUTATIONS.append((f"{name} +1", module, stages, plus))
    MUTATIONS.append((f"{name} -1", module, stages, minus))


# stage 1: the FSync flag's delay line (`tf`, 5 stages) against the correlations
mut("sync: FSync flag delay line (tf, depth 5)", "isdb_fpga.chain.sync", ["1-sync"],
    [("        depth = 5\n        tf = Signal(depth)", "        depth = 6\n        tf = Signal(depth)")],
    [("        depth = 5\n        tf = Signal(depth)", "        depth = 4\n        tf = Signal(depth)")])
# stage 3: the gain taken a stage early / late against u, w; the phase table address
mut("phase: gain into the gain products (g4)", "isdb_fpga.chain.phase", ["2-phase"],
    [("m.d.sync += [yu.eq(u * g4), yw.eq(w * g4)]",
      "g4d = Signal.like(g4)\n        m.d.sync += g4d.eq(g4)\n        m.d.sync += [yu.eq(u * g4d), yw.eq(w * g4d)]")],
    [("m.d.sync += [yu.eq(u * g4), yw.eq(w * g4)]", "m.d.sync += [yu.eq(u * g3), yw.eq(w * g3)]")])
mut("phase: phi_j into the rotation table (phiB)", "isdb_fpga.chain.phase", ["2-phase"],
    [("idx = (phiB + (1 << 21))[22:32]",
      "phid = Signal(32)\n        m.d.sync += phid.eq(phiB)\n        idx = (phid + (1 << 21))[22:32]")],
    [("idx = (phiB + (1 << 21))[22:32]", "idx = (phiA + (1 << 21))[22:32]")])
# the 4x clock's phase (common.Phase4: resynchronised to 2 on the toggle)
mut("common: Phase4 phase", "isdb_fpga.chain.common", ["3-eq", "5-demap"],
    [("ph.eq(Mux(tq ^ tq2, 2, ph + 1))", "ph.eq(Mux(tq ^ tq2, 3, ph + 1))")],
    [("ph.eq(Mux(tq ^ tq2, 2, ph + 1))", "ph.eq(Mux(tq ^ tq2, 1, ph + 1))")])
# stage 4: z's delay to C+6, the valid/meta pipeline (depth 5)
mut("eq: z delay to C+6", "isdb_fpga.chain.eq", ["3-eq"],
    [("for d in range(3, 7):", "for d in range(3, 8):")],
    [("for d in range(3, 7):", "for d in range(3, 6):")])
mut("eq: valid/meta pipeline (depth 5)", "isdb_fpga.chain.eq", ["3-eq"],
    [("        depth = 5\n        vpipe", "        depth = 6\n        vpipe")],
    [("        depth = 5\n        vpipe", "        depth = 4\n        vpipe")])
# stage 5: the staging buffer's read-ahead
MUTATIONS.append(("tmcc: burst read address one ahead (r + 1 -> r) -1", "isdb_fpga.chain.tmcc", ["4-tmcc"],
                  [("Mux(take & (r != words - 1), r + 1, r)", "r")]))
# stage 6: the demapper's delays to the 64-product design's schedule
mut("demap: LLR delay to A+9 (llr_d)", "isdb_fpga.chain.demap", ["5-demap"],
    [("for d in range(5, 10):", "for d in range(5, 11):")],
    [("for d in range(5, 10):", "for d in range(5, 9):")])
mut("demap: minimum metric delay to A+8 (best_d)", "isdb_fpga.chain.demap", ["5-demap"],
    [("for d in range(4, 9):", "for d in range(4, 10):")],
    [("for d in range(4, 9):", "for d in range(4, 8):")])
mut("demap: |y|^2 delay to A+8 (zz_d)", "isdb_fpga.chain.demap", ["5-demap"],
    [("for d in range(2, 9):", "for d in range(2, 10):")],
    [("for d in range(2, 9):", "for d in range(2, 8):")])
mut("demap: beat context (ctx9 = cp[8])", "isdb_fpga.chain.demap", ["5-demap"],
    [("for d in range(2, 10):", "for d in range(2, 11):"), ("v9, ctx9 = vp[8], cp[8]", "v9, ctx9 = vp[9], cp[9]")],
    [("v9, ctx9 = vp[8], cp[8]", "v9, ctx9 = vp[7], cp[7]")])
mut("demap: noise sample context (ctx8 = cp[7])", "isdb_fpga.chain.demap", ["5-demap"],
    [("v8, ctx8 = vp[7], cp[7]", "v8, ctx8 = vp[8], cp[8]")],
    [("v8, ctx8 = vp[7], cp[7]", "v8, ctx8 = vp[6], cp[6]")])
# the back end: the frame end's drain delay (f_end, 12), the abort's wait
mut("back: frame end drain (end_d, 12)", "isdb_fpga.chain.back", ["5-demap"],
    [("end_d = Signal(12)", "end_d = Signal(13)")],
    [("end_d = Signal(12)", "end_d = Signal(11)")])
mut("back: abort beat wait (ABORT_WAIT 12)", "isdb_fpga.chain.back", ["5-demap"],
    [("ABORT_WAIT = 12", "ABORT_WAIT = 13")],
    [("ABORT_WAIT = 12", "ABORT_WAIT = 11")])
# stage 8/9: the status words' pacing after `next` (d53de60's)
mut("finish: status word pacing (st_pace 2)", "isdb_fpga.chain.finish", ["status", "6-chain"],
    [("with m.If(st_pace != 2):", "with m.If(st_pace != 3):")],
    [("with m.If(st_pace != 2):", "with m.If(st_pace != 1):")])
# the LDPC decoder: the write latency and decision wait the schedule assumes,
# the output RAM's read latency
mut("ldpc: WRITE_LATENCY (16)", "isdb_fpga.ldpc", ["ldpc"],
    [("WRITE_LATENCY = 16\n", "WRITE_LATENCY = 17\n")],
    [("WRITE_LATENCY = 16\n", "WRITE_LATENCY = 15\n")])
mut("ldpc: DECIDE (19)", "isdb_fpga.ldpc", ["ldpc"],
    [("DECIDE = 19\n", "DECIDE = 20\n")],
    [("DECIDE = 19\n", "DECIDE = 18\n")])
mut("ldpc: output read latency (OLAT 4)", "isdb_fpga.ldpc", ["ldpc"],
    [("OLAT = 4\n", "OLAT = 5\n")],
    [("OLAT = 4\n", "OLAT = 3\n")])


def doc(name):
    from isdb_fpga.chain.sim import load

    d = load(os.path.join(VECTORS, f"chain-{name}.json"))
    d["_path"] = os.path.join(VECTORS, f"chain-{name}.json")
    return d


FIELD_RE = [
    (r"^monitor \S+\.(\w+) ", lambda mt: f"monitor:{mt.group(1)}"),
    (r"^slot \d+ beat \(positions, LLRs\)", "beat (positions, LLRs)"),
    (r"^slot \d+: the de-interleaved word", "de-interleaved word"),
    (r"^slot \d+: \d+ beats", "codeword beats/flags"),
    (r"^slot \d+: tag", "codeword tag"),
    (r"^codewords of slots", "codeword set"),
    (r"^K late", "K in time (late)"),
    (r"^next n0, K", "next n0, K"),
    (r"^\d+ restarts", "restarts"),
    (r"^Slots word \d+", "Slots words"),
    (r"^block \d+ \(kind", "Slots blocks"),
    (r"^\d+ Slots words", "Slots word count"),
    (r"^record \d+ not accepted", "stall"),
]


def field_of(msg):
    for pat, name in FIELD_RE:
        mt = re.match(pat, msg)
        if mt:
            return name(mt) if callable(name) else name
    # "<field> <key>: got ..." / "<field>: n records": the words before the key
    mt = re.match(r"^([a-zA-Z_ ,/()]+?)(?: (?:symbol|at|#|entry|position|frame symbol|label|value|bit|beat|"
                  r"TMCC bit|slot)\b.*)?(?::|$)", msg)
    return mt.group(1).strip() if mt else msg[:40]


def run_stage(stage, backend):
    """The stage's comparison: (the fields it compares, mismatch messages)."""
    from isdb_fpga.chain import stages

    if stage == "1-sync":
        from isdb_fpga.chain.sim import compare_stage1, simulate_stage1

        d = doc("1-sync")
        res = simulate_stage1(d, backend)
        return ["r", "nco_phase", "per_symbol", "period", "head", "carrier", "restarts"], compare_stage1(d, res)
    if stage == "2-phase":
        return ["y", "r"], stages.run_phase(doc("2-phase"), backend)[1]
    if stage == "3-eq":
        return (["z", "training ref", "sums word", "sums frame/count"],
                stages.run_eq(doc("3-eq"), doc("4-tmcc")["layout"], backend)[1])
    if stage == "4-tmcc":
        return (["soft", "burst", "burst tags", "tmcc bits", "tmcc result", "layout", "layout commit",
                 "staging overrun"], stages.run_tmcc(doc("4-tmcc"), backend)[1])
    if stage == "5-demap":
        return (["codeword set", "codeword beats/flags", "codeword tag", "beat (positions, LLRs)",
                 "de-interleaved word", "learned point", "next n0, K", "TMCC soft", "K in time (late)"],
                stages.run_back(doc("3-eq"), doc("5-demap"), doc("4-tmcc"), backend, None, True)[1])
    if stage == "status":
        sys.path.insert(0, os.path.join(HERE, "..", "tests"))
        from test_monitors import STATUS_RECS, FinishMonitorBench

        from isdb_fpga.chain import slots as S

        b = FinishMonitorBench()
        out, bad = b.run(STATUS_RECS, "mutstatus")
        bl = S.blocks(out)
        want = [v & 0xFFFF for p in FinishMonitorBench.PTS for v in p]
        st = [x for x in bl if x[0] == S.STATUS]
        if not st or st[0][3][47:51] != [0, 0, 0, 16]:
            bad.append("status counts: wrong")
        elif st[0][3][51:83] != want:
            bad.append("status learned points: differ")
        return ["status counts", "status learned points"], bad
    if stage == "6-chain":
        return (["Slots words", "Slots blocks", "Slots word count"],
                stages.run_chain(doc("6-chain"), doc("3-eq")["taps"], backend, 2_260_000)[1])
    if stage == "ldpc":
        from isdb_fpga import ldpcmodel, simldpc
        from isdb_fpga.chain.monitors import MonitorError

        bad = []
        for name in ("toy-a-34", "toy-b-34"):
            d = ldpcmodel.vectors(name)
            try:
                res = simldpc.simulate(d, backend=backend)
            except MonitorError as err:
                bad += [f"{x}" for x in str(err).split("; ")]
                continue
            for x in simldpc.compare(d, res):
                # "codeword i: bits: n differ ..., iterations a != b, ..." or "...: an overrun record"
                for part in x.split(": ", 1)[1].split(", "):
                    f = "overrun" if "overrun" in part else part.split(":")[0].split(" ")[0]
                    if f in ("bits", "iterations", "converged", "unsatisfied", "overrun"):
                        bad.append(f"{f}: {name} {x}")
        return ["bits", "iterations", "converged", "unsatisfied", "overrun"], bad
    raise ValueError(stage)


def one(index, backend):
    from isdb_fpga.chain.monitors import source_patch

    name, module, stages_, edits = MUTATIONS[index] if index >= 0 else ("none", None, None, [])
    out = {"mutation": name, "stages": {}}
    t0 = time.time()
    if index < 0:
        stages_ = sorted({s for m in MUTATIONS for s in m[2]})
    for st in stages_:
        try:
            if module:
                with source_patch(module, edits):
                    fields, bad = run_stage(st, backend)
            else:
                fields, bad = run_stage(st, backend)
        except Exception as err:  # a mutation that breaks elaboration or the run
            fields, bad = [], [f"error: {type(err).__name__}: {str(err)[:200]}"]
        noticed = sorted({field_of(b) for b in bad})
        out["stages"][st] = {"fields": fields, "noticed": noticed, "messages": bad[:6]}
    out["seconds"] = round(time.time() - t0)
    return out


def table(results):
    lines = ["| Mutation | Stage tests | Noticed by | Not noticed by |", "|---|---|---|---|"]
    for r in results:
        for st, v in r["stages"].items():
            seen = v["noticed"]
            missed = [f for f in v["fields"] if f not in seen]
            lines.append(f"| {r['mutation']} | {st} | {', '.join(seen) or '**nothing**'} | "
                         f"{', '.join(missed)} |")
    return "\n".join(lines)


def main(argv):
    if "--list" in argv:
        for i, (name, module, st, _e) in enumerate(MUTATIONS):
            print(f"{i:2} {name} ({module}; {', '.join(st)})")
        return 0
    from isdb_fpga.chain import simx

    backend = simx.compiled_backend()
    if backend is None:
        print("the mutation run needs a compiled backend (Verilator or CXXRTL)")
        return 2
    if "--one" in argv:
        print(json.dumps(one(int(argv[argv.index("--one") + 1]), backend)))
        return 0
    jobs = int(argv[argv.index("--jobs") + 1]) if "--jobs" in argv else max(1, (os.cpu_count() or 2) // 4)
    only = argv[argv.index("--only") + 1] if "--only" in argv else None
    idx = [-1] + [i for i, m in enumerate(MUTATIONS) if only is None or only in m[0]]

    def run(i):
        name = MUTATIONS[i][0] if i >= 0 else "none"
        try:
            r = subprocess.run([sys.executable, os.path.abspath(__file__), "--one", str(i)], capture_output=True,
                               text=True, timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            # a design that never finishes the run (e.g. a decoder that never
            # streams a result out): noticed, as a hang
            return {"mutation": name, "stages": {", ".join(MUTATIONS[i][2]): {
                "fields": [], "noticed": [f"hang (no end within {TIMEOUT} s)"], "messages": []}}}
        try:
            return json.loads(r.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError):
            return {"mutation": name, "stages": {"?": {"fields": [], "noticed": ["error"],
                                                        "messages": [r.stderr[-500:]]}}}

    t0 = time.time()
    with concurrent.futures.ThreadPoolExecutor(jobs) as ex:
        results = list(ex.map(run, idx))
    base = results.pop(0)
    clean = all(not v["noticed"] for v in base["stages"].values())
    print(f"unmutated: {'every stage clean' if clean else base}")
    print(table(results))
    print(f"{len(results)} mutations in {time.time() - t0:.0f} s ({backend}, {jobs} jobs)")
    if "--json" in argv:
        with open(argv[argv.index("--json") + 1], "w") as f:
            json.dump({"base": base, "results": results}, f, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
