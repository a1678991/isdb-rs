"""Value coverage of the chain and LDPC vector sets (`tools/value_coverage.py`,
issue #30 item 1): a field whose expected value is the same in every set is
a field no simulation checks (n0 at its reset value and the absent learned
points of builds 6-8). Fails on a constant or absent field that is neither
accepted (`value_coverage.ALLOWED`) nor a documented gap of the corpus
(`value_coverage.KNOWN_GAPS`), and on a known gap the corpus now covers.

Runs on the default synthetic corpus (`mise run fpga:vectors` and
`fpga:ldpc-vectors` write it into fpga/build; FPGA_VECTORS names another
directory) and skips without it; the recording sets (`*-rec-*`) are left
out. Pure JSON parsing: a few seconds. The markdown report is written to
`build/value_coverage.md`.
"""

import fnmatch
import functools
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "tools"))

import value_coverage as vc  # noqa: E402

VECTORS = os.environ.get("FPGA_VECTORS", os.path.join(os.path.dirname(__file__), "..", "build"))
MISSING = vc.missing_required(VECTORS)


@functools.lru_cache(maxsize=None)
def coverage():
    return vc.coverage(VECTORS, synthetic=True)


class ValueCoverageListsTest(unittest.TestCase):
    """The lists themselves (no vectors needed)."""

    def test_every_entry_names_a_field_and_a_reason(self):
        for table in (vc.ALLOWED, vc.KNOWN_GAPS):
            for pat, why in table.items():
                self.assertTrue(any(fnmatch.fnmatchcase(n, pat) for n in vc.SPECS),
                                f"{pat}: no such field")
                self.assertTrue(why.strip(), f"{pat}: no reason")

    def test_no_field_is_both_accepted_and_a_gap(self):
        for n in vc.SPECS:
            a = [p for p in vc.ALLOWED if fnmatch.fnmatchcase(n, p)]
            g = [p for p in vc.KNOWN_GAPS if fnmatch.fnmatchcase(n, p)]
            self.assertFalse(a and g, f"{n}: in ALLOWED ({a}) and KNOWN_GAPS ({g})")


@unittest.skipIf(MISSING, f"vector sets missing from {VECTORS}: {', '.join(MISSING[:4])}"
                 + (" …" if len(MISSING) > 4 else "")
                 + " (mise run fpga:vectors fpga:ldpc-vectors)")
class ValueCoverageTest(unittest.TestCase):
    def test_constant_fields_are_excused_and_gaps_open(self):
        cov = coverage()
        try:
            with open(os.path.join(VECTORS, "value_coverage.md"), "w") as f:
                f.write(vc.markdown(cov, all_fields=True))
        except OSError:
            pass
        bad = [f"{f.name}: {f.label} ({vc._fmt(f.values[0]) if f.values else 'no value'}; "
               f"sets {', '.join(f.per_set) or 'none'})" for f in cov.unexcused()]
        self.assertEqual(bad, [], "constant or absent fields neither in value_coverage.ALLOWED "
                         "(with the reason it cannot vary) nor in KNOWN_GAPS (with the input "
                         "that would vary it)")
        self.assertEqual(cov.stale_gaps(), [], "known gaps the corpus now covers: remove them "
                         "from value_coverage.KNOWN_GAPS")
        # the gate saw the whole corpus
        self.assertGreaterEqual(len(cov.sets), len(vc.REQUIRED))


if __name__ == "__main__":
    unittest.main()
