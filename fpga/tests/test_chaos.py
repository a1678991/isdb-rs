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
    def test_restart_just_after_a_slot_head(self):
        # A defect the chaos runs found (README "Chaos runs"), fixed: START
        # 5 symbols after a slot head of chain-7 makes the sync lock with
        # its heads at position 9 291 of its 9 296-symbol periods, so a
        # period end decides 4 symbols after each head while stage 3 still
        # sets up the head's slot; stage 3 took the status snapshot when it
        # queued the slot, the period end's, and every later status block
        # had the next period's peak and sum (words 0-5). Stage 3 now keeps
        # the head's own snapshot (`SlotPhase.h_snap_valid`;
        # tests/test_chain.py PhaseTest.test_snapshot_of_the_head).
        events = chaos.place("7-hdl", [{"kind": "start", "at": 1000 + 54 * 9296 + 5}])
        r = chaos.run_chaos("7-hdl", "head5", events=events, backend=COMPILED)
        record(self, r)
        self.assertTrue(all(v == "equal" for v in r["frames"].values()), r["frames"])


@unittest.skipUnless(have("7-hdl"), "no chain-7-hdl (mise run fpga:vectors)")
class CompareTest(unittest.TestCase):
    """`chaos.compare`'s restart-epoch allowance: a frame the chain alone
    decoded (its slot blocks come from a layout kept across an undrained
    restart) is excused its extra slot blocks only; its TMCC block is still
    compared, and its status block but for the counters."""

    def setUp(self):
        from isdb_fpga.chain import slots as S

        doc = chaos.vector("7-hdl")
        self.S = S
        self.words = doc["slots_words"]
        blocks, err = chaos.parse(self.words)
        self.assertIsNone(err)
        self.blocks = blocks
        frames = sorted({b[3] for b in blocks if b[1] == S.TMCC})
        self.assertGreaterEqual(len(frames), 3)
        self.f0, self.f1 = frames[0], frames[1]
        # frame f0 handed out at 1000, an undrained restart one symbol later:
        # f0 ambiguous, f1 and f1 + 1 in its restart epoch
        self.model = {"slots_words": self.words,
                      "frames": [{"frame": f, "done": 1000 + 100 * i} for i, f in enumerate(frames)],
                      "restarts": []}
        self.events = [{"kind": "short", "stream": 1001}]
        # a slot block of frame f1, which the model does not decode
        slot = next(b for b in blocks if b[1] == S.SLOT)
        self.extra = (slot[0], S.SLOT, slot[2], self.f1, slot[4])

    def test_extra_slots_of_an_epoch_frame_are_excused(self):
        problems, findings, outcomes = chaos.compare(self.model, self.blocks + [self.extra], self.events)
        self.assertEqual(problems, [])
        self.assertEqual(outcomes[self.f1], "restart epoch (TMCC equal)")
        self.assertTrue(any("restart epoch" in f for f in findings), findings)

    def test_a_missing_tmcc_of_an_epoch_frame_is_a_problem(self):
        hdl = [b for b in self.blocks if not (b[1] == self.S.TMCC and b[3] == self.f1)] + [self.extra]
        problems, _, outcomes = chaos.compare(self.model, hdl, self.events)
        self.assertEqual(outcomes[self.f1], "DIFFERS")
        self.assertTrue(any("missing" in p and f"({self.f1}, {self.S.TMCC}," in p for p in problems), problems)

    def test_a_corrupted_tmcc_of_an_epoch_frame_is_a_problem(self):
        hdl = []
        for b in self.blocks:
            if b[1] == self.S.TMCC and b[3] == self.f1:
                pay = list(b[4])
                pay[4] ^= 1  # a TMCC information bit
                b = (b[0], b[1], b[2], b[3], pay)
            hdl.append(b)
        problems, _, outcomes = chaos.compare(self.model, hdl + [self.extra], self.events)
        self.assertEqual(outcomes[self.f1], "DIFFERS")
        self.assertTrue(any(f"({self.f1}, {self.S.TMCC}," in p for p in problems), problems)


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
