#!/usr/bin/env python3
"""Behavioral and static tests for notarize-staged-artifact.

The script is executed for real against a real zip of a fake .app, with only
`codesign`, `xcrun` and `spctl` replaced by PATH shims that record every call.
Real `ditto`, `shasum` and `python3` are used, so these tests run on macOS.
Static workflow checks use PyYAML when available (development dependency).
"""

import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "notarize-staged-artifact.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "notarize-staged-artifact.yml"

SECRET = "-----BEGIN PRIVATE KEY-----TOPSECRETKEYMATERIAL-----END PRIVATE KEY-----"
KEY_ID = "KEYID12345"
ISSUER = "11111111-2222-3333-4444-555555555555"
SUBMISSION = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
LEAF_CONTENT = b"fake-leaf-certificate-der"
LEAF_SHA = hashlib.sha256(LEAF_CONTENT).hexdigest()

CODESIGN_SHIM = r"""#!/bin/bash
echo "codesign $*" >> "$SHIM_LOG"
case "$1" in
  --verify) exit "${SHIM_VERIFY_RC:-0}" ;;
  -dv)
    echo "Identifier=${SHIM_IDENT:-com.source-inc.gents}" >&2
    if [[ -n "${SHIM_NO_RUNTIME:-}" ]]; then
      echo "CodeDirectory v=20500 size=1 flags=0x0(none) hashes=1+7 location=embedded" >&2
    else
      echo "CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded" >&2
    fi
    echo "Authority=Developer ID Application: Test (NXJ753WW5X)" >&2
    echo "TeamIdentifier=${SHIM_TEAM:-NXJ753WW5X}" >&2
    exit 0 ;;
  -d)
    prefix="${2#--extract-certificates=}"
    printf '%s' "${SHIM_LEAF:-fake-leaf-certificate-der}" > "${prefix}0"
    exit 0 ;;
esac
exit 99
"""

XCRUN_SHIM = r"""#!/bin/bash
tool="$1"; shift
sub="$1"; shift
key=""
args=("$@")
for ((i=0; i<${#args[@]}; i++)); do
  [[ "${args[$i]}" == "--key" ]] && key="${args[$((i+1))]}"
done
if [[ -n "$key" ]]; then
  echo "$key" >> "$SHIM_STATE/keypaths"
  [[ -f "$key" ]] || { echo "key missing" >&2; exit 98; }
fi
echo "$tool $sub receipt_present=$([[ -f "$NOTARY_OUTPUT_DIR/receipt.json" ]] && echo 1 || echo 0)" >> "$SHIM_LOG"
if [[ "$tool" == "stapler" ]]; then
  app="${args[${#args[@]}-1]}"
  case "$sub" in
    validate) [[ -f "$app/.stapled" ]] && exit 0; echo "does not have a ticket" >&2; exit 65 ;;
    staple) touch "$app/.stapled"; exit 0 ;;
  esac
fi
case "$sub" in
  history)
    if [[ -n "${SHIM_HISTORY_RC:-}" ]]; then
      echo "Error: HTTP status code: 403. A required agreement is missing or has expired. key=$key" >&2
      exit "$SHIM_HISTORY_RC"
    fi
    default_history='{"history":[],"message":"ok"}'
    printf '%s' "${SHIM_HISTORY_JSON:-$default_history}"
    exit 0 ;;
  submit)
    default_submit='{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","message":"ok"}'
    printf '%s' "${SHIM_SUBMIT_JSON:-$default_submit}"
    exit 0 ;;
  wait) exit "${SHIM_WAIT_RC:-0}" ;;
  info)
    printf '{"id":"%s","name":"%s","status":"%s"}' "${args[0]}" "${SHIM_INFO_NAME}" "${SHIM_STATUS:-Accepted}"
    exit 0 ;;
  log)
    printf '{"issues":null}' > "${args[${#args[@]}-1]}"
    exit 0 ;;
esac
exit 97
"""

SPCTL_SHIM = r"""#!/bin/bash
echo "spctl $*" >> "$SHIM_LOG"
app="${@: -1}"
if [[ -f "$app/.stapled" ]]; then
  echo "$app: accepted"; echo "source=Notarized Developer ID"; exit 0
fi
echo "$app: rejected"; echo "source=Unnotarized Developer ID"; exit 3
"""


class NotarizeScriptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="notarize-test-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.shims = self.tmp / "shims"
        self.shims.mkdir()
        for name, body in (("codesign", CODESIGN_SHIM), ("xcrun", XCRUN_SHIM), ("spctl", SPCTL_SHIM)):
            path = self.shims / name
            path.write_text(body)
            path.chmod(0o755)
        self.state = self.tmp / "state"
        self.state.mkdir()
        self.inbox = self.tmp / "inbox"
        self.inbox.mkdir()
        self.runner_temp = self.tmp / "runner-temp"
        self.runner_temp.mkdir()
        self.out = self.runner_temp / "notary-output"
        app = self.tmp / "src" / "Gents.app" / "Contents" / "MacOS"
        app.mkdir(parents=True)
        (app / "gents").write_text("not executed\n")
        zip_path = self.tmp / "candidate.zip"
        subprocess.run(
            ["ditto", "-c", "-k", "--keepParent", str(self.tmp / "src" / "Gents.app"), str(zip_path)],
            check=True,
        )
        data = zip_path.read_bytes()
        self.sha = hashlib.sha256(data).hexdigest()
        (self.inbox / f"{self.sha}.zip").write_bytes(data)
        self.name = f"gents-notary-{self.sha[:16]}.zip"

    def run_script(self, mode="submit", submission_id="", sha=None, **env_overrides):
        env = {
            "PATH": f"{self.shims}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.tmp),
            "RUNNER_TEMP": str(self.runner_temp),
            "ARTIFACT_SHA256": sha if sha is not None else self.sha,
            "NOTARY_MODE": mode,
            "SUBMISSION_ID": submission_id,
            "NOTARY_INBOX_DIR": str(self.inbox),
            "NOTARY_OUTPUT_DIR": str(self.out),
            "EXPECTED_TEAM_ID": "NXJ753WW5X",
            "EXPECTED_BUNDLE_ID": "com.source-inc.gents",
            "EXPECTED_LEAF_CERT_SHA256": LEAF_SHA,
            "MACOS_NOTARY_API_KEY": SECRET,
            "MACOS_NOTARY_ISSUER_ID": ISSUER,
            "MACOS_NOTARY_KEY_ID": KEY_ID,
            "SHIM_LOG": str(self.state / "calls.log"),
            "SHIM_STATE": str(self.state),
            "SHIM_INFO_NAME": self.name,
            "GITHUB_RUN_ID": "123",
            "GITHUB_RUN_ATTEMPT": "1",
        }
        env.update(env_overrides)
        result = subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)
        self.assertNotIn("TOPSECRETKEYMATERIAL", result.stdout + result.stderr)
        self.assertNotIn(KEY_ID + ".p8", result.stdout + result.stderr)
        return result

    def calls(self):
        log = self.state / "calls.log"
        return log.read_text().splitlines() if log.exists() else []

    def notary_calls(self):
        return [c for c in self.calls() if c.startswith("notarytool")]

    def assert_no_credentials_used(self):
        self.assertEqual(self.notary_calls(), [])
        self.assertFalse((self.state / "keypaths").exists())

    def assert_credentials_removed(self):
        keypaths = (self.state / "keypaths").read_text().split()
        self.assertTrue(keypaths)
        for path in keypaths:
            self.assertFalse(Path(path).exists(), path)
            self.assertFalse(Path(path).parent.exists(), path)
        leftovers = [p.name for p in self.runner_temp.iterdir() if p.name != "notary-output"]
        self.assertEqual(leftovers, [])

    # ---- happy path -------------------------------------------------
    def test_submit_success_end_to_end(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        sequence = [c.split(" receipt")[0] for c in self.calls() if c.startswith(("notarytool", "stapler"))]
        self.assertEqual(
            sequence,
            [
                "stapler validate",
                "notarytool history",
                "notarytool submit",
                "notarytool wait",
                "notarytool info",
                "notarytool log",
                "stapler staple",
                "stapler validate",
            ],
        )
        submit_calls = [c for c in self.calls() if c.startswith("notarytool submit")]
        self.assertEqual(len(submit_calls), 1)
        self.assertIn("receipt_present=0", submit_calls[0])
        wait_call = next(c for c in self.calls() if c.startswith("notarytool wait"))
        self.assertIn("receipt_present=1", wait_call)
        receipt = json.loads((self.out / "receipt.json").read_text())
        self.assertEqual(receipt["submission_id"], SUBMISSION)
        self.assertEqual(receipt["submission_name"], self.name)
        self.assertEqual(receipt["artifact_sha256"], self.sha)
        self.assertIn(f"submission_id={SUBMISSION}", result.stdout)
        sums = (self.out / "SHA256SUMS").read_text()
        out_zip = self.out / f"Gents-{self.sha[:16]}-notarized.zip"
        self.assertIn(hashlib.sha256(out_zip.read_bytes()).hexdigest(), sums)
        # Inbox original untouched.
        self.assertEqual(hashlib.sha256((self.inbox / f"{self.sha}.zip").read_bytes()).hexdigest(), self.sha)
        self.assert_credentials_removed()

    # ---- input / artifact failures before credentials ----------------
    def test_bad_sha_input_fails_before_credentials(self):
        result = self.run_script(sha="ABC")
        self.assertEqual(result.returncode, 2)
        self.assert_no_credentials_used()

    def test_sha_mismatch_fails_before_credentials(self):
        result = self.run_script(sha="0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assert_no_credentials_used()
        (self.inbox / ("0" * 64 + ".zip")).write_bytes(b"different")
        result = self.run_script(sha="0" * 64)
        self.assertEqual(result.returncode, 3)
        self.assertIn("mismatch", result.stderr)
        self.assert_no_credentials_used()

    def test_wrong_team_fails_before_credentials(self):
        result = self.run_script(SHIM_TEAM="OTHERTEAM1")
        self.assertEqual(result.returncode, 5)
        self.assert_no_credentials_used()

    def test_wrong_identifier_fails_before_credentials(self):
        result = self.run_script(SHIM_IDENT="com.example.other")
        self.assertEqual(result.returncode, 5)
        self.assert_no_credentials_used()

    def test_wrong_cert_pin_fails_before_credentials(self):
        result = self.run_script(SHIM_LEAF="another-certificate")
        self.assertEqual(result.returncode, 5)
        self.assertIn("leaf certificate", result.stderr)
        self.assert_no_credentials_used()

    def test_missing_hardened_runtime_fails_before_credentials(self):
        result = self.run_script(SHIM_NO_RUNTIME="1")
        self.assertEqual(result.returncode, 5)
        self.assert_no_credentials_used()

    def test_invalid_signature_fails_before_credentials(self):
        result = self.run_script(SHIM_VERIFY_RC="1")
        self.assertEqual(result.returncode, 5)
        self.assert_no_credentials_used()

    def test_already_stapled_submit_fails(self):
        staged = self.tmp / "stapled-src" / "Gents.app"
        staged.mkdir(parents=True)
        (staged / ".stapled").write_text("")
        zip_path = self.tmp / "stapled.zip"
        subprocess.run(["ditto", "-c", "-k", "--keepParent", str(staged), str(zip_path)], check=True)
        data = zip_path.read_bytes()
        sha = hashlib.sha256(data).hexdigest()
        (self.inbox / f"{sha}.zip").write_bytes(data)
        result = self.run_script(sha=sha)
        self.assertEqual(result.returncode, 6)
        self.assert_no_credentials_used()

    def test_mode_input_validation(self):
        self.assertEqual(self.run_script(mode="other").returncode, 2)
        self.assertEqual(self.run_script(mode="submit", submission_id=SUBMISSION).returncode, 2)
        self.assertEqual(self.run_script(mode="resume", submission_id="not-a-uuid").returncode, 2)
        self.assertEqual(self.run_script(mode="resume", submission_id="").returncode, 2)
        self.assert_no_credentials_used()

    # ---- duplicate guard / submit ---------------------------------
    def test_duplicate_history_entry_blocks_submit(self):
        history = json.dumps(
            {"history": [{"id": SUBMISSION, "name": self.name, "status": "In Progress", "createdDate": "2026-10-20"}]}
        )
        result = self.run_script(SHIM_HISTORY_JSON=history)
        self.assertEqual(result.returncode, 9)
        self.assertIn(SUBMISSION, result.stderr)
        self.assertIn("mode=resume", result.stderr)
        self.assertFalse(any(c.startswith("notarytool submit") for c in self.calls()))
        self.assert_credentials_removed()

    def test_other_history_names_do_not_block(self):
        history = json.dumps({"history": [{"id": "x", "name": "unrelated.zip", "status": "Accepted"}]})
        result = self.run_script(SHIM_HISTORY_JSON=history)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_unparseable_history_blocks_submit(self):
        result = self.run_script(SHIM_HISTORY_JSON="not json")
        self.assertEqual(result.returncode, 8)
        self.assertFalse(any(c.startswith("notarytool submit") for c in self.calls()))

    def test_history_403_neutral_diagnostic_no_submit(self):
        result = self.run_script(SHIM_HISTORY_RC="1")
        self.assertEqual(result.returncode, 20)
        self.assertIn("HTTP 403", result.stderr)
        self.assertNotIn("agreement", result.stderr.lower())
        self.assertFalse(any(c.startswith("notarytool submit") for c in self.calls()))
        self.assert_credentials_removed()

    def test_submit_without_id_fails_and_points_to_history(self):
        result = self.run_script(SHIM_SUBMIT_JSON='{"message":"odd"}')
        self.assertEqual(result.returncode, 10)
        self.assertIn("check notary history", result.stderr)
        self.assertFalse(any(c.startswith("notarytool wait") for c in self.calls()))

    # ---- wait / info ------------------------------------------------
    def test_invalid_status_fails_and_keeps_log(self):
        result = self.run_script(SHIM_STATUS="Invalid", SHIM_WAIT_RC="1")
        self.assertEqual(result.returncode, 12)
        self.assertTrue((self.out / "notary-log.json").exists())
        self.assertTrue((self.out / "receipt.json").exists())
        self.assertFalse(any(c.startswith("stapler staple") for c in self.calls()))
        self.assert_credentials_removed()

    def test_in_progress_points_to_resume(self):
        result = self.run_script(SHIM_STATUS="In Progress", SHIM_WAIT_RC="1")
        self.assertEqual(result.returncode, 12)
        self.assertIn("mode=resume", result.stderr)

    # ---- resume -----------------------------------------------------
    def test_resume_never_submits(self):
        result = self.run_script(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(c.startswith(("notarytool submit", "notarytool history")) for c in self.calls()))
        receipt = json.loads((self.out / "receipt.json").read_text())
        self.assertEqual(receipt["mode"], "resume")

    def test_resume_rejects_foreign_submission(self):
        result = self.run_script(mode="resume", submission_id=SUBMISSION, SHIM_INFO_NAME="someone-else.zip")
        self.assertEqual(result.returncode, 11)
        self.assertFalse(any(c.startswith("stapler staple") for c in self.calls()))

    def test_missing_secret_fails_after_verification_without_notary(self):
        result = self.run_script(MACOS_NOTARY_KEY_ID="")
        self.assertEqual(result.returncode, 7)
        self.assertIn("MACOS_NOTARY_KEY_ID", result.stderr)
        self.assert_no_credentials_used()


class WorkflowStaticTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import yaml  # development-time dependency only
        except ImportError:  # pragma: no cover
            raise unittest.SkipTest("PyYAML not installed")
        cls.text = WORKFLOW.read_text()
        cls.wf = yaml.safe_load(cls.text)
        cls.on = cls.wf.get(True, cls.wf.get("on"))

    def steps(self):
        return self.wf["jobs"]["notarize"]["steps"]

    def test_dispatch_only_with_expected_inputs(self):
        self.assertEqual(list(self.on.keys()), ["workflow_dispatch"])
        self.assertEqual(set(self.on["workflow_dispatch"]["inputs"]), {"artifact_sha256", "mode", "submission_id"})

    def test_permissions_minimal(self):
        self.assertEqual(self.wf["permissions"], {"contents": "read"})

    def test_guard_runs_first_and_checks_full_ref(self):
        first = self.steps()[0]
        self.assertIn("pre-checkout", first["name"])
        self.assertIn('refs/heads/${TRUSTED_DEFAULT_BRANCH}', first["run"])
        self.assertIn("workflow_dispatch", first["run"])

    def test_actions_pinned_to_full_shas(self):
        uses = [s["uses"] for s in self.steps() if "uses" in s]
        self.assertEqual(
            uses,
            [
                "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
                "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
            ],
        )

    def test_inputs_never_interpolated_into_run_blocks(self):
        for step in self.steps():
            self.assertNotRegex(step.get("run", ""), r"\$\{\{\s*(inputs|github\.event)\.")

    def test_upload_always_and_runner_labels(self):
        upload = self.steps()[-1]
        self.assertEqual(upload.get("if"), "always()")
        self.assertEqual(
            self.wf["jobs"]["notarize"]["runs-on"],
            ["self-hosted", "macOS", "ARM64", "studio", "studio-1", "ci-support"],
        )

    def test_pins_and_concurrency(self):
        env = self.wf["env"]
        self.assertEqual(env["EXPECTED_TEAM_ID"], "NXJ753WW5X")
        self.assertEqual(
            env["EXPECTED_LEAF_CERT_SHA256"], "ffe161226e3ba89575fbb2fa4e0ffc2f7b0a7c99de8fc2f696a24fb8d74f1baf"
        )
        self.assertFalse(self.wf["concurrency"]["cancel-in-progress"])

    def test_secrets_only_in_notarize_step(self):
        for step in self.steps():
            blob = json.dumps(step)
            if "secrets." in blob:
                self.assertEqual(step.get("id"), "notarize")


if __name__ == "__main__":
    unittest.main()
