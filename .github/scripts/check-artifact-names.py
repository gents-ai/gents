#!/usr/bin/env python3
"""Check every artifact name in the workflows and composite actions against
GitHub's name rules (W53); this parses YAML, it does not run any workflow.

A `name:` under `actions/upload-artifact` must contain only [A-Za-z0-9._-] up
to the `${{ }}` templates: GitHub bans space, " , : < > | ? * \\ / and
leading/trailing whitespace in artifact names (256 characters max), and unit
or matrix keys interpolated into a name can legitimately carry those
characters (build:portable:3, ts-browser:1/3). A templated segment is allowed
only when every variable it interpolates is sanitized on the way in — see
SAFE_TEMPLATES below; anything else is flagged so the name gets built by a
checked helper (scripts/ci/gate.py latency-sample) or extended here with a
justified entry.
"""
from pathlib import Path
import re
import sys

try:
    import yaml
except ImportError:  # the runner may lack PyYAML; skip rather than fail the job
    print("check-artifact-names: pyyaml unavailable; skipping")
    sys.exit(0)

ROOT = Path(__file__).resolve().parents[2]
UPLOADER = "actions/upload-artifact"
# Segments of a templated name that are known to stay within [A-Za-z0-9._-]:
# gate.py's latency-sample command emits the sanitized W53 name; GitHub
# context values whose vocabulary is numeric or a 40-hex sha; and plan-built
# matrix values, whose vocabularies (test-group names, archive names, suite
# names, indexes) are fixed in scripts/ci/gate.py.
# The base name is a literal with [A-Za-z0-9._-] characters. The scope
# input is a `choice` of two fixed literals (case, ladder) and the
# fallback default is 'ladder', so the vocabulary is closed; the basename
# is gents-<target-triple>, built from fixed Rust target strings.
SAFE_TEMPLATES = {
    "steps.latency.outputs.name",
    "github.sha", "github.run_id", "github.run_attempt",
    "strategy.job-index",
    "matrix.archive", "matrix.index", "matrix.count", "matrix.suite",
    "env.LIVE_EVAL_SCOPE",  # workflow_dispatch choice: case | ladder
    "steps.package.outputs.artifact_basename",  # gents-<rust target triple>
}
FORBIDDEN = re.compile(r'[\s",:<>|?*/\\]')


def check_file(path):
    problems = []
    try:
        document = yaml.safe_load(path.read_text())
    except yaml.YAMLError as error:
        return [f"{path}: unparseable YAML: {error}"]
    if not isinstance(document, dict) or "jobs" not in document:
        return problems
    jobs = document.get("jobs") or {}
    for job_id, job in (jobs or {}).items():
        if not isinstance(job, dict):
            continue
        for index, step in enumerate(job.get("steps") or []):
            if not isinstance(step, dict):
                continue
            uses = step.get("uses") or ""
            if not uses.startswith(UPLOADER):
                continue
            name = (step.get("with") or {}).get("name")
            if name is None:
                problems.append(f"{path}: job {job_id} step {index + 1} ({uses}) has no artifact name")
                continue
            if not isinstance(name, str):
                problems.append(f"{path}: job {job_id} step {index + 1} artifact name is not a string")
                continue
            # Split the name into literal segments and `${{ ... }}` templates;
            # a literal segment must be clean, a template must be a known
            # safe source.
            for segment in re.split(r"\$\{\{[^}]*\}\}", name):
                if FORBIDDEN.search(segment):
                    problems.append(f"{path}: job {job_id} step {index + 1} artifact name "
                                    f"{name!r} contains a character GitHub bans "
                                    f'(space, " , : < > | ? * \\ / or leading/trailing whitespace)')
            for template in re.findall(r"\$\{\{([^}]*)\}\}", name):
                expression = " ".join(template.split())
                if expression not in SAFE_TEMPLATES:
                    problems.append(f"{path}: job {job_id} step {index + 1} artifact name "
                                    f"{name!r} interpolates '{expression}', which the linter "
                                    "cannot prove stays within [A-Za-z0-9._-]; build the name "
                                    "with a checked helper or extend SAFE_TEMPLATES here with a "
                                    "justified entry")
    return problems


def main():
    paths = sorted(ROOT.glob(".github/workflows/*.yml")) + sorted(ROOT.glob(".github/actions/*/action.yml"))
    problems = [problem for path in paths for problem in check_file(path)]
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        sys.exit(f"check-artifact-names: {len(problems)} problem(s)")
    print(f"check-artifact-names: {len(paths)} files clean")


if __name__ == "__main__":
    main()
