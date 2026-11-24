"""Exercise the real collector executable with controlled family records."""

import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


class Collector(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = os.environ.get("REGOLITH_COLLECT_BIN")
        if not cls.binary:
            raise unittest.SkipTest("set REGOLITH_COLLECT_BIN to run collector integration checks")

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = pathlib.Path(self.directory.name)

    def collect(self, records, guard=0, out="run.json"):
        source = self.root / "records.jsonl"
        source.write_text("".join(json.dumps(record) + "\n" for record in records))
        check = self.root / "guard.py"
        check.write_text(f"raise SystemExit({guard})\n")
        env = dict(os.environ, REGOLITH_BENCH_OUT=str(source), REGOLITH_LOADGUARD=str(check),
                   CARGO_TARGET_DIR=str(self.root / "target"))
        result = subprocess.run(
            [self.binary, "--out", str(self.root / out), "--commit", "synthetic",
             "--label", "collector calibration"], env=env, cwd=self.root,
            capture_output=True, text=True, timeout=15)
        return result

    def test_new_families_are_collected_with_normalized_rows(self):
        data = {
            "scan_commit": [{"keys":32, "isolation":"defralevel", "commit_us":2,
                             "scan_and_commit_us":5}],
            "read_rules": [{"name":"get", "value":10, "unit":"ns/read"}],
            "txn_callbacks": [{"case":"commit", "us":2, "min_us":1}],
            "append": {"allocate":{"latency_ns_p99":{"median":10, "min":9, "max":11}}},
            "sync": {"benchmarks":[{"benchmark":"sync_uncontended/mutex", "median_ns":4}]},
        }
        result = self.collect([{"family": name, "data": value} for name, value in data.items()])
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads((self.root / "run.json").read_text())
        self.assertEqual(doc["schema_version"], 2)
        for name, source in data.items():
            with self.subTest(name=name):
                family = doc["metrics"]["timings"][name]
                self.assertEqual(family["source"], source)
                self.assertTrue(family["rows"])
                self.assertEqual(family["trust"], "clean")

    def test_busy_hosts_contaminate_timings_and_keep_correctness_separate(self):
        records = [
            {"family":"transaction", "data":{"matrix":[{"flavor":"optimistic",
             "isolation":"defralevel", "throughput":{
                 "uncontended_commits_per_s":{"median":10,"min":9,"max":11}}}]}},
            {"family":"isolation", "data":{"matrix":[]}},
        ]
        result = self.collect(records, guard=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads((self.root / "run.json").read_text())
        self.assertFalse(doc["run"]["loadguard"]["passed"])
        self.assertEqual(doc["metrics"]["timings"]["transaction"]["trust"], "contaminated")
        self.assertEqual(doc["metrics"]["isolation"]["trust"], "contaminated")
        self.assertEqual(doc["metrics"]["correctness"]["trust"], "clean")

    def test_missing_families_are_absent(self):
        result = self.collect([])
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads((self.root / "run.json").read_text())
        for family in doc["metrics"]["timings"].values():
            self.assertEqual(family["trust"], "absent")
            self.assertEqual(family["rows"], [])

    def test_a_quiet_collector_cannot_clear_a_busy_benchmark_runner(self):
        records = [{"family":"transaction", "data":{
            "timing_trust":"contaminated", "matrix":[{"throughput":{}}]}}]
        result = self.collect(records, guard=0)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads((self.root / "run.json").read_text())
        self.assertTrue(doc["run"]["loadguard"]["passed"])
        self.assertEqual(doc["metrics"]["timings"]["transaction"]["trust"], "contaminated")
        self.assertEqual(doc["metrics"]["correctness"]["trust"], "clean")

    def test_a_busy_collector_does_not_taint_a_certified_benchmark_runner(self):
        records = [{"family":"transaction", "data":{
            "measurement":{"guard_passed":True}, "matrix":[{"throughput":{}}]}}]
        records.extend({"family": family, "data":{
            "measurement":{"guard_passed":True}, "matrix":[], "points":[]}}
            for family in ("isolation", "point_read"))
        result = self.collect(records, guard=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads((self.root / "run.json").read_text())
        self.assertFalse(doc["run"]["loadguard"]["passed"])
        self.assertEqual(doc["metrics"]["timings"]["transaction"]["trust"], "clean")
        self.assertEqual(doc["metrics"]["isolation"]["trust"], "clean")
        self.assertEqual(doc["metrics"]["throughput"]["workloads"]["read"]["trust"], "clean")

    def test_unknown_and_duplicate_families_refuse_publication(self):
        for records in ([{"family":"unknown", "data":{}}],
                        [{"family":"scan_commit", "data":[]}] * 2):
            with self.subTest(records=records):
                result = self.collect(records)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "run.json").exists())

    def test_a_published_run_cannot_be_overwritten(self):
        self.assertEqual(self.collect([]).returncode, 0)
        original = (self.root / "run.json").read_bytes()
        self.assertNotEqual(self.collect([]).returncode, 0)
        self.assertEqual((self.root / "run.json").read_bytes(), original)


if __name__ == "__main__":
    if "--build-log" in sys.argv:
        index = sys.argv.index("--build-log")
        path = pathlib.Path(sys.argv[index + 1])
        del sys.argv[index:index + 2]
        for line in path.read_text().splitlines():
            try:
                record = json.loads(line)
            except ValueError:
                continue
            if record.get("target", {}).get("name") == "collect" and record.get("executable"):
                os.environ["REGOLITH_COLLECT_BIN"] = record["executable"]
        if not os.environ.get("REGOLITH_COLLECT_BIN"):
            raise SystemExit("collector executable not found in the build log")
    unittest.main()
