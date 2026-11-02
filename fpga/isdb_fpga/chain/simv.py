"""The `verilator` backend of `simx.engine`: the design Verilated and
compiled with `harness.cc`, the same stream driver as the cxxrtl backend.

The design goes RTLIL -> Verilog (the venv's Yosys: `proc;
write_verilog`) -> C++ (Verilator) -> a shared library (Verilator's
makefile, `make -j`) and is loaded through the same ctypes interface as
the CXXRTL library: the harness sees a generated header (`adapter`) in
place of the CXXRTL C API, in which each top-level port (64 bits at most)
is a `cxxrtl_object` whose value is copied into the model before each
`eval()` (inputs) and out of it after (every port). `cxxrtl_step` is
`eval()`: the harness's settle / edge sequence is the same on both
backends, cycle for cycle.

Undefined values: CXXRTL has none (every value starts at its init, else
0; an undefined mux default gives 0); `--x-initial 0 --x-assign 0` give
Verilator the same. In these designs X appears only as a memory read
port's value before its first read and as a parallel mux's default, and
nothing depends on it: `synth/sim_compare.py --x-unique SEED` builds the
models with `--x-initial unique --x-assign unique` and random values
(`$FPGA_VERILATOR_SEED`) and gets the same records.

Requires `verilator` (5 or later) on PATH, or `$VERILATOR` naming it, and
a C++ compiler; `available()` says whether it is. The libraries are
cached in fpga/build/verilator/, keyed by the design's RTLIL without its
`src` attributes (file paths and line numbers, which the Verilog does not
carry), harness.cc, the generated adapter, the Verilator version and the
flags, and by nothing else: a change to this module's Python side does
not invalidate them unless it changes what is built, and the same design
from another checkout or entry point finds the same library.
"""

import functools
import os
import re
import shutil
import subprocess

from . import simx

BUILD = os.path.normpath(os.path.join(simx.HERE, "..", "..", "build", "verilator"))
# Verilator's C++ build is a make of many small files: -j jobs (default:
# the CPUs this process may use, at most 8; $FPGA_SIM_JOBS overrides).
VFLAGS = ["--cc", "--exe", "--top-module", "top", "--prefix", "Vtop",
          "-Wno-fatal", "-Wno-lint", "-Wno-style", "--x-initial", "0", "--x-assign", "0",
          "-CFLAGS", "-O2 -fPIC -std=c++17 -w", "-LDFLAGS", "-shared"]
YOSYS = "read_rtlil top.il; hierarchy -top top; proc; write_verilog -noattr top.v"


def executable():
    return shutil.which(os.environ.get("VERILATOR", "verilator"))


@functools.lru_cache(maxsize=None)
def version():
    """`verilator --version` (`Verilator 5.054 2026-11-01 rev v5.054`), or
    None without one."""
    tool = executable()
    if not tool:
        return None
    try:
        return subprocess.run([tool, "--version"], capture_output=True, text=True,
                              check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def available():
    """Whether the verilator backend can be built here: Verilator 5 or
    later and a C++ compiler."""
    v = version()
    m = re.match(r"Verilator (\d+)\.", v or "")
    return bool(m) and int(m.group(1)) >= 5 and simx.have_cxx()


def jobs():
    env = os.environ.get("FPGA_SIM_JOBS")
    if env:
        return int(env)
    return max(1, min(8, len(os.sched_getaffinity(0))))


_PORT = re.compile(r"\s*wire width (\d+) (input|output|inout) \d+\s+(?:signed\s+)?\\(\S+)")


def ports(il):
    """The top module's ports in the RTLIL: [(name, width, direction)]."""
    out = []
    top = False
    for line in il.splitlines():
        if line.startswith("module "):
            top = line.split()[1] == "\\top"
        elif top and line.startswith("end"):
            top = False
        elif top:
            m = _PORT.fullmatch(line.rstrip())
            if m:
                width, direction, name = int(m.group(1)), m.group(2), m.group(3)
                if direction == "inout":
                    raise ValueError(f"verilator adapter: {name} is an inout port")
                if width > 64:
                    raise ValueError(f"verilator adapter: {name} is {width} bits (64 at most)")
                if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
                    raise ValueError(f"verilator adapter: port name {name!r} is not a C identifier")
                out.append((name, width, direction))
    if not out:
        raise ValueError("verilator adapter: no ports in the RTLIL's top module")
    return out


def members(header, names):
    """The Verilated model's member of each port (Verilator renames a C++
    keyword: `new` is `__SYM__new`), from its Vtop.h."""
    have = set(re.findall(r"VL_(?:IN|OUT)(?:8|16|64)?\(&(\w+),", header))
    out = {}
    for n in names:
        m = n if n in have else f"__SYM__{n}"
        if m not in have:
            raise RuntimeError(f"verilator adapter: port {n} not found in Vtop.h")
        out[n] = m
    return out


def adapter(il, member=None):
    """The header harness.cc includes in place of the CXXRTL C API: a
    `cxxrtl_object` per top-level port (at most 64 bits: two 32-bit
    chunks, `next` null so the harness writes `curr`), `cxxrtl_step` =
    inputs into the model, `eval()`, every port back out. `member`: the
    model's member of each port (`members`), default the port's name."""
    ps = ports(il)
    init, put, get = [], [], []
    for i, (port, width, direction) in enumerate(ps):
        name = member[port] if member else port
        init.append(f'\t\tobjects[{i}].width = {width}; names["{port}"] = &objects[{i}];')
        if direction == "input":
            put.append(f"\t\tmodel->{name} = (uint64_t)objects[{i}].curr[0]"
                       f" | (uint64_t)objects[{i}].curr[1] << 32;")
        get.append(f"\t\tobjects[{i}].curr[0] = (uint32_t)(uint64_t)model->{name};")
        if width > 32:
            get.append(f"\t\tobjects[{i}].curr[1] = (uint32_t)((uint64_t)model->{name} >> 32);")
    return f"""// Generated by isdb_fpga/chain/simv.py: the CXXRTL C API subset
// harness.cc uses, on a Verilated model.
#include "Vtop.h"
// eval() settles the logic after a clock edge (no second step)
#define HARNESS_EDGE_SETTLES 1
#include <array>
#include <cstdint>
#include <cstdlib>
#include <memory>
#include <string>
#include <unordered_map>

struct cxxrtl_object {{
	size_t width = 0;
	uint32_t data[2] = {{0, 0}};
	uint32_t *curr = data;
	uint32_t *next = nullptr;
}};

struct VerilatedTop {{
	VerilatedContext context;
	std::unique_ptr<Vtop> model;
	std::array<cxxrtl_object, {len(ps)}> objects;
	std::unordered_map<std::string, cxxrtl_object *> names;
	VerilatedTop() {{
		// $FPGA_VERILATOR_SEED: random X values (with --x-initial unique
		// --x-assign unique), the check that the design does not use them
		if (const char *seed = std::getenv("FPGA_VERILATOR_SEED")) {{
			context.randReset(2);
			context.randSeed(std::atoi(seed));
		}}
		model.reset(new Vtop{{&context}});
{chr(10).join(init)}
		step();
	}}
	~VerilatedTop() {{ model->final(); }}
	void step() {{
{chr(10).join(put)}
		model->eval();
{chr(10).join(get)}
	}}
}};

typedef void *cxxrtl_toplevel;
typedef VerilatedTop *cxxrtl_handle;
extern "C" inline cxxrtl_toplevel cxxrtl_design_create() {{ return nullptr; }}
inline cxxrtl_handle cxxrtl_create(cxxrtl_toplevel) {{ return new VerilatedTop; }}
inline void cxxrtl_destroy(cxxrtl_handle h) {{ delete h; }}
inline cxxrtl_object *cxxrtl_get(cxxrtl_handle h, const char *name) {{
	auto i = h->names.find(name);
	return i == h->names.end() ? nullptr : i->second;
}}
inline void cxxrtl_step(cxxrtl_handle h) {{ h->step(); }}
"""


_SRC = re.compile(r"^\s*attribute \\src .*\n", re.M)


def _flags():
    return VFLAGS + os.environ.get("FPGA_VERILATOR_FLAGS", "").split()


class Verilator(simx._Cxx):
    build_dir = BUILD

    def __init__(self, dut, ports, tag):
        if not available():
            raise RuntimeError("the verilator backend needs Verilator >= 5 (on PATH or $VERILATOR)"
                               " and a C++ compiler")
        super().__init__(dut, ports, tag)

    @staticmethod
    def _key(il):
        # without the `src` attributes (the HDL's file paths and lines, as
        # imported: `tests/../isdb_fpga/...`), which `write_verilog
        # -noattr` drops: the same design from another checkout, entry
        # point or edited line finds the same library
        il = _SRC.sub("", il)
        return (il.encode() + open(os.path.join(simx.HERE, "harness.cc"), "rb").read()
                + adapter(il).encode() + version().encode() + " ".join(_flags()).encode()
                + YOSYS.encode())

    @staticmethod
    def _build(il, so):
        """yosys (RTLIL -> Verilog), verilator (-> C++ and its makefile),
        the adapter for the model's member names, make (-> the library);
        in a directory of this process's own, removed once the library is
        in place (kept, with its build.log, if a step fails)."""
        work = f"{so}.{os.getpid()}.build"
        shutil.rmtree(work, ignore_errors=True)
        os.makedirs(work)
        with open(os.path.join(work, "top.il"), "w") as f:
            f.write(il)
        with open(os.path.join(simx.HERE, "harness.cc")) as f:
            harness = f.read()
        inc = "#include <cxxrtl/capi/cxxrtl_capi.h>"
        if inc not in harness:
            raise RuntimeError(f"harness.cc no longer includes {inc}")
        with open(os.path.join(work, "harness.cc"), "w") as f:
            f.write(harness.replace(inc, '#include "verilator_capi.h"'))
        log = os.path.join(work, "build.log")

        def step(what, cmd):
            with open(log, "a") as f:
                f.write("$ " + " ".join(cmd) + "\n")
                f.flush()
                r = subprocess.run(cmd, cwd=work, stdout=f, stderr=subprocess.STDOUT)
            if r.returncode:
                with open(log) as f:
                    lines = f.read().splitlines()
                errors = [x for x in lines if re.search(r"error", x, re.I)][:20]
                raise RuntimeError(f"{what} failed (log: {log}):\n" + "\n".join(
                    [x[:300] for x in errors] + ["..."] + [x[:300] for x in lines[-5:]]))

        # relative paths: YoWASP's Yosys sees its own /tmp
        step("yosys", [simx.yosys_path(), "-q", "-p", YOSYS])
        step("verilator", [executable(), *_flags(), "--Mdir", "obj",
                           "-o", os.path.abspath(os.path.join(work, "model.so")),
                           "top.v", "harness.cc"])
        with open(os.path.join(work, "obj", "Vtop.h")) as f:
            member = members(f.read(), [p for p, _, _ in ports(il)])
        with open(os.path.join(work, "verilator_capi.h"), "w") as f:
            f.write(adapter(il, member))
        step("make", ["make", "-C", "obj", "-f", "Vtop.mk", "-j", str(jobs())])
        os.replace(os.path.join(work, "model.so"), so)
        # the Verilog is kept for inspection under the library's name
        os.replace(os.path.join(work, "top.v"), so[:-3] + ".v")
        shutil.rmtree(work, ignore_errors=True)
