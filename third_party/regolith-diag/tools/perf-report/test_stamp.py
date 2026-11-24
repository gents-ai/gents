"""Runner trust survives aggregation without tainting deterministic checks."""

import unittest
from unittest.mock import patch

from stamp import annotate, provenance


class MeasurementTrust(unittest.TestCase):
    def test_a_failed_guard_marks_every_timing_record(self):
        records = [{"family": name, "data": {}} for name in
                   ("point_read", "isolation", "append", "scan_commit", "read_rules", "sync")]
        annotate(records, False, {"host": {"cpu": "test"}})
        for record in records:
            self.assertEqual(record["data"]["trust"], "contaminated")
            self.assertFalse(record["data"]["measurement"]["guard_passed"])

    def test_transaction_correctness_and_allocations_remain_deterministic(self):
        records = [{"family":"transaction", "data":{"matrix":[]}},
                   {"family":"allocs", "data":{"paths":[]}}]
        annotate(records, False, {})
        self.assertNotIn("trust", records[0]["data"])
        self.assertEqual(records[0]["data"]["timing_trust"], "contaminated")
        self.assertEqual(records[1]["data"], {"paths":[]})

    def test_a_clean_runner_does_not_upgrade_declared_doubt(self):
        for trust in ("contaminated", "absent"):
            records = [{"family":"append", "data":{"trust":trust}}]
            annotate(records, True, {})
            self.assertEqual(records[0]["data"]["trust"], trust)

    def test_array_records_keep_their_rows(self):
        records = [{"family":"read_rules", "data":[{"name":"read", "value":10}]}]
        annotate(records, True, {"command":["cargo", "bench"]})
        self.assertEqual(records[0]["data"]["rows"], [{"name":"read", "value":10}])
        self.assertEqual(records[0]["data"]["measurement"]["command"], ["cargo", "bench"])

    def test_an_explicit_cargo_toolchain_is_recorded(self):
        with patch("stamp.output", return_value="recorded") as output:
            metadata = provenance(["cargo", "+1.99.0", "bench"])
        output.assert_any_call(["rustc", "+1.99.0", "--version"])
        self.assertEqual(metadata["toolchain"], "recorded")
