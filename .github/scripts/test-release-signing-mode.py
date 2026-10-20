import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = yaml.safe_load((ROOT / ".github/workflows/release-macos.yml").read_text())
STEPS = next(iter(WORKFLOW["jobs"].values()))["steps"]
SCRIPT = next(step["run"] for step in STEPS if step.get("name") == "Validate release signing mode")


class ReleaseSigningMode(unittest.TestCase):
    def run_mode(self, **overrides):
        env = dict(os.environ)
        env.update(GITHUB_EVENT_NAME="workflow_dispatch", GITHUB_REF_TYPE="branch",
                   GITHUB_REF_NAME="release/rc2", RELEASE_VERSION="0.21.0-rc2",
                   CANDIDATE_INPUT="false", DRY_RUN_UNSIGNED_INPUT="false",
                   TIMESTAMP_MODE_INPUT="enabled", TIMESTAMP_MODE_VAR="",
                   MACOS_CODESIGN_IDENTITY="test-identity",
                   MACOS_CODESIGN_KEYCHAIN_PASSWORD="test-password",
                   MACOS_NOTARY_API_KEY="", MACOS_NOTARY_ISSUER_ID="", MACOS_NOTARY_KEY_ID="")
        env.update(overrides)
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "env"
            env["GITHUB_ENV"] = str(output)
            result = subprocess.run(["bash", "-c", SCRIPT], env=env, capture_output=True, text=True)
            return result.returncode, output.read_text() if output.exists() else ""

    def test_candidate_still_signs(self):
        code, output = self.run_mode(CANDIDATE_INPUT="true")
        self.assertEqual(code, 0)
        self.assertIn("SIGNING_ENABLED=true", output)
        self.assertIn("NOTARIZATION_ENABLED=false", output)

    def test_normal_release_requires_notary_credentials(self):
        self.assertNotEqual(self.run_mode()[0], 0)
        code, output = self.run_mode(MACOS_NOTARY_API_KEY="key", MACOS_NOTARY_ISSUER_ID="issuer", MACOS_NOTARY_KEY_ID="id")
        self.assertEqual(code, 0)
        self.assertIn("NOTARIZATION_ENABLED=true", output)

    def test_candidate_cannot_weaken_tag_or_final_release(self):
        for override in [dict(GITHUB_REF_TYPE="tag", GITHUB_REF_NAME="v0.21.0-rc2"),
                         dict(RELEASE_VERSION="0.21.0"), dict(GITHUB_EVENT_NAME="push"),
                         dict(DRY_RUN_UNSIGNED_INPUT="true"), dict(TIMESTAMP_MODE_INPUT="none"),
                         dict(MACOS_CODESIGN_IDENTITY="")]:
            with self.subTest(override=override):
                self.assertNotEqual(self.run_mode(CANDIDATE_INPUT="true", **override)[0], 0)


if __name__ == "__main__":
    unittest.main()
