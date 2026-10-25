"""Simulate the front end on test vectors from the Rust model
(`cargo run -p isdb-frontend --example vectors -- OUT.json ...`) and compare
every symbol's internals and the output words.

python -m isdb_fpga.sim VECTORS.json [--jitter] [--load-port]
"""

import json
import random
import sys

from amaranth.sim import Simulator

from .control import ID_MAGIC
from .frontend import CYCLES, FORMAT_VERSION, SymbolFrontend
from .top import SymbolsRx

FIELDS = ("phase", "y", "dy", "yk", "gain", "period", "z", "dz", "zk", "q", "e", "err")


def load(path):
    with open(path) as f:
        return json.load(f)


def simulate(doc, jitter=False, load_port=False, seed=1):
    """Run the core on `doc`'s input; returns (symbols, words) as lists in
    the JSON's layout. `jitter` spaces samples 4..7 cycles apart at random
    (the ADC strobe is regular; the result must not depend on it).
    `load_port` loads the coefficients through the core's load port
    instead of the memories' initial contents."""
    p = doc["params"]
    taps = p["taps"]
    if load_port:
        dut = SymbolFrontend(taps, doc["cos_table"])
    else:
        dut = SymbolFrontend(taps, doc["cos_table"], p["h"], p["g"], p["k"])
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    syms, words = [], []
    rng = random.Random(seed)

    def sample(ctx):
        if ctx.get(dut.s_valid):
            syms.append({
                "phase": ctx.get(dut.s_phase),
                "y": [ctx.get(s) for s in dut.s_y],
                "dy": [ctx.get(s) for s in dut.s_dy],
                "yk": [ctx.get(s) for s in dut.s_yk],
                "gain": ctx.get(dut.s_gain),
                "period": ctx.get(dut.s_period),
                "z": [ctx.get(s) for s in dut.s_z],
                "dz": [ctx.get(s) for s in dut.s_dz],
                "zk": [ctx.get(s) for s in dut.s_zk],
                "q": [ctx.get(s) for s in dut.s_q],
                "e": ctx.get(dut.s_e),
                "err": ctx.get(dut.s_err),
            })
        if ctx.get(dut.o_valid):
            words.append(ctx.get(dut.o_word))

    async def bench(ctx):
        async def tick():
            await ctx.tick()
            sample(ctx)

        for name, sig in dut.regs.fields().items():
            val = p[name]
            ctx.set(sig, val & ((1 << len(sig)) - 1) if not sig.shape().signed else val)
        if load_port:
            for ph in range(len(p["h"])):
                for t in range(taps):
                    ctx.set(dut.c_we_h, 1)
                    ctx.set(dut.c_we_g, 1)
                    ctx.set(dut.c_we_k, 1)
                    ctx.set(dut.c_phase, ph)
                    ctx.set(dut.c_tap, t)
                    ctx.set(dut.c_h, p["h"][ph][t])
                    ctx.set(dut.c_g, p["g"][ph][t])
                    ctx.set(dut.c_k, p["k"][ph][t])
                    await tick()
            ctx.set(dut.c_we_h, 0)
            ctx.set(dut.c_we_g, 0)
            ctx.set(dut.c_we_k, 0)
        ctx.set(dut.start, 1)
        await tick()
        ctx.set(dut.start, 0)
        for (i, q) in doc["input"]:
            ctx.set(dut.i_valid, 1)
            ctx.set(dut.i_i, i)
            ctx.set(dut.i_q, q)
            await tick()
            ctx.set(dut.i_valid, 0)
            gap = CYCLES + (rng.randrange(4) if jitter else 0)
            for _ in range(gap - 1):
                await tick()
        for _ in range(60):
            await tick()

    sim.add_testbench(bench)
    sim.run()
    return syms, words


def simulate_top(doc, gap=6):
    """Configure `SymbolsRx` through gpio_out with the host's command words
    (`doc["commands"]`, encoded by `isdb_frontend::control::Encoder`),
    check the ID word and a register readback, then run the input and
    collect the words at the packer port. Returns (words, problems)."""
    p = doc["params"]
    dut = SymbolsRx(p["taps"], doc["cos_table"])
    sim = Simulator(dut)
    sim.add_clock(1e-8)
    words, problems = [], []

    async def bench(ctx):
        async def tick(n=1):
            for _ in range(n):
                await ctx.tick()
                if ctx.get(dut.pack_valid):
                    words.append(ctx.get(dut.pack_data0))

        await tick(4)
        ident = ctx.get(dut.gpio_in)
        expect = ID_MAGIC << 16 | FORMAT_VERSION << 12 | p["taps"]
        if ident != expect:
            problems.append(f"ID word {ident:#010x} != {expect:#010x}")
        cmds = doc["commands"]
        last = 0
        for w in cmds[:-2]:  # configuration (without CTRL start/run)
            ctx.set(dut.gpio_out, w)
            last = w
            await tick(gap)
        # READ kp: ADDR 5 then READ, with the toggle continuing.
        t = (last >> 31) ^ 1
        for op, arg in ((1, 5), (4, 0)):
            ctx.set(dut.gpio_out, t << 31 | op << 28 | arg << 1)
            t ^= 1
            await tick(gap)
        kp = ctx.get(dut.gpio_in)
        if kp != p["kp"] & 0xFFFFFFFF:
            problems.append(f"READ kp {kp:#x} != {p['kp'] & 0xFFFFFFFF:#x}")
        # The host's last two commands toggle from its own count; re-encode
        # them with ours (op and argument unchanged).
        for w in cmds[-2:]:
            ctx.set(dut.gpio_out, t << 31 | (w & 0x7FFFFFFF))
            t ^= 1
            await tick(gap)
        for (i, q) in doc["input"]:
            ctx.set(dut.adc_valid, 1)
            ctx.set(dut.adc_i, i)
            ctx.set(dut.adc_q, q)
            await tick()
            ctx.set(dut.adc_valid, 0)
            await tick(CYCLES - 1)
        await tick(60)

    sim.add_testbench(bench)
    sim.run()
    return words, problems


def compare(doc, syms, words):
    """Mismatch messages (empty when bit-exact)."""
    out = []
    exp = doc["symbols"]
    if len(syms) != len(exp):
        out.append(f"{len(syms)} symbols, {len(exp)} expected")
    for k, (a, b) in enumerate(zip(syms, exp)):
        bad = [f for f in FIELDS if a[f] != b[f]]
        if bad:
            out.append(f"symbol {k}: " + ", ".join(f"{f} {a[f]} != {b[f]}" for f in bad))
            if len(out) > 10:
                break
    ew = doc["words"]
    # The model emits a block's words as soon as its symbols exist; the
    # core can hold the last few in its packer at the end of the input.
    n = min(len(words), len(ew))
    if len(ew) - len(words) not in range(0, 3):
        out.append(f"{len(words)} words, {len(ew)} expected")
    for i in range(n):
        if words[i] != ew[i]:
            out.append(f"word {i}: {words[i]:#06x} != {ew[i]:#06x}")
            break
    return out


def main(argv):
    path = argv[1]
    doc = load(path)
    if "--via-gpio" in argv:
        words, bad = simulate_top(doc)
        ew = doc["words"]
        if len(ew) - len(words) not in range(0, 3):
            bad.append(f"{len(words)} words, {len(ew)} expected")
        diff = next((i for i, (a, b) in enumerate(zip(words, ew)) if a != b), None)
        if diff is not None:
            bad.append(f"word {diff}: {words[diff]:#06x} != {ew[diff]:#06x}")
        print(f"{path} via gpio_out: {len(doc['commands'])} commands, {len(words)} words; "
              + ("bit-exact" if not bad else f"{len(bad)} problems"))
        for b in bad:
            print("  " + b)
        return 1 if bad else 0
    syms, words = simulate(doc, jitter="--jitter" in argv, load_port="--load-port" in argv)
    bad = compare(doc, syms, words)
    print(f"{path}: {len(syms)} symbols, {len(words)} words; "
          + ("bit-exact" if not bad else f"{len(bad)} mismatches"))
    for b in bad:
        print("  " + b)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
