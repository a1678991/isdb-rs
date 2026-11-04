"""Chaos runs of the chain with the real decoder (`isdb_fpga.chain.chaos`,
issue #30 item 3): random host stops, STARTs, input losses and tap writes
on the real-decoder sets, the decoder's and bridge's health after each
event, the `Slots` sequence numbers, and every block against the Rust
model restarted at the same points (`isdbs3-phy`'s `examples/chainscenario`).

- `ChaosModelTest` (by default, where cargo and the vectors are): the
  model's scenario runner with no events but the sets' own reproduces
  each set's `slots_words`, so its restarts are the only difference.
- `ChaosTest.test_pinned_events` (opt-in, FPGA_CHAIN_REAL=1, as the
  other real-decoder scenarios: they need the compiled decoder):
  chain-7-hdl with a fixed list of five events (`chaos.CI_EVENTS`),
  within about 3 minutes on Verilator.
- `ChaosSweepTest` (opt-in, FPGA_CHAIN_CHAOS=<seeds>: a count N for seeds
  1..N, a range `A-B` or a list `3,7,11`): every seed on all four sets,
  one test each (xdist runs them in parallel; about 2-4 minutes each).

Each run writes build/chaos/<set>-s<seed>.json (the events and the
result); `tools/chaos_run.py SET SEED` reproduces one and prints it,
`tools/chaos_table.py` tabulates the results.
"""

import json
import os
import shutil
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain import chaos, simx  # noqa: E402

REAL = os.environ.get("FPGA_CHAIN_REAL") == "1"
SWEEP = os.environ.get("FPGA_CHAIN_CHAOS", "")
COMPILED = simx.compiled_backend()
CARGO = shutil.which("cargo")


def have(name):
    return os.path.exists(os.path.join(chaos.VECTORS, f"chain-{name}.json"))


def seeds(spec):
    """FPGA_CHAIN_CHAOS: N (seeds 1..N), A-B, or a comma-separated list."""
    out = []
    for part in filter(None, (p.strip() for p in spec.split(","))):
        if "-" in part:
            a, b = part.split("-")
            out += range(int(a), int(b) + 1)
        elif "," in spec:
            out.append(int(part))
        else:
            out += range(1, int(part) + 1)
    return out


def record(case, r):
    os.makedirs(os.path.join(chaos.BUILD, "chaos"), exist_ok=True)
    with open(os.path.join(chaos.BUILD, "chaos", f"{r['set']}-s{r['seed']}.json"), "w") as f:
        json.dump(r, f)
    print(chaos.report(r))
    case.assertEqual(r["problems"], [], f"chain-{r['set']} seed {r['seed']}:\n" + "\n".join(r["problems"][:12]))


@unittest.skipUnless(CARGO, "no cargo (the model's examples/chainscenario)")
class ChaosModelTest(unittest.TestCase):
    def test_without_events_the_model_is_the_set(self):
        os.makedirs(os.path.join(chaos.BUILD, "chaos"), exist_ok=True)
        done = 0
        for name in chaos.SETS:
            if not have(name):
                continue
            doc = chaos.vector(name)
            events = [{"kind": "setloss", "at": b, "lost": k, "resume": b, "stream": b}
                      for b, k in doc.get("losses", [])]
            stem = os.path.join(chaos.BUILD, "chaos", f"{name}-none")
            p, out = chaos.start_model(name, events, stem)
            log, _ = p.communicate()
            self.assertEqual(p.returncode, 0, log)
            with open(out) as f:
                got = json.load(f)["slots_words"]
            self.assertEqual(got, doc["slots_words"], f"chain-{name}: the model without events")
            done += 1
        if not done:
            self.skipTest("no real-decoder vector set (mise run fpga:vectors)")


@unittest.skipUnless(COMPILED and CARGO, "a compiled backend and cargo")
@unittest.skipUnless(have("7-hdl"), "no chain-7-hdl (mise run fpga:vectors)")
class ChaosTest(unittest.TestCase):
    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    def test_pinned_events(self):
        # chaos.CI_EVENTS: a loss, #28's stop inside frame 2's codewords
        # and START with a replay, a short stop, a START while running;
        # about 8 M symbols, the replay's frame 2 decoded and compared
        events = chaos.place("7-hdl", chaos.CI_EVENTS)
        r = chaos.run_chaos("7-hdl", "ci", events=events, backend=COMPILED)
        record(self, r)
        self.assertTrue(any(v.startswith("equal") for v in r["frames"].values()), r["frames"])

    @unittest.skipUnless(REAL, "FPGA_CHAIN_REAL=1")
    @unittest.expectedFailure
    def test_restart_just_after_a_slot_head(self):
        # A defect the chaos runs found (README "Chaos runs"): a restart
        # whose next slot head falls within about 30 symbols of the end of
        # the sync's first period after it (here START 5 symbols after a
        # slot head of chain-7: the head 9 291 symbols on) leaves the
        # status blocks' period peak and sum (words 0-5) unlike the
        # model's in every later frame; 10 symbols before the head, or a
        # few thousand after, they are equal. Remove expectedFailure with
        # the fix.
        events = chaos.place("7-hdl", [{"kind": "start", "at": 1000 + 54 * 9296 + 5}])
        record(self, chaos.run_chaos("7-hdl", "head5", events=events, backend=COMPILED))


class ChaosSweepTest(unittest.TestCase):
    """Generated: test_<set>_<seed> for FPGA_CHAIN_CHAOS's seeds."""


def _sweep_case(name, seed):
    @unittest.skipUnless(COMPILED and CARGO, "a compiled backend and cargo")
    @unittest.skipUnless(have(name), f"no chain-{name} (mise run fpga:vectors)")
    def test(self):
        record(self, chaos.run_chaos(name, seed, 5, 2.0, COMPILED))
    return test


for _seed in seeds(SWEEP):
    for _name in chaos.SETS:
        setattr(ChaosSweepTest, f"test_{_name.replace('-', '_')}_{_seed}", _sweep_case(_name, _seed))


if __name__ == "__main__":
    unittest.main()
