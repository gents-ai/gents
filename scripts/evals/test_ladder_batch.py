import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


class LadderBatchTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        source = pathlib.Path(__file__).resolve().parents[2]
        self.scripts = self.root / "scripts/evals"
        self.scripts.mkdir(parents=True)
        shutil.copy(source / "scripts/evals/run-ladder.sh", self.scripts)
        matrix = json.loads((source / "scripts/evals/matrix.json").read_text())
        matrix["suites"] = matrix["suites"][:2]
        matrix["default"] = [s["id"] for s in matrix["suites"]]
        (self.scripts / "matrix.json").write_text(json.dumps(matrix))
        shutil.copytree(source / "scripts/evals/targets", self.scripts / "targets")
        fixtures = pathlib.Path("crates/gents/tests/fixtures/configurator_evals")
        for relative in ["ladder/engineer_subject", *[s["path"] for s in matrix["suites"]]]:
            shutil.copytree(source / fixtures / relative, self.root / fixtures / relative)
        for relative in ["crates/gents-protocol/prompts/engineer.md", "crates/gents-protocol/presets/engineer-self-config.json"]:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(source / relative, destination)
        self.home = self.root / "eval-home"
        self.home.mkdir()
        (self.home / "init.json").write_text(json.dumps({"node_did": "did:test:node"}))
        (self.home / "runtime.json").write_text(json.dumps({"graphql": "http://127.0.0.1:9507/api/v0/graphql"}))
        self.bin = self.root / "bin"
        self.bin.mkdir()
        git = self.bin / "git"
        git.write_text("#!/bin/sh\necho abc123\n")
        git.chmod(0o755)
        stub = self.bin / "gents"
        stub.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
log = pathlib.Path(os.environ["CALL_LOG"])
with log.open("a") as stream:
    stream.write(json.dumps(args) + "\\n")
if args[0] == "query":
    print(json.dumps({"results": [{"node_did": "did:test:node"}]}))
elif args[:2] == ["eval", "batch"]:
    plan = json.loads(pathlib.Path(args[args.index("--plan") + 1]).read_text())
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    applied = [call[call.index("--root") + 1] for call in calls if call[:2] == ["config", "apply"]]
    assert len(applied) == 3, applied
    assert all(pathlib.Path(path).is_dir() for path in applied)
    assert len(plan["runs"]) == 4, plan
    assert not any(call[:2] == ["eval", "run"] for call in calls)
elif args[:2] != ["config", "apply"]:
    raise SystemExit("unexpected command: " + repr(args))
''')
        stub.chmod(0o755)
        self.log = self.root / "calls.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        GENTS_BIN=str(stub), GENTS_EVAL_HOME=str(self.home), GENTS_EVAL_PORT="9507",
                        GENTS_EVAL_WATCH="0", GENTS_EVAL_TARGET="workstation-1",
                        GENTS_EVAL_SPLITS="train validation", GENTS_EVAL_PER_TRIAL="3", CALL_LOG=str(self.log))

    def run_ladder(self, *arguments, **environment):
        return subprocess.run(["bash", str(self.scripts / "run-ladder.sh"), *arguments],
                              env=dict(self.env, **environment), capture_output=True, text=True)

    def test_all_prepared_before_one_batch_with_shared_trial_cap(self):
        result = self.run_ladder("all", "--backend", "workstation-2", "--trials", "2", "--concurrency", "8")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        batches = [call for call in calls if call[:2] == ["eval", "batch"]]
        self.assertEqual(len(batches), 1)
        batch = batches[0]
        self.assertEqual(batch[batch.index("--concurrency") + 1], "2")
        plan = json.loads(pathlib.Path(batch[batch.index("--plan") + 1]).read_text())
        ids = []
        for run in plan["runs"]:
            args = run["args"]
            ids.append(args[args.index("--run-id") + 1])
            self.assertEqual(args[args.index("--trials") + 1], "2")
            self.assertEqual(args[args.index("--concurrency") + 1], "2")
            self.assertIn("ladder-workstation-2", args[args.index("--profile") + 1])
            self.assertTrue(pathlib.Path(args[args.index("--cell") + 1].split("=", 1)[1].rsplit(":", 1)[0]).is_dir())
            for forbidden in ["--home", "--graphql", "--json"]:
                self.assertNotIn(forbidden, args)
        self.assertEqual(len(ids), len(set(ids)))
        self.assertEqual(len(ids), 4)

    def test_repeated_launches_retain_distinct_saved_plans(self):
        for _ in range(2):
            self.log.unlink(missing_ok=True)
            result = self.run_ladder("all", "--concurrency", "3")
            self.assertEqual(result.returncode, 0, result.stderr)
        plans = sorted(self.home.glob("batch-*.json"))
        self.assertEqual(len(plans), 2)
        run_ids = []
        for plan in plans:
            for run in json.loads(plan.read_text())["runs"]:
                args = run["args"]
                run_ids.append(args[args.index("--run-id") + 1])
        self.assertEqual(len(run_ids), 8)
        self.assertEqual(len(set(run_ids)), 8)

    def test_help_does_not_initialize_or_call_cli(self):
        result = self.run_ladder("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.log.exists())

    def test_invalid_matrix_stops_before_any_cli_call(self):
        (self.scripts / "matrix.json").write_text("{invalid")
        result = self.run_ladder("all")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("JSONDecodeError", result.stderr)
        self.assertFalse(self.log.exists())

    def test_rejects_duplicate_splits_before_any_cli_call(self):
        result = self.run_ladder("all", GENTS_EVAL_SPLITS="train train")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("distinct", result.stderr)
        self.assertFalse(self.log.exists())

    def test_plan_rejects_duplicate_run_ids(self):
        script = (self.scripts / "run-ladder.sh").read_text()
        program = script.split("<<'PYPLAN'\n", 1)[1].split("\nPYPLAN", 1)[0]
        plan = self.root / "duplicate.json"
        plan.write_text(json.dumps({"runs": [{"args": ["definition", "--run-id", "same"]}]}))
        import sys
        from unittest.mock import patch
        arguments = ["plan", str(plan), "definition", str(self.root), "profile", "train", "3", "2", "same"]
        before = plan.read_bytes()
        with patch.object(sys, "argv", arguments):
            with self.assertRaisesRegex(SystemExit, "duplicate run id"):
                exec(compile(program, "run-ladder.sh:plan", "exec"), {})
        self.assertEqual(plan.read_bytes(), before)

    def test_rejects_zero_trial_capacity_before_any_cli_call(self):
        result = self.run_ladder("all", "--concurrency", "2")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("at least", result.stderr)
        self.assertFalse(self.log.exists())

    def test_rejects_empty_selected_runs_without_batch(self):
        fixtures = self.root / "crates/gents/tests/fixtures/configurator_evals"
        for pack in fixtures.glob("ladder/*/pack_config.json"):
            config = json.loads(pack.read_text())
            for definition in config.get("eval_definitions", []):
                definition["cases"] = []
            pack.write_text(json.dumps(config))
        result = self.run_ladder("all", "--target", "workstation-2", "--concurrency", "3")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("contain no runs", result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertFalse(any(call[:2] == ["eval", "batch"] for call in calls))


if __name__ == "__main__":
    unittest.main()
