"""The formal checks (isdb_fpga/formal/, fpga/README.md "Formal checks",
issue #30 item 6): every harness's sby tasks pass (bmc, cover, prove),
and each fixed defect re-introduced on a scratch copy of its module
(`formal.BREAKS`) fails the bounded check with a counter-example.

The tools are the locked environment's YoWASP wheels (yowasp-yosys's
sby and smtbmc, yowasp-boolector); about 30 s of solver time in all on
one core of the 7950X. Run alone: `uv run --locked pytest -q
tests/test_formal.py`, or `mise run fpga:formal` (tools/formal.py, with
the counter-examples' tables).
"""

import os
import shutil
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga import formal  # noqa: E402


def _have_tools():
    bindir = os.path.dirname(sys.executable)
    return all(os.path.exists(os.path.join(bindir, n)) or shutil.which(n)
               for n in ("yowasp-sby", "yowasp-yosys", "yowasp-boolector"))


needs_tools = unittest.skipUnless(_have_tools(), "no YoWASP sby / boolector (cd fpga && uv sync)")


@needs_tools
class FormalTest(unittest.TestCase):
    def check(self, name):
        results = formal.run(name)
        self.assertEqual([r.task for r in results], formal.tasks_of(name))
        for r in results:
            with self.subTest(task=r.task):
                self.assertEqual(r.status, "PASS", "\n".join(r.summary))

    def test_banks(self):
        self.check("banks")

    def test_packer(self):
        self.check("packer")

    def test_frame(self):
        self.check("frame")


@needs_tools
class FormalBitesTest(unittest.TestCase):
    """The checks fail on the defects they are for (the fix reverted)."""

    def check(self, brk, message):
        [r] = formal.run(formal.BREAKS[brk][0], tasks=["bmc"], brk=brk)
        print(f"{brk}: {r.status}", *r.summary, sep="\n    ")
        self.assertEqual(r.status, "FAIL", "\n".join(r.summary))
        self.assertTrue(r.trace and os.path.exists(r.trace), r.trace)
        self.assertTrue(any(f'"{message}"' in s for s in r.summary), r.summary)

    def test_bank_leak_e95c563(self):
        self.check("e95c563", "after abort: the context holds no bank and its bank is free")

    def test_begin_takes_a_second_bank_e95c563(self):
        self.check("e95c563-begin", "alloc = the banks held by contexts + committed not freed")

    def test_packer_start_1817079(self):
        self.check("1817079", "a block offered while idle is taken: busy the cycle after")

    def test_packer_start_mid_block_1817079(self):
        self.check("1817079-latch", "a block's seq while it leaves is its number")

    def test_restart_leaves_the_frame_open(self):
        self.check("restart-open", "a restart closes the open frame")


if __name__ == "__main__":
    unittest.main()
