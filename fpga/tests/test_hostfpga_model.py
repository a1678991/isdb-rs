"""The timed host-FPGA model (`tools/hostfpga_model.py`, issue #30 item 4):
its reproductions of the board's findings and the properties it checks.

No simulator, no vectors: a discrete-event model in the standard library,
seeded, a few seconds in all. The reproductions are what the model must
show before it is trusted (fpga/README.md "A timed host–FPGA model"):

(a) build 6's latch moving every frame against the host's 15-frame read:
    every set torn, no taps; build 7's SUMS_CAPTURE: the first write about
    0.9 s after the open, in force from frame about 26;
(b) #27 / #29: a fraction of the sets reads a foreign word with the host's
    per-buffer overflow poll or `api_controller.sh` running, none with
    both off, and PR #31's verified reads catch them (rereads);
(c) the stream path (`Slots` format 2): the first taps in force from frame
    4 or 5, no register read exposed.

The properties: every multi-word value the host solves from is one
snapshot; every polled live value is a single word or versioned; no host
sequence waits for ever (bounded first taps, a solve every period, a
stretch without a layout ending in a recovery); the blocks the host waits
for arrive in bounded time.
"""

import os
import statistics
import sys
import unittest
from dataclasses import replace

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "tools"))

import hostfpga_model as hm  # noqa: E402

S = hm.SCENARIOS
_CACHE = {}


def run(name, seed=1, **changes):
    key = (name, seed, tuple(sorted(changes.items())))
    if key not in _CACHE:
        sc = replace(S[name], **changes) if changes else S[name]
        _CACHE[key] = hm.run(sc, seed)
    return _CACHE[key]


def runs(name, seeds=4):
    return [run(name, s) for s in range(1, seeds + 1)]


class EngineTest(unittest.TestCase):
    def test_deterministic(self):
        a = hm.run(S["issue29-C"], 3)
        b = hm.run(S["issue29-C"], 3)
        self.assertEqual(a, b)

    def test_shared_address_race(self):
        """Two clients: a foreign address write between a read's two
        requests returns the other register; outside that window not."""
        sim = hm.Sim(1)
        sc = S["issue29-E1"]
        fpga = hm.Fpga(sim, sc, hm.Stats())
        dev = hm.Device(sim, fpga, None)
        p = replace(hm.ISSUE29, jitter=0.0)
        host = hm.Client(sim, dev, "host", p)
        got = []

        def reader():
            v = yield from host.read(hm.ADC_VERSION)
            got.append(v)

        def foreign(at):
            yield at
            dev.set_addr(hm.ADC_STATUS)

        # the address write applies at wire/2 + iiod, the read samples wire
        # later: a foreign address write in between moves it
        apply = p.wire_s / 2 + p.iiod_s
        for at, foreign_seen in ((apply + p.wire_s / 2, True), (apply / 2, False),
                                 (apply + p.wire_s * 1.5, False)):
            sim.spawn(reader())
            sim.spawn(foreign(at))
            sim.run(sim.now + 0.01)
            self.assertEqual(got[-1][2], foreign_seen, at)

    def test_overrides(self):
        sc = hm.override(S["build9"], ["profile.wire_s=50e-6", "host.buffer_words=262144", "seconds=5.0"])
        self.assertEqual(sc.profile.wire_s, 50e-6)
        self.assertEqual(sc.host.buffer_words, 262144)
        self.assertEqual(sc.seconds, 5.0)


class RegisterMapTest(unittest.TestCase):
    """The rule of item 5 applied to the map: a multi-word value the host
    reads is snapshotted and versioned, and held at least as long as the
    host takes to read it."""

    def test_the_map(self):
        spans = sorted((a, a + n, name) for name, a, n, _, _ in hm.REGISTERS)
        for (a0, a1, n0), (b0, _, n1) in zip(spans, spans[1:]):
            self.assertLessEqual(a1, b0, f"{n0} overlaps {n1}")
        kinds = {"const", "config", "live", "frame", "latched", "version"}
        for name, _, _, kind, _ in hm.REGISTERS:
            self.assertIn(kind, kinds, name)
        self.assertEqual(len(hm.SUM_REGS), hm.SUM_WORDS)

    def test_groups_today(self):
        for image, host in ((hm.BUILD7, hm.HOST_BUILD8), (hm.BUILD9, hm.HOST_VERIFIED),
                            (hm.FORMAT2, hm.HOST_STREAM)):
            self.assertEqual(hm.check_groups(image, hm.BENCH, host), [], image.name)

    def test_build6_violates_the_rule(self):
        v = hm.check_groups(hm.BUILD6, hm.BENCH, hm.HOST_BUILD8)
        self.assertEqual([g for g, _ in v], ["training sums"])
        self.assertIn("every set torn", v[0][1])

    def test_allowed_groups_have_reasons(self):
        names = {g["name"] for g in hm.GROUPS}
        for g, why in hm.GROUPS_ALLOWED.items():
            self.assertIn(g, names)
            self.assertGreater(len(why), 40)

    def test_polled_live_values_are_single_words(self):
        """Each live register the register path polls every tick is one
        word used on its own (CARRIER reported, STATUS the recovery, FRAME
        the identity's TAPS_FROM, TAPS_FROM logged): none is part of a
        multi-word value, so none needs a version."""
        multi = {r for g in hm.GROUPS for r in g["regs"] if g["name"] != "training sums"}
        multi |= {"SUMS_R", "SUMS_P", "SUMS_COUNT"}
        polled = hm.polled_live(hm.BUILD9, hm.HOST_VERIFIED)
        self.assertEqual({n for n, _ in polled}, {"CARRIER", "TAPS_FROM", "STATUS", "FRAME"})
        for name, _ in polled:
            self.assertNotIn(name, multi)
        self.assertEqual(hm.polled_live(hm.FORMAT2, hm.HOST_STREAM), [])


class TornReadsTest(unittest.TestCase):
    """(a): build 6 and build 7 on this project's bench."""

    def test_build6_every_set_torn_and_no_taps(self):
        r = run("build6")
        # the board: 46 sets in 30 s, all torn
        self.assertGreaterEqual(r["sets_read"], 40)
        self.assertEqual(r["sets_torn"], r["sets_read"])
        self.assertEqual(r["sets_mixed"], r["sets_read"])
        self.assertEqual(r["solves"], 0)
        self.assertIsNone(r["first_in_force_frame"])
        self.assertTrue(hm.liveness(S["build6"], r))

    def test_build7_the_capture_brings_the_taps(self):
        r = run("build7")
        # the board: first write 0.90 s after the open (frame 5's sums), in
        # force from frame 26; 12 solves in 24 ticks
        self.assertAlmostEqual(r["first_write_t"], 0.90, delta=0.06)
        self.assertIn(r["first_write_from"], (4, 5, 6))
        self.assertIn(r["first_in_force_frame"], range(25, 29))
        self.assertGreaterEqual(r["solves"], 10)
        self.assertEqual(r["solved_mixed"], 0)
        self.assertEqual(hm.liveness(S["build7"], r), [])

    def test_build9_verified_reads_cost_eight_frames(self):
        r = run("build9")
        # the board: 200 written in 400 ticks in 600 s; first set in force
        # at frame 33-34 (Build 9 residuals: "+33-34 frames")
        self.assertIn(r["solves"], range(190, 215))
        self.assertIn(r["first_in_force_frame"], range(32, 36))
        self.assertEqual(r["solved_mixed"], 0)
        self.assertEqual(r["solved_wrong"], 0)
        self.assertEqual(r["unsettled"], 0)


class ForeignReadsTest(unittest.TestCase):
    """(b): #27 / #29 on the reporter's board (profile ISSUE29)."""

    def test_e2_positive_control(self):
        """The fit of the profile's window: 34 of 50 sets, 52 words."""
        rs = runs("issue29-E2")
        share = statistics.mean(r["sets_detected"] / r["sets_read"] for r in rs)
        words = statistics.mean(r["detected_words"] / r["sets_read"] for r in rs)
        self.assertAlmostEqual(share, 34 / 50, delta=0.12)
        self.assertAlmostEqual(words, 52 / 50, delta=0.3)

    def test_script_alone(self):
        """Run C (7 of 146) and E3 (7 of 158) with api_controller.sh."""
        rs = runs("issue29-C")
        det = statistics.mean(r["sets_detected"] for r in rs)
        self.assertGreaterEqual(det, 2.5)
        self.assertLessEqual(det, 14)
        for r in rs:
            self.assertGreater(r["sets_read"], 140)

    def test_both_off(self):
        """E1: 0 of 163."""
        for r in runs("issue29-E1", 2):
            self.assertEqual(r["sets_foreign"], 0)
            self.assertEqual(r["sets_detected"], 0)
            self.assertEqual(r["reads_wrong"], 0)

    def test_the_hosts_own_poll(self):
        """Run B: 47 of 50 sets, 186 words (3.7 a set)."""
        rs = runs("issue27-B", 2)
        for r in rs:
            self.assertGreater(r["sets_detected"] / r["sets_read"], 0.85)
            self.assertGreater(r["detected_words"] / r["sets_read"], 2.5)
            self.assertLess(r["detected_words"] / r["sets_read"], 6.0)
            self.assertGreater(r["overflow_flags"], 0)  # false flags, no loss

    def test_verified_reads_catch_them(self):
        """PR #31: every foreign value read again, none accepted, none
        solved; with nobody else on the device, no reread at all."""
        for name in ("issue27-B-verified", "issue29-C-verified"):
            for r in runs(name, 2):
                self.assertGreater(r["rereads"], 0, name)
                self.assertEqual(r["reads_wrong"], 0, name)
                self.assertEqual(r["solved_wrong"], 0, name)
                self.assertEqual(r["unsettled"], 0, name)
        r = run("build9-quiet")
        self.assertEqual(r["rereads"], 0)
        self.assertEqual(r["sets_foreign"], 0)

    def test_the_bench_the_same(self):
        """Build 9 with api_controller.sh on this bench: 1 reread in 65 201
        reads (the model: a few in 600 s, none accepted)."""
        rs = runs("build9", 3)
        mean = statistics.mean(r["rereads"] for r in rs)
        self.assertLessEqual(mean, 6)
        for r in rs:
            self.assertEqual(r["reads_wrong"], 0)


class StreamPathTest(unittest.TestCase):
    """(c): the sums in the `Slots` stream."""

    def test_first_taps_from_frame_4(self):
        for name in ("stream", "stream-issue29"):
            r = run(name)
            self.assertEqual(r["first_write_from"], 2, name)
            self.assertIn(r["first_in_force_frame"], (4, 5), name)
            self.assertLess(r["first_write_t"], 0.25, name)

    def test_no_exposed_read(self):
        """With api_controller.sh running: no sum, SUMS_FRAME or status
        register read; the loop's only reads are `gpio_out`'s at attach."""
        r = run("stream")
        self.assertEqual(r["sets_read"], 0)
        self.assertLessEqual(r["exposed"]["loop"], 2)
        self.assertEqual(r["reads_wrong"], 0)
        self.assertEqual(hm.check_groups(hm.FORMAT2, hm.BENCH, hm.HOST_STREAM), [])

    def test_every_sums_block_arrives_in_bounded_time(self):
        r = run("stream")
        # one per decoded frame, each within a buffer's fill at the decoded
        # rate (65 536 words at 6.35 M words/s), the blocks' delay and the
        # transfer
        self.assertEqual(r["sums_blocks"], r["sums_emitted"])
        sig = hm.Signal()
        bound = (65536 / (sig.decoded_words / sig.frame_s) + sig.block_delay_slots * sig.frame_s / 120
                 + sig.buffer_net_s + sig.consumer_s + 1e-3)
        self.assertLess(r["sums_latency_max"], bound)
        self.assertLess(r["status_latency_max"], bound)

    def test_a_larger_buffer_delays_the_first_taps(self):
        """Build 8's 262 144-word buffers: the sums block waits up to 41 ms."""
        sc = replace(S["stream"], host=replace(hm.HOST_STREAM, buffer_words=262144))
        r = hm.run(sc)
        self.assertIn(r["first_in_force_frame"], (4, 5, 6))
        self.assertGreater(r["sums_latency_max"], 0.03)


class PropertiesTest(unittest.TestCase):
    """Over every scenario."""

    def test_every_solved_set_is_one_snapshot(self):
        for name in S:
            r = run(name)
            self.assertEqual(r["solved_mixed"], 0, name)

    def test_no_sequence_waits_for_ever(self):
        found = {}
        for name, sc in S.items():
            problems = hm.liveness(sc, run(name))
            if problems:
                found[name] = problems
        self.assertEqual(set(found), set(hm.KNOWN_GAPS), found)

    def test_a_no_layout_stretch_ends_in_a_recovery(self):
        for name in ("nolayout-registers", "nolayout-stream"):
            r = run(name)
            sc = S[name]
            self.assertEqual(r["recoveries"], 1, name)
            a, b = sc.signal.no_layout[0]
            start = sc.signal.lock_s + a * sc.signal.frame_s
            # within the loop's period (the first tick that sees it),
            # recover_after, the polls and (on the stream path) the stale
            # time of the last status block
            h = sc.host
            t = r["recover_t"][0]
            self.assertGreater(t, start + h.recover_after_s, name)
            self.assertLess(t, start + h.every_s + h.recover_after_s + h.stream_stale_s + 2 * h.poll_s, name)
            self.assertEqual(hm.liveness(sc, r), [], name)
        # on the stream path the status blocks of the stretch arrive in
        # bursts (a 65 536-word buffer takes about 3 s at the no-layout rate)
        r = run("nolayout-stream")
        self.assertGreater(r["status_gaps_max"], S["nolayout-stream"].host.stream_stale_s)

    def test_the_hold_is_released_at_start(self):
        """dedbc14: a hold a register-path run left no longer starves the
        stream path."""
        self.assertTrue(hm.liveness(S["stream-held-prefix"], run("stream-held-prefix")))
        r = run("stream-held")
        self.assertEqual(hm.liveness(S["stream-held"], r), [])
        self.assertIn(r["first_in_force_frame"], (4, 5))

    def test_the_old_sum_read_shows_a_transient_0_to_a_fast_host(self):
        """#27 item 4: the old HDL's READ showed 0 for up to 4 400 symbol
        clocks: invisible over iiod (0.4 ms and more between the READ and
        the read), wrong words for a host on the board, even verified (two
        reads within the window agree on 0); none with build 9's READ."""
        self.assertEqual(run("issue29-E1")["transient_zero"], 0)
        old = run("local-old-hdl")
        self.assertGreater(old["transient_zero"], 0)
        self.assertGreater(old["solved_wrong"], 0)
        new = run("local-new-hdl")
        self.assertEqual(new["transient_zero"], 0)
        self.assertEqual(new["solved_wrong"], 0)

    def test_torn_by_the_no_layout_bit(self):
        """A finding: the register path compares SUMS_CAPTURE's whole word
        before and after the sum words, and its NO_LAYOUT bit follows the
        layout while a capture is held, so a layout lost during a set
        discards a good held set as torn (and warns that the capture did
        not hold)."""
        r = run("nolayout-registers")
        self.assertEqual(r["torn_no_layout_only"], 1)
        self.assertEqual(r["solved_mixed"], 0)


if __name__ == "__main__":
    unittest.main()
