#!/usr/bin/env python3
"""One line per field of an `isdb rx --system isdbs3 --offload auto --json`
summary from the FPGA receive chain (the Slots path): the open report, the
slow loop's counters (verified reads, captures and solves accepted or
rejected, reverts, recoveries), the chain's last status and counters, the
capture's end. For the board runs of tools/isdbs3-issue-checks.sh.

    isdbs3-run-summary.py RUN.json [RUN.log]

With RUN.log (the run's stderr), also its warnings and the decoder-health
lines.
"""

import json
import re
import sys


def main(argv):
    j = json.load(open(argv[1]))
    c = j.get("chain") or {}
    r = c.get("radio") or {}
    sl = r.get("slow_loop") or {}
    st = c.get("status") or {}
    out = []
    out.append(f"open: {r.get('open')}")
    out.append(f"features: {r.get('features', 0):#x}")
    keys = ["ticks", "solves", "solves_rejected", "singular", "torn_reads", "unsettled_reads",
            "words_read", "rereads", "captures_rejected", "captures_rejected_by", "reverts",
            "recoveries", "unverified", "last_good_frame", "last_frame", "error"]
    out.append("slow loop: " + ", ".join(f"{k} {sl.get(k)}" for k in keys))
    k = st.get("counters") or {}
    out.append(f"status: frame {st.get('frame')}, n0 {st.get('n0_q20')} (MER {st.get('mer_db')}), "
               f"llr_scale {st.get('llr_scale')}, taps_from {st.get('taps_from')}, counters {k}")
    out.append(f"blocks {c.get('blocks')}, losses {c.get('losses')}, blocks_lost {c.get('blocks_lost')}, "
               f"frames_missing {c.get('frames_missing')}, undecoded {c.get('frames_undecoded')}, "
               f"orphan_slots {c.get('orphan_slots')}, capture_error {c.get('capture_error')}")
    out.append(f"radio counters: {r.get('counters')}, overflows {r.get('overflows')}")
    for line in out:
        print(line)
    if len(argv) > 2:
        for line in open(argv[2], errors="replace"):
            line = re.sub(r"^\d+\.\d+ ", "", line.rstrip())
            if re.search(r"WARN|decoder:|STATUS|banks held|did not hold|reverted|recover|rejected|exit status|Exit status", line):
                print("log: " + line[:400])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
