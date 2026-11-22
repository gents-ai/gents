"""The gate's record of passed work, keyed by exactly the inputs of the work.

A unit of gate work (a job, or one entry of a job's matrix) passes on its
inputs: the files the gate says can change it, what the plan asks of it and
the runner platform. Its key hashes those, so a unit is skipped only if a run
of this repository passed an identical key on the same platform.

The store is the repository's Actions artifacts: a pass uploads an artifact
named `gate-pass-<key>` and the plan looks it up by name. Unlike the Actions
cache, an artifact is readable from every ref, so a pull request's pass serves
its merge queue entry. A record is trusted only when the run that uploaded it
ran this repository's code (its head repository is the repository): a fork's
run can upload any name but is never believed.
"""

import concurrent.futures
import hashlib
import json
import os
import subprocess
from pathlib import Path

# Bump to invalidate every recorded pass.
VERSION = "gate-reuse-1"
PREFIX = "gate-pass-"
RETENTION_DAYS = 7


def tree(root, revision="HEAD"):
    """Every file of the revision: (path, mode, blob id)."""
    raw = subprocess.run(["git", "ls-tree", "-r", "-z", "--full-tree", revision], cwd=root, check=True,
                         stdout=subprocess.PIPE).stdout.decode()
    entries = []
    for line in filter(None, raw.split("\0")):
        meta, path = line.split("\t", 1)
        mode, _, blob = meta.split(" ")
        entries.append((path, mode, blob))
    return entries


def platform(runs_on):
    """The OS and architecture a runs-on value lands on, as RUNNER_OS/RUNNER_ARCH name them."""
    if isinstance(runs_on, list):
        return f"{runs_on[1]}/{runs_on[2]}"
    if runs_on.startswith("ubuntu-"):
        return "Linux/X64"
    if runs_on.startswith("macos-"):
        return "macOS/ARM64"
    return runs_on


def key(unit, plan_slice, runner_platform, entries):
    """The unit's input key: its name, what the plan asks of it, its platform
    and the files of its closure."""
    digest = hashlib.sha256()
    digest.update(json.dumps([VERSION, unit, plan_slice, runner_platform], sort_keys=True).encode())
    for path, mode, blob in sorted(entries):
        digest.update(f"\n{mode} {blob} {path}".encode())
    return digest.hexdigest()


def trusted_run(artifacts):
    """The run id of the first live record a run of this repository's code uploaded."""
    for artifact in artifacts:
        run = artifact.get("workflow_run") or {}
        if artifact.get("expired") or not run.get("id"):
            continue
        if run.get("repository_id") and run.get("head_repository_id") == run.get("repository_id"):
            return run["id"]
    return None


def find(repository, unit_key):
    """The run that recorded a pass of `unit_key`, if any. A failed query is a miss."""
    try:
        raw = subprocess.run(["gh", "api", f"repos/{repository}/actions/artifacts?name={PREFIX}{unit_key}&per_page=20"],
                             check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=60).stdout
        return trusted_run(json.loads(raw).get("artifacts", []))
    except (subprocess.SubprocessError, OSError, ValueError) as error:
        print(f"::warning::gate reuse: looking up {unit_key} failed ({error}); running the work", flush=True)
        return None


def lookup(repository, keys):
    """key -> run id of a trusted pass, for every key that has one."""
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        found = dict(zip(keys, pool.map(lambda unit_key: find(repository, unit_key), keys)))
    return {unit_key: run for unit_key, run in found.items() if run}


def record(unit, info, environment=None):
    """Write the pass record of a unit that just passed here: (artifact name,
    directory), or None when there is nothing to record or the runner is not the
    platform the key names."""
    environment = os.environ if environment is None else environment
    if not info:
        print(f"gate reuse: {unit} has no input key in this plan; nothing to record")
        return None
    here = f"{environment.get('RUNNER_OS')}/{environment.get('RUNNER_ARCH')}"
    if here != info["platform"]:
        print(f"::warning::gate reuse: {unit} ran on {here}, but its key names {info['platform']}; not recorded")
        return None
    directory = Path(environment.get("RUNNER_TEMP", ".")) / "gate-pass" / info["key"]
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "record.json").write_text(json.dumps({
        "unit": unit, "key": info["key"], "platform": info["platform"],
        "run_id": environment.get("GITHUB_RUN_ID"), "sha": environment.get("GITHUB_SHA"),
        "event": environment.get("GITHUB_EVENT_NAME")}, indent=2))
    return f"{PREFIX}{info['key']}", str(directory)


def run_url(repository, run):
    return f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{repository}/actions/runs/{run}"
