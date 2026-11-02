"""Simulate the LDPC decoder (`LdpcDecoder`) on LDPC vectors files and
compare every codeword's n hard decisions, iterations, convergence and
unsatisfied count bit for bit (and, with a trace in the file, name the
first iteration whose decisions the result matches, to locate a
mismatch).

Vectors: the Rust model's `ldpc-*.json` (`isdbs3-phy` `examples/
ldpcvectors`) or the Python model's `ldpcvec-py-*.json` (`python -m
isdb_fpga.ldpcmodel`); the decoder is built for the file's code (a toy
code's "rows", or the B44 rate) with S = the file's step.

python -m isdb_fpga.simldpc VECTORS.json [--max N] [--all-rates] [--fill-known]

--all-rates builds the decoder with all eleven rates' ROMs (slower to
elaborate; the default is the file's rate only). --fill-known loads only
the sent positions of a shortened (TMCC) codeword and lets the decoder
write the known zeros (+127) itself; the file's values there are checked
to be +127 first.
"""

import json
import sys
import time

from amaranth.sim import Simulator

from . import ldpcmodel
from .ldpc import LdpcDecoder


def load(path):
    with open(path) as f:
        return json.load(f)


def is_ldpc(doc):
    return doc.get("format") == "isdb-rs ldpc vectors"


def decoder_for(doc, all_rates=False, tables=None, banks=3):
    """(decoder, rate index) for a vectors file."""
    sched = doc["schedule"]
    if sched["kind"] != "layered":
        raise ValueError("the HDL runs the layered schedule only")
    s = sched["step"]
    if "rows" in doc:
        codes = {doc["rate"]: doc["rows"]}
        ng = doc["n"] // ldpcmodel.GROUP
    else:
        tables = tables or ldpcmodel.load_tables()
        codes = dict(tables) if all_rates else {doc["rate"]: tables[doc["rate"]]}
        ng = doc["n"] // ldpcmodel.GROUP
    short = doc.get("shortened")
    shortened = (short["info_start"], short["info_len"]) if short else ldpcmodel.TMCC_SHORT
    dut = LdpcDecoder(s, codes, ngroups=ng, shortened=shortened, banks=banks)
    return dut, sorted(codes).index(doc["rate"])


def output_order(dut, doc):
    """The natural codeword index of each output bit: the NG groups in
    order, 374 bits each; information groups as stored, parity group G + l
    bit i = p_{l + i q}."""
    k, q, n = doc["k"], doc["q"], doc["n"]
    G = k // ldpcmodel.GROUP
    out = []
    for g in range(n // ldpcmodel.GROUP):
        for j in range(ldpcmodel.GROUP):
            out.append(g * ldpcmodel.GROUP + j if g < G else k + (g - G) + j * q)
    return out


def default_plan(count):
    """Codeword i: wait for `load_ready`, begin, all its LLRs, start (context
    0, one after the other: the next loads while earlier ones decode)."""
    out = []
    for i in range(count):
        out += [("begin", i, 0, True), ("llr", i, 0, 0, None), ("start", i, 0)]
    return out


def simulate(doc, limit=None, all_rates=False, fill_known=False, log=None, banks=3, plan=None,
             max_iter=None):
    """Run the codewords through one decoder instance by `plan` (default
    `default_plan`): a list of ("begin", i, ctx, wait_for_load_ready),
    ("llr", i, ctx, lo, hi) (codeword i's LLRs at positions lo .. hi - 1,
    one per cycle; hi None: to the end) and ("start", i, ctx). Returns the
    result records in the order they leave: {"bits" (None for an overrun
    record), "iterations", "converged", "unsatisfied", "overrun",
    "cycles", "passes", "seconds"}; the last also has "overruns" and
    "queue_overflow"."""
    dut, ridx = decoder_for(doc, all_rates, banks=banks)
    words = doc["codewords"][:limit]
    plan = default_plan(len(words)) if plan is None else plan
    commits = sum(1 for op in plan if op[0] == "start")
    order = output_order(dut, doc)
    n = doc["n"]
    short = doc.get("shortened")
    known = set()
    if short and fill_known:
        s0, sl = short["info_start"], short["info_len"]
        known = {i for i in range(doc["k"]) if not s0 <= i < s0 + sl}
        for i, cw in enumerate(words):
            bad = [j for j in known if cw["llr"][j] != 127]
            if bad:
                raise ValueError(f"codeword {i}: known zeros not +127 at {bad[:4]}")
    sim = Simulator(dut)
    sim.add_clock(5e-9)
    results = []

    async def loader(ctx):
        ctx.set(dut.in_rate, ridx)
        for op in plan:
            if op[0] == "begin":
                _, i, c, wait = op
                while wait and not ctx.get(dut.load_ready):
                    await ctx.tick()
                ctx.set(dut.begin, 1)
                ctx.set(dut.in_ctx, c)
                await ctx.tick()
                ctx.set(dut.begin, 0)
            elif op[0] == "llr":
                _, i, c, lo, hi = op
                llr = words[i]["llr"]
                ctx.set(dut.in_ctx, c)
                for pos in range(lo, len(llr) if hi is None else hi):
                    if pos in known:
                        continue
                    ctx.set(dut.in_valid, 1)
                    ctx.set(dut.in_pos, pos)
                    ctx.set(dut.in_llr, llr[pos])
                    await ctx.tick()
                ctx.set(dut.in_valid, 0)
            else:
                _, i, c = op
                ctx.set(dut.start, 1)
                ctx.set(dut.in_ctx, c)
                ctx.set(dut.st_rate, ridx)
                ctx.set(dut.st_max_iter, doc["max_iter"] if max_iter is None else max_iter)
                ctx.set(dut.st_offset, doc["offset"])
                ctx.set(dut.st_short, int(bool(known)))
                await ctx.tick()
                ctx.set(dut.start, 0)

    async def reader(ctx):
        ctx.set(dut.o_ready, 1)
        for i in range(commits):
            t0 = time.time()
            while not ctx.get(dut.res_valid):
                await ctx.tick()
            res = {"iterations": ctx.get(dut.res_iterations),
                   "converged": bool(ctx.get(dut.res_converged)),
                   "unsatisfied": ctx.get(dut.res_unsat),
                   "overrun": bool(ctx.get(dut.res_overrun)),
                   "cycles": ctx.get(dut.cycles), "passes": ctx.get(dut.passes), "bits": None}
            await ctx.tick()
            if not res["overrun"]:
                stream = []
                while True:
                    if ctx.get(dut.o_valid):
                        stream.append(ctx.get(dut.o_data))
                        if ctx.get(dut.o_last):
                            await ctx.tick()
                            break
                    await ctx.tick()
                bits = [0] * n
                for w, word in enumerate(stream):
                    for b in range(16):
                        idx = 16 * w + b
                        if idx < n:
                            bits[order[idx]] = (word >> b) & 1
                res["bits"] = bits
            res["seconds"] = time.time() - t0
            results.append(res)
            if log:
                log(i, res)
        await ctx.tick()
        results[-1]["overruns"] = ctx.get(dut.overruns)
        results[-1]["queue_overflow"] = ctx.get(dut.queue_overflow)

    sim.add_testbench(loader, background=True)
    sim.add_testbench(reader)
    sim.run()
    return results


def compare(doc, results):
    """Mismatch messages (empty when bit-exact)."""
    out = []
    n = doc["n"]
    for i, (cw, res) in enumerate(zip(doc["codewords"], results)):
        if res["overrun"]:
            out.append(f"codeword {i}: an overrun record")
            continue
        want_bits = ldpcmodel.unpack(cw["bits_hex"], n)
        bad = [f for f in ("iterations", "converged", "unsatisfied") if res[f] != cw[f]]
        if res["bits"] != want_bits:
            diff = [j for j in range(n) if res["bits"][j] != want_bits[j]]
            msg = f"bits: {len(diff)} differ, first at {diff[0]}"
            if "trace_hex" in cw:
                same = [it + 1 for it, h in enumerate(cw["trace_hex"])
                        if ldpcmodel.unpack(h, n) == res["bits"]]
                msg += f"; the result equals the trace after iterations {same}" if same else \
                    "; the result is none of the traced iterations"
            bad.append(msg)
        if bad:
            out.append(f"codeword {i}: " + ", ".join(
                b if b.startswith("bits") else f"{b} {res[b]} != {cw[b]}" for b in bad))
    return out


def main(argv):
    limit = int(argv[argv.index("--max") + 1]) if "--max" in argv else None
    paths = [a for i, a in enumerate(argv[1:], 1)
             if not a.startswith("--") and argv[i - 1] != "--max"]
    failed = 0
    for path in paths:
        doc = load(path)
        t0 = time.time()

        def log(i, res):
            print(f"  codeword {i}: iterations {res['iterations']} converged {res['converged']} "
                  f"unsatisfied {res['unsatisfied']}, {res['passes']} passes, "
                  f"{res['cycles']} cycles, {res['seconds']:.1f} s", flush=True)

        print(f"{path}: rate {doc['rate']}, {doc['schedule']}, limit {doc['max_iter']}",
              flush=True)
        results = simulate(doc, limit, "--all-rates" in argv, "--fill-known" in argv, log=log)
        bad = compare(doc, results)
        print(f"{path}: {len(results)} codewords in {time.time() - t0:.0f} s; "
              + ("bit-exact" if not bad else f"{len(bad)} mismatches"), flush=True)
        for b in bad:
            print("  " + b)
        failed += bool(bad)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
