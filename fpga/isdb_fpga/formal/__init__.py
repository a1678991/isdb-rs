"""Formal checks of the small control blocks (issue #30, item 6):
bounded model checking with SymbiYosys, through the YoWASP wheels of the
locked uv environment (`yowasp-sby` and `yowasp-yosys-smtbmc` from
yowasp-yosys, the SMT solver from yowasp-boolector), so no vendor tool and
no system Yosys is needed.

Each harness (a module of this package) is an `Elaboratable` that
instantiates the block under test unchanged, drives its inputs from the
harness's top-level ports (free in every cycle: the solver chooses them),
constrains them with `Assume` only where the block's port contract says a
value cannot occur, and states the block's properties with `Assert`, plus
`Cover`s that show the harness reaches the interesting events (a check
whose assumptions exclude the event it is about would pass vacuously).
Amaranth 0.5 writes the properties as `$check` cells (flavour assert,
assume or cover) into the RTLIL, which sby's model generation (Yosys's
`chformal`, `write_smt2`) and the smtbmc engine take as they are.

The harness's `.sby` file (next to its module, same name) holds the
modes, the depth and the Yosys script; `run` writes the harness's RTLIL
into a work directory, copies the `.sby` there and runs sby. The
harnesses (README "Formal checks" has the properties, the assumptions,
the depths and the run times):

- `banks`: `LdpcDecoder`'s bank allocator (issue #28's class);
- `packer`: `Packer`'s sequence numbers across START (PR #39's defect);
- `frame`: `FrameCtl`'s frame boundaries.

`BREAKS` re-introduces a fixed defect on a scratch copy of the module's
source (`monitors.source_patch`, as the monitors' proofs do): `run(name,
brk=...)` must then FAIL with a counter-example trace.

Internal signals a property needs (the allocator's `alloc`, a context's
bank) are found by name in the elaborated fragment of the block (`taps`):
the HDL carries no formal-only ports.
"""

import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass

from amaranth.back import rtlil
from amaranth.hdl import Fragment, Signal

HERE = os.path.dirname(os.path.abspath(__file__))
FPGA = os.path.dirname(os.path.dirname(HERE))

# name -> (module, harness class)
HARNESSES = {
    "banks": ("isdb_fpga.formal.banks", "BankHarness"),
    "packer": ("isdb_fpga.formal.packer", "PackerHarness"),
    "frame": ("isdb_fpga.formal.frame", "FrameHarness"),
}

# a fixed defect re-introduced: name -> (harness, module patched, edits, what)
BREAKS = {
    "e95c563": ("banks", "isdb_fpga.ldpc", [
        ("reuse.eq(self.begin & holds & ~self.start)", "reuse.eq(0)"),
        ("with m.Elif(self.abort):", "with m.Elif(self.abort & 0):"),
    ], "the allocation rule before e95c563 (#28): `begin` always takes a new bank, `abort` frees nothing"),
    "e95c563-begin": ("banks", "isdb_fpga.ldpc", [
        ("reuse.eq(self.begin & holds & ~self.start)", "reuse.eq(0)"),
    ], "half of it: a `begin` on a context that holds an uncommitted bank takes another one (the "
       "board's leak across stop/START, before `abort` existed)"),
    "1817079": ("packer", "isdb_fpga.chain.packer", [
        ("with m.If(self.start | restart):", "with m.If(self.start):"),
        ("                with m.If(self.b_valid):", "                with m.Elif(self.b_valid):"),
    ], "the `start` handling before 1817079 (PR #39): honoured only while idle, and a block offered "
       "with it is not taken"),
    "1817079-latch": ("packer", "isdb_fpga.chain.packer", [
        ("with m.If(self.start | restart):", "with m.If(self.start):"),
    ], "the sequence half of it: a `start` while a block leaves is lost (the latch removed)"),
    "restart-open": ("frame", "isdb_fpga.chain.frame", [
        ("m.d.sync += [c.cur_valid.eq(0), q_valid.eq(0)]", "m.d.sync += [q_valid.eq(0)]"),
    ], "a synthetic fault (no defect of record): a restart reports the open frame dropped but "
       "leaves it open"),
}


def taps(fragment, names):
    """{name: Signal}: the signals with these names that `fragment`'s own
    statements (not its subfragments') assign (a block's internal state,
    read by a property); each name must belong to one signal."""
    found = {}

    def visit(sig):
        if sig.name in names:
            if sig.name in found and found[sig.name] is not sig:
                raise ValueError(f"two signals named {sig.name!r}")
            found[sig.name] = sig

    for stmts in fragment.statements.values():
        for st in stmts:
            for sig in st._lhs_signals():
                visit(sig)
    missing = set(names) - set(found)
    if missing:
        raise KeyError(f"no signals {sorted(missing)} in the fragment")
    return found


def elaborate(block, names=()):
    """(fragment, taps) of `block`, elaborated once: the harness adds the
    fragment as its submodule, so the tapped signals are the design's."""
    frag = Fragment.get(block, None)
    return frag, taps(frag, names) if names else {}


def harness_class(name):
    import importlib

    mod, cls = HARNESSES[name]
    return getattr(importlib.import_module(mod), cls)


def harness(name):
    return harness_class(name)()


def write(name, path):
    """Write harness `name`'s RTLIL to `path` (top module `top`)."""
    h = harness(name)
    ports = [v for v in vars(h).values() if isinstance(v, Signal)]
    with open(path, "w") as f:
        f.write(rtlil.convert(h, name="top", ports=ports))


@dataclass
class Result:
    name: str
    task: str
    status: str  # PASS, FAIL, ERROR, UNKNOWN, TIMEOUT
    seconds: float
    workdir: str
    trace: str | None  # the counter-example (bmc / prove) or a cover trace
    summary: list


def tools_env(shim):
    """The environment for sby: its Yosys, smtbmc and witness tools are the
    YoWASP entry points next to this interpreter (sby calls them by the
    plain names unless YOSYS / SMTBMC / WITNESS say otherwise), and
    smtbmc's `boolector` is yowasp-boolector, through a symlink in
    `shim` put first on PATH."""
    bindir = os.path.dirname(sys.executable)

    def tool(n):
        p = os.path.join(bindir, n)
        if not os.path.exists(p):
            p = shutil.which(n)
        if not p:
            raise FileNotFoundError(f"{n} (the locked uv environment: cd fpga && uv sync)")
        return p

    os.makedirs(shim, exist_ok=True)
    link, target = os.path.join(shim, "boolector"), tool("yowasp-boolector")
    if not (os.path.islink(link) and os.readlink(link) == target):
        tmp = f"{link}.{os.getpid()}"
        if os.path.lexists(tmp):
            os.remove(tmp)
        os.symlink(target, tmp)
        os.replace(tmp, link)  # atomic: parallel runs share the directory
    env = dict(os.environ)
    _warm(shim, [tool("yowasp-boolector"), "--version"], [tool("yowasp-yosys"), "-V"])
    env.update(YOSYS=tool("yowasp-yosys"), SMTBMC=tool("yowasp-yosys-smtbmc"),
               WITNESS=tool("yowasp-yosys-witness"),
               PATH=shim + os.pathsep + bindir + os.pathsep + env.get("PATH", ""))
    return env, tool("yowasp-sby")


def _warm(lockdir, *cmds):
    """Run each YoWASP tool once, one process at a time: its first run
    compiles the WebAssembly into the user's cache and says so on stderr,
    which smtbmc would read as the solver's answer (and parallel test
    workers would each compile it)."""
    import fcntl

    with open(os.path.join(lockdir, ".warm.lock"), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        for cmd in cmds:
            subprocess.run(cmd, capture_output=True, check=False)


def tasks_of(name):
    """The tasks of harness `name`'s .sby file ([tasks] section)."""
    out, on = [], False
    with open(os.path.join(HERE, f"{name}.sby")) as f:
        for line in f:
            s = line.strip()
            if s.startswith("["):
                on = s == "[tasks]"
            elif on and s and not s.startswith("#"):
                out.append(s.split()[0])
    return out


def run(name, tasks=None, brk=None, out=None, timeout=None):
    """Run harness `name` (its .sby tasks, default all) and return a
    `Result` per task. `brk`: a BREAKS key of this harness (the design
    patched). Work directory: `out` (default fpga/build/formal), then
    `name` or `name-brk`."""
    from ..chain.monitors import source_patch

    # absolute: sby runs with the work directory as its cwd, and the
    # solver shim's directory goes on PATH (a relative `--out` would be
    # looked up under the work directory)
    out = os.path.abspath(out or os.path.join(FPGA, "build", "formal"))
    tag = f"{name}-{brk}" if brk else name
    wd = os.path.join(out, tag)
    os.makedirs(wd, exist_ok=True)
    il = os.path.join(wd, f"{name}.il")
    if brk:
        h, module, edits, _ = BREAKS[brk]
        if h != name:
            raise ValueError(f"break {brk} is for harness {h}")
        # import the harness before the patch: source_patch swaps (and
        # restores) the references of modules already loaded; one first
        # imported inside it would keep the patched class for good
        harness_class(name)
        with source_patch(module, edits):
            write(name, il)
    else:
        write(name, il)
    sby = os.path.join(wd, f"{name}.sby")
    shutil.copyfile(os.path.join(HERE, f"{name}.sby"), sby)
    env, sby_exe = tools_env(os.path.join(out, "bin"))
    results = []
    for task in tasks or tasks_of(name):
        tdir = os.path.join(wd, f"{name}_{task}")
        t0 = time.monotonic()
        cmd = [sby_exe, "-f", sby, task]
        try:
            p = subprocess.run(cmd, cwd=wd, env=env, capture_output=True, text=True, timeout=timeout)
            log = p.stdout + p.stderr
        except subprocess.TimeoutExpired as e:
            log = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
            results.append(Result(name, task, "TIMEOUT", time.monotonic() - t0, tdir, None, [log[-2000:]]))
            continue
        dt = time.monotonic() - t0
        status = "ERROR"
        st = os.path.join(tdir, "status")
        if os.path.exists(st):
            with open(st) as f:
                status = (f.read().split() or ["ERROR"])[0]
        checks = _checks(il)
        summary = [_name(ln.split("summary: ", 1)[1], checks) for ln in log.splitlines() if "summary: " in ln]
        if status == "ERROR":
            summary.append(log[-3000:])
        results.append(Result(name, task, status, dt, tdir, _trace(tdir, summary), summary))
    return results


def _checks(il):
    """{"check_N": (message, file:line)} of the $check cells of an RTLIL file."""
    out, src = {}, None
    with open(il) as f:
        lines = iter(f)
        for line in lines:
            w = line.strip()
            if w.startswith("attribute \\src "):
                src = w.split(" ", 2)[2].strip('"')
            elif w.startswith("cell $check $"):
                name = "check_" + w.split("$")[-1]
                fmt = next(lines).strip()
                msg = fmt.split(" ", 2)[2].strip('"') if fmt.startswith("parameter \\FORMAT") else ""
                path, _, ln = src.rpartition(":") if src else ("", "", "")
                rel = os.path.relpath(os.path.normpath(path), FPGA) if path else "?"
                out[name] = (msg, f"{rel}:{ln}")
    return out


def _name(line, checks):
    """A summary line with sby's check name and path replaced by the
    property's message and its place in the harness."""
    for word in line.split():
        key = word.rsplit(".", 1)[-1]
        if key in checks:
            msg, where = checks[key]
            head = line.split(" at ", 1)[0].replace(word, f'"{msg}"' if msg else key)
            tail = line.split(" step ", 1)[1] if " step " in line else None
            return f"{head} ({where}){f' step {tail}' if tail else ''}"
    return line


def _trace(tdir, summary):
    """The first trace sby names in its summary (a counter-example, or the
    first cover trace), as a path."""
    for s in summary:
        if s.lstrip().startswith(("counterexample trace", "cover trace")):
            for word in s.replace(",", " ").split():
                if word.endswith(".vcd"):
                    p = word if os.path.isabs(word) else os.path.join(os.path.dirname(tdir), word)
                    return os.path.normpath(p)
    return None


def trace_table(vcd, names, scope="top"):
    """The values of the signals `names` of `scope` ("top", "top.dec") at
    each step of an sby trace (VCD: a step every 10 time units), as rows
    [step, value, ...] (integers; None where unknown)."""
    ids, path, cur, rows = {}, [], {}, []
    t = None

    def emit():
        if t is not None and t % 10 == 0:
            rows.append([t // 10] + [cur.get(n) for n in names])

    with open(vcd) as f:
        for line in f:
            w = line.split()
            if not w:
                continue
            if w[0] == "$scope":
                path.append(w[2])
            elif w[0] == "$upscope":
                path.pop()
            elif w[0] == "$var" and ".".join(path) == scope and w[4] in names:
                ids.setdefault(w[3], w[4])
            elif w[0][0] == "#":
                emit()
                t = int(w[0][1:])
            elif w[0][0] == "b" and len(w) == 2 and w[1] in ids:
                v = w[0][1:]
                cur[ids[w[1]]] = None if any(ch in v for ch in "xz") else int(v, 2)
    emit()
    return rows


def format_table(rows, names):
    head = ["step"] + list(names)
    cells = [head] + [["-" if v is None else str(v) for v in r] for r in rows]
    width = [max(len(r[i]) for r in cells) for i in range(len(head))]
    return "\n".join("  ".join(c.rjust(w) for c, w in zip(r, width)) for r in cells)
