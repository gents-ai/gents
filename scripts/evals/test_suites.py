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

STUB = """#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
args = sys.argv[1:]
pathlib.Path(os.environ["CALL_LOG"]).write_text(json.dumps(args))
out = pathlib.Path(args[args.index("--out") + 1])
if "--from" in args:
    shutil.copytree(args[args.index("--from") + 1], out)
else:
    out.mkdir(parents=True)
    (out / "manifest.json").write_text("{}")
print(json.dumps({"subject": str(out)}))
"""


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

    def test_every_fixture_is_justified_in_its_readme_and_names_files_the_suite_has(self):
        for suite in suites.suites(SOURCE):
            directory = SOURCE / suites.FIXTURES / suite["path"]
            readme = (directory / "README.md").read_text()
            for fixture in suite.get("subject_fixtures", []):
                self.assertIn(f"`{fixture['name']}`", readme, f"{suite['id']}: its README justifies {fixture['name']}")
                for asset in fixture.get("assets", []):
                    self.assertTrue((directory / asset).exists(), f"{suite['id']}: {asset}")
            definition = json.loads((directory / "pack_config.json").read_text())["eval_definitions"][0]
            for pack in definition["subject"].get("enabled_core_packs", []):
                self.assertIn(f"`{pack}`", readme, f"{suite['id']}: its README justifies enabling {pack}")

    def test_selection_matches_an_id_or_its_first_word(self):
        self.assertEqual(suites.select(SOURCE, "l4", "1/1"), ["l4-automation"])
        self.assertEqual(suites.select(SOURCE, "native-packs", "1/1"), ["native-packs"])
        with self.assertRaisesRegex(SystemExit, "ambiguous"):
            suites.select(SOURCE, "engineer", "1/1")
        default = suites.matrix(SOURCE)["default"]
        self.assertEqual(suites.select(SOURCE, "all", "2/2"), default[1::2])


class FixtureDeclarationTests(unittest.TestCase):
    def test_a_suite_fixture_needs_a_distinct_name_and_a_reason(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / suites.FIXTURES / "probe").mkdir(parents=True)
            (root / suites.FIXTURES / "probe/pack_config.json").write_text(json.dumps({"eval_definitions": [{"definition_id": "probe"}]}))
            for fixtures in [[{"name": "a"}], [{"name": "a", "why": "x"}, {"name": "a", "why": "y"}],
                             [{"name": "a", "why": "x", "remove": []}]]:
                (root / suites.FIXTURES / "probe/suite.json").write_text(json.dumps(
                    {"id": "probe", "title": "Probe", "coverage": [], "subject_fixtures": fixtures}))
                with self.subTest(fixtures=fixtures), self.assertRaisesRegex(SystemExit, "distinct name and a why"):
                    suites.suites(root)


class PrepareTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        for relative in [suites.MATRIX, "scripts/evals/suites.py"]:
            (self.root / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(SOURCE / relative, self.root / relative)
        (self.root / suites.MATRIX).write_text(json.dumps({"default": ["probe"]}))
        self.suite = self.root / suites.FIXTURES / "probe"
        self.suite.mkdir(parents=True)
        (self.suite / "pack_config.json").write_text(json.dumps({"eval_definitions": [{"definition_id": "probe-def"}]}))
        (self.suite / "suite.json").write_text(json.dumps({"id": "probe", "title": "Probe", "coverage": ["x"]}))
        self.stub = self.root / "gents"
        self.stub.write_text(STUB)
        self.stub.chmod(0o755)
        self.log = self.root / "call.json"

    def run_suites(self, *arguments, **environment):
        base = {key: value for key, value in os.environ.items()
                if key not in {"GENTS_EVAL_COMMAND_EXECUTION", "GENTS_EVAL_SUBJECT", "GENTS_BIN"}}
        base.update(GENTS_BIN=str(self.stub), CALL_LOG=str(self.log))
        return subprocess.run([sys.executable, str(self.root / "scripts/evals/suites.py"), *arguments],
                              env=base | environment, capture_output=True, text=True)

    def prepare(self, **environment):
        result = self.run_suites("prepare-subject", "probe", str(self.root / "out"), **environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        return pathlib.Path(result.stdout.strip())

    def test_gents_writes_the_subject_with_no_commands_by_default_and_the_suite_fixtures(self):
        (self.root / "out").mkdir()
        self.assertEqual(self.prepare(GENTS_EVAL_COMMAND_EXECUTION=""), self.root / "out")
        self.assertEqual(json.loads(self.log.read_text()),
                         ["utils", "eval", "subject", "--out", str(self.root / "out"), "--command-execution", "off",
                          "--suite", str(self.suite.resolve())])

    def test_unrestricted_is_passed_on_and_an_unknown_mode_is_refused(self):
        self.prepare(GENTS_EVAL_COMMAND_EXECUTION="unrestricted")
        call = json.loads(self.log.read_text())
        self.assertEqual(call[call.index("--command-execution") + 1], "unrestricted")
        self.assertEqual(self.run_suites("command-execution", GENTS_EVAL_COMMAND_EXECUTION="unrestricted").stdout, "unrestricted\n")
        refused = self.run_suites("prepare-subject", "probe", str(self.root / "out"), GENTS_EVAL_COMMAND_EXECUTION="sandboxed")
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("GENTS_EVAL_COMMAND_EXECUTION must be one of off, unrestricted", refused.stderr)

    def test_the_subject_override_is_the_base_the_fixtures_layer_on(self):
        other = self.root / "elsewhere"
        other.mkdir()
        (other / "marker").write_text("override")
        out = self.prepare(GENTS_EVAL_SUBJECT=str(other))
        call = json.loads(self.log.read_text())
        self.assertEqual(call[call.index("--from") + 1], str(other))
        self.assertTrue((out / "marker").is_file())

    def test_a_failed_subject_write_stops_the_launch(self):
        self.stub.write_text("#!/bin/sh\necho 'a fixture may not change' >&2\nexit 3\n")
        result = self.run_suites("prepare-subject", "probe", str(self.root / "out"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("gents utils eval subject failed: a fixture may not change", result.stderr)

    def test_a_named_suite_is_selected_where_the_default_list_names_absent_suites(self):
        (self.root / suites.MATRIX).write_text(json.dumps({"default": ["absent"]}))
        self.assertEqual(suites.select(self.root, "probe", "1/1"), ["probe"])
        with self.assertRaisesRegex(SystemExit, "default names no suite.json"):
            suites.select(self.root, "all", "1/1")

    def test_unknown_suite_keys_are_refused(self):
        for key in ["subject", "split", "preset_self_config"]:
            (self.suite / "suite.json").write_text(json.dumps({"id": "probe", "title": "Probe", "coverage": [], key: "x"}))
            with self.subTest(key=key), self.assertRaisesRegex(SystemExit, "unknown keys"):
                suites.suites(self.root)


if __name__ == "__main__":
    unittest.main()
