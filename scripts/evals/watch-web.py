#!/usr/bin/env python3
"""Read native eval snapshots without opening trial databases or controlling runs."""

import argparse
import hashlib
import json
import logging
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit, parse_qs
from urllib.request import urlopen
import webbrowser


def alive(pid):
    if not isinstance(pid, int) or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


class Snapshots:
    def __init__(self, root):
        self.root = root.resolve()
        self.cache = {}
        self.lock = threading.Lock()

    def read(self, path):
        try:
            stamp = path.stat().st_mtime_ns
            cached = self.cache.get(path)
            if cached and cached[0] == stamp:
                return cached[1]
            value = json.loads(path.read_text())
            self.cache[path] = (stamp, value)
            return value
        except (OSError, ValueError):
            return self.cache.get(path, (None, {}))[1]

    def runs(self, selected=None):
        with self.lock:
            result = []
            paths = list(self.root.glob("*/eval/runs/*/report.json"))
            paths += list(self.root.glob("eval/runs/*/report.json"))
            for path in paths:
                if not path.resolve().is_relative_to(self.root):
                    continue
                key = hashlib.sha256(str(path.relative_to(self.root)).encode()).hexdigest()[:20]
                if selected and key != selected:
                    continue
                snapshot = self.read(path)
                report = snapshot.get("report", {})
                run = report.get("run", {})
                if not run:
                    continue
                progress = self.read(path.parent / "progress.json")
                live = {(s["cell_id"], s["case_id"], s["trial_index"]): s
                        for s in progress.get("slots", {}).values()}
                slots = []
                counts = {}
                for cell in report.get("cells", []):
                    for slot in cell.get("slots", []):
                        active = live.get((cell["cell_id"], slot["case_id"], slot["trial_index"]))
                        running = bool(active and alive(active.get("pid")))
                        state = "running" if running else slot["class"]
                        if active and not running and state in ("planned", "abandoned"):
                            state = "stopped"
                        counts[state] = counts.get(state, 0) + 1
                        if selected:
                            latest = slot.get("latest") or {}
                            retained = snapshot.get("trials", {}).get(latest.get("trial_id"), {})
                            observed = active if active and (running or state == "stopped") else retained
                            observed = observed or {}
                            slots.append(dict(slot, cell_id=cell["cell_id"], state=state,
                                live=observed.get("live") or {}, goal=observed.get("goal") or [],
                                stage=active.get("stage_id") if active and (running or state == "stopped") else None,
                                updated_at=observed.get("written_at") or observed.get("ended_at")))
                item = dict(key=key, run=run, home=str(path.parents[3]), counts=counts,
                            divergences=report.get("divergences"),
                            updated_at=progress.get("holder", {}).get("written_at") or snapshot.get("written_at"))
                if selected:
                    item["slots"] = slots
                    definition = self.read(path.parent / "definition.json")
                    item["cases"] = [{"case_id": c["case_id"], "stages": [s["stage_id"] for s in c.get("stages", [])]}
                                     for c in definition.get("cases", [])]
                result.append(item)
            return sorted(result, key=lambda r: r["run"].get("created_at", ""), reverse=True)


def merged_divergences(runs):
    merged = []
    for run in runs:
        merged += [item for item in run.get("divergences") or [] if item not in merged]
    return merged


def comparison_summary(runs):
    slots = [s for run in runs for s in run.get("slots", [])]
    counts = {}
    scores = {"setup": [], "repair": []}
    tokens = {field: {"reported": 0, "reporting": 0, "complete": 0, "settled_complete": []}
              for field in ("input_tokens", "output_tokens")}
    calls = failures = improved = worse = unchanged = 0
    for slot in slots:
        state = slot["state"]
        counts[state] = counts.get(state, 0) + 1
        live = slot.get("live", {})
        calls += live.get("tool_calls") or 0
        failures += live.get("failed_tool_calls") or 0
        pair = {}
        for stage in scores:
            check = next((c for c in live.get("stages", {}).get(stage, {}).get("checks", [])
                          if c.get("check") == "crew_spec_match"), {})
            raw = check.get("raw") or {}
            if raw.get("total", 0) > 0:
                pair[stage] = 100 * raw["satisfied"] / raw["total"]
                scores[stage].append(pair[stage])
        if len(pair) == 2:
            improved += pair["repair"] > pair["setup"]
            worse += pair["repair"] < pair["setup"]
            unchanged += pair["repair"] == pair["setup"]
        for field, result in tokens.items():
            value = live.get(field)
            reported = value if value is not None else live.get("reported_" + field)
            if reported is not None:
                result["reported"] += reported
                result["reporting"] += 1
            if value is not None:
                result["complete"] += 1
                if state in ("pass", "fail"):
                    result["settled_complete"].append(value)
    for result in tokens.values():
        values = result.pop("settled_complete")
        result["settled_complete_count"] = len(values)
        result["settled_complete_mean"] = sum(values) / len(values) if values else None
    return dict(trials=len(slots), counts=counts, tool_calls=calls, failed_tool_calls=failures,
                scores={stage: {"mean": sum(values)/len(values) if values else None, "measured": len(values)}
                        for stage, values in scores.items()}, tokens=tokens,
                repair=dict(improved=improved, worse=worse, unchanged=unchanged))


def serve(root, port):
    snapshots = Snapshots(root)
    comparison_sources = {}
    page = Path(__file__).with_name("watch-web.html")

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            request = urlsplit(self.path)
            status = 200
            mime = "application/json"
            if request.path == "/":
                body = page.read_bytes()
                mime = "text/html; charset=utf-8"
            elif request.path == "/compare":
                body = page.with_name("watch-compare.html").read_bytes()
                mime = "text/html; charset=utf-8"
            elif request.path == "/api/comparison":
                rounds = []
                for entry in snapshots.read(root / "compare.json").get("rounds", []):
                    source_root = Path(entry["root"]).resolve()
                    source = comparison_sources.setdefault(source_root, Snapshots(source_root))
                    index = [r for r in source.runs()
                             if r["run"].get("definition", {}).get("definition_id") == "factory-setup"]
                    details = [r for item in index for r in source.runs(item["key"])]
                    rounds.append(dict(label=entry["label"], change=entry.get("change", ""),
                        dashboard=entry.get("dashboard"), summary=comparison_summary(details),
                        sources=sorted({r["run"].get("source_commit", "unknown") for r in details}),
                        versions=sorted({r["run"]["definition"]["comparability_version"] for r in details}),
                        runs=[dict(key=r["key"], run_id=r["run"]["run_id"]) for r in details],
                        divergences=merged_divergences(details),
                        unrecorded=sum(r.get("divergences") is None for r in details)))
                body = json.dumps({"rounds": rounds}).encode()
            elif request.path == "/api/health":
                body = json.dumps({"root": str(snapshots.root)}).encode()
            elif request.path == "/api/runs":
                body = json.dumps({"runs": snapshots.runs(), "now": datetime.now(timezone.utc).isoformat()}).encode()
            elif request.path == "/api/run":
                key = parse_qs(request.query).get("key", [""])[0]
                runs = snapshots.runs(key) if key else []
                body = json.dumps(runs[0] if runs else {"error": "Run not found"}).encode()
                status = 200 if runs else 404
            else:
                status, body = 404, b'{"error":"Not found"}'
            self.send_response(status)
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; object-src 'none'; frame-ancestors 'none'")
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    logging.info("Eval dashboard: http://127.0.0.1:%s", server.server_port)
    server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.home() / "gents-eval-homes")
    parser.add_argument("--port", type=int, default=9495)
    parser.add_argument("--ensure", action="store_true", help="Reuse or start a background viewer")
    parser.add_argument("--open", action="store_true", help="Open the dashboard in a browser")
    args = parser.parse_args()
    args.root = args.root.resolve()
    url = f"http://127.0.0.1:{args.port}"
    if args.ensure:
        def ready():
            try:
                with urlopen(url + "/api/health", timeout=1) as response:
                    data = json.load(response)
            except (OSError, ValueError):
                return False
            if data.get("root") != str(args.root):
                parser.error("port serves another dashboard root; choose a different --port")
            return True
        if not ready():
            args.root.mkdir(parents=True, exist_ok=True)
            with (args.root / "watch-web.log").open("a") as log:
                child = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                    "--root", str(args.root), "--port", str(args.port)],
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
            for _ in range(50):
                if ready():
                    break
                if child.poll() is not None:
                    parser.error(f"viewer could not start; see {args.root / 'watch-web.log'}")
                time.sleep(0.1)
            else:
                parser.error("viewer did not become ready")
        logging.info("Eval dashboard: %s", url)
        if args.open:
            webbrowser.open(url)
    else:
        if args.open:
            threading.Timer(0.5, lambda: webbrowser.open(url)).start()
        serve(args.root, args.port)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    main()
