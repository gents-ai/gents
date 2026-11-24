"""Calibrate the performance verdict with controlled measurements."""

import copy
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

from compare import compare


FAMILIES = ("transaction", "scan_commit", "append", "read_rules", "txn_callbacks", "sync")


def run(value=100.0):
    families = {}
    for name in FAMILIES:
        families[name] = {
            "trust": "clean", "context": {"quick": False},
            "rows": [{"key": "work", "name": name + " work", "unit": "commits/s",
                      "higher_is_better": True, "median": value,
                      "min": value - 1, "max": value + 1}],
        }
    return {
        "schema_version": 2,
        "run": {"loadguard": {"passed": True}, "host": {"cpu": "test", "cores": 16}},
        "metrics": {
            "throughput": {"workloads": {"read": {
                "trust": "clean", "points": [{"threads": 1, "ops": 100,
                                                "min": 99, "max": 101}]}}},
            "timings": families,
        },
    }


def row(doc, family="transaction"):
    return doc["metrics"]["timings"][family]["rows"][0]


def delta(report, family="transaction"):
    return next(d for d in report.deltas if d.family == family)


class TimingVerdicts(unittest.TestCase):
    def test_a_transaction_throughput_drop_is_a_regression(self):
        before, after = run(), run()
        row(after).update(median=50, min=49, max=51)
        report = compare(before, after)
        self.assertEqual(report.verdict, "regressed")
        self.assertEqual(delta(report).pct, -50)

    def test_latency_increase_is_a_regression(self):
        before, after = run(), run()
        for doc in (before, after):
            row(doc, "scan_commit").update(unit="us", higher_is_better=False)
        row(after, "scan_commit").update(median=150, min=149, max=151)
        self.assertEqual(delta(compare(before, after), "scan_commit").verdict, "regressed")

    def test_every_family_participates_in_the_verdict(self):
        for family in FAMILIES:
            with self.subTest(family=family):
                before, after = run(), run()
                row(after, family).update(median=50, min=49, max=51)
                self.assertEqual(delta(compare(before, after), family).verdict, "regressed")

    def test_the_actual_write_harness_schema_produces_verdicts(self):
        before, after = run(), run()
        for doc, value in [(before, 100), (after, 50)]:
            doc["metrics"]["throughput"]["workloads"]["buffered_write"] = {
                "trust":"clean", "reps":9, "by_threads":[{
                    "threads":16, "ops_per_sec_median":value, "ops_per_sec_min":value - 1,
                    "ops_per_sec_max":value + 1, "stability":"stable"}]}
        report = compare(before, after)
        self.assertTrue(any(d.name == "buffered_write @ 16t" and d.verdict == "regressed"
                            for d in report.regressions))

    def test_read_counts_are_not_mistaken_for_read_rates_or_ranges(self):
        before, after = run(), run()
        for doc in (before, after):
            doc["metrics"]["throughput"]["workloads"]["read"] = {
                "trust":"clean", "threads":[{"threads":1, "ops":1000000, "ops_per_s":50}]}
        measured = delta(compare(before, after), "throughput")
        self.assertEqual(measured.before, 50)
        self.assertEqual(measured.verdict, "unverified")

    def test_unsupported_throughput_shapes_are_explicit_gaps(self):
        before, after = run(), run()
        after["metrics"]["throughput"]["workloads"]["scan"] = {
            "trust":"clean", "stream_mib_per_s":500}
        report = compare(before, after)
        self.assertTrue(any(d.name == "scan" and d.verdict == "unverified" for d in report.deltas))

    def test_overlapping_samples_are_noise(self):
        before, after = run(), run()
        row(after).update(median=90, min=89, max=105)
        self.assertEqual(delta(compare(before, after)).verdict, "noise")

    def test_small_changes_are_noise(self):
        before, after = run(), run()
        row(before).update(min=100, max=100)
        row(after).update(median=99, min=99, max=99)
        self.assertEqual(delta(compare(before, after)).verdict, "noise")

    def test_known_insufficient_repetitions_are_unverified(self):
        before, after = run(), run()
        for doc in (before, after):
            doc["metrics"]["timings"]["transaction"]["context"]["reps"] = 1
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_a_failed_workload_is_not_a_throughput_improvement(self):
        before, after = run(), run()
        row(after).update(median=200, min=199, max=201,
                          invalid_reason="some operations were abandoned")
        report = compare(before, after)
        self.assertEqual(delta(report).verdict, "unverified")
        self.assertEqual(report.verdict, "unverified")

    def test_complete_unchanged_measurements_pass(self):
        self.assertEqual(compare(run(), run()).verdict, "pass")

    def test_a_missing_family_cannot_pass(self):
        before, after = run(), run()
        del after["metrics"]["timings"]["transaction"]
        report = compare(before, after)
        self.assertEqual(delta(report).verdict, "unverified")
        self.assertEqual(report.verdict, "unverified")

    def test_a_removed_row_is_reported(self):
        before, after = run(), run()
        after["metrics"]["timings"]["transaction"]["rows"] = []
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_unknown_families_are_explicit_gaps(self):
        before, after = run(), run()
        after["metrics"]["timings"]["new_family"] = copy.deepcopy(
            after["metrics"]["timings"]["transaction"])
        self.assertEqual(delta(compare(before, after), "new_family").verdict, "unverified")

    def test_missing_ranges_are_not_a_measured_difference(self):
        before, after = run(), run()
        del row(after)["max"]
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_invalid_measurements_are_unverified(self):
        for value in (float("nan"), float("inf"), -1, None):
            with self.subTest(value=value):
                before, after = run(), run()
                row(after)["median"] = value
                self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_invalid_ranges_are_unverified(self):
        before, after = run(), run()
        row(after).update(min=120, max=80)
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_busy_hosts_never_certify_transaction_timings(self):
        before, after = run(), run()
        after["run"]["loadguard"]["passed"] = False
        row(after).update(median=50, min=49, max=51)
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_unknown_trust_is_unverified(self):
        before, after = run(), run()
        after["metrics"]["timings"]["transaction"]["trust"] = "maybe"
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_quick_and_full_runs_are_not_compared(self):
        before, after = run(), run()
        after["metrics"]["timings"]["transaction"]["context"]["quick"] = True
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_runner_trust_is_independent_of_the_collector(self):
        before, after = run(), run()
        for doc in (before, after):
            doc["run"]["loadguard"]["passed"] = False
            doc["metrics"]["timings"]["transaction"]["measurement"] = {
                "guard_passed":True, "toolchain":"test compiler", "host":{"cpu":"test", "cores":16}}
        self.assertEqual(delta(compare(before, after)).verdict, "noise")
        after["metrics"]["timings"]["transaction"]["measurement"]["guard_passed"] = False
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_actual_benchmark_runners_must_match(self):
        before, after = run(), run()
        for doc, cpu in [(before, "first CPU"), (after, "second CPU")]:
            doc["metrics"]["timings"]["transaction"]["measurement"] = {
                "guard_passed":True, "toolchain":"test compiler", "host":{"cpu":cpu, "cores":16}}
        measured = delta(compare(before, after))
        self.assertEqual(measured.verdict, "unverified")
        self.assertIn("host cpu differs", measured.reason)

    def test_different_hosts_are_not_compared(self):
        before, after = run(), run()
        after["run"]["host"]["cpu"] = "another CPU"
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_units_are_part_of_the_contract(self):
        before, after = run(), run()
        row(after)["unit"] = "ns"
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_missing_host_details_do_not_certify_a_timing(self):
        before, after = run(), run()
        for doc in (before, after):
            del doc["run"]["host"]["cpu"]
        self.assertEqual(delta(compare(before, after)).verdict, "unverified")

    def test_schema_versions_are_checked(self):
        before, after = run(), run()
        after["schema_version"] = 99
        with self.assertRaisesRegex(ValueError, "schema_version"):
            compare(before, after)

    def test_legacy_transaction_ranges_can_be_compared(self):
        before, after = run(), run()
        for doc, value in [(before, 100), (after, 50)]:
            doc["schema_version"] = 1
            del doc["metrics"]["timings"]
            doc["metrics"]["correctness"] = {
                "trust": "clean", "quick": False,
                "matrix": [{"flavor": "optimistic", "isolation": "defralevel",
                            "throughput": {
                                "contended_threads": 8, "hot_keys": 8,
                                "uncontended_commits_per_s": {
                                    "median": value, "min": value - 1, "max": value + 1},
                                "contended_commits_per_s": {
                                    "median": value, "min": value - 1, "max": value + 1}}}],
            }
        self.assertEqual(delta(compare(before, after)).verdict, "regressed")

    def test_duplicate_rows_are_refused(self):
        before, after = run(), run()
        after["metrics"]["timings"]["transaction"]["rows"] *= 2
        with self.assertRaisesRegex(ValueError, "duplicate"):
            compare(before, after)

    def test_the_cli_fails_on_a_transaction_regression(self):
        before, after = run(), run()
        row(after).update(median=50, min=49, max=51)
        with tempfile.TemporaryDirectory() as directory:
            base, cur = (pathlib.Path(directory) / name for name in ("base.json", "cur.json"))
            base.write_text(json.dumps(before))
            cur.write_text(json.dumps(after))
            result = subprocess.run(
                [sys.executable, str(pathlib.Path(__file__).with_name("compare.py")),
                 "--baseline", str(base), "--current", str(cur), "--fail-on-regression"],
                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("REGRESSED", result.stdout)


if __name__ == "__main__":
    unittest.main()
