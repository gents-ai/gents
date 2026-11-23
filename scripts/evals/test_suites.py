import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SOURCE = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SOURCE / "scripts/evals"))
import suites  # noqa: E402


class RegistryTests(unittest.TestCase):
    def test_every_suite_is_registered_once_and_the_default_names_suites(self):
        registry = suites.matrix(SOURCE)
        ids = [suite["id"] for suite in registry["suites"]]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertTrue(set(registry["default"]) <= set(ids))
        self.assertNotIn("suites", json.loads((SOURCE / suites.MATRIX).read_text()))
        for suite in registry["suites"]:
            directory = SOURCE / suites.FIXTURES / suite["path"]
            definition = json.loads((directory / "pack_config.json").read_text())["eval_definitions"][0]
            self.assertEqual(suite["definition_id"], definition["definition_id"])

    def test_selection_matches_an_id_or_its_first_word(self):
        self.assertEqual(suites.select(SOURCE, "l4", "1/1"), ["l4-automation"])
        self.assertEqual(suites.select(SOURCE, "native-packs", "1/1"), ["native-packs"])
        with self.assertRaisesRegex(SystemExit, "ambiguous"):
            suites.select(SOURCE, "engineer", "1/1")
        default = suites.matrix(SOURCE)["default"]
        self.assertEqual(suites.select(SOURCE, "all", "2/2"), default[1::2])


class PatchTests(unittest.TestCase):
    config = {
        "node": {},
        "agents": [{"agent_slot": "engineer", "context_id": "c"}],
        "contexts": [{"context_id": "c", "system_prompt": "./p.md", "tools_id": "t", "skill_ids": ["a"]}],
        "tools": [{"tools_id": "t", "built_ins": {"enable_goal_tools": True, "enable_goal_creation": False}},
                  {"tools_id": "other", "host": {"files": {"mode": "ReadOnly"}}}],
    }

    def test_documents_are_patched_by_id_and_other_keys_by_merge_patch(self):
        patched = suites.apply_patch(self.config, {
            "tools": {"t": {"built_ins": {"enable_goal_creation": True}}, "other": None},
            "contexts": {"c": {"skill_ids": ["a", "b"]}},
            "skills": {"b": {"name": "b", "instructions": "Use it."}},
            "node": {"display_name": "eval"},
        })
        self.assertEqual(patched["tools"], [{"tools_id": "t", "built_ins": {"enable_goal_tools": True, "enable_goal_creation": True}}])
        self.assertEqual(patched["contexts"][0]["skill_ids"], ["a", "b"])
        self.assertEqual(patched["skills"], [{"skill_id": "b", "name": "b", "instructions": "Use it."}])
        self.assertEqual(patched["node"], {"display_name": "eval"})
        self.assertEqual(self.config["tools"][0]["built_ins"]["enable_goal_creation"], False, "the input is not mutated")

    def test_removing_a_missing_document_is_refused(self):
        with self.assertRaisesRegex(SystemExit, "does not have"):
            suites.apply_patch(self.config, {"tools": {"absent": None}})


class PrepareTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        for relative in [suites.MATRIX, suites.PROMPT, suites.PRESET, "scripts/evals/suites.py"]:
            (self.root / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(SOURCE / relative, self.root / relative)
        fixtures = self.root / suites.FIXTURES
        (self.root / suites.MATRIX).write_text(json.dumps({"default": ["probe"]}))
        subject = fixtures / "subject"
        subject.mkdir(parents=True)
        (subject / "pack_config.json").write_text(json.dumps(PatchTests.config))
        (subject / "p.md").write_text("stale prompt")
        self.suite = fixtures / "probe"
        self.suite.mkdir()
        (self.suite / "pack_config.json").write_text(json.dumps({"eval_definitions": [{"definition_id": "probe-def"}]}))
        (self.suite / "suite.json").write_text(json.dumps({"id": "probe", "title": "Probe", "coverage": ["x"], "subject": "subject"}))

    def run_suites(self, *arguments, **environment):
        environment = {key: value for key, value in os.environ.items() if key != "GENTS_EVAL_COMMAND_EXECUTION"} | environment
        return subprocess.run([sys.executable, str(self.root / "scripts/evals/suites.py"), *arguments],
                              env=environment, capture_output=True, text=True)

    def prepare(self, **environment):
        result = self.run_suites("prepare-subject", "probe", str(self.root / "out"), **environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        return pathlib.Path(result.stdout.strip())

    def test_the_prompt_is_the_shipped_one_and_the_engineer_runs_no_commands_by_default(self):
        out = self.prepare(GENTS_EVAL_COMMAND_EXECUTION="")
        self.assertEqual((out / "p.md").read_bytes(), (SOURCE / suites.PROMPT).read_bytes())
        expected = json.loads(json.dumps(PatchTests.config))
        expected["tools"][0]["host"] = {"bash": {"mode": "Off"}}
        self.assertEqual(json.loads((out / "pack_config.json").read_text()), expected)

    def test_unrestricted_grants_the_engineer_host_bash_and_an_unknown_mode_is_refused(self):
        config = json.loads((self.prepare(GENTS_EVAL_COMMAND_EXECUTION="unrestricted") / "pack_config.json").read_text())
        tools = {t["tools_id"]: t for t in config["tools"]}
        self.assertEqual(tools["t"]["host"], {"bash": {"mode": "Unrestricted"}})
        self.assertEqual(tools["other"], PatchTests.config["tools"][1])
        self.assertEqual(self.run_suites("command-execution", GENTS_EVAL_COMMAND_EXECUTION="unrestricted").stdout, "unrestricted\n")
        refused = self.run_suites("prepare-subject", "probe", str(self.root / "out"), GENTS_EVAL_COMMAND_EXECUTION="sandboxed")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("GENTS_EVAL_COMMAND_EXECUTION must be one of off, unrestricted", refused.stderr)

    def test_preset_and_patch_apply_to_the_engineer_tools(self):
        suite = json.loads((self.suite / "suite.json").read_text())
        (self.suite / "suite.json").write_text(json.dumps(dict(suite, preset_self_config=True)))
        (self.suite / "subject_patch.json").write_text(json.dumps({"tools": {"t": {"built_ins": {"enable_goal_creation": True}}}}))
        config = json.loads((self.prepare() / "pack_config.json").read_text())
        tools = {t["tools_id"]: t for t in config["tools"]}
        self.assertEqual(tools["t"]["self_config"], json.loads((SOURCE / suites.PRESET).read_text()))
        self.assertTrue(tools["t"]["built_ins"]["enable_goal_creation"])
        self.assertNotIn("self_config", tools["other"])

    def test_the_subject_override_replaces_the_registered_subject(self):
        other = self.root / "elsewhere"
        shutil.copytree(self.suite.parent / "subject", other)
        (other / "marker").write_text("override")
        self.assertTrue((self.prepare(GENTS_EVAL_SUBJECT=str(other)) / "marker").is_file())

    def test_unknown_suite_keys_are_refused(self):
        (self.suite / "suite.json").write_text(json.dumps({"id": "probe", "title": "Probe", "coverage": [], "subject": "subject", "split": "train"}))
        with self.assertRaisesRegex(SystemExit, "unknown keys"):
            suites.suites(self.root)


if __name__ == "__main__":
    unittest.main()
