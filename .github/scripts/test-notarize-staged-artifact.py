#!/usr/bin/env python3
"""Behavioral and static tests for notarize-staged-artifact.

The script's three phases (verify, submit, complete) are executed for real,
in sequence, against real zips, with only `codesign`, `xcrun` and `spctl`
replaced by PATH shims that record every call. Real `ditto`, `shasum` and
`python3` are used, so these tests run on macOS. Static workflow checks use
PyYAML when available (development dependency only).

Run: python3 .github/scripts/test-notarize-staged-artifact.py
"""

import hashlib
import json
import os
import stat
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "notarize-staged-artifact.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "notarize-staged-artifact.yml"

SECRET = "-----BEGIN PRIVATE KEY-----TOPSECRETKEYMATERIAL-----END PRIVATE KEY-----"
KEY_ID = "KEYID12345"
ISSUER = "11111111-2222-3333-4444-555555555555"
SUBMISSION = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
OTHER_ID = "99999999-8888-7777-6666-555555555555"
LEAF_CONTENT = b"fake-leaf-certificate-der"
LEAF_SHA = hashlib.sha256(LEAF_CONTENT).hexdigest()
CRED_VARS = {
    "MACOS_NOTARY_API_KEY": SECRET,
    "MACOS_NOTARY_ISSUER_ID": ISSUER,
    "MACOS_NOTARY_KEY_ID": KEY_ID,
}

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
receipt=0; [[ -f "$RUN_DIR/out/receipt.json" ]] && receipt=1
intent=0; compgen -G "$NOTARY_STATE_DIR/*/intent.json" >/dev/null && intent=1
senv=0; env | grep -q TOPSECRET && senv=1
echo "$tool $sub receipt=$receipt intent=$intent secret_env=$senv" >> "$SHIM_LOG"
if [[ "$tool" == "stapler" ]]; then
  app="${args[${#args[@]}-1]}"
  case "$sub" in
    validate) [[ -f "$app/.stapled" ]] && exit 0; echo "does not have a ticket" >&2; exit 65 ;;
    staple) touch "$app/.stapled"; exit 0 ;;
  esac
fi
id="${args[0]}"
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
    if [[ -n "${SHIM_SUBMIT_RC:-}" ]]; then echo "network error" >&2; exit "$SHIM_SUBMIT_RC"; fi
    default_submit='{"id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","message":"ok"}'
    printf '%s' "${SHIM_SUBMIT_JSON:-$default_submit}"
    exit 0 ;;
  wait)
    if [[ -n "${SHIM_WAIT_403:-}" ]]; then echo "HTTP status code: 403. Forbidden" >&2; exit 1; fi
    exit "${SHIM_WAIT_RC:-0}" ;;
  info)
    printf '{"id":"%s","name":"%s","status":"%s"}' "${SHIM_INFO_ID:-$id}" "${SHIM_INFO_NAME}" "${SHIM_STATUS:-Accepted}"
    exit 0 ;;
  log)
    [[ -n "${SHIM_LOG_RC:-}" ]] && { echo "boom" >&2; exit "$SHIM_LOG_RC"; }
    printf '{"jobId":"%s","status":"%s","sha256":"%s","issues":null}' \
      "${SHIM_LOG_JOB:-$id}" "${SHIM_STATUS:-Accepted}" "${SHIM_LOG_SHA:-$ARTIFACT_SHA256}" > "${args[${#args[@]}-1]}"
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


def _zip_entry(zf, name, data=b"", mode=0o100644):
    info = zipfile.ZipInfo(name)
    info.external_attr = mode << 16
    zf.writestr(info, data)


class Harness(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="notarize-test-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.shims = self.tmp / "shims"
        self.shims.mkdir()
        for name, body in (("codesign", CODESIGN_SHIM), ("xcrun", XCRUN_SHIM), ("spctl", SPCTL_SHIM)):
            path = self.shims / name
            path.write_text(body)
            path.chmod(0o755)
        self.state = self.tmp / "shimstate"
        self.state.mkdir()
        self.inbox = self.tmp / "inbox"
        self.inbox.mkdir()
        self.runner_temp = self.tmp / "runner-temp"
        self.runner_temp.mkdir()
        self.notary_state = self.tmp / "notary-state"
        self.run_counter = 0
        app = self.tmp / "src" / "Gents.app" / "Contents" / "MacOS"
        app.mkdir(parents=True)
        (app / "gents").write_text("not executed\n")
        zip_path = self.tmp / "candidate.zip"
        subprocess.run(
            ["ditto", "-c", "-k", "--keepParent", str(self.tmp / "src" / "Gents.app"), str(zip_path)],
            check=True,
        )
        self.sha = self.stage(zip_path.read_bytes())
        self.name = f"gents-notary-{self.sha[:16]}.zip"

    def stage(self, data):
        sha = hashlib.sha256(data).hexdigest()
        (self.inbox / f"{sha}.zip").write_bytes(data)
        return sha

    def stage_zip(self, build):
        path = self.tmp / f"adv-{len(list(self.tmp.glob('adv-*')))}.zip"
        with zipfile.ZipFile(path, "w") as zf:
            build(zf)
        return self.stage(path.read_bytes())

    def env(self, mode, submission_id, sha, phase, overrides):
        self.run_dir = self.runner_temp / f"notarize-{self.run_counter}-1"
        env = {
            "PATH": f"{self.shims}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.tmp),
            "RUNNER_TEMP": str(self.runner_temp),
            "RUN_DIR": str(self.run_dir),
            "ARTIFACT_SHA256": sha,
            "NOTARY_MODE": mode,
            "SUBMISSION_ID": submission_id,
            "NOTARY_INBOX_DIR": str(self.inbox),
            "NOTARY_STATE_DIR": str(self.notary_state),
            "EXPECTED_TEAM_ID": "NXJ753WW5X",
            "EXPECTED_BUNDLE_ID": "com.source-inc.gents",
            "EXPECTED_LEAF_CERT_SHA256": LEAF_SHA,
            "SHIM_LOG": str(self.state / "calls.log"),
            "SHIM_STATE": str(self.state),
            "SHIM_INFO_NAME": f"gents-notary-{sha[:16]}.zip",
            "GITHUB_RUN_ID": str(self.run_counter),
            "GITHUB_RUN_ATTEMPT": "1",
        }
        if phase != "verify":
            env.update(CRED_VARS)
        env.update(overrides)
        return env

    def run_phases(self, mode="submit", submission_id="", sha=None, phases=("verify", "submit", "complete"), **overrides):
        """Run phases like the workflow does; stop at the first failure."""
        self.run_counter += 1
        sha = sha if sha is not None else self.sha
        result = None
        for phase in phases:
            env = self.env(mode, submission_id, sha, phase, overrides)
            result = subprocess.run(["bash", str(SCRIPT), phase], env=env, capture_output=True, text=True)
            result.phase = phase
            self.assertNotIn("TOPSECRETKEYMATERIAL", result.stdout + result.stderr)
            self.assertNotIn(KEY_ID + ".p8", result.stdout + result.stderr)
            if result.returncode != 0:
                break
        return result

    def calls(self):
        log = self.state / "calls.log"
        return log.read_text().splitlines() if log.exists() else []

    def called(self, prefix):
        return [c for c in self.calls() if c.startswith(prefix)]

    def assert_no_credentials_used(self):
        self.assertEqual(self.called("notarytool"), [])
        self.assertFalse((self.state / "keypaths").exists())

    def assert_credentials_removed(self):
        keypaths = (self.state / "keypaths").read_text().split()
        self.assertTrue(keypaths)
        for path in keypaths:
            self.assertFalse(Path(path).parent.exists(), path)
        leftovers = [p.name for p in self.runner_temp.iterdir() if p.name.startswith("gents-notarize-cred")]
        self.assertEqual(leftovers, [])

    def state_files(self, sha=None):
        d = self.notary_state / (sha or self.sha)
        return sorted(p.name for p in d.iterdir()) if d.exists() else []


class HappyPathTests(Harness):
    def test_submit_success_end_to_end(self):
        result = self.run_phases()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.phase, "complete")
        sequence = [c.split(" receipt=")[0] for c in self.calls() if c.startswith(("notarytool", "stapler"))]
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
        submit = self.called("notarytool submit")
        self.assertEqual(len(submit), 1)
        self.assertIn("receipt=0 intent=1", submit[0])  # durable intent exists before submit
        self.assertIn("receipt=1", self.called("notarytool wait")[0])
        out = self.run_dir / "out"
        receipt = json.loads((out / "receipt.json").read_text())
        self.assertEqual(receipt["submission_id"], SUBMISSION)
        self.assertEqual(receipt["artifact_sha256"], self.sha)
        self.assertEqual(json.loads((out / "intent.json").read_text())["state"], "submitting")
        self.assertIn("intent.json", self.state_files())
        self.assertIn("receipt.json", self.state_files())
        self.assertIn(f"accepted-{SUBMISSION}.json", self.state_files())
        out_zip = out / f"Gents-{self.sha[:16]}-notarized.zip"
        self.assertIn(hashlib.sha256(out_zip.read_bytes()).hexdigest(), (out / "SHA256SUMS").read_text())
        self.assertEqual(hashlib.sha256((self.inbox / f"{self.sha}.zip").read_bytes()).hexdigest(), self.sha)
        self.assert_credentials_removed()

    def test_verify_phase_has_no_secrets(self):
        result = self.run_phases(phases=("verify",))
        self.assertEqual(result.returncode, 0, result.stderr)
        # The verify-phase stapler call ran without secrets in its environment.
        self.assertEqual([c for c in self.calls() if "secret_env=1" in c], [])
        self.assertTrue([c for c in self.calls() if c.startswith("stapler validate") and "secret_env=0" in c])
        self.assert_no_credentials_used()

    def test_verify_refuses_secrets_in_environment(self):
        result = self.run_phases(phases=("verify",), **CRED_VARS)
        self.assertEqual(result.returncode, 2)
        self.assertIn("without notary credentials", result.stderr)

    def test_run_directory_must_be_unique(self):
        self.run_counter -= 1  # reuse the same run dir on the next call
        self.run_phases(phases=("verify",))
        self.run_counter -= 1
        result = self.run_phases(phases=("verify",))
        self.assertEqual(result.returncode, 2)
        self.assertIn("already exists", result.stderr)

    def test_legitimate_internal_symlink_passes(self):
        def build(zf):
            _zip_entry(zf, "Gents.app/", mode=0o040755)
            _zip_entry(zf, "Gents.app/Contents/Frameworks/X.framework/Versions/A/X", b"bin")
            _zip_entry(zf, "Gents.app/Contents/Frameworks/X.framework/Versions/Current", b"A", mode=0o120755)

        sha = self.stage_zip(build)
        result = self.run_phases(sha=sha, phases=("verify",))
        self.assertEqual(result.returncode, 0, result.stderr)


class ArchiveContainmentTests(Harness):
    def assert_rejected(self, build):
        sha = self.stage_zip(build)
        result = self.run_phases(sha=sha)
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertEqual(result.phase, "verify")
        self.assertEqual(self.called("codesign"), [])
        self.assert_no_credentials_used()
        return result

    def test_traversal_entry(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "Gents.app/../evil")))

    def test_absolute_entry(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "/tmp/evil")))

    def test_symlinked_app_root(self):
        self.assert_rejected(lambda zf: _zip_entry(zf, "Gents.app", b"/Applications/Other.app", mode=0o120755))

    def test_escaping_internal_symlink(self):
        self.assert_rejected(
            lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "Gents.app/link", b"../../outside", mode=0o120755))
        )

    def test_absolute_internal_symlink(self):
        self.assert_rejected(
            lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "Gents.app/link", b"/etc", mode=0o120755))
        )

    def test_entry_beneath_symlink(self):
        self.assert_rejected(
            lambda zf: (
                _zip_entry(zf, "Gents.app/link", b"Contents", mode=0o120755),
                _zip_entry(zf, "Gents.app/link/file", b"x"),
            )
        )

    def test_two_apps(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "A.app/x"), _zip_entry(zf, "B.app/x")))

    def test_extra_top_level_file(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "README", b"hi")))

    def test_non_app_root(self):
        self.assert_rejected(lambda zf: _zip_entry(zf, "Gents/x"))

    def test_device_entry(self):
        self.assert_rejected(lambda zf: _zip_entry(zf, "Gents.app/dev", mode=0o020644))


class VerificationTests(Harness):
    def test_bad_sha_input_fails_before_credentials(self):
        result = self.run_phases(sha="ABC")
        self.assertEqual(result.returncode, 2)
        self.assert_no_credentials_used()

    def test_sha_mismatch_fails_before_credentials(self):
        (self.inbox / ("0" * 64 + ".zip")).write_bytes(b"different")
        result = self.run_phases(sha="0" * 64)
        self.assertEqual(result.returncode, 3)
        self.assertIn("mismatch", result.stderr)
        self.assert_no_credentials_used()

    def test_signature_failures_fail_before_credentials(self):
        for override in (
            {"SHIM_TEAM": "OTHERTEAM1"},
            {"SHIM_IDENT": "com.example.other"},
            {"SHIM_LEAF": "another-certificate"},
            {"SHIM_NO_RUNTIME": "1"},
            {"SHIM_VERIFY_RC": "1"},
        ):
            with self.subTest(override=override):
                result = self.run_phases(**override)
                self.assertEqual(result.returncode, 5, result.stderr)
                self.assertEqual(result.phase, "verify")
        self.assert_no_credentials_used()

    def test_already_stapled_submit_fails(self):
        staged = self.tmp / "stapled-src" / "Gents.app"
        staged.mkdir(parents=True)
        (staged / ".stapled").write_text("")
        zip_path = self.tmp / "stapled.zip"
        subprocess.run(["ditto", "-c", "-k", "--keepParent", str(staged), str(zip_path)], check=True)
        result = self.run_phases(sha=self.stage(zip_path.read_bytes()))
        self.assertEqual(result.returncode, 6)
        self.assert_no_credentials_used()

    def test_mode_input_validation(self):
        self.assertEqual(self.run_phases(mode="other").returncode, 2)
        self.assertEqual(self.run_phases(mode="submit", submission_id=SUBMISSION).returncode, 2)
        self.assertEqual(self.run_phases(mode="resume", submission_id="not-a-uuid").returncode, 2)
        self.assertEqual(self.run_phases(mode="resume", submission_id="").returncode, 2)
        self.assert_no_credentials_used()

    def test_missing_secret_fails_without_notary_calls(self):
        result = self.run_phases(MACOS_NOTARY_KEY_ID="")
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.phase, "submit")
        self.assertEqual(self.called("notarytool"), [])

    def test_submit_requires_completed_verify(self):
        result = self.run_phases(phases=("submit",))
        self.assertEqual(result.returncode, 3)
        self.assert_no_credentials_used()


class DuplicateGuardTests(Harness):
    def test_existing_state_blocks_submit_before_history(self):
        self.assertEqual(self.run_phases().returncode, 0)
        before = len(self.called("notarytool"))
        result = self.run_phases()
        self.assertEqual(result.returncode, 9)
        self.assertIn(SUBMISSION, result.stderr)
        self.assertIn("mode=resume", result.stderr)
        new_calls = self.called("notarytool")[before:]
        self.assertEqual(new_calls, [])  # refused before any notary call, incl. history
        self.assertEqual(len(self.called("notarytool submit")), 1)

    def test_duplicate_history_entry_blocks_submit(self):
        history = json.dumps(
            {"history": [{"id": SUBMISSION, "name": self.name, "status": "In Progress", "createdDate": "2026-10-20"}]}
        )
        result = self.run_phases(SHIM_HISTORY_JSON=history)
        self.assertEqual(result.returncode, 9)
        self.assertIn(SUBMISSION, result.stderr)
        self.assertEqual(self.called("notarytool submit"), [])
        self.assertEqual(self.state_files(), [])
        self.assert_credentials_removed()

    def test_other_history_names_do_not_block(self):
        history = json.dumps({"history": [{"id": OTHER_ID, "name": "unrelated.zip", "status": "Accepted"}]})
        self.assertEqual(self.run_phases(SHIM_HISTORY_JSON=history).returncode, 0)

    def test_incomplete_history_schema_fails_closed(self):
        for history in (
            "not json",
            json.dumps({"message": "no history key"}),
            json.dumps({"history": [{"id": OTHER_ID, "status": "Accepted"}]}),  # missing name
            json.dumps({"history": ["string-entry"]}),
        ):
            with self.subTest(history=history):
                self.notary_state = self.tmp / f"state-{abs(hash(history))}"
                result = self.run_phases(SHIM_HISTORY_JSON=history)
                self.assertEqual(result.returncode, 8)
        self.assertEqual(self.called("notarytool submit"), [])

    def test_history_403_neutral_diagnostic_no_submit(self):
        result = self.run_phases(SHIM_HISTORY_RC="1")
        self.assertEqual(result.returncode, 20)
        self.assertIn("HTTP 403", result.stderr)
        self.assertNotIn("agreement", result.stderr.lower())
        self.assertEqual(self.called("notarytool submit"), [])
        self.assert_credentials_removed()

    def test_submit_nonzero_records_uncertain_and_blocks_retry(self):
        result = self.run_phases(SHIM_SUBMIT_RC="1")
        self.assertEqual(result.returncode, 21)
        self.assertIn("check notary history", result.stderr)
        self.assertEqual(self.state_files(), ["intent.json", "outcome.json"])
        outcome = json.loads((self.notary_state / self.sha / "outcome.json").read_text())
        self.assertEqual(outcome["state"], "uncertain")
        self.assertTrue((self.run_dir / "out" / "intent.json").exists())
        # A later submit attempt is refused by the persisted uncertain intent.
        retry = self.run_phases()
        self.assertEqual(retry.returncode, 9)
        self.assertEqual(len(self.called("notarytool submit")), 1)

    def test_submit_without_id_records_uncertain(self):
        result = self.run_phases(SHIM_SUBMIT_JSON='{"message":"odd"}')
        self.assertEqual(result.returncode, 10)
        self.assertIn("check notary history", result.stderr)
        self.assertIn("outcome.json", self.state_files())
        self.assertEqual(self.called("notarytool wait"), [])


class CompletionTests(Harness):
    def test_invalid_status_fails_and_keeps_log(self):
        result = self.run_phases(SHIM_STATUS="Invalid", SHIM_WAIT_RC="1")
        self.assertEqual(result.returncode, 12)
        self.assertTrue((self.run_dir / "out" / "notary-log.json").exists())
        self.assertTrue((self.run_dir / "out" / "receipt.json").exists())
        self.assertEqual(self.called("stapler staple"), [])
        self.assert_credentials_removed()

    def test_in_progress_points_to_resume(self):
        result = self.run_phases(SHIM_STATUS="In Progress", SHIM_WAIT_RC="1")
        self.assertEqual(result.returncode, 12)
        self.assertIn("mode=resume", result.stderr)

    def test_wait_403_neutral_diagnostic(self):
        result = self.run_phases(SHIM_WAIT_403="1")
        self.assertEqual(result.returncode, 20)
        self.assertIn("do not submit again", result.stderr)
        self.assertEqual(self.called("stapler staple"), [])

    def test_log_unavailable_fails_closed(self):
        result = self.run_phases(SHIM_LOG_RC="1")
        self.assertEqual(result.returncode, 21)
        self.assertEqual(self.called("stapler staple"), [])

    def test_log_job_id_mismatch_fails_closed(self):
        result = self.run_phases(SHIM_LOG_JOB=OTHER_ID)
        self.assertEqual(result.returncode, 12)
        self.assertEqual(self.called("stapler staple"), [])

    def test_log_sha_mismatch_fails_closed(self):
        result = self.run_phases(SHIM_LOG_SHA="f" * 64)
        self.assertEqual(result.returncode, 12)
        self.assertIn("does not bind", result.stderr)
        self.assertEqual(self.called("stapler staple"), [])

    def test_info_id_mismatch_fails_closed(self):
        result = self.run_phases(SHIM_INFO_ID=OTHER_ID)
        self.assertEqual(result.returncode, 11)
        self.assertEqual(self.called("stapler staple"), [])


class ResumeTests(Harness):
    def test_resume_never_submits(self):
        result = self.run_phases(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.called("notarytool submit"), [])
        self.assertEqual(self.called("notarytool history"), [])
        receipt = json.loads((self.run_dir / "out" / "receipt.json").read_text())
        self.assertEqual(receipt["mode"], "resume")

    def test_resume_identity_checked_before_wait(self):
        result = self.run_phases(mode="resume", submission_id=SUBMISSION, SHIM_INFO_NAME="someone-else.zip")
        self.assertEqual(result.returncode, 11)
        self.assertEqual(result.phase, "submit")
        self.assertEqual(self.called("notarytool wait"), [])

    def test_resume_rejects_id_differing_from_recorded_receipt(self):
        self.assertEqual(self.run_phases().returncode, 0)
        result = self.run_phases(mode="resume", submission_id=OTHER_ID)
        self.assertEqual(result.returncode, 11)
        self.assertEqual(self.called("notarytool wait")[1:], [])

    def test_resume_after_uncertain_submit_reconciles(self):
        self.assertEqual(self.run_phases(SHIM_SUBMIT_RC="1").returncode, 21)
        result = self.run_phases(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.called("notarytool submit")), 1)


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

    def step(self, step_id):
        return next(s for s in self.steps() if s.get("id") == step_id)

    def test_dispatch_only_with_expected_inputs(self):
        self.assertEqual(list(self.on.keys()), ["workflow_dispatch"])
        self.assertEqual(set(self.on["workflow_dispatch"]["inputs"]), {"artifact_sha256", "mode", "submission_id"})

    def test_permissions_minimal(self):
        self.assertEqual(self.wf["permissions"], {"contents": "read"})

    def test_guard_runs_first_and_checks_full_ref(self):
        first = self.steps()[0]
        self.assertIn("pre-checkout", first["name"])
        self.assertIn("refs/heads/${TRUSTED_DEFAULT_BRANCH}", first["run"])

    def test_actions_pinned_to_full_shas(self):
        uses = [s["uses"] for s in self.steps() if "uses" in s]
        self.assertEqual(uses[0], "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1")
        self.assertEqual(set(uses[1:]), {"actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"})

    def test_inputs_never_interpolated_into_run_blocks(self):
        for step in self.steps():
            self.assertNotRegex(step.get("run", ""), r"\$\{\{")

    def test_phase_order_and_receipt_upload_before_wait(self):
        names = [s.get("id") or s.get("name") for s in self.steps()]
        verify, submit, complete = names.index("verify"), names.index("submit"), names.index("complete")
        receipt_upload = names.index("Upload submission receipt")
        self.assertLess(verify, submit)
        self.assertLess(submit, receipt_upload)
        self.assertLess(receipt_upload, complete)
        self.assertIn("always()", self.steps()[receipt_upload]["if"])

    def test_secrets_only_in_credentialed_steps(self):
        for step in self.steps():
            if "secrets." in json.dumps(step):
                self.assertIn(step.get("id"), ("submit", "complete"))
        self.assertNotIn("secrets.", json.dumps(self.step("verify")))
        self.assertNotIn("secrets.", json.dumps(self.wf.get("env", {})))

    def test_unique_run_directory(self):
        for step_id in ("verify", "submit", "complete"):
            self.assertEqual(
                self.step(step_id)["env"]["RUN_DIR"],
                "${{ runner.temp }}/notarize-${{ github.run_id }}-${{ github.run_attempt }}",
            )

    def test_cleanup_and_labels_and_pins(self):
        self.assertEqual(self.steps()[-1].get("if"), "always()")
        self.assertIn('rm -rf "${RUN_DIR}"', self.steps()[-1]["run"])
        self.assertEqual(
            self.wf["jobs"]["notarize"]["runs-on"],
            ["self-hosted", "macOS", "ARM64", "studio", "studio-1", "ci-support"],
        )
        env = self.wf["env"]
        self.assertEqual(env["EXPECTED_TEAM_ID"], "NXJ753WW5X")
        self.assertEqual(env["NOTARY_STATE_DIR"], "/Users/admin/gents-notary-state")
        self.assertEqual(
            env["EXPECTED_LEAF_CERT_SHA256"], "ffe161226e3ba89575fbb2fa4e0ffc2f7b0a7c99de8fc2f696a24fb8d74f1baf"
        )
        self.assertFalse(self.wf["concurrency"]["cancel-in-progress"])


if __name__ == "__main__":
    unittest.main()
