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
MAILBOX = "crates/gents/tests/fixtures/configurator_evals/engineer_mailbox"
SECRET_URL = "http://127.0.0.1:{port}/v1"
MODEL = "inkling-2-mini"

FAKE_LADDER = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
env = os.environ
home = pathlib.Path(env["GENTS_EVAL_HOME"])
home.mkdir(parents=True, exist_ok=True)
print("ladder sees", json.load(open(env["GENTS_EVAL_TARGET_FILE"]))["inference_backends"][0]["endpoint"])
selection = sys.argv[1]
record = {key: env.get(key) for key in ["GENTS_EVAL_TEMPERATURE", "GENTS_EVAL_TOP_P", "GENTS_EVAL_SPLITS", "GENTS_EVAL_TARGET", "GENTS_EVAL_SHARD", "GENTS_EVAL_PORT", "GENTS_EVAL_COMMAND_EXECUTION"]}
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
sys.path.insert(0, str(pathlib.Path(__file__).parent))
import suites as registry
matrix = registry.matrix(pathlib.Path(__file__).resolve().parents[2])
index, total = map(int, env["GENTS_EVAL_SHARD"].split("/"))
plan = json.loads(env.get("LADDER_CLASSES", "{}"))
suites = {s["id"]: s for s in matrix["suites"]}
runs = [(suite, plan.get(suite, {"case-a": "pass,pass,pass", "case-b": "pass,fail,pass"})) for suite in matrix["default"][index - 1::total]]
if env.get("HANG_SUITE") in [suite for suite, _ in runs]:
    import time
    time.sleep(60)
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
    definition_id = suites[suite]["definition_id"]
    view = {"report": {"run": {"run_id": run.name, "split": "train", "definition": {"definition_id": definition_id}},
                       "cells": [{"cell_id": "engineer", "slots": slots, "usage": {"input_tokens": 1000, "output_tokens": 50}}]}, "trials": trials}
    (run / "report.json").write_text(json.dumps(view))
'''

FAKE_CONTAINER = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
env = os.environ
command, args = sys.argv[1], sys.argv[2:]
options = {}
for flag, value in zip(args[::2], args[1::2]):
    options.setdefault(flag, []).append(value)
run_dir = pathlib.Path(env["GENTS_EVAL_CONTAINER_ROOT"]) / options["--name"][0]


def summarize(states):
    slots = []
    for suite, case_id, split, index, cls in json.loads((run_dir / "plan.json").read_text()):
        state = states.get(cls, "done")
        slot = f"{suite}.{case_id}.t{index}"
        entry = {"slot": slot, "suite": suite, "case_id": case_id, "split": split, "trial_index": index,
                 "state": state, "attempts": 1}
        if state == "done":
            entry.update({"class": cls, "score_bp": 10000 if cls == "pass" else 0, "input_tokens": 1000, "output_tokens": 50,
                          "verdicts": [] if cls == "pass" else [{"stage_id": "both", "check": "crew_spec_match", "kind": "failed", "detail": "1 of 2 requirements met"}]})
            export = run_dir / "slots" / slot / "attempts/1/eval"
            report = export / "home/eval/runs/run-1/report.json"
            report.parent.mkdir(parents=True, exist_ok=True)
            report.write_text(json.dumps({"report": {"cells": [{"slots": [{"case_id": case_id, "latest": {"trial_id": "t"}}]}]},
                                          "trials": {"t": {"live": {"tool_calls": 7, "model_turns": 4}}}}))
            (run_dir / "slots" / slot / "state.json").write_text(json.dumps({"state": "done", "run_id": "run-1", "export": str(export)}))
        slots.append(entry)
    totals = {"slots": len(slots), "unfinished": sum(s["state"] != "done" for s in slots),
              "input_tokens": sum(s.get("input_tokens", 0) for s in slots), "output_tokens": sum(s.get("output_tokens", 0) for s in slots)}
    mode = env.get("FAKE_MODE") or (run_dir / "mode").read_text()
    (run_dir / "summary.json").write_text(json.dumps({"command_execution": mode, "results": "unauthenticated",
                                                      "image_commit": "abc", "totals": totals, "slots": slots}))
    (run_dir / "orchestrator.pid").unlink(missing_ok=True)


if command == "start":
    if env.get("FAIL_START"):
        sys.exit(2)
    run_dir.mkdir(parents=True)
    target = json.load(open(options["--target"][0]))
    record = {"argv": sys.argv[1:], "host": env.get("GENTS_EVAL_CONTAINER_HOST"), "endpoint": target["inference_backends"][0]["endpoint"],
              "provider_kind": target["inference_backends"][0]["provider_kind"]}
    plan = []
    classes = json.loads(env.get("SUITE_CLASSES", "{}"))
    if "--definition" in options:
        definition = pathlib.Path(options["--definition"][0])
        cases = json.load(open(definition / "pack_config.json"))["eval_definitions"][0]["cases"]
        record["cases"] = cases
        case = json.load(open(definition / cases[0]))
        for index, cls in enumerate(c for c in env.get("CLASSES", "").split(",") if c):
            plan.append([options["--suite"][0], case["case_id"], case["split"], index, cls])
    else:
        for suite in options["--suite"]:
            plan.append([suite, "case-a", "train", 0, classes.get(suite, "pass")])
    with open(env["RECORD"], "a") as stream:
        stream.write(json.dumps(record) + "\n")
    (run_dir / "plan.json").write_text(json.dumps(plan))
    (run_dir / "mode").write_text(options["--command-execution"][0])
    (run_dir / "logs/slot").mkdir(parents=True)
    (run_dir / "logs/slot/attempt-1.log").write_text("relay to " + env["ENDPOINT_FOR_LOG"] + "\n")
    if env.get("STOP_ENDPOINT"):
        pathlib.Path(env["STOP_ENDPOINT"]).write_text("stop")
    (run_dir / "orchestrator.pid").write_text("1")
    if not env.get("HANG"):
        summarize({"hang": "timed out"})
elif command == "stop":
    with open(env["RECORD"], "a") as stream:
        stream.write(json.dumps({"argv": sys.argv[1:]}) + "\n")
    summarize({"hang": "stopped"})
'''

FAKE_GENTS = r'''#!/usr/bin/env python3
import json, os, sys
print(json.dumps({"argv": sys.argv[1:], "requests": [{"prompt": "uses " + os.environ["ENDPOINT_FOR_LOG"]}]}))
'''


class Models(http.server.BaseHTTPRequestHandler):
    served = [MODEL]
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
        for name in ("live_engineer_eval.py", "matrix.json", "suites.py"):
            shutil.copy(SOURCE / "scripts/evals" / name, scripts)
        sys.path.insert(0, str(SOURCE / "scripts/evals"))
        import suites
        for suite in suites.suites(SOURCE):
            for relative in [suite["path"], suite["subject"]]:
                if not (self.root / "repo" / suites.FIXTURES / relative).exists():
                    shutil.copytree(SOURCE / suites.FIXTURES / relative, self.root / "repo" / suites.FIXTURES / relative)
        self.assertTrue((self.root / "repo" / MAILBOX / "suite.json").is_file())
        (scripts / "fake_ladder.py").write_text(FAKE_LADDER)
        (scripts / "run-ladder.sh").write_text('exec "$PYTHON" "$(dirname "$0")/fake_ladder.py" "$@"\n')
        (scripts / "fake_container.py").write_text(FAKE_CONTAINER)
        (scripts / "run-container.sh").write_text('exec "$PYTHON" "$(dirname "$0")/fake_container.py" "$@"\n')
        self.gents = self.root / "gents"
        self.gents.write_text(FAKE_GENTS)
        self.gents.chmod(0o755)
        self.out = self.root / "work/out"
        self.runs = self.root / "runs"
        self.record = self.root / "record.json"
        self.stop = self.root / "stop"
        handler = type("Handler", (Models,), {"stop_file": str(self.stop)})
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = SECRET_URL.format(port=self.server.server_address[1])

    def drive(self, *arguments, **environment):
        env = dict(os.environ, GENTS_CI_LIVE_MODEL_BASE_URL=self.url, GENTS_CI_LIVE_MODEL_NAME=MODEL,
                   RECORD=str(self.record), ENDPOINT_FOR_LOG=self.url, PYTHON=sys.executable, LIVE_EVAL_POLL_SECS="0.1")
        env.update(environment)
        script = self.root / "repo/scripts/evals/live_engineer_eval.py"
        return subprocess.run([sys.executable, str(script), *arguments], env=env, capture_output=True, text=True)

    def contained(self, scope, *arguments, **environment):
        return self.drive("run", "--scope", scope, "--out", str(self.out), "--image", "gents-eval:abc",
                          "--container-root", str(self.runs), *arguments, **environment)

    def run_eval(self, classes, *arguments, **environment):
        return self.contained("case", *arguments, CLASSES=classes, **environment)

    def records(self):
        return [json.loads(line) for line in self.record.read_text().splitlines()]

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

    def test_the_case_runs_the_shipped_engineer_unrestricted_in_containers_and_records_it(self):
        result = self.run_eval("pass,fail,pass")
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        self.assertEqual((verdict["outcome"], verdict["passes"], verdict["min_pass_rate"], verdict["model"]),
                         ("pass", 2, 0.5, MODEL))
        self.assertEqual(verdict["gents_revision"], "abc", "a contained run records the commit its image was built from")
        case = verdict["cases"]["engineer-mailbox/kiln-flag-and-question"]
        self.assertEqual((case["planned"], case["finished"], case["passes"], case["pass_rate"], case["verdict"]),
                         (3, 3, 2, 0.6667, "pass"))
        self.assertEqual(case["pass_rate_95"], [0.2077, 0.9385])
        self.assertEqual((case["input_tokens_per_pass"], case["tool_calls_per_pass"]), (1000, 7))
        self.assertEqual((verdict["command_execution"], verdict["isolation"], verdict["results"]),
                         ("unrestricted", "a container per trial", "unauthenticated"))
        self.assertEqual((verdict["suite"], verdict["case_id"], verdict["subject"]),
                         ("engineer-mailbox", "kiln-flag-and-question", "engineer_full"))
        self.assertEqual(verdict["tokens"], {"input": 3000, "output": 150})
        [record] = self.records()
        argv = record["argv"]
        option = lambda flag: argv[argv.index(flag) + 1]
        self.assertEqual((record["host"], record["provider_kind"]), ("local", "Vllm"))
        self.assertEqual((option("--suite"), option("--splits"), option("--trials"), option("--command-execution"), option("--image"),
                          option("--per-replica")), ("engineer-mailbox", "train", "3", "unrestricted", "gents-eval:abc", "8"))
        self.assertEqual(record["cases"], ["./cases/kiln_flag_and_question.json"])
        self.assertEqual(sorted(p.name for p in (self.out / "trials").iterdir()),
                         [f"engineer-mailbox.kiln-flag-and-question.t{i}.report.json" for i in range(3)])
        self.assertTrue((self.out / "container/summary.json").is_file())
        self.assertEqual(list(self.runs.iterdir()), [], "the run's exports are removed once its record is kept")
        summary = (self.out / "summary.md").read_text()
        self.assertIn("Live Engineer eval: PASS", summary)
        self.assertIn("Command execution: unrestricted, isolation: a container per trial; results unauthenticated", summary)
        self.assertIn("at least 50% of 3 trials", summary)
        self.assertIn("| engineer-mailbox/kiln-flag-and-question | 2 / 3 (3) | 67% (21%-94%) | 1000.0 / 50.0 | 7.0 |", summary)
        self.assertIn("both/crew_spec_match: 1 of 2 requirements met", summary)

    def test_minority_fails(self):
        result = self.run_eval("pass,fail,fail")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.result()["outcome"], "fail")
        self.assertIn("1 of 3 graded trials passed", self.result()["reason"])

    def test_missing_or_unfinished_trials_fail_even_when_the_present_ones_pass(self):
        result = self.run_eval("pass,pass")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("1 of 3 planned trials have no grade", self.result()["reason"])
        shutil.rmtree(self.out)
        result = self.run_eval("pass,pass,hang")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("1 of 3 planned trials have no grade", self.result()["reason"])

    def test_a_launcher_that_does_not_start_is_a_harness_error(self):
        result = self.run_eval("pass", FAIL_START="1")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.result()["outcome"], "error")
        self.assertIn("run-container.sh start exited 2", self.result()["reason"])

    def test_a_run_under_another_mode_is_refused(self):
        result = self.run_eval("pass,pass,pass", FAKE_MODE="off")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("recorded command execution 'off'", self.result()["reason"])

    def test_a_contained_run_needs_a_loopback_endpoint(self):
        result = self.run_eval("pass,pass,pass", GENTS_CI_LIVE_MODEL_BASE_URL=self.url.replace("127.0.0.1", "localhost"))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("must be http://127.0.0.1:<port>/v1", self.result()["reason"])
        self.assertFalse(self.record.exists())

    def test_a_contained_scope_needs_an_image(self):
        result = self.drive("run", "--scope", "case", "--out", str(self.out))
        self.assertEqual(result.returncode, 2)
        self.assertIn("needs --image", result.stderr)

    def test_endpoint_lost_during_the_run_is_not_evidence(self):
        result = self.run_eval("fail,fail,fail", STOP_ENDPOINT=str(self.stop))
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(self.result()["outcome"], "skipped")
        self.assertIn("after the run started", self.result()["reason"])

    def test_artifacts_never_carry_the_endpoint(self):
        self.run_eval("pass,pass,pass")
        for path in self.out.rglob("*"):
            if path.is_file():
                self.assertNotIn(self.url, path.read_text(), path)
        self.assertIn("<model endpoint>", (self.out / "container/logs/slot/attempt-1.log").read_text())

    def test_suites_run_every_suite_on_the_shipped_engineer_and_record_each_rate(self):
        result = self.contained("suites", "--trials", "1",
                                SUITE_CLASSES=json.dumps({"engineer-goals": "fail"}))
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        sys.path.insert(0, str(SOURCE / "scripts/evals"))
        import suites
        shipped = [s["id"] for s in suites.suites(SOURCE) if s["subject"] == "engineer_full"]
        argv = self.records()[0]["argv"]
        self.assertEqual([argv[i + 1] for i, flag in enumerate(argv) if flag == "--suite"], shipped)
        self.assertNotIn("--definition", argv)
        rows = {row["suite"]: row for row in verdict["suites"]}
        self.assertEqual(rows["engineer-goals"], {"suite": "engineer-goals", "trials": 1, "finished": 1, "passes": 0,
                                                  "pass_rate": 0.0, "pass_rate_95": [0.0, 0.7935]})
        self.assertEqual(verdict["cases"]["engineer-goals/case-a"]["verdict"], "fail")
        self.assertEqual(verdict["command_execution"], "unrestricted")
        self.assertIn("Live Engineer suites: PASS", (self.out / "summary.md").read_text())

    def test_suites_stopped_at_the_deadline_fail_with_what_did_not_finish(self):
        result = self.contained("suites", "--trials", "1", "--eval-timeout", "1", HANG="1",
                                SUITE_CLASSES=json.dumps({"engineer-goals": "hang"}))
        self.assertEqual(result.returncode, 1, result.stderr)
        verdict = self.result()
        self.assertTrue(verdict["stopped_at_deadline"])
        self.assertEqual(self.records()[-1]["argv"][:1], ["stop"])
        self.assertIn("1 of 10 trials did not finish with a grade", verdict["reason"])

    def run_full(self, *arguments, **environment):
        return self.drive("run", "--scope", "ladder", "--out", str(self.out), "--gents", str(self.gents),
                          "--stagger", "0", *arguments, **environment)

    def test_ladder_passes_when_every_gated_rung_meets_its_floor(self):
        result = self.run_full()
        self.assertEqual(result.returncode, 0, result.stderr)
        verdict = self.result()
        self.assertEqual(verdict["outcome"], "pass")
        self.assertEqual((verdict["command_execution"], verdict["isolation"]), ("off", "embedded trials on the runner"))
        records = self.records()
        self.assertEqual(sorted(r["GENTS_EVAL_SHARD"] for r in records), ["1/4", "2/4", "3/4", "4/4"])
        self.assertEqual(len({r["GENTS_EVAL_PORT"] for r in records}), 4)
        self.assertTrue(all(r["argv"] == ["all", "3", "2"] and r["GENTS_EVAL_TEMPERATURE"] == "" and r["GENTS_EVAL_TOP_P"] == ""
                            and r["GENTS_EVAL_COMMAND_EXECUTION"] == "off" for r in records))
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
        result = self.run_full("--shards", "1", LINGER_PID=str(pid_file), LIVE_EVAL_STOP_GRACE_SECS="1")
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
        self.out.mkdir(parents=True)
        (self.out / "result.json").write_text(json.dumps({"outcome": "pass", "reason": "3 of 3 trials passed"}))
        result = self.run_full("--home", str(home))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("fresh home", result.stderr)
        self.assertEqual(self.result()["outcome"], "error")
        self.assertIn("fresh home", self.result()["reason"])

    def test_rejects_a_pass_rate_above_one(self):
        result = self.drive("probe", "--out", str(self.out), "--min-pass-rate", "1.5")
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
