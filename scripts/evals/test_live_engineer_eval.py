import http.server
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest

SOURCE = pathlib.Path(__file__).resolve().parents[2]
L2 = "crates/gents/tests/fixtures/configurator_evals/ladder/l2_agent"
SECRET_URL = "http://127.0.0.1:{port}/v1"

FAKE_LADDER = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
env = os.environ
home = pathlib.Path(env["GENTS_EVAL_HOME"])
home.mkdir(parents=True, exist_ok=True)
print("ladder sees", json.load(open(env["GENTS_EVAL_TARGET_FILE"]))["inference_backends"][0]["endpoint"])
selection = sys.argv[1]
record = {key: env.get(key) for key in ["GENTS_EVAL_TEMPERATURE", "GENTS_EVAL_TOP_P", "GENTS_EVAL_SPLITS", "GENTS_EVAL_TARGET", "GENTS_EVAL_SHARD", "GENTS_EVAL_PORT"]}
if selection != "all":
    definition = pathlib.Path(env["GENTS_EVAL_DEFINITION_SOURCE"])
    record["cases"] = json.load(open(definition / "pack_config.json"))["eval_definitions"][0]["cases"]
record["argv"] = sys.argv[1:]
with open(env["RECORD"], "a") as stream:
    stream.write(json.dumps(record) + "\n")
if env.get("STOP_ENDPOINT"):
    pathlib.Path(env["STOP_ENDPOINT"]).write_text("stop")
(home / "server.log").write_text("backend " + env["ENDPOINT_FOR_LOG"] + "\n")
if env.get("LINGER_PID"):
    import subprocess
    subprocess.Popen([sys.executable, "-c", "import os, signal, sys, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); open(sys.argv[1], 'w').write(str(os.getpid())); time.sleep(600)", env["LINGER_PID"]])
    import time
    while not pathlib.Path(env["LINGER_PID"]).exists():
        time.sleep(0.05)
if selection == "all":
    matrix = json.load(open(pathlib.Path(__file__).parent / "matrix.json"))
    index, total = map(int, env["GENTS_EVAL_SHARD"].split("/"))
    plan = json.loads(env.get("LADDER_CLASSES", "{}"))
    suites = {s["id"]: s for s in matrix["suites"]}
    runs = [(suite, plan.get(suite, {"case-a": "pass,pass,pass", "case-b": "pass,fail,pass"})) for suite in matrix["default"][index - 1::total]]
    if env.get("HANG_SUITE") in [suite for suite, _ in runs]:
        import time
        time.sleep(60)
else:
    classes = env.get("CLASSES", "")
    if not classes:
        sys.exit(4)
    runs = [("l2-agent", {"scout-grants-two-file-tools": classes})]
for suite, cases in runs:
    run = home / f"eval/runs/ladder-{suite}-abc-live-ci-train-1"
    run.mkdir(parents=True)
    slots, trials = [], {}
    for case_id, classes in cases.items():
        for index, cls in enumerate(classes.split(",")):
            trial_id = f"{case_id}-t{index}"
            slots.append({"case_id": case_id, "trial_index": index, "class": cls,
                          "score_bp": 10000 if cls == "pass" else 0,
                          "verdicts": [] if cls == "pass" else [{"stage_id": "configure", "check": "crew_spec_match", "kind": "failed", "detail": "27 of 28 requirements met"}],
                          "latest": {"trial_id": trial_id, "stages": [{"stage_id": "configure", "terminal_state": "completed"}, {"stage_id": "reentry", "terminal_state": "completed"}]}})
            trials[trial_id] = {"live": {"elapsed_secs": 100 + index, "model_turns": 12, "tool_calls": 14, "failed_tool_calls": 1,
                                         "documents": {"Agent": 2}, "tools": {"config": {"calls": 14, "failed": 1}},
                                         "stages": {"configure": {"checks": [{"raw": {"unmet": [] if cls == "pass" else ["scout tools host.files.mode"]}}]}}}}
    definition_id = {"l2-agent": "configurator-l2-agent"}.get(suite) if selection != "all" else suites[suite]["definition_id"]
    view = {"report": {"run": {"run_id": run.name, "split": "train", "definition": {"definition_id": definition_id}},
                       "cells": [{"cell_id": "engineer", "slots": slots, "usage": {"input_tokens": 1000, "output_tokens": 50}}]}, "trials": trials}
    (run / "report.json").write_text(json.dumps(view))
'''

FAKE_GENTS = r'''#!/usr/bin/env python3
import json, os, sys
print(json.dumps({"argv": sys.argv[1:], "requests": [{"prompt": "uses " + os.environ["ENDPOINT_FOR_LOG"]}]}))
'''


class Models(http.server.BaseHTTPRequestHandler):
    served = ["GLM-5.3-Flash-NVFP4"]
    stop_file = None

    def do_GET(self):
        if self.stop_file and pathlib.Path(self.stop_file).exists():
            self.send_error(503)
            return
        body = json.dumps({"data": [{"id": name} for name in self.served]}).encode()
        self.send_response(200 if self.path == "/v1/models" else 404)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class LiveEngineerEvalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = pathlib.Path(temporary.name)
        scripts = self.root / "repo/scripts/evals"
        scripts.mkdir(parents=True)
        shutil.copy(SOURCE / "scripts/evals/live_engineer_eval.py", scripts)
        shutil.copy(SOURCE / "scripts/evals/matrix.json", scripts)
        shutil.copytree(SOURCE / L2, self.root / "repo" / L2)
        (scripts / "fake_ladder.py").write_text(FAKE_LADDER)
        (scripts / "run-ladder.sh").write_text('exec "$PYTHON" "$(dirname "$0")/fake_ladder.py" "$@"\n')
        self.gents = self.root / "gents"
        self.gents.write_text(FAKE_GENTS)
        self.gents.chmod(0o755)
        self.out = self.root / "work/out"
        self.record = self.root / "record.json"
        self.stop = self.root / "stop"
        handler = type("Handler", (Models,), {"stop_file": str(self.stop)})
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = SECRET_URL.format(port=self.server.server_address[1])

    def drive(self, *arguments, **environment):
        env = dict(os.environ, GENTS_CI_LIVE_MODEL_BASE_URL=self.url, GENTS_CI_LIVE_MODEL_NAME="GLM-5.3-Flash-NVFP4",
                   RECORD=str(self.record), ENDPOINT_FOR_LOG=self.url, PYTHON=sys.executable)
        env.update(environment)
        script = self.root / "repo/scripts/evals/live_engineer_eval.py"
        return subprocess.run([sys.executable, str(script), *arguments], env=env, capture_output=True, text=True)

    def run_eval(self, classes, **environment):
        return self.drive("run", "--out", str(self.out), "--gents", str(self.gents), CLASSES=classes, **environment)

    def result(self):
        return json.loads((self.out / "result.json").read_text())

    def test_probe_reports_a_served_model(self):
        result = self.drive("probe", "--out", str(self.out))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.out / "result.json").exists())

    def test_probe_skips_without_a_configured_endpoint(self):
        result = self.drive("probe", "--out", str(self.out), GENTS_CI_LIVE_MODEL_BASE_URL="")
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(self.result()["outcome"], "skipped")
        self.assertIn("GENTS_CI_LIVE_MODEL_BASE_URL is not set", self.result()["reason"])

    def test_probe_skips_an_unreachable_endpoint_and_redacts_it(self):
        self.stop.write_text("stop")
        result = self.drive("probe", "--out", str(self.out))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("unreachable", self.result()["reason"])
        self.assertNotIn("127.0.0.1", (self.out / "result.json").read_text())
        self.assertIn("SKIPPED", (self.out / "summary.md").read_text())

    def test_probe_skips_an_endpoint_serving_another_model(self):
        result = self.drive("probe", "--out", str(self.out), GENTS_CI_LIVE_MODEL_NAME="other-model")
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("does not serve other-model", self.result()["reason"])

    def test_majority_passes_with_target_sampling_and_one_case(self):
        result = self.run_eval("pass,fail,pass")
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        self.assertEqual((verdict["outcome"], verdict["passes"], verdict["required_passes"]), ("pass", 2, 2))
        self.assertEqual(verdict["tokens"], {"input": 1000, "output": 50})
        record = json.loads(self.record.read_text().splitlines()[0])
        self.assertEqual((record["GENTS_EVAL_TEMPERATURE"], record["GENTS_EVAL_TOP_P"]), ("", ""))
        self.assertEqual(record["GENTS_EVAL_SPLITS"], "train")
        self.assertEqual(record["cases"], ["./cases/scout_grants_two_file_tools.json"])
        self.assertEqual(record["argv"], ["l2-agent", "3", "3"])
        self.assertEqual(sorted(p.name for p in (self.out / "trials").iterdir()), ["0.json", "0.log", "1.json", "1.log", "2.json", "2.log"])
        summary = (self.out / "summary.md").read_text()
        self.assertIn("Live Engineer eval: PASS", summary)
        self.assertIn("at least 2 of 3 trials", summary)
        self.assertIn("scout tools host.files.mode", summary)
        self.assertIn("failed tool calls by tool: {'config': 1}", summary)
        self.assertEqual(json.loads((self.out / "trials/0.json").read_text())["argv"][:3], ["utils", "eval", "trial"])

    def test_minority_fails(self):
        result = self.run_eval("pass,fail,fail")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.result()["outcome"], "fail")
        self.assertIn("1 of 3 trials passed", self.result()["reason"])

    def test_missing_slots_fail_even_when_the_present_ones_pass(self):
        result = self.run_eval("pass,pass")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("2 of 3 planned trials", self.result()["reason"])

    def test_no_report_is_a_harness_error(self):
        result = self.run_eval("")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.result()["outcome"], "error")
        self.assertIn("exited 4", self.result()["reason"])

    def test_endpoint_lost_during_the_run_is_not_evidence(self):
        result = self.run_eval("fail,fail,fail", STOP_ENDPOINT=str(self.stop))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(self.result()["outcome"], "skipped")
        self.assertIn("after the run started", self.result()["reason"])

    def test_artifacts_never_carry_the_endpoint(self):
        self.run_eval("pass,pass,pass")
        host = "127.0.0.1"
        for path in self.out.rglob("*"):
            if path.is_file():
                self.assertNotIn(host, path.read_text(), path)
        self.assertIn("<model endpoint>", (self.out / "ladder.log").read_text())

    def run_full(self, *arguments, **environment):
        return self.drive("run", "--scope", "ladder", "--out", str(self.out), "--gents", str(self.gents),
                          "--stagger", "0", *arguments, **environment)

    def test_ladder_passes_when_every_gated_rung_meets_its_floor(self):
        result = self.run_full()
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        self.assertEqual(verdict["outcome"], "pass")
        records = [json.loads(line) for line in self.record.read_text().splitlines()]
        self.assertEqual(sorted(r["GENTS_EVAL_SHARD"] for r in records), ["1/4", "2/4", "3/4", "4/4"])
        self.assertEqual(len({r["GENTS_EVAL_PORT"] for r in records}), 4)
        self.assertTrue(all(r["argv"] == ["all", "3", "4"] and r["GENTS_EVAL_TEMPERATURE"] == "" and r["GENTS_EVAL_TOP_P"] == "" for r in records))
        rungs = {row["rung"]: row for row in verdict["rungs"]}
        self.assertEqual(rungs["l2-agent"], {"rung": "l2-agent", "gated": True, "cases": 2, "cases_meeting_threshold": 2,
                                             "passes": 5, "finished": 6, "verdict": "pass"})
        self.assertEqual(rungs["factory-setup"]["verdict"], "reported")
        self.assertEqual(verdict["tokens"], {"input": 6000, "output": 300})
        self.assertIn("Live Engineer ladder: PASS", (self.out / "summary.md").read_text())

    def test_ladder_records_a_case_below_threshold_without_failing_the_job(self):
        plan = {"l3-datastore": {"case-a": "pass,fail,fail", "case-b": "pass,pass,pass"}}
        result = self.run_full(LADDER_CLASSES=json.dumps(plan))
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        self.assertEqual(verdict["cases"]["l3-datastore/case-a"]["verdict"], "fail")
        self.assertEqual(verdict["cases"]["l2-agent/case-a"]["verdict"], "pass")
        summary = (self.out / "summary.md").read_text()
        self.assertIn("`l3-datastore` / `case-a`: 1 of 3 finished passed", summary)
        self.assertTrue((self.out / "trials/l3-datastore-case-a-1.json").is_file())

    def test_ladder_fails_when_a_rung_collapses(self):
        plan = {"l2-agent": {"case-a": "fail,fail,fail", "case-b": "fail,fail,pass"}}
        result = self.run_full(LADDER_CLASSES=json.dumps(plan))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("l2-agent passed 1 of 6 finished trials (floor 50%)", self.result()["reason"])

    def test_ungated_factory_setup_never_fails_the_job(self):
        plan = {"factory-setup": {"factory": "fail,fail,fail"}}
        result = self.run_full(LADDER_CLASSES=json.dumps(plan))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ladder_deadline_leaves_a_rung_incomplete_and_fails(self):
        result = self.run_full("--eval-timeout", "5", HANG_SUITE="l4-automation")
        self.assertEqual(result.returncode, 1, result.stderr)
        verdict = self.result()
        self.assertIn("l4-automation finished only 0 trials before the deadline", verdict["reason"])
        self.assertEqual(verdict["stopped_at_deadline"], 1)

    def test_loopback_endpoint_is_not_scrubbed_from_artifacts_but_a_remote_host_is(self):
        sys.path.insert(0, str(self.root / "repo/scripts/evals"))
        self.addCleanup(sys.path.remove, str(self.root / "repo/scripts/evals"))
        import live_engineer_eval as driver
        self.assertEqual(driver.redact_text("http://10.1.2.3:8000/v1/models at 10.1.2.3", "http://10.1.2.3:8000/v1"),
                         "<model endpoint>/models at <model endpoint>")
        self.assertEqual(driver.redact_text("server on 127.0.0.1:9493", "http://127.0.0.1:8000/v1"), "server on 127.0.0.1:9493")

    def test_a_server_that_ignores_sigterm_does_not_outlive_the_eval(self):
        pid_file = self.root / "linger.pid"
        result = self.run_eval("pass,pass,pass", LINGER_PID=str(pid_file), LIVE_EVAL_STOP_GRACE_SECS="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        pid = int(pid_file.read_text())
        import time
        for _ in range(50):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            try:
                os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                pass
            time.sleep(0.1)
        else:
            os.kill(pid, 9)
            self.fail("the lingering server survived the eval")

    def test_requires_a_fresh_home(self):
        home = self.root / "home"
        home.mkdir()
        result = self.drive("run", "--out", str(self.out), "--gents", str(self.gents), "--home", str(home), CLASSES="pass")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fresh home", result.stderr)

    def test_rejects_a_threshold_above_the_trials(self):
        result = self.drive("probe", "--out", str(self.out), "--trials", "2", "--required", "3")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
