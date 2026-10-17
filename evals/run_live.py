"""Run isolated real-model browser trials concurrently, preserving every result."""

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gents", default="gents")
    parser.add_argument("--endpoint", action="append", required=True)
    parser.add_argument("--concurrency-per-server", type=int, default=32)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout-secs", type=int, default=900)
    args = parser.parse_args()
    if args.concurrency_per_server < 1:
        parser.error("concurrency must be positive")
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    pack = Path(__file__).resolve().parents[1]
    with (root / "build.log").open("w") as log:
        subprocess.run(
            [args.gents, "pack", "build", str(pack), "--out", str(root / "base.pack")],
            stdout=log,
            stderr=log,
            check=True,
            timeout=600,
        )
    original = json.loads((pack / "manifest.json").read_text())
    reservations = []
    trials = []
    for server, endpoint in enumerate(args.endpoint, 1):
        for index in range(args.concurrency_per_server):
            name = f"server-{server}-trial-{index + 1:02d}"
            variant = root / "packs" / name
            variant.mkdir(parents=True)
            for asset in original["assets"]:
                dest = variant / asset
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(pack / asset, dest)
            manifest = json.loads(json.dumps(original))
            reservation = socket.socket()
            for port in range(20000, 30000):
                try:
                    reservation.bind(("127.0.0.1", port))
                    break
                except OSError:
                    continue
            else:
                raise RuntimeError("No free worker port in 20000..29999")
            reservations.append(reservation)
            port = reservation.getsockname()[1]
            for plugin in manifest["plugins"]:
                plugin.pop("source", None)
                if plugin.get("service"):
                    plugin["service"]["argv"] += ["--port", str(port)]
                    plugin["manifold"]["net"]["OutboundHttp"] = [
                        f"http://127.0.0.1:{port}"
                    ]
            (variant / "manifest.json").write_text(
                json.dumps(manifest, indent=2) + "\n"
            )
            trials.append(
                {
                    "name": name,
                    "endpoint": endpoint,
                    "mode": "suite" if index % 2 == 0 else "submit_recovery",
                    "worker_port": port,
                }
            )
    (root / "plan.json").write_text(
        json.dumps(
            {
                "comparability_version": 2,
                "concurrency_per_server": args.concurrency_per_server,
                "base_archive_sha256": hashlib.sha256(
                    (root / "base.pack").read_bytes()
                ).hexdigest(),
                "trials": trials,
            },
            indent=2,
        )
    )

    def build(trial):
        with (root / "packs" / trial["name"] / "build.log").open("w") as log:
            subprocess.run(
                [
                    args.gents,
                    "pack",
                    "build",
                    str(root / "packs" / trial["name"]),
                    "--out",
                    str(root / "packs" / (trial["name"] + ".pack")),
                ],
                stdout=log,
                stderr=log,
                check=True,
                timeout=180,
            )

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(build, trials))
    for trial in trials:
        trial["archive_sha256"] = hashlib.sha256(
            (root / "packs" / (trial["name"] + ".pack")).read_bytes()
        ).hexdigest()
    for reservation in reservations:
        reservation.close()

    def run(trial):
        output = root / trial["name"]
        command = [
            sys.executable,
            str(pack / "worker/tests/live_model.py"),
            "--gents",
            args.gents,
            "--endpoint",
            trial["endpoint"],
            "--managed",
            "--pack-archive",
            str(root / "packs" / (trial["name"] + ".pack")),
            "--output",
            str(output),
            "--timeout-secs",
            str(args.timeout_secs),
        ]
        if trial["mode"] == "submit_recovery":
            command.append("--submit-recovery")
        started = time.time()
        with (
            (root / (trial["name"] + ".log")).open("w") as log,
            subprocess.Popen(
                command,
                stdout=log,
                stderr=log,
                env={**os.environ, "TOKIO_WORKER_THREADS": "2"},
            ) as process,
        ):
            try:
                returncode = process.wait(timeout=6 * (args.timeout_secs + 30) + 900)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    returncode = process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    process.kill()
                    returncode = process.wait()
        report_path = output / "report.json"
        report = (
            json.loads(report_path.read_text())
            if report_path.exists()
            else {
                "passed": False,
                "error": "Trial exited before producing a report; inspect trial log",
            }
        )
        report.update(
            trial,
            exit_code=returncode,
            started_at=started,
            finished_at=time.time(),
        )
        report["passed"] = returncode == 0 and report.get("passed", False)
        print(
            f"{trial['name']}: {'PASS' if report['passed'] else 'FAIL'} {report.get('error', '')}",
            flush=True,
        )
        return report

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(trials)) as pool:
        futures = {pool.submit(run, trial): trial for trial in trials}
        for future in concurrent.futures.as_completed(futures):
            try:
                results.append(future.result())
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                results.append(
                    {
                        **futures[future],
                        "passed": False,
                        "error": str(error),
                        "failure_phase": "harness",
                    }
                )
            pending = root / "results.tmp"
            pending.write_text(json.dumps(results, indent=2))
            pending.replace(root / "results.json")
    summary = {
        "trials": len(results),
        "passed": sum(r["passed"] for r in results),
        "tasks": sum(len(r.get("tasks", [])) for r in results),
        "browser_calls": sum(
            t["browser_calls"] for r in results for t in r.get("tasks", [])
        ),
        "concurrency_per_server": args.concurrency_per_server,
    }
    (root / "summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))
    return 0 if all(r["passed"] for r in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
