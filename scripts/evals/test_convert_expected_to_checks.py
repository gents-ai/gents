"""Tests for the one-time `expected` -> `checks` conversion."""

import collections
import importlib.util
import json
import pathlib
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("convert", HERE / "convert-expected-to-checks.py")
convert = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(convert)

CONVERTED = HERE.parent.parent / "packs" / "eval_monitor_findings" / "cases"

LEGACY_CAPTURE = [{"name": "items", "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}]


def legacy_case():
    return {
        "case_id": "recovery-condition-cleared",
        "axis": "recovery",
        "split": "test",
        "reducer": "all",
        "stages": [
            {
                "stage_id": "report",
                "prompt": "Monitoring input for correlation c-09-a.",
                "deadline_secs": 600,
                "capture": LEGACY_CAPTURE,
                "expected": {
                    "item_count": 1,
                    "payload_well_formed": True,
                    "pairs_unique": True,
                    "finding_count": 1,
                    "open_count": 1,
                    "resolved_count": 0,
                    "findings": [{"correlation": "c-09-a", "state": "open", "keywords_all": ["replication", "42"]}],
                    "findings_exact": True,
                    "no_finding_containing": ["failover"],
                },
            }
        ],
    }


class ConvertTest(unittest.TestCase):
    def test_every_expected_key_maps_to_one_check_in_a_fixed_order(self):
        stage = convert.convert_case(legacy_case())["stages"][0]
        self.assertEqual(
            [check["check"] for check in stage["checks"]],
            [
                "mailbox_item_count",
                "payload_well_formed",
                "finding_pairs_unique",
                "finding_count",
                "finding_state_counts",
                "findings_match",
                "mailbox_text_excludes",
            ],
        )
        by_name = {check["check"]: check for check in stage["checks"]}
        self.assertEqual(by_name["mailbox_item_count"]["params"], {"count": 1})
        self.assertEqual(by_name["payload_well_formed"]["params"], {"version": 1})
        self.assertEqual(by_name["finding_pairs_unique"]["params"], {})
        self.assertEqual(by_name["finding_state_counts"]["params"], {"open": 1, "resolved": 0})
        self.assertEqual(
            by_name["findings_match"]["params"],
            {"matchers": [{"correlation": "c-09-a", "state": "open", "keywords_all": ["replication", "42"]}], "exact": True},
        )
        self.assertEqual(by_name["mailbox_text_excludes"]["params"], {"keywords": ["failover"]})
        for check in stage["checks"]:
            self.assertEqual((check["tier"], check["weight"]), ("acceptance", 1))
        self.assertEqual(stage["capture"], [convert.CAPTURE])
        self.assertEqual(
            convert.CAPTURE["filter"], {"requester_did": {"_eq": "$trial"}}
        )
        self.assertEqual(convert.CAPTURE["fields"], ["title", "summary", "payload", "status"])
        self.assertNotIn("expected", stage)

    def test_split_test_becomes_held_out_axis_is_dropped_and_fixtures_pass_through(self):
        legacy = legacy_case()
        legacy["fixtures"] = {"files": [{"path": "inventory/edge-07.json", "contents": "{}\n"}]}
        case = convert.convert_case(legacy)
        self.assertEqual(list(case), ["case_id", "split", "reducer", "fixtures", "stages"])
        self.assertEqual(case["split"], "held_out")
        self.assertEqual(case["fixtures"], legacy["fixtures"])

    def test_the_override_table_moves_two_validation_cases_to_held_out(self):
        self.assertEqual(
            sorted(convert.SPLIT_OVERRIDES),
            ["file-without-contradiction-multi", "three-conditions-with-service"],
        )
        for case_id, (split, reason) in convert.SPLIT_OVERRIDES.items():
            self.assertEqual(split, "held_out")
            self.assertTrue(reason)
            legacy = legacy_case()
            legacy.update({"case_id": case_id, "split": "validation"})
            self.assertEqual(convert.convert_case(legacy)["split"], "held_out")
            # An override written against a split the pre-work no longer has is refused.
            legacy["split"] = "train"
            with self.assertRaises(ValueError):
                convert.convert_case(legacy)
        legacy = legacy_case()
        legacy["split"] = "validation"
        self.assertEqual(convert.convert_case(legacy)["split"], "validation")

    def test_the_converted_cases_split_eight_six_six(self):
        splits = collections.Counter(
            json.loads(path.read_text(encoding="utf-8"))["split"] for path in sorted(CONVERTED.glob("*.json"))
        )
        self.assertEqual(splits, {"train": 8, "validation": 6, "held_out": 6})

    def test_a_stage_without_bans_or_item_count_omits_those_checks(self):
        legacy = legacy_case()
        expected = legacy["stages"][0]["expected"]
        for key in ("item_count", "open_count", "resolved_count", "no_finding_containing"):
            del expected[key]
        expected["findings"] = []
        names = [check["check"] for check in convert.convert_case(legacy)["stages"][0]["checks"]]
        self.assertEqual(names, ["payload_well_formed", "finding_pairs_unique", "finding_count", "findings_match"])

    def test_unknown_keys_half_state_counts_and_other_captures_are_refused(self):
        for mutate in (
            lambda case: case["stages"][0]["expected"].update({"no_finding_for_correlation": ["c"]}),
            lambda case: case["stages"][0]["expected"].pop("resolved_count"),
            lambda case: case["stages"][0].update({"capture": []}),
            lambda case: case.update({"split": "dev"}),
            lambda case: case.update({"notes": "x"}),
        ):
            legacy = legacy_case()
            mutate(legacy)
            with self.assertRaises(ValueError):
                convert.convert_case(legacy)

    def test_output_is_byte_identical_across_runs(self):
        with tempfile.TemporaryDirectory() as root:
            source = pathlib.Path(root, "in")
            source.mkdir()
            (source / "recovery-condition-cleared.json").write_text(json.dumps(legacy_case()), encoding="utf-8")
            first, second = pathlib.Path(root, "a"), pathlib.Path(root, "b")
            self.assertEqual(convert.main(["--input", str(source), "--output", str(first)]), 0)
            self.assertEqual(convert.main(["--input", str(source), "--output", str(second)]), 0)
            name = "recovery_condition_cleared.json"
            self.assertEqual((first / name).read_bytes(), (second / name).read_bytes())
            self.assertTrue((first / name).read_text(encoding="utf-8").endswith("}\n"))


if __name__ == "__main__":
    unittest.main()
