import copy
import json
import pathlib
import tempfile
import unittest
from unittest.mock import patch


class EngineerBaselineTests(unittest.TestCase):
    def test_current_cases_are_idempotent_and_unexpected_grants_fail(self):
        root = pathlib.Path(__file__).resolve().parents[2]
        script = (root / "scripts/evals/run-ladder.sh").read_text()
        program = script.split("<<'PYENGINEERBASELINE'\n", 1)[1].split("\nPYENGINEERBASELINE", 1)[0]
        fixture = root / "crates/gents/tests/fixtures/configurator_evals/ladder"
        subject = json.loads((fixture / "engineer_subject/pack_config.json").read_text())
        subject["tools"][0]["self_config"] = json.loads(
            (root / "crates/gents-protocol/presets/engineer-self-config.json").read_text()
        )
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary)
            definition, seed = work / "definition", work / "seed"
            definition.mkdir()
            seed.mkdir()
            cases = sorted(fixture.glob("*/cases/*.json"))
            self.assertTrue(cases)
            (definition / "pack_config.json").write_text(json.dumps({
                "eval_definitions": [{"cases": [f"{i}.json" for i in range(len(cases))]}]
            }))
            for i, case in enumerate(cases):
                (definition / f"{i}.json").write_text(case.read_text())
            (seed / "pack_config.json").write_text(json.dumps(subject))

            def stage():
                with patch("sys.argv", ["baseline", str(definition), str(seed)]):
                    exec(compile(program, "run-ladder.sh:baseline", "exec"), {})

            stage()
            staged = [(definition / f"{i}.json").read_bytes() for i in range(len(cases))]
            stage()
            self.assertEqual(staged, [(definition / f"{i}.json").read_bytes() for i in range(len(cases))])
            for field, value in [("built_ins", {"enable_schema_tool": False}), ("agents", {"unexpected": True})]:
                with self.subTest(field=field):
                    unexpected = copy.deepcopy(subject)
                    unexpected["tools"][0][field] = value
                    (seed / "pack_config.json").write_text(json.dumps(unexpected))
                    with self.assertRaisesRegex(ValueError, "unexpected Engineer invariant mismatch for " + field):
                        stage()
                    self.assertEqual(staged, [(definition / f"{i}.json").read_bytes() for i in range(len(cases))])

    def test_initial_grant_is_exact_and_other_agents_are_untouched(self):
        root = pathlib.Path(__file__).resolve().parents[2]
        script = (root / "scripts/evals/run-ladder.sh").read_text()
        program = script.split("<<'PYENGINEERBASELINE'\n", 1)[1].split("\nPYENGINEERBASELINE", 1)[0]
        fixture = root / "crates/gents/tests/fixtures/configurator_evals/ladder"
        case = json.loads((fixture / "l2_agent/cases/scout_grants_two_file_tools.json").read_text())
        subject = json.loads((fixture / "engineer_subject/pack_config.json").read_text())
        grant = json.loads((root / "crates/gents-protocol/presets/engineer-self-config.json").read_text())
        subject["tools"][0]["self_config"] = grant
        original = copy.deepcopy(case)
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary)
            definition, seed = work / "definition", work / "seed"
            definition.mkdir()
            seed.mkdir()
            (seed / "pack_config.json").write_text(json.dumps(subject))
            (definition / "pack_config.json").write_text(json.dumps({"eval_definitions": [{"cases": ["case.json"]}]}))
            (definition / "case.json").write_text(json.dumps(case))
            with patch("sys.argv", ["baseline", str(definition), str(seed)]):
                exec(compile(program, "run-ladder.sh:baseline", "exec"), {})
            staged = json.loads((definition / "case.json").read_text())
            wrong_scope = copy.deepcopy(original)
            for stage in wrong_scope["stages"]:
                stage["capture"][0]["filter"] = {"node_did": {"_eq": "different-node"}}
            (definition / "case.json").write_text(json.dumps(wrong_scope))
            with patch("sys.argv", ["baseline", str(definition), str(seed)]):
                exec(compile(program, "run-ladder.sh:baseline", "exec"), {})
            self.assertEqual(json.loads((definition / "case.json").read_text()), wrong_scope)
        for old_stage, stage in zip(original["stages"], staged["stages"]):
            old_group = old_stage["checks"][0]["params"]["agents"]
            group = stage["checks"][0]["params"]["agents"]
            self.assertEqual(old_group["expect"][0], group["expect"][0])
            self.assertEqual(len(old_group["expect"][1]["tools"]), len(group["expect"][1]["tools"]))
            self.assertEqual(old_stage["checks"][1:], stage["checks"][1:])
            checks = {item["field"]: item for item in group["expect"][1]["tools"]}
            self.assertEqual(checks["built_ins"]["equals"], {"enable_schema_tool": True})
            self.assertEqual(checks["self_config.self_config_categories"]["equals"], grant["self_config_categories"])
            changed = copy.deepcopy(subject["tools"][0])
            changed["built_ins"]["enable_schema_tool"] = False
            self.assertNotEqual(checks["built_ins"]["equals"], changed["built_ins"])
            changed["self_config"]["self_config_categories"] = ["agent"]
            self.assertNotEqual(checks["self_config.self_config_categories"]["equals"], changed["self_config"]["self_config_categories"])


if __name__ == "__main__":
    unittest.main()
