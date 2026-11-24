#!/usr/bin/env python3
"""Render the `ci-latency-data` branch's samples into a Markdown trend (W53).

Reads `latency.jsonl` (one sample per gate job: timestamp, run_id, job, steps,
total_seconds), prints a Markdown table of recent runs per job with an inline
sparkline of total seconds, and writes the table to $GITHUB_STEP_SUMMARY when
set. Read-only: it never writes the branch; the caller commits the file it
prints when asked.

Usage: ci-latency-report.py [--since-days N] [--last M] [--out PATH]
"""

import argparse
import json
import os
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone

REPO_ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], check=True,
                           capture_output=True, text=True).stdout.strip()
DATA_BRANCH = "origin/ci-latency-data"
DATA_FILE = "latency.jsonl"


def samples(since_days):
    raw = subprocess.run(["git", "show", f"{DATA_BRANCH}:{DATA_FILE}"], cwd=REPO_ROOT,
                         check=True, capture_output=True, text=True).stdout
    cutoff = datetime.now(timezone.utc) - timedelta(days=since_days)
    parsed = []
    for line in raw.splitlines():
        try:
            sample = json.loads(line)
            sample["_ts"] = datetime.fromisoformat(sample["timestamp"].replace("Z", "+00:00"))
        except (ValueError, KeyError):
            continue
        if sample["_ts"] >= cutoff:
            parsed.append(sample)
    return parsed


def sparkline(values):
    blocks = "▁▂▃▄▅▆▇█"
    if not values:
        return ""
    low, high = min(values), max(values)
    if high == low:
        return blocks[0] * len(values)
    return "".join(blocks[min(7, int((v - low) / (high - low) * 7))] for v in values)


def table(samples, last):
    by_job = defaultdict(list)
    for sample in samples:
        by_job[sample["job"]].append(sample)
    lines = ["# CI latency trend", "",
             f"{sum(len(v) for v in by_job.values())} samples, "
             f"newest {max(s['_ts'] for s in samples).isoformat() if samples else '-'}", "",
             "| job | samples | last | mean | max | trend |", "|---|---|---|---|---|---|"]
    for job in sorted(by_job):
        recent = sorted(by_job[job], key=lambda s: s["_ts"])[-last:]
        totals = [s["total_seconds"] for s in recent]
        lines.append(f"| `{job}` | {len(totals)} | {totals[-1]:.0f}s | "
                     f"{sum(totals) / len(totals):.0f}s | {max(totals):.0f}s | "
                     f"`{sparkline(totals)}` |")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--since-days", type=int, default=30)
    parser.add_argument("--last", type=int, default=40)
    parser.add_argument("--out", help="Also write the report to PATH")
    arguments = parser.parse_args()
    recent = samples(arguments.since_days)
    if not recent:
        print("No latency samples yet; the collector populates the branch on its first runs.")
        return
    report = table(recent, arguments.last)
    print(report)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write("\n" + report + "\n")
    if arguments.out:
        # e.g. --out trend.md, committed to ci-latency-data by the collector,
        # so the trend has a stable URL with history instead of living only
        # in each workflow run's summary.
        with open(arguments.out, "w") as out:
            out.write(report + "\n")


if __name__ == "__main__":
    sys.exit(main())
