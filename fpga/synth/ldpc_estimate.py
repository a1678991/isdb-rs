"""Resource estimate of the LDPC decoder (`LdpcDecoder`) for the XC7Z020
with Yosys `synth_xilinx` (YoWASP, user space), per S, with the address
tables of every rate (ldpc-tables.json, else the .rs source): LUT, FF,
block RAM (RAMB36E1 + RAMB18E1 / 2), LUTRAM, DSP48E1, CARRY4. Before place
and route: for budgeting against the plan's 14 000 LUT / 50 BRAM36 / 0
DSP.

python synth/ldpc_estimate.py [S ...] [--rates 93,41] [--banks K]
"""

import os
import subprocess
import sys
import time

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from isdb_fpga import ldpcmodel  # noqa: E402
from isdb_fpga.ldpc import LdpcDecoder  # noqa: E402


def estimate(s_units, codes, tag, banks=3):
    out = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out, exist_ok=True)
    dut = LdpcDecoder(s_units, codes, banks=banks)
    il = os.path.join(out, f"{tag}.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name=tag, ports=dut.ports()))
    stat = os.path.join(out, f"{tag}.stat")
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top {tag}; tee -q -o {stat} stat"
    t0 = time.time()
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode:
        print(res.stdout[-3000:] + res.stderr[-3000:])
        return None
    counts = {}
    for line in open(stat).read().split("design hierarchy")[-1].splitlines():
        p = line.split()
        if len(p) == 2 and p[0].isdigit():
            counts[p[1]] = int(p[0])
    luts = sum(v for k, v in counts.items() if k.startswith("LUT"))
    ffs = sum(v for k, v in counts.items() if k.startswith("FD"))
    lram = sum(v for k, v in counts.items() if k.startswith("RAM") and not k.startswith("RAMB"))
    bram = counts.get("RAMB36E1", 0) + counts.get("RAMB18E1", 0) / 2
    row = dict(S=s_units, K=banks, LUT=luts, FF=ffs, BRAM36=bram, RAMB36=counts.get("RAMB36E1", 0),
               RAMB18=counts.get("RAMB18E1", 0), LUTRAM=lram, DSP=counts.get("DSP48E1", 0),
               CARRY4=counts.get("CARRY4", 0), seconds=round(time.time() - t0))
    print(f"S {s_units}, K {banks}: {luts} LUT, {ffs} FF, {bram:g} BRAM36 ({row['RAMB36']} RAMB36E1 + "
          f"{row['RAMB18']} RAMB18E1), {lram} LUTRAM cells, {row['DSP']} DSP48E1, "
          f"{row['CARRY4']} CARRY4 ({row['seconds']} s)", flush=True)
    return row


def main(argv):
    tables = ldpcmodel.load_tables()
    rates = sorted(tables)
    if "--rates" in argv:
        rates = [int(r) for r in argv[argv.index("--rates") + 1].split(",")]
    banks = int(argv[argv.index("--banks") + 1]) if "--banks" in argv else 3
    args = [a for i, a in enumerate(argv[1:], 1)
            if not a.startswith("--") and argv[i - 1] not in ("--rates", "--banks")]
    units = [int(a) for a in args] or [17, 34]
    codes = {r: tables[r] for r in rates}
    print(f"rates {rates}")
    for s in units:
        estimate(s, codes, f"ldpc_s{s}_k{banks}" + ("" if len(rates) == len(tables) else
                                                    "_r" + "_".join(map(str, rates))), banks)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
