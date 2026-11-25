#!/usr/bin/env python3
"""Run live Engineer evals against a served model and judge them.

  live_engineer_eval.py probe --out DIR
  live_engineer_eval.py run --out DIR --scope case|suites --image TAG [--container-root DIR]
  live_engineer_eval.py run --out DIR --scope ladder --gents BIN [--home DIR]

Both read GENTS_CI_LIVE_MODEL_BASE_URL (OpenAI-compatible vLLM, including
/v1) and GENTS_CI_LIVE_MODEL_NAME, and write DIR/result.json and
DIR/summary.md. Exit status: 0 pass (or probe usable), 1 fail or harness
error, 3 skipped because the endpoint is not configured, unreachable or not
serving the model.

Every run uses the target's own sampling (no temperature or top_p override).
Graders read stored documents and files, and the reply where the reply is the
answer. Each result records the command-execution mode its Engineer ran under
and the isolation that held it.

Every scope runs the shipped Engineer: the one `gents init --engineer`
makes at this revision, every core pack installed. Scopes `case` and
`suites` run it with the bash and background processes init gives it, each
trial in its own container (scripts/evals/run-container.sh on this machine,
image TAG); the endpoint must be a loopback URL. `case` runs engineer-mailbox
kiln-flag-and-question. `suites` runs the train and validation cases of
every suite outside the ladder's default list and fails only when a trial
does not finish with a grade.

Scope `ladder` runs it with command execution off (no bash, no background
processes; embedded trials) through scripts/evals/run-ladder.sh: the ladder's
rungs, train and validation cases, over SHARDS launchers until the deadline.
Case verdicts are a trend; the job fails when a rung passes fewer than
RUNG_FLOOR of its graded trials or grades fewer than MIN_FINISHED before the
deadline.

Every scope runs TRIALS trials per case, CONCURRENCY at once: the ceiling is
the isolated environments the host holds, not the model server. A case passes
when at least MIN_PASS_RATE of its trials pass the grader. Every result
records, per case, the trials planned and graded, the passes, the pass rate
with its 95% Wilson interval, and the tokens and tool calls per passing
trial, beside the model id, the Gents revision, the command-execution mode
and the isolation.
"""
import argparse
import collections
import json
import math
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

import suites

ROOT = pathlib.Path(__file__).resolve().parents[2]
SUITE = "engineer-mailbox"
CASE = "kiln-flag-and-question"
SHIPPED_SUBJECT = "gents init --engineer"
CELL = "engineer"
TARGET = "live-ci"
PROVIDER_KIND = "Vllm"
MIN_FINISHED = 6
MAX_TRANSCRIPTS = 15
SKIPPED = 3
PROBE_TIMEOUT_SECS = 15
SAMPLING = "target defaults (no override)"
RUN_CONTAINER = ROOT / "scripts/evals/run-container.sh"
POLL_SECS = float(os.environ.get("LIVE_EVAL_POLL_SECS", "15"))
STOP_WAIT_SECS = 300
MODES = {
    "case": {"command_execution": "unrestricted", "isolation": "a container per trial"},
    "suites": {"command_execution": "unrestricted", "isolation": "a container per trial"},
    "ladder": {"command_execution": "off", "isolation": "embedded trials on the runner"},
}


def write_result(out, result):
    out.mkdir(parents=True, exist_ok=True)
    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    (out / "summary.md").write_text(render_summary(result))


def endpoint():
    return (os.environ.get("GENTS_CI_LIVE_MODEL_BASE_URL", "").strip(),
            os.environ.get("GENTS_CI_LIVE_MODEL_NAME", "").strip())


def probe(base_url, model):
    """Returns None when the endpoint serves the model, else the reason it cannot be used."""
    if not base_url:
        return "GENTS_CI_LIVE_MODEL_BASE_URL is not set"
    if not model:
        return "GENTS_CI_LIVE_MODEL_NAME is not set"
    try:
        with urllib.request.urlopen(base_url.rstrip("/") + "/models", timeout=PROBE_TIMEOUT_SECS) as response:
            served = [entry.get("id") for entry in json.load(response).get("data", [])]
    except (urllib.error.URLError, OSError, ValueError, AttributeError) as error:
        return f"the model endpoint is unreachable ({type(error).__name__}: {redact_text(str(error), base_url)})"
    if model not in served:
        return f"the model endpoint does not serve {model} (it serves: {', '.join(map(str, served)) or 'nothing'})"
    return None


def redaction_terms(base_url):
    terms = [base_url.rstrip("/")] if base_url else []
    host = urllib.parse.urlparse(base_url).hostname if base_url else None
    if host and len(host) > 3 and host not in ("localhost", "127.0.0.1", "::1"):
        terms.append(host)
    return terms


def redact_text(text, base_url):
    for term in redaction_terms(base_url):
        text = text.replace(term, "<model endpoint>")
    return text


def redact_tree(out, base_url):
    """Artifacts are public on a public repository; the endpoint address is not."""
    terms = [term.encode() for term in redaction_terms(base_url)]
    for path in out.rglob("*"):
        if path.is_file():
            data = path.read_bytes()
            redacted = data
            for term in terms:
                redacted = redacted.replace(term, b"<model endpoint>")
            if redacted != data:
                path.write_bytes(redacted)


def one_case_definition(destination, suite_id=SUITE):
    """Copies the suite's definition with only CASE, so another case added to the suite does not join the run."""
    suite = suites.suite(ROOT, suite_id)
    shutil.copytree(ROOT / suites.FIXTURES / suite["path"], destination)
    config = json.loads((destination / "pack_config.json").read_text())
    definition = config["eval_definitions"][0]
    cases = [path for path in definition["cases"]
             if json.loads((destination / path).read_text())["case_id"] == CASE]
    if len(cases) != 1:
        raise SystemExit(f"{SUITE} has {len(cases)} cases named {CASE}")
    definition["cases"] = cases
    (destination / "pack_config.json").write_text(json.dumps(config, indent=2) + "\n")
    return json.loads((destination / cases[0]).read_text())["split"]


def target_file(path, base_url, model):
    path.write_text(json.dumps({
        "node": {},
        "inference_backends": [{
            "backend_id": TARGET, "name": "Live CI model", "provider_kind": PROVIDER_KIND,
            "openai_wire_api": "chat_completions", "endpoint": base_url,
            "auth": {"kind": "unauthenticated"}, "max_concurrent": 32, "max_queue_depth": 100,
        }],
        "inference_profiles": [{"profile_id": TARGET, "backend_id": TARGET, "model_name": model}],
    }, indent=2) + "\n")


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def end_group(process, grace=float(os.environ.get("LIVE_EVAL_STOP_GRACE_SECS", "30"))):
    """Stops a launcher's process group: run-ladder.sh leaves its runtime server
    serving in it, and a busy server can outlive SIGTERM."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        process.poll()
        try:
            os.killpg(process.pid, 0)
        except (ProcessLookupError, PermissionError):
            return
        time.sleep(0.5)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def launch(args, home, log_path, target, selection, concurrency, **extra):
    env = dict(os.environ, GENTS_BIN=str(args.gents), GENTS_EVAL_HOME=str(home), GENTS_EVAL_TARGET=TARGET,
               GENTS_EVAL_TARGET_FILE=str(target), GENTS_EVAL_PORT=str(free_port()), GENTS_EVAL_WATCH="0",
               GENTS_EVAL_TEMPERATURE="", GENTS_EVAL_TOP_P="",
               GENTS_EVAL_COMMAND_EXECUTION=MODES[args.scope]["command_execution"], **extra)
    command = ["bash", str(ROOT / "scripts/evals/run-ladder.sh"), selection, str(args.trials), str(concurrency)]
    log = open(log_path, "w")
    return subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True), log


def wait_all(processes, deadline):
    """Returns each launcher's exit status, or None for one stopped at the deadline.
    Reports stay readable after their launcher's group is ended."""
    statuses = []
    for process, log in processes:
        try:
            statuses.append(process.wait(timeout=max(1, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            end_group(process)
            process.wait()
            statuses.append(None)
        log.close()
    return statuses


def end_launchers(processes):
    for process, _ in processes:
        end_group(process)


def collect_trial(args, home, destination, run_id, case_id, index):
    destination.parent.mkdir(parents=True, exist_ok=True)
    with open(destination, "w") as stream, open(destination.with_suffix(".log"), "w") as log:
        subprocess.run([str(args.gents), "utils", "eval", "trial", run_id, CELL, case_id, str(index),
                        "--json", "--requests", "--home", str(home)],
                       stdout=stream, stderr=log, timeout=300, check=False)


def trial_row(slot, trials):
    latest = slot.get("latest") or {}
    live = (trials.get(latest.get("trial_id")) or {}).get("live") or {}
    unmet = []
    for stage_id, stage in (live.get("stages") or {}).items():
        for check in stage.get("checks", []):
            unmet += [f"{stage_id}: {item}" for item in (check.get("raw") or {}).get("unmet") or []]
    return {
        "trial_index": slot["trial_index"],
        "class": slot["class"],
        "score_bp": slot.get("score_bp"),
        "stages": [{"stage_id": s["stage_id"], "terminal_state": s.get("terminal_state"),
                    "failure_kind": s.get("failure_kind")} for s in latest.get("stages", [])],
        "agent_documents": (live.get("documents") or {}).get("Agent"),
        "tool_calls": live.get("tool_calls"),
        "tokens": trial_tokens(latest, live),
        "failed_tool_calls": live.get("failed_tool_calls"),
        "model_turns": live.get("model_turns"),
        "elapsed_secs": live.get("elapsed_secs"),
        "failed_verdicts": [f"{v['stage_id']}/{v['check']}: {v.get('detail') or v.get('reason_code')}"
                            for v in slot.get("verdicts", []) if v["kind"] != "passed"],
        "unmet": unmet,
        "tools": live.get("tools") or {},
        "last_tool": live.get("last_tool"),
    }


def trial_tokens(latest, live):
    """The trial's provider-reported usage, or the counts its live snapshot observed when the provider
    reported none."""
    usage = (latest or {}).get("usage") or {}
    if usage.get("input_tokens") is None and usage.get("output_tokens") is None:
        usage = live or {}
    return {"input": usage.get("input_tokens"), "output": usage.get("output_tokens")}


def report_trial(view, case_id):
    """The slot for CASE_ID in a one-trial run's report view, with its live snapshot."""
    for cell in (view or {}).get("report", {}).get("cells", []):
        for slot in cell.get("slots", []):
            if slot.get("case_id") == case_id:
                return slot, (view.get("trials") or {})
    return None, {}


def wilson(passes, n, z=1.96):
    """The 95% Wilson score interval of a pass rate."""
    if not n:
        return None
    rate = passes / n
    scale = 1 + z * z / n
    centre = (rate + z * z / (2 * n)) / scale
    half = z * math.sqrt(rate * (1 - rate) / n + z * z / (4 * n * n)) / scale
    return [round(max(0.0, centre - half), 4), round(min(1.0, centre + half), 4)]


def mean(values):
    values = [value for value in values if isinstance(value, (int, float))]
    return round(sum(values) / len(values), 1) if values else None


def case_stats(rows, planned, min_pass_rate):
    """A case's trials judged against MIN_PASS_RATE of PLANNED: pass once enough passed, fail once the
    trials left cannot reach it, else incomplete; rates count graded trials only."""
    graded = [row for row in rows if row["class"] in ("pass", "fail")]
    passed = [row for row in graded if row["class"] == "pass"]
    needed = max(1, math.ceil(min_pass_rate * planned))
    if len(passed) >= needed:
        verdict = "pass"
    elif len(passed) + planned - len(graded) < needed:
        verdict = "fail"
    else:
        verdict = "incomplete"
    return {"planned": planned, "finished": len(graded), "passes": len(passed),
            "pass_rate": round(len(passed) / len(graded), 4) if graded else None,
            "pass_rate_95": wilson(len(passed), len(graded)),
            "input_tokens_per_pass": mean(row["tokens"]["input"] for row in passed),
            "output_tokens_per_pass": mean(row["tokens"]["output"] for row in passed),
            "tool_calls_per_pass": mean(row.get("tool_calls") for row in passed),
            "verdict": verdict}


def finish(result, out, base_url, started, args):
    if result["outcome"] not in ("pass", "skipped"):
        lost = probe(base_url, result.get("model"))
        if lost:
            result.update(outcome="skipped", reason=f"{lost} after the run started; the result is not evidence ({result['reason']})")
    result["eval_secs"] = round(time.monotonic() - started)
    if args.job_started:
        result["job_secs_at_report"] = round(time.time() - args.job_started)
    write_result(out, result)
    redact_tree(out, base_url)
    return {"pass": 0, "skipped": SKIPPED}.get(result["outcome"], 1)


def copy_logs(home, out, prefix=""):
    for log in [home / "server.log", *home.glob("batch-*.log")]:
        if log.is_file():
            shutil.copy(log, out / f"{prefix}{log.name}")


def container_suites():
    """Every suite the ladder scope does not run: those outside matrix.json's default list."""
    registry = suites.matrix(ROOT)
    return [suite["id"] for suite in registry["suites"] if suite["id"] not in registry["default"]]


def run_container(args, *arguments, log):
    env = dict(os.environ, GENTS_EVAL_CONTAINER_HOST="local", GENTS_EVAL_CONTAINER_ROOT=str(args.container_root))
    return subprocess.run(["bash", str(RUN_CONTAINER), *arguments], env=env, stdout=log, stderr=subprocess.STDOUT,
                          check=False).returncode


def wait_for_orchestrator(args, name, run_dir, deadline, log):
    """Returns whether the run ended by itself; past the deadline it is stopped."""
    pid_file = run_dir / "orchestrator.pid"
    while pid_file.exists():
        if time.monotonic() >= deadline:
            run_container(args, "stop", "--name", name, log=log)
            stop_by = time.monotonic() + STOP_WAIT_SECS
            while pid_file.exists() and time.monotonic() < stop_by:
                time.sleep(1)
            return False
        time.sleep(POLL_SECS)
    return True


def contained_row(entry, view):
    slot, trials = report_trial(view, entry.get("case_id"))
    live = (trials.get(((slot or {}).get("latest") or {}).get("trial_id")) or {}).get("live") or {}
    return {
        "suite": entry.get("suite"), "case_id": entry.get("case_id"), "split": entry.get("split"),
        "trial_index": entry.get("trial_index"), "state": entry.get("state"), "attempts": entry.get("attempts"),
        "class": entry.get("class"), "score_bp": entry.get("score_bp"),
        "tokens": trial_tokens({"usage": entry}, live),
        "tool_calls": live.get("tool_calls"), "model_turns": live.get("model_turns"),
        "failed_verdicts": [f"{v.get('stage_id')}/{v.get('check')}: {v.get('detail') or v.get('reason_code')}"
                            for v in entry.get("verdicts", []) if v.get("kind") != "passed"],
    }


def keep_run_record(run_dir, out, summary):
    """Copies what the run recorded about itself into OUT, removes the run's exports, and returns each
    finished slot's report view."""
    views = {}
    record = out / "container"
    record.mkdir(parents=True, exist_ok=True)
    for name in ("run.json", "summary.json", "summary.txt", "orchestrator.log"):
        if (run_dir / name).is_file():
            shutil.copy(run_dir / name, record / name)
    if (run_dir / "logs").is_dir():
        shutil.copytree(run_dir / "logs", record / "logs", dirs_exist_ok=True)
    for entry in (summary or {}).get("slots", []):
        state = json.loads((run_dir / "slots" / entry["slot"] / "state.json").read_text()) \
            if (run_dir / "slots" / entry["slot"] / "state.json").is_file() else {}
        report = pathlib.Path(state.get("export") or "/nonexistent") / "home/eval/runs" / str(state.get("run_id")) / "report.json"
        if entry.get("state") == "done" and report.is_file() and not report.is_symlink():
            (out / "trials").mkdir(parents=True, exist_ok=True)
            shutil.copy(report, out / "trials" / f"{entry['slot']}.report.json")
            try:
                views[entry["slot"]] = json.loads(report.read_text())
            except ValueError:
                pass
    shutil.rmtree(run_dir, ignore_errors=True)
    return views


def run_contained(args, out, work, result, selected, definition=None, split=None):
    """Runs SELECTED suites' train and validation cases, each trial in its own container, and
    returns the run's summary, or None when it left none."""
    if urllib.parse.urlparse(work_endpoint(work)).hostname != "127.0.0.1":
        result.update(outcome="error", reason="a contained run reaches the model through a relay on this machine; "
                                              "GENTS_CI_LIVE_MODEL_BASE_URL must be http://127.0.0.1:<port>/v1")
        return None
    name = f"live-{args.scope}-{int(time.time())}"
    run_dir = args.container_root / name
    command = ["start", "--name", name, "--image", args.image, "--target", str(work / f"{TARGET}.json"),
               "--trials", str(args.trials), "--per-replica", str(args.concurrency),
               "--command-execution", MODES[args.scope]["command_execution"],
               "--trial-timeout", str(args.trial_timeout)]
    for suite_id in selected:
        command += ["--suite", suite_id]
    if definition is not None:
        command += ["--definition", str(definition), "--splits", split]
    started = time.monotonic()
    with open(out / "container.log", "w") as log:
        status = run_container(args, *command, log=log)
        if status != 0:
            result.update(outcome="error", reason=f"run-container.sh start exited {status}; see container.log")
            return None
        finished = wait_for_orchestrator(args, name, run_dir, started + args.eval_timeout, log)
    result["stopped_at_deadline"] = not finished
    summary_path = run_dir / "summary.json"
    summary = json.loads(summary_path.read_text()) if summary_path.is_file() else None
    views = keep_run_record(run_dir, out, summary)
    if summary is None:
        result.update(outcome="error", reason="the contained run left no summary; see container/orchestrator.log")
        return None
    summary["rows"] = [contained_row(entry, views.get(entry.get("slot"))) for entry in summary["slots"]]
    result.update(results=summary.get("results"), image=args.image, gents_revision=summary.get("image_commit"),
                  divergences=summary.get("divergences"),
                  tokens={side: sum(row["tokens"][side] or 0 for row in summary["rows"]) for side in ("input", "output")})
    if summary.get("command_execution") != MODES[args.scope]["command_execution"]:
        result.update(outcome="error", reason=f"the run recorded command execution {summary.get('command_execution')!r}")
        return None
    return summary


def work_endpoint(work):
    return json.loads((work / f"{TARGET}.json").read_text())["inference_backends"][0]["endpoint"]


def contained_cases(rows, args):
    cases = {}
    for row in rows:
        cases.setdefault(f"{row['suite']}/{row['case_id']}", []).append(row)
    return {key: dict(suite=members[0]["suite"], case_id=members[0]["case_id"], split=members[0]["split"],
                      **case_stats(members, args.trials, args.min_pass_rate))
            for key, members in cases.items()}


def run_case(args, out, home, work, result):
    split = one_case_definition(work / "definition")
    summary = run_contained(args, out, work, result, [SUITE], definition=work / "definition", split=split)
    if summary is None:
        return
    rows = summary["rows"]
    stats = case_stats(rows, args.trials, args.min_pass_rate)
    unfinished = args.trials - stats["finished"]
    result.update(cases=contained_cases(rows, args), trials_detail=rows, passes=stats["passes"],
                  outcome="pass" if stats["verdict"] == "pass" and not unfinished else "fail",
                  reason=f"{stats['passes']} of {stats['finished']} graded trials passed; "
                         f"{args.min_pass_rate:.0%} of {args.trials} required"
                         + (f"; {unfinished} of {args.trials} planned trials have no grade" if unfinished else ""))


def run_suites(args, out, home, work, result):
    selected = container_suites()
    summary = run_contained(args, out, work, result, selected)
    if summary is None:
        return
    rows = summary["rows"]
    suite_rows = []
    for suite_id in selected:
        members = [row for row in rows if row["suite"] == suite_id]
        graded = [row for row in members if row["class"] in ("pass", "fail")]
        passes = sum(row["class"] == "pass" for row in graded)
        suite_rows.append({"suite": suite_id, "trials": len(members), "finished": len(graded), "passes": passes,
                           "pass_rate": round(passes / len(graded), 4) if graded else None,
                           "pass_rate_95": wilson(passes, len(graded))})
    unfinished = [row for row in rows if row["state"] != "done"]
    result.update(suites=suite_rows, cases=contained_cases(rows, args), trials_detail=rows,
                  outcome="fail" if unfinished else "pass",
                  reason=(f"{len(unfinished)} of {len(rows)} trials did not finish with a grade" if unfinished else
                          f"every trial finished with a grade; {sum(r['passes'] for r in suite_rows)} of {len(rows)} passed"))


def run_full_ladder(args, out, home, work, result):
    registry = suites.matrix(ROOT)
    by_definition = {s["definition_id"]: s["id"] for s in registry["suites"]}
    rungs = registry["default"]
    homes = [home / f"s{index}" for index in range(1, args.shards + 1)]
    started = time.monotonic()
    processes = []
    for index, shard_home in enumerate(homes, 1):
        processes.append(launch(args, shard_home, out / f"ladder-s{index}.log", work / f"{TARGET}.json", "all",
                                max(1, args.concurrency // args.shards), GENTS_EVAL_SHARD=f"{index}/{args.shards}"))
        time.sleep(args.stagger)
    statuses = wait_all(processes, started + args.eval_timeout)
    result["launcher_exits"] = statuses
    cases, tokens, failing_slots = {}, collections.Counter(), []
    divergences = result.setdefault("divergences", [])
    for shard_home in homes:
        for report in sorted((shard_home / "eval" / "runs").glob("ladder-*/report.json")):
            view = json.loads(report.read_text())
            run = view["report"]["run"]
            recorded = view["report"].get("divergences")
            for item in recorded if recorded is not None else [
                    {"severity": "narrows", "kind": "divergences_unrecorded", "detail": f"run {run['run_id']} records none"}]:
                if item not in divergences:
                    divergences.append(item)
            rung = by_definition.get(run["definition"]["definition_id"], run["definition"]["definition_id"])
            shutil.copy(report, out / f"report-{run['run_id']}.json")
            for cell in view["report"]["cells"]:
                tokens.update({key: value for key, value in (cell.get("usage") or {}).items() if isinstance(value, int)})
                for slot in cell["slots"]:
                    entry = cases.setdefault(f"{rung}/{slot['case_id']}", {"rung": rung, "case_id": slot["case_id"],
                                                                         "split": run["split"], "trials": []})
                    entry["trials"].append(trial_row(slot, view.get("trials", {})))
                    entry.setdefault("_slots", []).append((shard_home, run["run_id"], slot))
    rung_rows = []
    for rung in rungs:
        members = {case_id: entry for case_id, entry in cases.items() if entry["rung"] == rung}
        passes = finished = 0
        for case_id, entry in members.items():
            entry.update(case_stats(entry["trials"], args.trials, args.min_pass_rate))
            passes += entry["passes"]
            finished += entry["finished"]
            if entry["verdict"] == "fail":
                failing_slots += [(rung, *item) for item in entry["_slots"] if item[2]["class"] == "fail"]
        if finished < MIN_FINISHED:
            verdict = "incomplete"
        else:
            verdict = "pass" if passes >= args.rung_floor * finished else "fail"
        rung_rows.append({"rung": rung, "cases": len(members),
                          "cases_meeting_threshold": sum(e["verdict"] == "pass" for e in members.values()),
                          "passes": passes, "finished": finished, "verdict": verdict})
    for entry in cases.values():
        entry.pop("_slots", None)
    for rung, shard_home, run_id, slot in failing_slots[:MAX_TRANSCRIPTS]:
        collect_trial(args, shard_home, out / "trials" / f"{rung}-{slot['case_id']}-{slot['trial_index']}.json",
                      run_id, slot["case_id"], slot["trial_index"])
    end_launchers(processes)
    for index, shard_home in enumerate(homes, 1):
        copy_logs(shard_home, out, f"s{index}-")
    failed = [r for r in rung_rows if r["verdict"] == "fail"]
    short = [r for r in rung_rows if r["verdict"] == "incomplete"]
    if failed or short:
        reason = "; ".join([f"{r['rung']} passed {r['passes']} of {r['finished']} finished trials (floor {args.rung_floor:.0%})" for r in failed]
                           + [f"{r['rung']} finished only {r['finished']} trials before the deadline" for r in short])
        outcome = "fail"
    else:
        reason = "every rung met its floor"
        outcome = "pass"
    result.update(outcome=outcome, reason=reason, rungs=rung_rows, cases=cases,
                  tokens={"input": tokens.get("input_tokens"), "output": tokens.get("output_tokens")},
                  deadline_secs=args.eval_timeout, stopped_at_deadline=sum(s is None for s in statuses))


def run(args):
    started = time.monotonic()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    # The workflow reports result.json's reason with the exit status: one left by
    # an earlier run must never stand for this one.
    for stale in ("result.json", "summary.md"):
        (out / stale).unlink(missing_ok=True)
    base_url, model = endpoint()
    result = base_result(args, model)
    blocked = probe(base_url, model)
    if blocked:
        write_result(out, dict(result, outcome="skipped", reason=blocked))
        return SKIPPED
    home = args.home or out.parent / "live-eval-home"
    if home.exists():
        reason = f"{home} exists; the eval needs a fresh home"
        write_result(out, dict(result, outcome="error", reason=reason))
        print(reason, file=sys.stderr)
        return 1
    work = out.parent / "live-eval-inputs"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    target_file(work / f"{TARGET}.json", base_url, model)
    {"case": run_case, "suites": run_suites, "ladder": run_full_ladder}[args.scope](args, out, home, work, result)
    return finish(result, out, base_url, started, args)


def base_result(args, model):
    result = {"scope": args.scope, "model": model, "gents_revision": revision(), "trials": args.trials,
              "concurrency": args.concurrency, "min_pass_rate": args.min_pass_rate, "sampling": SAMPLING,
              **MODES[args.scope]}
    if args.scope == "case":
        result.update(suite=SUITE, case_id=CASE, subject=SHIPPED_SUBJECT)
    elif args.scope == "suites":
        result.update(subject=SHIPPED_SUBJECT)
    else:
        result.update(rung_floor=args.rung_floor, min_finished=MIN_FINISHED,
                      shards=args.shards)
    return result


def revision():
    if os.environ.get("GITHUB_SHA"):
        return os.environ["GITHUB_SHA"]
    probe = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True, check=False)
    return probe.stdout.strip() or None


def render_summary(result):
    scope = result.get("scope", "case")
    title = {"case": "Live Engineer eval", "suites": "Live Engineer suites"}.get(scope, "Live Engineer ladder")
    lines = [f"## {title}: {result['outcome'].upper()}", ""]
    if result.get("divergences") is not None:
        lines += ["```", suites.banner(result["divergences"]), "```", ""]
    if scope == "case":
        lines += [f"- Case: `{result['suite']}` / `{result['case_id']}` on the shipped Engineer (`{result['subject']}`, every core pack)",
                  f"- Rule: pass when at least {result['min_pass_rate']:.0%} of {result['trials']} trials pass the grader"]
    elif scope == "suites":
        lines += [f"- Scope: train and validation cases of every suite outside the ladder on the shipped Engineer (`{result['subject']}`), "
                  f"{result['trials']} trial(s) each",
                  "- Rule: fails when a trial does not finish with a grade; pass rates are recorded"]
    else:
        lines += ["- Scope: the ladder's rungs, train and validation cases",
                  f"- Case threshold (recorded, a trend): at least {result['min_pass_rate']:.0%} of {result['trials']} trials pass the grader",
                  f"- Job rule: fails when a rung passes fewer than {result['rung_floor']:.0%} of its finished trials "
                  f"or finishes fewer than {result['min_finished']}"]
    lines += [f"- Model: `{result.get('model') or 'unset'}`, Gents `{result.get('gents_revision')}`, "
              f"sampling: {result.get('sampling')}",
              f"- Command execution: {result.get('command_execution')}, isolation: {result.get('isolation')}"
              + (f"; results {result['results']}" if result.get("results") else ""),
              f"- Result: {result['reason']}"]
    if result.get("eval_secs") is not None:
        lines.append(f"- Eval wall time: {result['eval_secs']} s"
                     + (f"; job time at report: {result['job_secs_at_report']} s" if result.get("job_secs_at_report") else ""))
    tokens = result.get("tokens") or {}
    if tokens.get("input") is not None:
        lines.append(f"- Tokens: {tokens['input']} input, {tokens.get('output')} output")
    if result["outcome"] == "skipped":
        lines += ["", "This run is not evidence about the product: the model endpoint could not be used."]
    if result.get("rungs"):
        lines += ["", "| Rung | Cases meeting threshold | Trials passed / finished | Verdict |", "|---|---|---|---|"]
        for row in result["rungs"]:
            lines.append(f"| {row['rung']} | {row['cases_meeting_threshold']} of {row['cases']} | "
                         f"{row['passes']} / {row['finished']} | {row['verdict']} |")
        for verdict in ("fail", "incomplete"):
            named = {case_id: entry for case_id, entry in result["cases"].items() if entry.get("verdict") == verdict}
            if named:
                lines += ["", f"Cases {'below the threshold' if verdict == 'fail' else 'without enough finished trials'}:"]
                for key, entry in sorted(named.items()):
                    missed = sorted({m for t in entry["trials"] for m in t["failed_verdicts"] + t["unmet"]})
                    lines.append(f"- `{entry['rung']}` / `{entry['case_id']}`: {entry['passes']} of {entry['finished']} finished passed"
                                 + (f"; missed: {'; '.join(missed[:3])}" if missed else ""))
    if result.get("suites"):
        lines += ["", "| Suite | Trials passed / finished / planned |", "|---|---|"]
        for row in result["suites"]:
            lines.append(f"| {row['suite']} | {row['passes']} / {row['finished']} / {row['trials']} |")
    if result.get("scope") != "ladder" and result.get("cases"):
        lines += ["", "| Case | Passed / graded (planned) | Pass rate (95% CI) | Tokens in / out per pass | Tool calls per pass |",
                  "|---|---|---|---|---|"]
        for key, case in sorted(result["cases"].items()):
            interval = case["pass_rate_95"]
            rate = "-" if case["pass_rate"] is None else f"{case['pass_rate']:.0%} ({interval[0]:.0%}-{interval[1]:.0%})"
            lines.append(f"| {key} | {case['passes']} / {case['finished']} ({case['planned']}) | {rate} | "
                         f"{case['input_tokens_per_pass']} / {case['output_tokens_per_pass']} | {case['tool_calls_per_pass']} |")
    contained = result.get("trials_detail") or []
    if contained:
        lines += ["", "| Suite / case | Trial | State | Grader | Tokens in / out |", "|---|---|---|---|---|"]
        for row in contained:
            lines.append(f"| {row['suite']} / {row['case_id']} | {row['trial_index']} | {row['state']} ({row['attempts']} attempt(s)) | "
                         f"{row['class']} ({row['score_bp']} bp) | {row['tokens']['input']} / {row['tokens']['output']} |")
        for row in contained:
            if row["failed_verdicts"]:
                lines += ["", f"{row['suite']} / {row['case_id']} trial {row['trial_index']} missed:"] + \
                         [f"- {item}" for item in row["failed_verdicts"][:20]]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["probe", "run"])
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--scope", choices=sorted(MODES), default="case")
    parser.add_argument("--gents", type=pathlib.Path, help="the gents binary scope ladder runs")
    parser.add_argument("--image", help="the eval image scopes case and suites run (run-container.sh image)")
    parser.add_argument("--container-root", type=pathlib.Path, default=pathlib.Path("/var/tmp/gents-live-eval"),
                        help="where contained runs keep their slots until the eval copies their record")
    parser.add_argument("--per-replica", type=int, default=2, help="contained trials at once")
    parser.add_argument("--trial-timeout", type=int, default=3600, help="seconds each contained attempt may run")
    parser.add_argument("--home", type=pathlib.Path)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--min-pass-rate", type=float, default=0.5,
                        help="share of a case's trials that must pass the grader")
    parser.add_argument("--concurrency", type=int, default=8,
                        help="trials at once: contained trials on the one replica, or split across ladder launchers")
    parser.add_argument("--shards", type=int, default=4)
    parser.add_argument("--stagger", type=float, default=10.0, help="seconds between ladder launcher starts")
    parser.add_argument("--rung-floor", type=float, default=0.5)
    parser.add_argument("--eval-timeout", type=int, help="deadline in seconds (case 1800, suites 14400, ladder 6000)")
    parser.add_argument("--job-started", type=float)
    args = parser.parse_args()
    if args.trials < 1 or args.concurrency < 1 or not 0 < args.min_pass_rate <= 1:
        parser.error("--trials and --concurrency must be positive and --min-pass-rate in (0, 1]")
    if args.eval_timeout is None:
        args.eval_timeout = {"case": 1800, "suites": 14400, "ladder": 6000}[args.scope]
    if args.command == "probe":
        base_url, model = endpoint()
        blocked = probe(base_url, model)
        if blocked:
            write_result(args.out, dict(base_result(args, model), outcome="skipped", reason=blocked))
            print(f"skipped: {blocked}")
            return SKIPPED
        print(f"{model} is served")
        return 0
    if args.scope == "ladder" and not args.gents:
        parser.error("scope ladder needs --gents")
    if args.scope != "ladder" and not args.image:
        parser.error(f"scope {args.scope} needs --image")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
