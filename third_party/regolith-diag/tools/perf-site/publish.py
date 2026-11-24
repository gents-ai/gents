#!/usr/bin/env python3
"""Append one benchmark run to the published history.

The history is the point. A single rendered `perf.html` per CI run
answers "is this run fast" and nothing else: the moment it is replaced
the comparison it supported is gone. Keeping every run document and
rendering from the whole set instead answers "when did this regress",
which is the question a performance gate actually has to answer.

Layout under the site root:

    perf.html           the dashboard, self-contained, reads the JSON below
    runs/index.json     manifest, newest first
    runs/<commit>.json  one run document per CI run, verbatim

`index.html` is the project landing page. It is maintained on the
gh-pages branch and is never written from here; the publish step keeps
existing files, which is what preserves it.

The manifest is regenerated from the directory on every publish rather
than appended to, so a run file that was removed cannot leave a dangling
row behind, and a run file that was added out of band is still picked up.
"""

import json
import pathlib
import shutil
import sys

KEEP = 200


def load(path):
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"publish: skipping {path.name}: {e}", file=sys.stderr)
        return None


def summarize(doc, name):
    run = doc.get("run", {})
    metrics = doc.get("metrics", {})
    tp = metrics.get("throughput", {}).get("workloads", {})
    peak = None
    for w in tp.values():
        for p in w.get("points", []):
            if peak is None or p.get("ops", 0) > peak:
                peak = p.get("ops", 0)
    return {
        "file": name,
        "commit": run.get("commit", ""),
        "label": run.get("label", ""),
        "timestamp": run.get("timestamp", ""),
        "toolchain": run.get("toolchain", ""),
        "loadguard_passed": bool((run.get("loadguard") or {}).get("passed")),
        "trust": {k: v.get("trust") for k, v in metrics.items() if isinstance(v, dict)},
        "peak_ops": peak,
    }


def main():
    if len(sys.argv) != 3:
        print("usage: publish.py <run.json> <site-root>", file=sys.stderr)
        return 2
    run_path, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    doc = load(run_path)
    if doc is None:
        print(f"publish: {run_path} is not readable JSON", file=sys.stderr)
        return 1

    commit = (doc.get("run") or {}).get("commit") or ""
    if not commit:
        print("publish: the run document carries no commit, refusing to file it", file=sys.stderr)
        return 1

    runs = root / "runs"
    runs.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(run_path, runs / f"{commit}.json")

    entries = []
    for f in runs.glob("*.json"):
        if f.name == "index.json":
            continue
        d = load(f)
        if d is not None:
            entries.append(summarize(d, f.name))

    entries.sort(key=lambda e: e.get("timestamp") or "", reverse=True)

    # Bounded, and the drop is reported rather than silent.
    if len(entries) > KEEP:
        for e in entries[KEEP:]:
            (runs / e["file"]).unlink(missing_ok=True)
        print(f"publish: pruned {len(entries) - KEEP} run(s) beyond the newest {KEEP}")
        entries = entries[:KEEP]

    (runs / "index.json").write_text(json.dumps({"runs": entries}, indent=1) + "\n")
    print(f"publish: {len(entries)} run(s) on file, newest {entries[0]['commit'][:12]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
