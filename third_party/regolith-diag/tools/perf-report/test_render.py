"""Both schema generations render timing coverage without hiding gaps."""

import pathlib
import tempfile
import unittest

from render_html import render, spark_bars
from test_compare import run


class RenderTimings(unittest.TestCase):
    def test_a_throughput_drop_is_colored_as_a_regression(self):
        bars = spark_bars([("commit", 100, 50, "commits/s")], higher_is_better=True)
        self.assertIn('class="d-regressed"', bars)

    def test_timing_families_and_unverified_ranges_are_visible(self):
        before, after = run(), run()
        after["metrics"]["timings"]["read_rules"]["rows"][0]["max"] = None
        with tempfile.TemporaryDirectory() as directory:
            page, report = render(after, before, pathlib.Path(directory) / "report.html")
        self.assertEqual(report.verdict, "unverified")
        self.assertIn("Transaction and mechanism timings", page)
        self.assertIn("not recorded", page)
        self.assertIn("read rules", page)
        self.assertIn("schema v2", page)

    def test_absent_families_render_as_gaps(self):
        current = {"schema_version": 1, "metrics": {}}
        with tempfile.TemporaryDirectory() as directory:
            page, report = render(current, None, pathlib.Path(directory) / "report.html")
        self.assertIsNone(report)
        self.assertIn("not collected in this run", page)
        self.assertIn("schema v1", page)

    def test_invalid_measurements_render_as_unavailable(self):
        before, after = run(), run()
        after["metrics"]["timings"]["transaction"]["rows"][0]["median"] = float("nan")
        with tempfile.TemporaryDirectory() as directory:
            page, report = render(after, before, pathlib.Path(directory) / "report.html")
        self.assertEqual(report.verdict, "unverified")
        self.assertIn("n/a", page)

    def test_workload_names_are_escaped(self):
        current = run()
        current["metrics"]["timings"]["transaction"]["rows"][0]["name"] = "<script>bad()</script>"
        with tempfile.TemporaryDirectory() as directory:
            page, _ = render(current, None, pathlib.Path(directory) / "report.html")
        self.assertIn("&lt;script&gt;", page)
        self.assertNotIn("<script>bad()", page)
