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


def decoder_for(doc, all_rates=False, tables=None):
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
    dut = LdpcDecoder(s, codes, ngroups=ng, shortened=shortened)
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


def simulate(doc, limit=None, all_rates=False, fill_known=False, log=None, back_to_back=True):
    """Run the codewords through one decoder instance (loaded and started
    back to back, the next codeword loading while one decodes). Returns
    per codeword {"bits", "iterations", "converged", "unsatisfied",
    "cycles", "passes"}."""
    dut, ridx = decoder_for(doc, all_rates)
    words = doc["codewords"][:limit]
    order = output_order(dut, doc)
    n = doc["n"]
    short = doc.get("shortened")
    known = set()
    if short and fill_known:
        code_k = doc["k"]
        s0, sl = short["info_start"], short["info_len"]
        known = {i for i in range(code_k) if not s0 <= i < s0 + sl}
        for i, cw in enumerate(words):
            bad = [j for j in known if cw["llr"][j] != 127]
            if bad:
                raise ValueError(f"codeword {i}: known zeros not +127 at {bad[:4]}")
    sim = Simulator(dut)
    sim.add_clock(5e-9)
    results = [None] * len(words)
    state = {"clock": 0}

    async def loader(ctx):
        for i, cw in enumerate(words):
            while not ctx.get(dut.in_ready):
                await ctx.tick()
            ctx.set(dut.in_rate, ridx)
            for pos, v in enumerate(cw["llr"]):
                if pos in known:
                    continue
                ctx.set(dut.in_valid, 1)
                ctx.set(dut.in_pos, pos)
                ctx.set(dut.in_llr, v)
                await ctx.tick()
                while not ctx.get(dut.in_ready):
                    ctx.set(dut.in_valid, 0)
                    await ctx.tick()
            ctx.set(dut.in_valid, 0)
            while not ctx.get(dut.start_ready):
                await ctx.tick()
            ctx.set(dut.start, 1)
            ctx.set(dut.st_rate, ridx)
            ctx.set(dut.st_max_iter, doc["max_iter"])
            ctx.set(dut.st_offset, doc["offset"])
            ctx.set(dut.st_short, int(bool(known)))
            await ctx.tick()
            ctx.set(dut.start, 0)
            if not back_to_back:
                while results[i] is None:
                    await ctx.tick()

    async def reader(ctx):
        ctx.set(dut.o_ready, 1)
        for i in range(len(words)):
            t0 = time.time()
            while not ctx.get(dut.res_valid):
                await ctx.tick()
            res = {"iterations": ctx.get(dut.res_iterations),
                   "converged": bool(ctx.get(dut.res_converged)),
                   "unsatisfied": ctx.get(dut.res_unsat),
                   "cycles": ctx.get(dut.cycles), "passes": ctx.get(dut.passes)}
            stream = []
            while True:
                await ctx.tick()
                if ctx.get(dut.o_valid):
                    stream.append(ctx.get(dut.o_data))
                    if ctx.get(dut.o_last):
                        break
            bits = [0] * n
            for w, word in enumerate(stream):
                for b in range(16):
                    idx = 16 * w + b
                    if idx < n:
                        bits[order[idx]] = (word >> b) & 1
            res["bits"] = bits
            res["seconds"] = time.time() - t0
            results[i] = res
            if log:
                log(i, res)
        await ctx.tick()

    sim.add_testbench(loader, background=True)
    sim.add_testbench(reader)
    sim.run()
    del state
    return results


def compare(doc, results):
    """Mismatch messages (empty when bit-exact)."""
    out = []
    n = doc["n"]
    for i, (cw, res) in enumerate(zip(doc["codewords"], results)):
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
