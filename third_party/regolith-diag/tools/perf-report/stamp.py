#!/usr/bin/env python3
"""Keep a benchmark runner's trust and provenance in its family artifact."""

import argparse
import json
import os
import pathlib
import platform
import socket
import subprocess
import tempfile


TIMED = {"point_read", "read", "write_buffered", "buffered_write", "write_durable",
         "durable_write", "scan", "batch", "large_value", "isolation", "scan_commit",
         "append", "read_rules", "txn_callbacks", "sync"}
MIXED = {"transaction", "correctness"}
TRUST = ("clean", "contaminated", "absent")


def annotate(records, passed, metadata):
    for record in records:
        family = record["family"]
        if family not in TIMED | MIXED:
            continue
        data = record["data"]
        if isinstance(data, list):
            data = record["data"] = {"rows": data}
        if not isinstance(data, dict):
            raise ValueError(f"{family}: expected an object or array")
        field = "timing_trust" if family in MIXED else "trust"
        declared = data.get(field, "clean")
        if declared not in TRUST:
            raise ValueError(f"{family}: unknown trust {declared!r}")
        data[field] = TRUST[max(TRUST.index(declared), 0 if passed else 1)]
        data["measurement"] = dict(metadata, guard_passed=passed)
    return records


def output(command):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def provenance(command):
    cpu = platform.processor() or None
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    affinity = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
    compiler = None
    if command and pathlib.Path(command[0]).name == "cargo":
        toolchain = command[1:2] if len(command) > 1 and command[1].startswith("+") else []
        compiler = output(["rustc", *toolchain, "--version"])
    return {
        "host": {"cpu": cpu, "cores": os.cpu_count(), "hostname": socket.gethostname(),
                 "allowed_cpus": affinity,
                 "store": os.environ.get("REGOLITH_BENCH_STORE") or
                 output(["stat", "-f", "-c", "%T", tempfile.gettempdir()])},
        "toolchain": compiler,
        "command": command,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=pathlib.Path)
    parser.add_argument("--guard", required=True, choices=("success", "failure", "skipped", "cancelled"))
    parser.add_argument("--command", nargs=argparse.REMAINDER, required=True)
    args = parser.parse_args()
    records = [json.loads(line) for line in args.path.read_text().splitlines() if line.strip()]
    annotate(records, args.guard == "success", provenance(args.command))
    temporary = args.path.with_suffix(args.path.suffix + ".tmp")
    temporary.write_text("".join(json.dumps(record) + "\n" for record in records))
    temporary.replace(args.path)


if __name__ == "__main__":
    main()
