"""`SlotsRx`'s registers against their one source, `isdb_fpga/chain/registers.toml`
(issue #30 item 5; `isdb_fpga.chain.regmap` loads it).

Static checks (no simulation): the map is well formed; the two tables
generated from it (`top.py`'s docstring, fpga/README.md "Register map") are
current (`tools/regmap.py --check`); `top.py`'s REG_* constants, FEATURES,
LAYOUT_LAG, NONE, EQ_APPLY_FRAMES, the `Slots` version and MAX_ITER's reset
and `control.py`'s command protocol, CTRL bits and front-end registers are
the map's; every register has at least one simulated check derived from its
entry (printed as a table; a "staged" or "strobe" register added without a
check here fails).

Simulated checks (pysim, `SlotsRx` without the decoder, through `gpio_out`
with the map's command timing; each class is one simulation, a subtest per
register):

- `RegisterResetTest`: every address of the window read once after
  configuration: each register's documented reset, 0 on every unmapped
  address;
- `RegisterWriteSweepTest`: all ones written to every read-only register
  and unmapped address, then every address read again; then all zeros,
  then every address whose reset is not 0: nothing changed (read-only
  registers ignore writes; no write aliases into another register);
- `RegisterBehaviourTest` (with the invariant monitors): walking ones and
  zeros through every "rw" register (the bits above its width read 0), a
  field that reads another register (FEATURES' MAX_ITER) following it;
  CARRIER and TAPS ("staged") reading the value in use until CTRL START
  loads them (the taps only with INIT bit 0); TAPS_FROM arming the
  equaliser with the frame written and that frame + 2 in the clock of its
  write (5f60649), its read unchanged; the SUMS_CAPTURE strobe (any value
  arms, START releases); a sum register's READ answered `answer_after`
  clocks after its decode with the previous value held until then (the
  arrays' first and last elements; the address after each answers at once,
  0); FEATURES' flags; the ID word's chain version; CTRL SLOTS in the front
  end's STATUS. About 30 s of pysim over the three classes.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "tools"))

import regmap as regmap_tool  # noqa: E402  (tools/regmap.py)

from isdb_fpga.chain import regmap  # noqa: E402

MAP = regmap.load()
P = MAP.protocol
OPS = P["ops"]
ALL = 0xFFFFFFFF


def s32(v):
    v &= ALL
    return v - (1 << 32) if v & 0x80000000 else v


def walking(width):
    """Walking ones and walking zeros over `width` bits, then all ones and 0."""
    mask = (1 << width) - 1
    return [1 << b for b in range(width)] + [mask ^ (1 << b) for b in range(width)] + [ALL, 0]


# ---- static -----------------------------------------------------------------

def checks_for(r):
    """The simulated checks a register gets from its map entry (the classes
    below run them)."""
    out = ["reset"]
    if r.access == "ro":
        out.append("ignores writes")
    elif r.access == "rw":
        out.append("walking ones and zeros")
    elif r.access == "staged" and r.name in STAGED:
        out.append(f"staged: {STAGED[r.name][0]}")
    elif r.access == "strobe" and r.name in STROBE:
        out.append(f"strobe: {STROBE[r.name][0]}")
    if r.answer_after:
        out.append(f"answered after {r.answer_after} clocks")
    out += [f"field {f.name} reads {f.source}" for f in r.fields if f.source]
    if r.name == "FEATURES":
        out.append("flags are the built features")
    if r.name == "VERSION":
        out.append("the ID word's chain version")
    return out


class RegisterMapTest(unittest.TestCase):
    """The map and the copies derived from it or checked against it."""

    def test_the_map_is_well_formed(self):
        self.assertEqual(regmap.problems(MAP), [])

    def test_the_generated_tables_are_current(self):
        stale = [os.path.relpath(p, regmap_tool.FPGA) for p, _, _ in regmap_tool.stale(MAP)]
        self.assertEqual(stale, [], "run `uv run --locked python tools/regmap.py`")

    def test_top_constants_are_the_maps(self):
        from isdb_fpga import ldpc
        from isdb_fpga.chain import top
        from isdb_fpga.chain.slots import SLOTS_VERSION

        consts = {n[4:]: v for n, v in vars(top).items()
                  if n.startswith("REG_") and n not in ("REG_BASE", "REG_END")}
        self.assertEqual(consts, {r.name: r.address for r in MAP.registers if r.built})
        self.assertEqual((top.REG_BASE, top.REG_END), MAP.window)
        self.assertEqual(top.FEATURES, MAP.feature_bits)
        self.assertEqual(top.LAYOUT_LAG, MAP["LAYOUT_LAG"].resets[0])
        self.assertEqual(top.NONE, MAP["FRAME"].resets[0])
        self.assertEqual(top.EQ_APPLY_FRAMES, MAP["TAPS_FROM"].raw["frames"])
        self.assertEqual(SLOTS_VERSION, MAP["VERSION"].resets[0])
        self.assertEqual(top.ID_CHAIN, SLOTS_VERSION << MAP.id_word["chain_lsb"])
        self.assertEqual(ldpc.DEFAULT_MAX_ITER, MAP["MAX_ITER"].resets[0])

    def test_control_is_the_maps(self):
        from isdb_fpga import control

        self.assertEqual({n: getattr(control, f"OP_{n}") for n in OPS}, OPS)
        self.assertEqual((control.CTRL_RUN, control.CTRL_START, control.CTRL_SYMBOLS),
                         tuple(1 << MAP.ctrl[n] for n in ("RUN", "START", "SYMBOLS")))
        self.assertEqual(control.STATUS, MAP.frontend["STATUS"]["address"])
        self.assertEqual(control.REGS[MAP.frontend["KP"]["address"]], "kp")
        # the front end's registers stay out of the chain's window
        lo, hi = MAP.window
        self.assertFalse(any(lo <= a < hi for a in list(control.REGS) + [control.STATUS, control.SYMBOLS]))

    def test_every_register_has_a_simulated_check(self):
        rows = [(f"{r.address:#05x}", r.name, r.access, "; ".join(checks_for(r))) for r in MAP.registers]
        width = [max(len(x[i]) for x in rows) for i in range(3)]
        print("\nregister coverage (tests/test_registers.py):")
        for row in rows:
            print("  " + "  ".join(c.ljust(w) for c, w in zip(row, width)) + "  " + row[3])
        untested = [r.name for r in MAP.registers if len(checks_for(r)) < 2]
        self.assertEqual(untested, [], "registers with no check of their write semantics")


# ---- simulation ---------------------------------------------------------------

class Bench:
    """The host's command words on `gpio_out` with the map's timing."""

    def __init__(self, ctx, dut):
        self.ctx = ctx
        self.dut = dut
        self.t = 0
        self.staged = None

    async def cmd(self, op, arg=0, ticks=None):
        self.t ^= 1
        word = (self.t << P["toggle_bit"] | OPS[op] << P["op_lsb"]
                | (arg & ((1 << P["arg_width"]) - 1)) << P["arg_lsb"])
        self.ctx.set(self.dut.gpio_out, word)
        await self.ctx.tick("sync").repeat(ticks or P["decode_after"])
        return self.ctx.get(self.dut.gpio_in)

    @staticmethod
    def read_ticks(addr):
        hit = MAP.at(addr)
        if hit is None:
            return P["decode_after"] + P["readback_after_answer"]
        return MAP.gpio_in_after(hit[0])

    async def read(self, addr):
        await self.cmd("ADDR", addr)
        return await self.cmd("READ", ticks=self.read_ticks(addr))

    async def write(self, addr, values):
        """`isdb_frontend::control::Encoder::write`: ADDR, then WRITE_S for a
        27-bit signed value, else HI (when it changes) and WRITE."""
        await self.cmd("ADDR", addr)
        for v in values:
            v &= ALL
            if -(1 << 26) <= s32(v) < 1 << 26:
                await self.cmd("WRITE_S", v)
            else:
                if self.staged != v >> 16:
                    await self.cmd("HI", v >> 16)
                    self.staged = v >> 16
                await self.cmd("WRITE", v & 0xFFFF)

    async def ctrl(self, *names):
        await self.cmd("CTRL", sum(1 << MAP.ctrl[n] for n in names))


def simulate(body, monitors=False):
    """Run `body(bench, out)` on `SlotsRx` (no decoder); `out` with the
    monitors' report under "monitors"."""
    from amaranth.hdl import ClockDomain, Module
    from amaranth.sim import Simulator

    from isdb_fpga.chain.common import cos_table
    from isdb_fpga.chain.top import SlotsRx

    dut = SlotsRx(32, cos_table(), decoder=False, monitors=monitors)
    col = None
    if monitors:
        from isdb_fpga.chain.monitors import Collector

        col = Collector(dut)
    top = Module()
    top.submodules.dut = dut
    for d in ("sync", "fast", "ldpc"):
        top.domains += ClockDomain(d)
    sim = Simulator(top)
    sim.add_clock(25e-9, domain="sync")
    sim.add_clock(6.25e-9, phase=12.5e-9, domain="fast")
    sim.add_clock(5e-9, domain="ldpc")
    out = {"dut": dut}

    async def bench(ctx):
        b = Bench(ctx, dut)
        await body(b, out)
        if col is not None:
            out["monitors"] = col.report(lambda n: ctx.get(col.ports()[n]))

    sim.add_testbench(bench)
    sim.run()
    return out


def window():
    lo, hi = MAP.window
    return range(lo, hi)


def compare_with_reset(values):
    """{register name or "unmapped": [differences]} for every address read."""
    out = {r.name: [] for r in MAP.registers}
    out["unmapped"] = []
    for a, v in values.items():
        hit = MAP.at(a)
        want = MAP.reset_at(a)
        if v != want:
            name = "unmapped" if hit is None else hit[0].name
            out[name].append(f"{a:#05x} reads {v:#x}, not {want:#x}")
    return out


class RegisterResetTest(unittest.TestCase):
    """Every address of the window after configuration: the map's reset."""

    def test_every_address_reads_its_reset(self):
        async def body(b, out):
            out["values"] = {a: await b.read(a) for a in window()}

        diff = compare_with_reset(simulate(body)["values"])
        for name, bad in diff.items():
            with self.subTest(register=name):
                self.assertEqual(bad, [])


class RegisterWriteSweepTest(unittest.TestCase):
    """All ones, then all zeros, written to every read-only register and
    unmapped address (WRITE advancing the address), each followed by reads:
    still the reset values."""

    def test_read_only_registers_ignore_writes(self):
        targets = [a for a in window() if MAP.at(a) is None or not MAP.at(a)[0].writable]
        runs = []
        for a in targets:
            if runs and runs[-1][-1] == a - 1:
                runs[-1].append(a)
            else:
                runs.append([a])

        async def body(b, out):
            # all ones, then every address: a register that took the write
            # (or one a write aliased into: none has its mask as its reset)
            # reads other than its reset
            for run in runs:
                await b.write(run[0], [ALL] * len(run))
            out["ones"] = {a: await b.read(a) for a in window()}
            # all zeros, then the addresses a stored 0 would show on
            for run in runs:
                await b.write(run[0], [0] * len(run))
            out["zeros"] = {a: await b.read(a) for a in window() if MAP.reset_at(a)}

        out = simulate(body)
        ones, zeros = compare_with_reset(out["ones"]), compare_with_reset(out["zeros"])
        for name in ones:
            with self.subTest(register=name):
                self.assertEqual([f"after all ones: {x}" for x in ones[name]]
                                 + [f"after all zeros: {x}" for x in zeros[name]], [])


# The "staged" and "strobe" registers' checks: (what is checked, the bench).
# Each appends failures to out[name].

async def staged_carrier(b, out, r):
    """Written: the read keeps the value in use; CTRL START loads it."""
    bad = out.setdefault(r.name, [])
    in_use = r.resets[0]
    for v in walking(32) + [0x8000_0000, 0x7FFF_FFFF, s32(-1234) & ALL]:
        await b.write(r.address, [v])
        got = await b.read(r.address)
        if got != in_use:
            bad.append(f"written {v:#x}: reads {got:#x} before START, not the value in use {in_use:#x}")
        await b.ctrl("START")
        got = await b.read(r.address)
        if got != v & ALL:
            bad.append(f"written {v:#x}: reads {got:#x} after START")
        in_use = got


async def staged_taps(b, out, r):
    """Written: the reads keep the taps in use; START without INIT bit 0
    keeps them; START with it puts the written taps in use (patterns giving
    every bit of every tap both values, each tap a different value)."""
    bad = out.setdefault(r.name, [])
    init = MAP["INIT"]
    taps_bit = 1 << init.field("TAPS").lsb
    in_use = list(r.resets)
    codes = [sum(1 << b_ for b_ in range(32) if b_ >> j & 1) for j in range(5)]
    patterns = [ALL, 0] + [p for c in codes for p in (c, c ^ ALL)]
    rot = lambda v, k: ((v << k) | (v >> (32 - k))) & ALL if k else v  # noqa: E731
    await b.write(init.address, [0])
    for i, p in enumerate(patterns):
        new = [rot(p, k) for k in range(r.count)]
        await b.write(r.address, new)
        got = [await b.read(a) for a in r.addresses]
        if got != in_use:
            bad.append(f"pattern {p:#x}: the taps read {got} after the write, not those in use")
        if i == 0:
            # START without INIT bit 0: the taps in use stay
            await b.ctrl("START")
            got = [await b.read(a) for a in r.addresses]
            if got != in_use:
                bad.append(f"START without INIT bit 0 changed the taps in use to {got}")
            await b.write(init.address, [taps_bit])
        await b.ctrl("START")
        got = [await b.read(a) for a in r.addresses]
        if got != new:
            bad.append(f"pattern {p:#x}: START with INIT bit 0 put {got} in use, not {new}")
        in_use = got
    frm = await b.read(MAP["TAPS_FROM"].address)
    if frm != MAP["TAPS_FROM"].resets[0]:
        bad.append(f"TAPS_FROM reads {frm:#x} after START loaded the taps")
    # the address after the array: no tap there
    after = await b.read(r.end)
    if after != MAP.reset_at(r.end):
        bad.append(f"{r.end:#x} reads {after:#x} after the taps were written")


async def staged_taps_from(b, out, r):
    """Written N: the equaliser armed with N and N + frames in the clock of
    the write (5f60649 armed the previous write's); the read still the frame
    of the taps in use."""
    bad = out.setdefault(r.name, [])
    eq = out["dut"].chain.back.eq
    frames = r.raw["frames"]
    for v in [5, 87] + walking(32)[:32] + [0x7FFF_FFFE]:
        await b.write(r.address, [v])
        await b.ctx.tick("sync")  # the write's clock
        got = (b.ctx.get(eq.pend_from), b.ctx.get(eq.pend_at))
        want = (v, (v + frames) & ALL)
        if got != want:
            bad.append(f"written {v:#x}: armed with (frame, at) {got}, not {want}")
        rd = await b.read(r.address)
        if rd != r.resets[0]:
            bad.append(f"written {v:#x}: reads {rd:#x}, not the taps in use's frame {r.resets[0]:#x}")


async def strobe_capture(b, out, r):
    """Any value arms (ARMED, VALID clear); START releases an armed
    capture; no latch without frames (SEQ stays)."""
    bad = out.setdefault(r.name, [])
    armed = 1 << r.field("ARMED").lsb
    idle = r.resets[0]
    got = await b.read(r.address)
    if got != idle:
        bad.append(f"reads {got:#x} before the first write, not {idle:#x}")
    for v in (0, ALL, 1, 0x1234_5678):
        await b.write(r.address, [v])
        got = await b.read(r.address)
        if got != idle | armed:
            bad.append(f"written {v:#x}: reads {got:#x}, not {idle | armed:#x}")
    await b.ctrl("START")
    start = next(c for c in MAP.raw["ctrl"] if c["name"] == "START")
    await b.ctx.tick("sync").repeat(start["restart_within"])
    got = await b.read(r.address)
    if got != idle:
        bad.append(f"START did not release the armed capture: reads {got:#x}")


STAGED = {
    "CARRIER": ("the value in use until CTRL START loads the written one", staged_carrier),
    "TAPS": ("the taps in use until CTRL START with INIT bit 0", staged_taps),
    "TAPS_FROM": ("arms the equaliser with the frame written and frame + 2", staged_taps_from),
}
STROBE = {
    "SUMS_CAPTURE": ("any value arms; START releases", strobe_capture),
}


async def rw_walk(b, out, r):
    bad = out.setdefault(r.name, [])
    sources = [(reg, f) for reg in MAP.registers for f in reg.fields if f.source == r.name]
    for v in walking(r.width):
        await b.write(r.address, [v])
        got = await b.read(r.address)
        if got != v & r.mask:
            bad.append(f"written {v:#x}: reads {got:#x}, not {v & r.mask:#x}")
        for reg, f in sources:
            w = await b.read(reg.address)
            if (w & f.mask) >> f.lsb != v & r.mask:
                bad.append(f"written {v:#x}: {reg.name}.{f.name} reads {(w & f.mask) >> f.lsb:#x}")
    await b.write(r.address, [r.resets[0]])


async def answer_timing(b, out, r):
    """The first and last element: the previous READ's value on gpio_in
    until `answer_after` clocks after the decode, then the register's; the
    address after the array answers in its decode clock (0)."""
    bad = out.setdefault(r.name, [])
    version = MAP["VERSION"]
    for addr in (r.address, r.end - 1, r.end):
        await b.read(version.address)  # gpio_in shows 2 after it
        await b.cmd("ADDR", addr)
        b.t ^= 1
        b.ctx.set(b.dut.gpio_out, b.t << P["toggle_bit"] | OPS["READ"] << P["op_lsb"])
        trace = []
        for _ in range(MAP.gpio_in_after(r) + 3):
            await b.ctx.tick("sync")
            trace.append(b.ctx.get(b.dut.gpio_in))
        shown = P["decode_after"] + P["readback_after_answer"]  # the first clock a READ shows anything
        hit = MAP.at(addr)
        at = MAP.gpio_in_after(hit[0]) if hit else shown
        want = MAP.reset_at(addr)
        held = trace[shown - 1:at - 1]
        if hit and hit[0].holds_previous and any(v != version.resets[0] for v in held):
            bad.append(f"{addr:#x}: gpio_in {held} before the answer, not the previous READ's value")
        if trace[at - 1:] != [want] * (len(trace) - at + 1):
            bad.append(f"{addr:#x}: gpio_in {trace} after the READ: not {want:#x} from clock {at}")


async def features_and_ctrl(b, out):
    feat = MAP["FEATURES"]
    bad = out.setdefault("FEATURES", [])
    w = await b.read(feat.address)
    flags = sum(f.mask for f in MAP.features.values())
    if w & flags != MAP.feature_bits:
        bad.append(f"flags {w & flags:#x}, not the built ones {MAP.feature_bits:#x}")
    # the ID word: the chain's Slots version in its bits
    bad = out.setdefault("VERSION", [])
    ident = await b.cmd("NOP", ticks=P["decode_after"] + P["readback_after_answer"])
    lsb, width = MAP.id_word["chain_lsb"], MAP.id_word["chain_width"]
    if (ident >> lsb) & ((1 << width) - 1) != MAP["VERSION"].resets[0]:
        bad.append(f"the ID word {ident:#x} does not carry the version in bits {lsb + width - 1}-{lsb}")
    # CTRL SLOTS shows in the front end's STATUS
    bad = out.setdefault("CTRL", [])
    st = MAP.frontend["STATUS"]
    for names in (("RUN", "SYMBOLS", "SLOTS"), ("RUN", "SYMBOLS"), ()):
        await b.ctrl(*names)
        got = await b.read(st["address"])
        want = sum(1 << st["fields"][n] for n in names)
        if got != want:
            bad.append(f"CTRL {'|'.join(names) or 0}: the front end's STATUS reads {got:#x}, not {want:#x}")


class RegisterBehaviourTest(unittest.TestCase):
    """The write semantics of every writable register, the sum registers'
    answer, FEATURES, the ID word and CTRL SLOTS (module docstring)."""

    def test_documented_behaviour(self):
        async def body(b, out):
            for r in MAP.registers:
                if r.access == "rw":
                    await rw_walk(b, out, r)
                elif r.access == "staged":
                    await STAGED[r.name][1](b, out, r)
                elif r.access == "strobe":
                    await STROBE[r.name][1](b, out, r)
                if r.answer_after:
                    await answer_timing(b, out, r)
            await features_and_ctrl(b, out)

        out = simulate(body, monitors=True)
        checked = [r.name for r in MAP.registers if r.access != "ro" or r.answer_after]
        for name in checked + ["FEATURES", "VERSION", "CTRL"]:
            with self.subTest(register=name):
                self.assertIn(name, out, "not checked")
                self.assertEqual(out[name], [])
        with self.subTest("monitors"):
            self.assertEqual(out["monitors"], [])


if __name__ == "__main__":
    unittest.main()
