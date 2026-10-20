#!/usr/bin/env python3
"""Behavioral tests for rc1-notary-auth.sh (auth-only notary preflight).

Runs the real helper against a mocked `xcrun` on PATH and asserts its actual
behavior: allowlisted-only output, HTTP-status-driven failure classification,
private temp handling, and single-shot invocation. No GitHub services are
contacted and no real notary credential or network call is involved.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

AUTH_HELPER = Path(__file__).with_name("rc1-notary-auth.sh")

VALID_ENV = {
    "MACOS_NOTARY_API_KEY": "test-fake-key-material",
    "MACOS_NOTARY_ISSUER_ID": "test-issuer-id",
    "MACOS_NOTARY_KEY_ID": "testkeyid",
}


class AuthHelperBehaviorTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="rc1-auth-tests."))
        self.addCleanup(self._remove_root)
        self.fake_bin = self.root / "bin"
        self.fake_bin.mkdir()
        self.tmp_base = self.root / "tmproot"
        self.tmp_base.mkdir()

    def _remove_root(self):
        import shutil
        shutil.rmtree(self.root, ignore_errors=True)

    def _run_helper(self, stderr_body: str | None = None, history_status: int = 0,
                    stdout_body: str = "",
                    env_overrides: dict[str, str] | None = None) -> subprocess.CompletedProcess:
        fake_xcrun = self.fake_bin / "xcrun"
        script = ["#!/bin/sh"]
        if stderr_body is not None:
            script.append(f"printf '%s' {stderr_body!r} >&2")
        if stdout_body:
            script.append(f"printf '%s' {stdout_body!r}")
        script.append(f"exit {history_status}")
        fake_xcrun.write_text("\n".join(script) + "\n")
        fake_xcrun.chmod(0o755)

        env = dict(os.environ)
        env.update(VALID_ENV)
        env["PATH"] = f"{self.fake_bin}:{env['PATH']}"
        env["RUNNER_TEMP"] = str(self.tmp_base)
        if env_overrides:
            for key, value in env_overrides.items():
                if value is None:
                    env.pop(key, None)
                else:
                    env[key] = value
        return subprocess.run(
            ["/bin/sh", str(AUTH_HELPER)], capture_output=True, text=True,
            env=env, timeout=30)

    def _leftovers(self) -> list[str]:
        return sorted(p.name for p in self.tmp_base.iterdir())

    # --- success path -------------------------------------------------

    def test_success_emits_allowlisted_line_only(self):
        result = self._run_helper(stdout_body='{"submissions":[]}')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("success: auth-only preflight passed", result.stdout)
        self.assertNotIn("submissions", result.stdout)
        self.assertEqual(result.stderr, "")
        self.assertEqual(self._leftovers(), [], "temp dir must be removed on success")

    def test_success_requires_nonempty_history_capture(self):
        result = self._run_helper(stdout_body="")
        self.assertEqual(result.returncode, 3)
        self.assertIn("notary history returned no data", result.stderr)
        self.assertEqual(self._leftovers(), [])

    # --- 403 classification (HTTP status driven, wording independent) --

    def test_stderr_http_403_gets_fixed_403_diagnostic(self):
        result = self._run_helper(
            stderr_body="Error: HTTP status 403: ACCESS_DENIED key revoked",
            history_status=64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("notary credential rejected (HTTP 403)", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("ACCESS_DENIED", result.stdout + result.stderr)
        self.assertEqual(self._leftovers(), [])

    def test_agreement_wording_with_403_gets_the_same_403_diagnostic(self):
        # Agreement language must NOT change the diagnostic; the HTTP status
        # alone drives classification.
        result = self._run_helper(
            stderr_body="Error: HTTP status 403: You must accept the notary agreement",
            history_status=64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("notary credential rejected (HTTP 403)", result.stderr)
        self.assertNotIn("agreement", result.stderr)
        self.assertNotIn("You must accept", result.stdout + result.stderr)

    def test_agreement_wording_with_other_status_gets_generic_diagnostic(self):
        # A non-403 failure that mentions an agreement is still generic.
        result = self._run_helper(
            stderr_body="Error: HTTP status 409: agreement state conflict",
            history_status=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("notarytool history exited 1", result.stderr)
        self.assertNotIn("403", result.stderr)
        self.assertNotIn("agreement state conflict", result.stdout + result.stderr)

    def test_agreement_mention_without_403_gets_generic_diagnostic(self):
        result = self._run_helper(
            stderr_body="Error: transport failure while fetching agreement",
            history_status=7)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("notarytool history exited 7", result.stderr)
        self.assertNotIn("403", result.stderr)
        self.assertNotIn("transport failure", result.stdout + result.stderr)

    def test_stdout_only_403_is_not_classified_as_403(self):
        # Classification reads ONLY the private stderr capture.
        result = self._run_helper(
            stdout_body="Error: HTTP status 403: ACCESS_DENIED",
            history_status=64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("notarytool history exited 64", result.stderr)
        self.assertNotIn("HTTP 403", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_non_403_failure_never_leaks_raw_provider_error(self):
        raw = "Error: HTTP status 500: provider blew up with internal details"
        result = self._run_helper(stderr_body=raw, history_status=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exited 1", result.stderr)
        self.assertNotIn("HTTP status 500", result.stdout + result.stderr)
        self.assertNotIn("internal details", result.stdout + result.stderr)

    # --- credential hygiene --------------------------------------------

    def test_missing_secret_fails_without_invoking_notarytool(self):
        result = self._run_helper(stderr_body="should not run",
                                  env_overrides={"MACOS_NOTARY_API_KEY": None})
        self.assertEqual(result.returncode, 2)
        self.assertIn("missing notary secret(s)", result.stderr)
        self.assertIn("MACOS_NOTARY_API_KEY", result.stderr)
        self.assertNotIn("should not run", result.stdout)

    def test_key_material_never_appears_in_output(self):
        result = self._run_helper(
            stderr_body="Error: HTTP status 403: ACCESS_DENIED",
            history_status=64)
        self.assertNotIn("test-fake-key-material", result.stdout + result.stderr)

    def test_key_id_never_appears_in_any_temp_path(self):
        # The .p8 is written to a constant AuthKey.p8 filename.
        helper = AUTH_HELPER.read_text()
        self.assertIn("AuthKey.p8", helper)
        self.assertNotIn("AuthKey_${MACOS_NOTARY_KEY_ID}", helper)

    def test_temp_dir_prefers_runner_temp(self):
        helper = AUTH_HELPER.read_text()
        self.assertIn("${RUNNER_TEMP:-${TMPDIR:-/tmp}}", helper)

    def test_cleanup_trap_covers_all_signals(self):
        helper = AUTH_HELPER.read_text()
        self.assertIn("trap cleanup EXIT INT TERM", helper)
        self.assertIn("best-effort", helper)

    def test_probe_runs_exactly_once_no_loop(self):
        code_only = "\n".join(
            line for line in AUTH_HELPER.read_text().splitlines()
            if not line.lstrip().startswith("#"))
        self.assertEqual(code_only.count("xcrun notarytool history"), 1)
        for loop in ("for ", "while ", "until "):
            self.assertNotIn(loop, code_only)

    def test_no_submission_or_staple_path_in_helper(self):
        helper = AUTH_HELPER.read_text()
        for forbidden in ("notarytool submit", "stapler", "codesign", "gh api"):
            self.assertNotIn(forbidden, helper)


if __name__ == "__main__":
    unittest.main(verbosity=2)
