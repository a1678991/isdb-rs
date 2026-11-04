"""A chaos run of the chain with the real decoder (`isdb_fpga.chain.chaos`):
random host stops, STARTs, input losses and tap writes on a real-decoder
set, the decoder's and bridge's health after each event, the `Slots`
stream's sequence and the comparison with the model restarted at the same
points (`isdbs3-phy`'s `examples/chainscenario`). Prints the event list,
the health after each event and the comparison; exits 1 on a problem.

python tools/chaos_run.py SET SEED [--events N] [--budget B] [--backend verilator|cxxrtl]
python tools/chaos_run.py SET SEED --replay build/chaos/SET-sSEED.json   # the same events again
python tools/chaos_run.py SET SEED --draw                               # the events only, no run
python tools/chaos_run.py 7-hdl ci --pinned                             # the opt-in CI case's events
python tools/chaos_run.py SET NAME --place EVENTS.json                  # events by hand, e.g.
    [{"kind": "loss", "at": 500000, "lost": 3000}, {"kind": "long", "at": 3362856, "pause": 250000}]

SET: 7-hdl, 8-loss, 9-fade or 10-bch (fpga/build/chain-SET.json, from
`mise run fpga:vectors`). Run from fpga/. Writes build/chaos/SET-sSEED.json
(the events and the result) and, beside it, the model's and the chain's
outputs.
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import chaos  # noqa: E402


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("set", choices=chaos.SETS)
    ap.add_argument("seed", help="the seed (an integer), or a name for a --replay, --place or --pinned run")
    ap.add_argument("--events", type=int, default=5, help="events to draw (default 5)")
    ap.add_argument("--budget", type=float, default=2.0,
                    help="symbols delivered at most, in inputs (replays; default 2)")
    ap.add_argument("--backend", default=None, help="verilator or cxxrtl (default: simx.compiled_backend)")
    ap.add_argument("--replay", help="a result file whose events to run again")
    ap.add_argument("--place", help="a JSON list of events by hand (kind, at, pause / lost / resume / taps)")
    ap.add_argument("--pinned", action="store_true", help="chaos.CI_EVENTS (the opt-in CI case) instead of a draw")
    ap.add_argument("--draw", action="store_true", help="print the drawn events and stop")
    a = ap.parse_args(argv[1:])
    seed = int(a.seed) if a.seed.lstrip("-").isdigit() else a.seed
    events = None
    if a.replay:
        with open(a.replay) as f:
            events = json.load(f)["events"]
    if a.place:
        with open(a.place) as f:
            events = chaos.place(a.set, json.load(f))
    if a.pinned:
        events = chaos.place(a.set, chaos.CI_EVENTS)
    if events is None and not isinstance(seed, int):
        ap.error("a named run needs --replay, --place or --pinned")
    if a.draw:
        events = events or chaos.draw_events(a.set, seed, a.events, a.budget)
        for i, ev in enumerate(events):
            print(f"[{i}] {chaos.describe(ev)}")
        return 0
    r = chaos.run_chaos(a.set, seed, a.events, a.budget, a.backend, events, log=lambda s: print(s, flush=True))
    out = os.path.join(chaos.BUILD, "chaos", f"{a.set}-s{seed}.json")
    with open(out, "w") as f:
        json.dump(r, f)
    print(chaos.report(r))
    print(f"(written {os.path.relpath(out)})")
    return 1 if r["problems"] else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
