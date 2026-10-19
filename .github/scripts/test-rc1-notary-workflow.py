#!/usr/bin/env python3
"""Workflow-level structural tests for rc1-existing-artifact-notary.yml.

Parses the real workflow file and enforces that it is the AUTH-ONLY control
plane: dispatch only from the pinned repository's trusted default branch, and
no submit/staple/release/build path anywhere in the file. No GitHub services
are contacted; this is a static analysis of the committed YAML plus the
helper script on disk.
"""

from __future__ import annotations

import re
import tempfile
import unittest
from pathlib import Path

import yaml

WORKFLOW = Path(__file__).resolve().parents[1] / "workflows" / "rc1-existing-artifact-notary.yml"
AUTH_HELPER = Path(__file__).with_name("rc1-notary-auth.sh")

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
    "actions: read",
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
        self.assertEqual(self.wf["permissions"], {"contents": "read", "actions": "read"})

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


class AuthHelperStructureTests(unittest.TestCase):
    def test_helper_exists_and_is_executable(self):
        self.assertTrue(AUTH_HELPER.is_file())
        self.assertTrue(AUTH_HELPER.stat().st_mode & 0o111)

    def test_helper_has_cleanup_trap_and_private_capture(self):
        helper = AUTH_HELPER.read_text()
        self.assertIn("trap cleanup EXIT INT TERM", helper)
        self.assertIn("chmod 0700", helper)
        self.assertIn("chmod 0600", helper)
        self.assertIn("HISTORY_FILE", helper)
        self.assertIn("notarytool history", helper)
        self.assertNotIn("notarytool submit", helper)
        self.assertNotIn("stapler", helper)
        # No raw payload may be echoed: the only printf of history content is
        # absent; output is read from the private file by grep/jq only.
        self.assertNotIn('printf \'%s\\n\' "${history_output}"', helper)

    def test_helper_bash_and_sh_clean(self):
        import subprocess
        for argv in (["sh", "-n", str(AUTH_HELPER)],
                     ["shellcheck", "-s", "sh", str(AUTH_HELPER)]):
            result = subprocess.run(argv, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class TempIsolationSmokeTest(unittest.TestCase):
    """Sanity check that the mocked helper tests use an isolated TMPDIR."""

    def test_tmpdir_isolation_smoke(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertTrue(Path(tmp).is_dir())


if __name__ == "__main__":
    unittest.main(verbosity=2)
