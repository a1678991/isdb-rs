"""Vivado out-of-context timing of the LDPC decoder alone: the Verilog of
`LdpcDecoder` (S, K, all eleven rates; the synthesis build, sim=False)
through the venv's YoWASP Yosys, then `ldpc_ooc.tcl` (synth_design
-mode out_of_context, the clock at PERIOD ns, place, route): the setup
WNS and the endpoints within MARGIN ns, grouped by register. Read-only on
the Vivado side: nothing but the run directory is written.

python synth/ldpc_vivado.py [--s 34] [--banks 3] [--period 5.0] [--margin 0.3] [--bridge]
                            [--vivado /opt/Xilinx/2026.1/Vivado/bin/vivado]
Output: fpga/build/vivado-ldpc/ (ldpc.v, timing.rpt, util.rpt, vivado.log).
--bridge: the chain's LDPC bridge (`chain.ldpc_if.LdpcBridge`) instead, its
`ldpc` domain at PERIOD and its `sync` (chain) domain at 25 ns, asynchronous;
output in fpga/build/vivado-bridge/.
"""

import os
import subprocess
import sys
import time

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga.ldpc import LdpcDecoder  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))


def arg(argv, name, default):
    return type(default)(argv[argv.index(name) + 1]) if name in argv else default


def main(argv):
    s = arg(argv, "--s", 34)
    k = arg(argv, "--banks", 3)
    period = arg(argv, "--period", 5.0)
    margin = arg(argv, "--margin", 0.3)
    vivado = arg(argv, "--vivado", "/opt/Xilinx/2026.1/Vivado/bin/vivado")
    bridge = "--bridge" in argv
    out = os.path.join(HERE, "..", "build", "vivado-bridge" if bridge else "vivado-ldpc")
    os.makedirs(out, exist_ok=True)
    il = os.path.join(out, "ldpc.il")
    v = os.path.join(out, "ldpc.v")
    if bridge:
        # the chain's LDPC bridge alone: its two domains as clock ports
        from amaranth.hdl import ClockDomain, Elaboratable, Module

        from isdb_fpga.chain.ldpc_if import LdpcBridge, LdpcPort

        port = LdpcPort()
        br = LdpcBridge(port)

        cd_sync, cd_ldpc = ClockDomain("sync"), ClockDomain("ldpc")

        class Top(Elaboratable):
            def elaborate(self, platform):
                m = Module()
                m.domains += [cd_sync, cd_ldpc]
                m.submodules.br = br
                return m

        sigs, seen = [cd_sync.clk, cd_sync.rst, cd_ldpc.clk, cd_ldpc.rst], set()
        cands = list(vars(port).values()) + list(port.in_pos) + list(port.in_llr)
        cands += list(vars(br).values())
        for x in cands:
            if hasattr(x, "shape") and hasattr(x, "name") and id(x) not in seen:
                seen.add(id(x))
                sigs.append(x)
        text = rtlil.convert(Top(), name="ldpc_decoder", ports=sigs)
    else:
        dut = LdpcDecoder(s, banks=k)
        text = rtlil.convert(dut, name="ldpc_decoder", ports=dut.ports())
    with open(il, "w") as f:
        f.write(text)
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    r = subprocess.run([yosys, "-q", "-p", f"read_rtlil {il}; proc; opt_clean; write_verilog -norename {v}"],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stdout + r.stderr)
        return r.returncode
    t0 = time.time()
    log = os.path.join(out, "vivado.log")
    with open(log, "w") as f:
        r = subprocess.run([vivado, "-mode", "batch", "-nojournal", "-nolog", "-source",
                            os.path.join(HERE, "ldpc_ooc.tcl"), "-tclargs", v, "ldpc_decoder",
                            str(period), str(margin), out] + (["clk", "25.0"] if bridge else []),
                           stdout=f, stderr=subprocess.STDOUT, cwd=out)
    for line in open(log):
        if line.startswith(("OOC ", "NEAR ")):
            print(line.rstrip())
    what = "bridge" if bridge else f"S {s}, K {k}"
    print(f"{what}, {period} ns: {time.time() - t0:.0f} s (log {log})")
    return r.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv))
