import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/release-macos.yml").read_text()
START = WORKFLOW.index("      - name: Validate release signing mode")
BODY = WORKFLOW[START:].split("        run: |\n", 1)[1].split("\n      - ", 1)[0]
SIGNING_SCRIPT = "\n".join(line[10:] for line in BODY.splitlines())


class ReleaseModes(unittest.TestCase):
    def signing(self, *, candidate=False, unsigned=False, tag=False,
                event="workflow_dispatch", notary=True, timestamp="auto", sha="a" * 40):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "env"
            env = {
                **os.environ,
                "SIGNED_UNNOTARIZED_INPUT": str(candidate).lower(),
                "DRY_RUN_UNSIGNED_INPUT": str(unsigned).lower(),
                "GITHUB_EVENT_NAME": event,
                "GITHUB_REF_TYPE": "tag" if tag else "branch",
                "GITHUB_REF_NAME": "v0.21.0-rc1" if tag else "release/rc1-candidate",
                "TIMESTAMP_MODE_INPUT": timestamp,
                "MACOS_CODESIGN_IDENTITY": "Developer ID Application: Fixture",
                "MACOS_CODESIGN_KEYCHAIN_PASSWORD": "fixture",
                "MACOS_NOTARY_API_KEY": "fixture" if notary else "",
                "MACOS_NOTARY_ISSUER_ID": "fixture" if notary else "",
                "MACOS_NOTARY_KEY_ID": "fixture" if notary else "",
                "GITHUB_ENV": str(output),
                "CANDIDATE_SHA": sha,
                "GITHUB_SHA": "a" * 40,
            }
            result = subprocess.run(["bash", "-c", SIGNING_SCRIPT], env=env,
                                    capture_output=True, text=True)
            return result.returncode, output.read_text() if output.exists() else ""

    def test_normal_release_requires_notarization(self):
        self.assertEqual(self.signing(tag=True)[0], 0)
        self.assertNotEqual(self.signing(tag=True, notary=False)[0], 0)
        self.assertIn("NOTARIZATION_ENABLED=true", self.signing(tag=True)[1])

    def test_candidate_is_signed_without_notary_secrets(self):
        code, output = self.signing(candidate=True, notary=False)
        self.assertEqual(code, 0)
        self.assertIn("SIGNING_ENABLED=true", output)
        self.assertIn("NOTARIZATION_ENABLED=false", output)
        self.assertIn("-signed-unnotarized-candidate", output)

    def test_candidate_requires_exact_sha(self):
        for sha in ("", "a" * 39, "b" * 40):
            self.assertNotEqual(self.signing(candidate=True, sha=sha)[0], 0)

    def test_candidate_cannot_be_tag_unsigned_or_push(self):
        for options in ({"tag": True}, {"unsigned": True}, {"event": "push"}):
            with self.subTest(options=options):
                self.assertNotEqual(self.signing(candidate=True, **options)[0], 0)

    def test_signed_modes_require_timestamp(self):
        for candidate in (False, True):
            self.assertNotEqual(self.signing(candidate=candidate, timestamp="none")[0], 0)

    def test_unsigned_dry_run_stays_manual_only(self):
        self.assertEqual(self.signing(unsigned=True, notary=False)[0], 0)
        self.assertNotEqual(self.signing(unsigned=True, tag=True)[0], 0)

    def test_candidate_removes_bundler_notarization_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            uname = folder / "uname"
            uname.write_text('#!/bin/bash\nif [[ "$1" == "-s" ]]; then echo Darwin; else echo arm64; fi\n')
            uname.chmod(0o755)
            npm = folder / "npm"
            npm.write_text('#!/bin/bash\n/usr/bin/env > "$CAPTURE_ENV"\nexit 42\n')
            npm.chmod(0o755)
            keys = ("APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH",
                    "APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID", "MACOS_NOTARY_API_KEY")
            env = {**os.environ, **dict.fromkeys(keys, "must-not-reach-bundler"),
                   "PATH": f"{folder}:/usr/bin:/bin", "CAPTURE_ENV": str(folder / "captured"),
                   "RUNNER_TEMP": directory, "CARGO_TARGET_DIR": directory,
                   "RELEASE_VERSION": "0.21.0-rc1", "APPLE_SIGNING_IDENTITY": "fixture",
                   "NOTARIZATION_ENABLED": "false",
                   "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF_TYPE": "branch"}
            result = subprocess.run(["bash", str(ROOT / "scripts/package-macos-desktop.sh")],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 42, result.stderr)
            captured = dict(line.split("=", 1) for line in (folder / "captured").read_text().splitlines())
            self.assertTrue(all(key not in captured for key in keys))
            self.assertEqual(captured["APPLE_SIGNING_IDENTITY"], "fixture")


if __name__ == "__main__":
    unittest.main()
