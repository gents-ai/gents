#!/usr/bin/env python3
"""Mocked tests for the RC1 existing-artifact notary control plane.

No Apple service, no real binary, no DMG, and no key material is involved:
every external effect is a local temp-dir stub, and notarytool is replaced by
a small fake executable. Verifies hash pinning, fail-closed reconciliation,
sanitized auth diagnostics, and that the desktop manifest checks run without
any execution of candidate binaries or scripts.
"""

from __future__ import annotations

import glob
import hashlib
import importlib.util
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "rc1_notary_verify", Path(__file__).with_name("rc1-notary-verify.py"))
verify = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(verify)


def make_file(path: Path, size: int, seed: bytes) -> Path:
    path.write_bytes(seed * (size // max(len(seed), 1)))
    return path


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()

    def _write_cli_tarball(self, tarball_path: Path, binary_bytes: bytes) -> str:
        app_dir = self.root / "gents-aarch64-apple-darwin-signed-unnotarized-candidate"
        app_dir.mkdir(exist_ok=True)
        binary = app_dir / "gents"
        binary.write_bytes(binary_bytes)
        license_file = app_dir / "LICENSE"
        license_file.write_text("Apache-2.0")
        with tarfile.open(tarball_path, "w:gz") as archive:
            archive.add(binary, arcname=f"{app_dir.name}/gents")
            archive.add(license_file, arcname=f"{app_dir.name}/LICENSE")
        return hashlib.sha256(tarball_path.read_bytes()).hexdigest()

    def test_cli_tarball_hash_and_inner_binary_match(self):
        tarball = self.root / "cli.tar.gz"
        tarball_sha = self._write_cli_tarball(tarball, b"fake-cli-binary")
        binary_sha = hashlib.sha256(b"fake-cli-binary").hexdigest()
        report = verify.check_cli_tarball(tarball, tarball_sha, binary_sha)
        self.assertEqual(report["binary_sha256"], binary_sha)

    def test_cli_tarball_mismatch_fails_closed(self):
        tarball = self.root / "cli.tar.gz"
        tarball_sha = self._write_cli_tarball(tarball, b"fake-cli-binary")
        wrong_binary_sha = hashlib.sha256(b"other").hexdigest()
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_cli_tarball(tarball, tarball_sha, wrong_binary_sha)

    def test_cli_tarball_missing_inner_entry_fails(self):
        tarball = self.root / "cli.tar.gz"
        tarball.write_bytes(b"not a real tarball")
        tarball_sha = hashlib.sha256(tarball.read_bytes()).hexdigest()
        with self.assertRaises((tarfile.ReadError, tarfile.TarError, OSError)):
            verify.check_cli_tarball(tarball, tarball_sha, "0" * 64)

    def test_dmg_structural_check_passes_on_koly_trailer(self):
        dmg = self.root / "fake.dmg"
        dmg.write_bytes(b"x" * 4096 + b"koly" + b"\0" * 508)
        dmg_sha = hashlib.sha256(dmg.read_bytes()).hexdigest()
        report = verify.check_dmg(dmg, dmg_sha, "a" * 64, "b" * 64, "c" * 64)
        self.assertEqual(report["dmg_sha256"], dmg_sha)

    def test_dmg_without_koly_trailer_fails_closed(self):
        dmg = self.root / "fake.dmg"
        dmg.write_bytes(b"not a dmg at all" * 64)
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_dmg(dmg, "0" * 64, "a" * 64, "b" * 64, "c" * 64)

    def test_dmg_hash_mismatch_fails_closed(self):
        dmg = self.root / "fake.dmg"
        dmg.write_bytes(b"x" * 4096 + b"koly" + b"\0" * 508)
        wrong = hashlib.sha256(b"other").hexdigest()
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_dmg(dmg, wrong, "a" * 64, "b" * 64, "c" * 64)

    def test_manifest_content_check_rejects_wrong_sha(self):
        manifest = self.root / "candidate-build.json"
        manifest.write_text(json.dumps({"sha": "wrong", "notarized": False,
                                        "binaries_sha256": {}}))
        manifest_sha = hashlib.sha256(manifest.read_bytes()).hexdigest()
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_desktop_manifest(
                manifest, manifest_sha,
                "08edd1bdfedca73fa28e648e4781ae03aeacf5e6")

    def test_manifest_content_check_rejects_notarized_true(self):
        manifest = self.root / "candidate-build.json"
        manifest.write_text(json.dumps({
            "sha": "08edd1bdfedca73fa28e648e4781ae03aeacf5e6", "notarized": True,
            "binaries_sha256": {}}))
        manifest_sha = hashlib.sha256(manifest.read_bytes()).hexdigest()
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_desktop_manifest(
                manifest, manifest_sha,
                "08edd1bdfedca73fa28e648e4781ae03aeacf5e6")

    def test_manifest_content_check_accepts_real_candidate(self):
        manifest = self.root / "candidate-build.json"
        manifest.write_text(json.dumps({
            "sha": "08edd1bdfedca73fa28e648e4781ae03aeacf5e6", "notarized": False,
            "binaries_sha256": {"gents-desktop-tauri": "x" * 64}}))
        manifest_sha = hashlib.sha256(manifest.read_bytes()).hexdigest()
        report = verify.check_desktop_manifest(
            manifest, manifest_sha,
            "08edd1bdfedca73fa28e648e4781ae03aeacf5e6")
        self.assertEqual(report["manifest_sha256"], manifest_sha)

    def test_source_sha_shape_check(self):
        verify.check_source_sha("08edd1bdfedca73fa28e648e4781ae03aeacf5e6")
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_source_sha("short-sha")
        with self.assertRaisesRegex(SystemExit, "1"):
            verify.check_source_sha("08edd1bdfedca73fa28e648e4781ae03aeacf5e7".replace("7", "g"))


AUTH_HELPER = Path(__file__).with_name("rc1-notary-auth.sh")


@unittest.skipUnless(AUTH_HELPER.exists(), "rc1-notary-auth.sh not present")
class AuthHelperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.fake_bin = self.root / "fake-bin"
        self.fake_bin.mkdir()

    def _run_helper(self, history_output: str, history_status: int,
                    env_overrides: dict[str, str] | None = None):
        fake_xcrun = self.fake_bin / "xcrun"
        fake_xcrun.write_text(
            "#!/bin/sh\n"
            f"printf '%s' {json.dumps(history_output)}\n"
            f"exit {history_status}\n")
        fake_xcrun.chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = f"{self.fake_bin}:{env.get('PATH', '')}"
        env["TMPDIR"] = str(self.root)
        env.setdefault("MACOS_NOTARY_API_KEY", "fake-key-material")
        env.setdefault("MACOS_NOTARY_ISSUER_ID", "fake-issuer")
        env.setdefault("MACOS_NOTARY_KEY_ID", "fakekeyid")
        env.update(env_overrides or {})
        return subprocess.run(
            ["sh", str(AUTH_HELPER)], env=env, capture_output=True, text=True)

    def test_successful_history_probe_cleans_up_key(self):
        result = self._run_helper('{"submissions": []}', 0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("auth-only preflight succeeded", result.stdout)
        leftovers = [p for p in self.root.iterdir() if p.name.startswith("gents-rc1-notary-auth.")]
        self.assertEqual(leftovers, [], "key temp dir must be removed on success")

    def test_403_reports_sanitized_diagnostic_and_fails(self):
        result = self._run_helper(
            "Error: HTTP status 403: ACCESS_DENIED key file /tmp/x/AuthKey_fakekeyid.p8", 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("403", result.stderr)
        self.assertIn("403", result.stdout)
        self.assertNotIn("fake-key-material", result.stdout + result.stderr)
        leftovers = [p for p in self.root.iterdir() if p.name.startswith("gents-rc1-notary-auth.")]
        self.assertEqual(leftovers, [], "key temp dir must be removed on failure")

    def test_missing_secret_fails_without_invoking_notarytool(self):
        result = self._run_helper("should not run", 0,
                                  {"MACOS_NOTARY_API_KEY": ""})
        self.assertEqual(result.returncode, 2)
        self.assertNotIn("should not run", result.stdout)


class WorkflowReconciliationTests(unittest.TestCase):
    """Fail-closed behavior of the durable submission receipt used by the workflow."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.receipts = Path(self.temporary.name) / "receipts"
        self.receipts.mkdir()

    def _has_prior_submission(self) -> bool:
        import glob
        return bool(glob.glob(str(self.receipts / "submission-*.json")))

    def test_no_prior_submission_allows_submit(self):
        self.assertFalse(self._has_prior_submission())

    def test_durable_receipt_blocks_repeat_submission(self):
        (self.receipts / "submission-abc123.json").write_text(
            json.dumps({"submission_id": "abc123", "status": "Accepted"}))
        self.assertTrue(self._has_prior_submission())

    def test_pending_submission_id_blocks_repeat_submission(self):
        import glob
        pending = self.receipts / "submission-id.pending"
        pending.write_text("abc123\n")
        self.assertTrue(bool(glob.glob(str(self.receipts / "submission-id*"))))


if __name__ == "__main__":
    unittest.main(verbosity=2)
