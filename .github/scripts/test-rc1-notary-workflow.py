#!/usr/bin/env python3
"""Workflow-level structural tests for rc1-existing-artifact-notary.yml.

Parses the real workflow file and enforces that it is the AUTH-ONLY control
plane: dispatch only from the pinned repository's trusted default branch, a
hard-pinned checkout action, minimal permissions, and no submit/staple/
release/build path anywhere in the file. No GitHub services are contacted;
this is a static analysis of the committed YAML plus the helper script.
"""

from __future__ import annotations

import re
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

WORKFLOW = Path(__file__).resolve().parents[1] / "workflows" / "rc1-existing-artifact-notary.yml"

# Verified via `gh api repos/actions/checkout/git/ref/tags/v7`; the ref peels
# directly to this commit (object.type == "commit", not an annotated tag).
CHECKOUT_PIN_SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1"

FORBIDDEN_JOB_NAME = re.compile(
    r"submit|staple|release|publish|upload|build|notariz", re.IGNORECASE)
FORBIDDEN_STEP_PATTERNS = [
    (re.compile(r"\bnotarytool\s+submit\b", re.IGNORECASE),
     "xcrun notarytool submit"),
    (re.compile(r"\bstapler\b", re.IGNORECASE), "xcrun stapler"),
    (re.compile(r"\bgh\s+(api|release)\b"), "gh api / gh release"),
    (re.compile(r"\bcodesign\b", re.IGNORECASE), "codesign"),
    (re.compile(r"secrets\.(?!MACOS_NOTARY_)"), "non-notary secret"),
    (re.compile(r"\bdmg\b", re.IGNORECASE), "DMG handling"),
]
REQUIRED_SUBSTRINGS = [
    "workflow_dispatch",
    "github.event.repository.default_branch",
    "gents-ai/gents",
    "rc1-notary-auth.sh",
    "MACOS_NOTARY_API_KEY",
    "MACOS_NOTARY_ISSUER_ID",
    "MACOS_NOTARY_KEY_ID",
    "contents: read",
]


class WorkflowStructureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.raw = WORKFLOW.read_text()
        cls.wf = yaml.safe_load(cls.raw)

    def test_workflow_exists_and_parses(self):
        self.assertTrue(WORKFLOW.is_file())
        self.assertIn("jobs", self.wf)

    def test_dispatch_is_manual_only(self):
        on = self.wf[True] if True in self.wf else self.wf["on"]
        self.assertEqual(sorted(on.keys()), ["workflow_dispatch"])
        dispatch = on["workflow_dispatch"] or {}
        self.assertNotIn("inputs", dispatch)

    def test_permissions_are_minimal(self):
        # The auth-only workflow needs no actions/read or any write scope.
        self.assertEqual(self.wf["permissions"], {"contents": "read"})

    def test_no_provenance_env_leak(self):
        self.assertNotIn("ACTIONS_BUILD_PROVENANCE", self.raw)
        for env_key in self.wf.get("env", {}):
            self.assertFalse(
                env_key.startswith("ACTIONS_"),
                f"unexpected runner-provenance env var: {env_key}")

    def test_repository_is_pinned(self):
        env = self.wf.get("env", {})
        self.assertEqual(env.get("GENTS_REPO"), "gents-ai/gents")

    def test_jobs_are_auth_only(self):
        job_names = list(self.wf["jobs"].keys())
        self.assertEqual(
            job_names, ["repository-guard", "notary-auth-preflight"],
            "auth-only workflow must have exactly the guard and preflight jobs")
        for name in job_names:
            self.assertIsNone(FORBIDDEN_JOB_NAME.search(name))

    def test_no_submit_staple_release_build_path_in_run_blocks(self):
        jobs = self.wf["jobs"]
        for job_name, job in jobs.items():
            for step in job.get("steps", []):
                run = step.get("run")
                if run is None:
                    continue
                for pattern, label in FORBIDDEN_STEP_PATTERNS:
                    match = pattern.search(run)
                    self.assertIsNone(
                        match,
                        f"job {job_name}: forbidden {label} path via "
                        f"{match.group(0) if match else ''!r} in:\n{run}")

    def test_checkout_is_hard_pinned_to_verified_sha(self):
        for job_name, job in self.wf["jobs"].items():
            checkout_steps = [s for s in job["steps"] if "uses" in s]
            self.assertEqual(
                len(checkout_steps), 1,
                f"job {job_name}: exactly one action step (the pinned checkout)")
            uses = checkout_steps[0]["uses"]
            self.assertEqual(
                uses, f"actions/checkout@{CHECKOUT_PIN_SHA}",
                f"job {job_name}: checkout must be pinned to the verified "
                f"upstream commit {CHECKOUT_PIN_SHA}, got {uses!r}")
            self.assertEqual(
                checkout_steps[0].get("with", {}).get("persist-credentials"), False,
                f"job {job_name}: checkout must not persist credentials")

    def test_trusted_default_branch_guard_is_explicit(self):
        guard = self.wf["jobs"]["repository-guard"]["steps"]
        guard_runs = [s.get("run", "") for s in guard if "run" in s]
        joined = "\n".join(guard_runs)
        self.assertIn("TRUSTED_DEFAULT_BRANCH", joined)
        self.assertNotIn("GITHUB_REPOSITORY_DEFAULT_BRANCH", self.raw)

    def test_guard_compares_full_ref_excluding_tags(self):
        for job_name in ("repository-guard", "notary-auth-preflight"):
            steps = self.wf["jobs"][job_name]["steps"]
            guard_steps = [s for s in steps if "pre-checkout" in (s.get("name") or "")]
            self.assertEqual(
                len(guard_steps), 1,
                f"job {job_name} must have exactly one job-level pre-checkout guard")
            # Guard must be the literal first step, before any checkout.
            self.assertIs(steps[0], guard_steps[0],
                          f"job {job_name}: guard must run before any checkout step")
            run = guard_steps[0]["run"]
            # Full-ref comparison: a tag named like the default branch must
            # not pass; only refs/heads/<branch> is accepted.
            self.assertIn('"${GITHUB_REF}" != "refs/heads/${TRUSTED_DEFAULT_BRANCH}"', run,
                          f"job {job_name}: guard must compare full GITHUB_REF against refs/heads/")
            self.assertNotIn("GITHUB_REF_NAME", run,
                             f"job {job_name}: short-name REF_NAME comparison must not be used")
            self.assertIn("GITHUB_HEAD_REF", run)

    def test_helper_is_invoked_with_credentials(self):
        preflight = self.wf["jobs"]["notary-auth-preflight"]["steps"]
        probe_steps = [
            s for s in preflight
            if "rc1-notary-auth.sh" in (s.get("run") or "")
        ]
        self.assertEqual(len(probe_steps), 1,
                         "helper must be invoked exactly once, in the preflight job")
        step_env = probe_steps[0].get("env", {})
        for var in ("MACOS_NOTARY_API_KEY", "MACOS_NOTARY_ISSUER_ID",
                    "MACOS_NOTARY_KEY_ID"):
            self.assertEqual(
                step_env.get(var), "${{ secrets." + var + " }}",
                f"{var} must come from the repository secret")

    def test_helper_runs_once_no_loop(self):
        preflight_runs = "\n".join(
            s.get("run", "") for s in self.wf["jobs"]["notary-auth-preflight"]["steps"])
        for loop in ("for ", "while ", "until "):
            self.assertNotIn(loop, preflight_runs)

    def test_required_strings_present(self):
        for needle in REQUIRED_SUBSTRINGS:
            self.assertIn(needle, self.raw)

    def test_durable_submission_receipt_logic_is_gone(self):
        self.assertNotIn("submission-attempt.json", self.raw)
        self.assertNotIn("receipts", self.raw)

    def test_runtime_runs_only_stdlib_auth_suite(self):
        guard = self.wf["jobs"]["repository-guard"]["steps"]
        test_steps = [s for s in guard
                      if "test-rc1-notary-auth.py" in (s.get("run") or "")]
        self.assertEqual(len(test_steps), 1,
                         "guard job must run the auth helper behavioral suite")
        run = test_steps[0]["run"]
        self.assertNotIn("test-rc1-notary-workflow.py", run)
        self.assertNotIn("test-rc1-notary-verify.py", run,
                         "removed verify suite must no longer be referenced")
        self.assertNotIn("rc1-notary-verify.py", run)

    def test_preflight_without_development_tools(self):
        steps = self.wf["jobs"]["repository-guard"]["steps"]
        run = next(s["run"] for s in steps if s.get("name", "").startswith("Preflight control"))
        with tempfile.TemporaryDirectory() as directory:
            # Only required command lookup entries; no ShellCheck, jq or PyYAML.
            os.symlink("/bin/sh", Path(directory) / "sh")
            python = Path(directory) / "python3"
            python.write_text("#!/bin/sh\nexit 91\n")  # must not import YAML
            python.chmod(0o700)
            result = subprocess.run(["/bin/bash", "-c", run], cwd=WORKFLOW.parents[2],
                                    env={"PATH": directory}, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            python.unlink()
            missing = subprocess.run(["/bin/bash", "-c", run], cwd=WORKFLOW.parents[2],
                                     env={"PATH": directory}, capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("python3", missing.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
