#!/usr/bin/env python3
"""Run live Engineer evals against a served model and judge them.

  live_engineer_eval.py probe --out DIR
  live_engineer_eval.py run --out DIR --gents BIN [--scope case|ladder] [--home DIR]

Both read GENTS_CI_LIVE_MODEL_BASE_URL (OpenAI-compatible, including /v1) and
GENTS_CI_LIVE_MODEL_NAME, and write DIR/result.json and DIR/summary.md.
Exit status: 0 pass (or probe usable), 1 fail or harness error, 3 skipped
because the endpoint is not configured, unreachable or not serving the model.

Every run goes through scripts/evals/run-ladder.sh with the target's own
sampling (no temperature or top_p override); the graders read stored documents.

Scope `case` runs L2 scout-grants-two-file-tools TRIALS times and passes when
at least REQUIRED trials pass the grader.

Scope `ladder` runs L1-L5 and the factory-setup capstone (train and validation
cases, TRIALS each) over SHARDS launchers until the deadline. Each case is
judged against the same REQUIRED-of-TRIALS threshold and recorded; on a healthy
build about 4 of 37 cases fall below it (eval report of 2026-11-13), so case
verdicts are a trend, not the job's outcome. The job fails when a rung L1-L5
passes fewer than RUNG_FLOOR of its finished trials, or finishes fewer than
MIN_FINISHED trials before the deadline. Healthy builds scored 70-100% per rung;
the build that could not create agents scored 0% on L2 and 33% on L5.
factory-setup (1-2 of 3 on healthy builds) is reported, not gated.
"""
import argparse
import collections
import json
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

ROOT = pathlib.Path(__file__).resolve().parents[2]
SUITE = "l2-agent"
CASE = "scout-grants-two-file-tools"
CELL = "engineer"
TARGET = "live-ci"
GATED_RUNGS = ["l1-inference", "l2-agent", "l3-datastore", "l4-automation", "l5-agents-tools"]
MIN_FINISHED = 6
MAX_TRANSCRIPTS = 15
SKIPPED = 3
PROBE_TIMEOUT_SECS = 15
SAMPLING = "target defaults (no override)"


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


def matrix():
    return json.loads((ROOT / "scripts/evals/matrix.json").read_text())


def one_case_definition(destination):
    """Copies the suite's definition with only CASE, so another case added to the suite does not join the run."""
    suite = next(s for s in matrix()["suites"] if s["id"] == SUITE)
    shutil.copytree(ROOT / "crates/gents/tests/fixtures/configurator_evals" / suite["path"], destination)
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
            "backend_id": TARGET, "name": "Live CI model", "provider_kind": "OpenAiCompatible",
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
               GENTS_EVAL_TEMPERATURE="", GENTS_EVAL_TOP_P="", **extra)
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
        "failed_tool_calls": live.get("failed_tool_calls"),
        "model_turns": live.get("model_turns"),
        "elapsed_secs": live.get("elapsed_secs"),
        "failed_verdicts": [f"{v['stage_id']}/{v['check']}: {v.get('detail') or v.get('reason_code')}"
                            for v in slot.get("verdicts", []) if v["kind"] != "passed"],
        "unmet": unmet,
        "tools": live.get("tools") or {},
        "last_tool": live.get("last_tool"),
    }


def case_verdict(passes, finished, trials, required):
    if passes >= required:
        return "pass"
    if passes + (trials - finished) < required:
        return "fail"
    return "incomplete"


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


def run_case(args, out, home, work, result):
    split = one_case_definition(work / "definition")
    started = time.monotonic()
    processes = [launch(args, home, out / "ladder.log", work / f"{TARGET}.json", SUITE, args.trials,
                        GENTS_EVAL_DEFINITION_SOURCE=str(work / "definition"), GENTS_EVAL_SPLITS=split)]
    status = wait_all(processes, started + args.eval_timeout)[0]
    result["ladder_exit"] = status
    reports = sorted((home / "eval" / "runs").glob(f"ladder-{SUITE}-*/report.json"))
    if len(reports) == 1:
        view = json.loads(reports[0].read_text())
        shutil.copy(reports[0], out / "report.json")
        cell = next(c for c in view["report"]["cells"] if c["cell_id"] == CELL)
        slots = sorted((s for s in cell["slots"] if s["case_id"] == CASE), key=lambda s: s["trial_index"])
        rows = [trial_row(slot, view.get("trials", {})) for slot in slots]
        passes = sum(row["class"] == "pass" for row in rows)
        usage = cell.get("usage") or {}
        result.update(outcome="pass" if passes >= args.required else "fail",
                      reason=f"{passes} of {len(rows)} trials passed the grader; {args.required} required",
                      passes=passes, trials_detail=rows,
                      tokens={"input": usage.get("input_tokens"), "output": usage.get("output_tokens")})
        for slot in slots:
            collect_trial(args, home, out / "trials" / f"{slot['trial_index']}.json",
                          view["report"]["run"]["run_id"], CASE, slot["trial_index"])
        if len(slots) != args.trials:
            result.update(outcome="fail", reason=f"{len(slots)} of {args.trials} planned trials have a slot; " + result["reason"])
    else:
        why = "timed out" if status is None else f"exited {status}"
        result.update(outcome="error", reason=f"the eval {why} and left {len(reports)} reports; see ladder.log")
    end_launchers(processes)
    copy_logs(home, out)


def run_full_ladder(args, out, home, work, result):
    suites = matrix()
    by_definition = {s["definition_id"]: s["id"] for s in suites["suites"]}
    rungs = suites["default"]
    homes = [home / f"s{index}" for index in range(1, args.shards + 1)]
    started = time.monotonic()
    processes = []
    for index, shard_home in enumerate(homes, 1):
        processes.append(launch(args, shard_home, out / f"ladder-s{index}.log", work / f"{TARGET}.json", "all",
                                args.per_launcher, GENTS_EVAL_SHARD=f"{index}/{args.shards}"))
        time.sleep(args.stagger)
    statuses = wait_all(processes, started + args.eval_timeout)
    result["launcher_exits"] = statuses
    cases, tokens, failing_slots = {}, collections.Counter(), []
    for shard_home in homes:
        for report in sorted((shard_home / "eval" / "runs").glob("ladder-*/report.json")):
            view = json.loads(report.read_text())
            run = view["report"]["run"]
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
            case_passes = sum(t["class"] == "pass" for t in entry["trials"])
            case_finished = sum(t["class"] in ("pass", "fail") for t in entry["trials"])
            entry.update(passes=case_passes, finished=case_finished,
                         verdict=case_verdict(case_passes, case_finished, args.trials, args.required))
            passes += case_passes
            finished += case_finished
            if entry["verdict"] == "fail":
                failing_slots += [(rung, *item) for item in entry["_slots"] if item[2]["class"] == "fail"]
        gated = rung in GATED_RUNGS
        if not gated:
            verdict = "reported"
        elif finished < MIN_FINISHED:
            verdict = "incomplete"
        else:
            verdict = "pass" if passes >= args.rung_floor * finished else "fail"
        rung_rows.append({"rung": rung, "gated": gated, "cases": len(members),
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
        reason = "every rung L1-L5 met its floor"
        outcome = "pass"
    result.update(outcome=outcome, reason=reason, rungs=rung_rows, cases=cases,
                  tokens={"input": tokens.get("input_tokens"), "output": tokens.get("output_tokens")},
                  deadline_secs=args.eval_timeout, stopped_at_deadline=sum(s is None for s in statuses))


def run(args):
    started = time.monotonic()
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    base_url, model = endpoint()
    result = base_result(args, model)
    blocked = probe(base_url, model)
    if blocked:
        write_result(out, dict(result, outcome="skipped", reason=blocked))
        return SKIPPED
    home = args.home or out.parent / "live-eval-home"
    if home.exists():
        raise SystemExit(f"{home} exists; the eval needs a fresh home")
    work = out.parent / "live-eval-inputs"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    target_file(work / f"{TARGET}.json", base_url, model)
    (run_case if args.scope == "case" else run_full_ladder)(args, out, home, work, result)
    return finish(result, out, base_url, started, args)


def base_result(args, model):
    result = {"scope": args.scope, "model": model, "trials": args.trials, "required_passes": args.required,
              "sampling": SAMPLING}
    if args.scope == "case":
        result.update(suite=SUITE, case_id=CASE)
    else:
        result.update(rung_floor=args.rung_floor, min_finished=MIN_FINISHED, gated_rungs=GATED_RUNGS,
                      shards=args.shards)
    return result


def render_summary(result):
    title = "Live Engineer eval" if result.get("scope", "case") == "case" else "Live Engineer ladder"
    lines = [f"## {title}: {result['outcome'].upper()}", ""]
    if result.get("scope", "case") == "case":
        lines += [f"- Case: `{result['suite']}` / `{result['case_id']}` (the Engineer creates and configures a second agent, then re-enters to finish it)",
                  f"- Rule: pass when at least {result['required_passes']} of {result['trials']} trials pass the grader"]
    else:
        lines += ["- Scope: L1-L5 and factory-setup, train and validation cases",
                  f"- Case threshold (recorded, a trend): at least {result['required_passes']} of {result['trials']} trials pass the grader",
                  f"- Job rule: fails when a rung L1-L5 passes fewer than {result['rung_floor']:.0%} of its finished trials "
                  f"or finishes fewer than {result['min_finished']}; factory-setup is reported, not gated"]
    lines += [f"- Model: `{result.get('model') or 'unset'}`, sampling: {result.get('sampling')}",
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
    rows = result.get("trials_detail") or []
    if rows:
        lines += ["", "| Trial | Grader | Stages (terminal state) | Agents | Tool calls (failed) | Turns | Wall |",
                  "|---|---|---|---|---|---|---|"]
        for row in rows:
            stages = ", ".join(f"{s['stage_id']} {s['terminal_state'] or 'not terminal'}" for s in row["stages"]) or "none"
            lines.append(f"| {row['trial_index']} | {row['class']} ({row['score_bp']} bp) | {stages} | "
                         f"{row['agent_documents']} | {row['tool_calls']} ({row['failed_tool_calls']}) | "
                         f"{row['model_turns']} | {row['elapsed_secs']} s |")
        for row in rows:
            missed = row["failed_verdicts"] + row["unmet"]
            if missed:
                lines += ["", f"Trial {row['trial_index']} missed:"] + [f"- {item}" for item in missed[:20]]
            failing = {name: tool.get("failed") for name, tool in row["tools"].items() if tool.get("failed")}
            if failing and row["class"] != "pass":
                lines.append(f"- failed tool calls by tool: {failing}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["probe", "run"])
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--scope", choices=["case", "ladder"], default="case")
    parser.add_argument("--gents", type=pathlib.Path)
    parser.add_argument("--home", type=pathlib.Path)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--required", type=int, default=2)
    parser.add_argument("--shards", type=int, default=4)
    parser.add_argument("--per-launcher", type=int, default=4, help="concurrent trials per ladder launcher")
    parser.add_argument("--stagger", type=float, default=10.0, help="seconds between ladder launcher starts")
    parser.add_argument("--rung-floor", type=float, default=0.5)
    parser.add_argument("--eval-timeout", type=int, help="deadline in seconds (case 1500, ladder 6000)")
    parser.add_argument("--job-started", type=float)
    args = parser.parse_args()
    if not 1 <= args.required <= args.trials:
        parser.error("--required must be between 1 and --trials")
    if args.eval_timeout is None:
        args.eval_timeout = 1500 if args.scope == "case" else 6000
    if args.command == "probe":
        base_url, model = endpoint()
        blocked = probe(base_url, model)
        if blocked:
            write_result(args.out, dict(base_result(args, model), outcome="skipped", reason=blocked))
            print(f"skipped: {blocked}")
            return SKIPPED
        print(f"{model} is served")
        return 0
    if not args.gents:
        parser.error("run needs --gents")
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
