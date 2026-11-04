"""Run the formal checks (isdb_fpga/formal/, fpga/README.md "Formal
checks"): write each harness's RTLIL, run SymbiYosys on it and report
PASS / FAIL per task, with the counter-example's trace on a failure.

    uv run --locked python tools/formal.py                 # every harness, every task
    uv run --locked python tools/formal.py banks packer    # these harnesses
    uv run --locked python tools/formal.py banks --task bmc
    uv run --locked python tools/formal.py --break e95c563 # a fixed defect back: must FAIL
    uv run --locked python tools/formal.py --list

Work directories: fpga/build/formal/<harness>[-<break>]/<harness>_<task>/
(sby's: logfile.txt, engine_0/trace*.vcd). Exit status 0 when every task
passed (with --break: when the check failed, as it must).
"""

import argparse
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga import formal  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("names", nargs="*", help=f"harnesses (default all: {', '.join(formal.HARNESSES)})")
    ap.add_argument("--task", action="append", help="sby task (bmc, cover, prove); repeatable")
    ap.add_argument("--break", dest="brk", choices=sorted(formal.BREAKS),
                    help="re-introduce a fixed defect (its harness only); the check must fail")
    ap.add_argument("--out", help="work directory (default fpga/build/formal)")
    ap.add_argument("--list", action="store_true", help="list the harnesses, tasks and breaks")
    a = ap.parse_args()
    if a.list:
        for n in formal.HARNESSES:
            print(f"{n}: tasks {', '.join(formal.tasks_of(n))}")
        for b, (h, mod, _, what) in sorted(formal.BREAKS.items()):
            print(f"--break {b} ({h}, {mod}): {what}")
        return 0
    if a.brk:
        names = [formal.BREAKS[a.brk][0]]
        tasks = a.task or ["bmc"]
    else:
        names = a.names or list(formal.HARNESSES)
        tasks = a.task
    ok = True
    for n in names:
        for r in formal.run(n, tasks=tasks, brk=a.brk, out=a.out):
            want = "FAIL" if a.brk else "PASS"
            good = r.status == want
            ok &= good
            tag = f"{n}{'-' + a.brk if a.brk else ''} {r.task}"
            print(f"{tag}: {r.status} in {r.seconds:.1f} s{'' if good else f' (expected {want})'}")
            for s in r.summary:
                print(f"    {s}")
            if r.trace and (r.status == "FAIL" or not good):
                print(f"    trace: {r.trace}")
                if r.status == "FAIL" and r.task != "cover":
                    names = formal.harness_class(n).TRACE
                    table = formal.format_table(formal.trace_table(r.trace, names), names)
                    print("\n".join("    " + ln for ln in table.splitlines()))
            print(f"    log: {os.path.join(r.workdir, 'logfile.txt')}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
