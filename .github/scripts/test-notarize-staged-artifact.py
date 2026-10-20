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
import importlib.util
import json
import os
import stat
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "notarize-staged-artifact.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "notarize-staged-artifact.yml"
HELPER = ROOT / ".github" / "scripts" / "notary_json.py"

_spec = importlib.util.spec_from_file_location("notary_json", HELPER)
nj = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nj)

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
    echo "Format=${SHIM_FORMAT:-Mach-O thin (arm64)}" >&2
    echo "CDHash=${SHIM_CDHASH-595a5f783971fe0d9271b8357395d52114f51438}" >&2
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
if [[ -n "${SHIM_LOCK_STATE_ON:-}" && "$sub" == "$SHIM_LOCK_STATE_ON" ]]; then
  chmod 0500 "$NOTARY_STATE_DIR/$ARTIFACT_SHA256"
fi
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
    tickets='[{"path":"gents","digestAlgorithm":"SHA-256","cdhash":"'"${SHIM_TICKET_CDHASH:-595a5f783971fe0d9271b8357395d52114f51438}"'","arch":"arm64"}]'
    [[ -n "${SHIM_TICKET_NONE:-}" ]] && tickets='[]'
    printf '{"jobId":"%s","status":"%s","sha256":"%s","issues":null,"ticketContents":%s}' \
      "${SHIM_LOG_JOB:-$id}" "${SHIM_STATUS:-Accepted}" "${SHIM_LOG_SHA:-$ARTIFACT_SHA256}" "$tickets" > "${args[${#args[@]}-1]}"
    exit 0 ;;
esac
exit 97
"""

LIPO_SHIM = r"""#!/bin/bash
# lipo -archs <file>: Mach-O fixtures start with the 64-bit magic cffaedfe.
file="${@: -1}"
[[ "$(head -c 4 "$file" | od -An -tx1 | tr -d ' \n')" == "cffaedfe" ]] || { echo "not a Mach-O" >&2; exit 1; }
if [[ -n "${SHIM_ARCHS:-}" ]]; then echo "$SHIM_ARCHS"; else echo arm64; fi
"""

CP_SHIM = r"""#!/bin/bash
# Pass through to /bin/cp; optionally corrupt the CLI output copy.
/bin/cp "$@" || exit $?
dest="${@: -1}"
if [[ -n "${SHIM_CORRUPT_COPY:-}" && "$dest" == */out/gents ]]; then
  chmod u+w "$dest"; printf 'corrupted' >> "$dest"
fi
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
        for name, body in (
            ("codesign", CODESIGN_SHIM), ("xcrun", XCRUN_SHIM), ("spctl", SPCTL_SHIM),
            ("lipo", LIPO_SHIM), ("cp", CP_SHIM),
        ):
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


class AppArchitectureTests(Harness):
    def stage_app_with_macho(self):
        def build(zf):
            _zip_entry(zf, "Gents.app/", mode=0o040755)
            _zip_entry(zf, "Gents.app/Contents/MacOS/bridge_runner", b"\xcf\xfa\xed\xfe macho", mode=0o100755)
            _zip_entry(zf, "Gents.app/Contents/Resources/readme.txt", b"text")

        return self.stage_zip(build)

    def test_thin_arm64_app_passes(self):
        result = self.run_phases(sha=self.stage_app_with_macho(), phases=("verify",))
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_fat_mach_o_inside_app_rejected(self):
        result = self.run_phases(sha=self.stage_app_with_macho(), SHIM_ARCHS="x86_64 arm64")
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assertIn("bridge_runner", result.stderr)
        self.assert_no_credentials_used()


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

    # --- filesystem aliases (runner volume is case/normalization-insensitive)
    def test_case_alias_beneath_symlink(self):
        self.assert_rejected(
            lambda zf: (
                _zip_entry(zf, "Gents.app/Contents/x", b"x"),
                _zip_entry(zf, "Gents.app/Link", b"Contents", mode=0o120755),
                _zip_entry(zf, "Gents.app/link/escape", b"x"),
            )
        )

    def test_unicode_alias_beneath_symlink(self):
        self.assert_rejected(
            lambda zf: (
                _zip_entry(zf, "Gents.app/Contents/x", b"x"),
                _zip_entry(zf, "Gents.app/caf\u00e9", b"Contents", mode=0o120755),
                _zip_entry(zf, "Gents.app/cafe\u0301/escape", b"x"),
            )
        )

    def test_case_alias_duplicate_names(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "Gents.app/A", b"1"), _zip_entry(zf, "Gents.app/a", b"2")))

    def test_unicode_alias_duplicate_names(self):
        self.assert_rejected(
            lambda zf: (_zip_entry(zf, "Gents.app/caf\u00e9", b"1"), _zip_entry(zf, "Gents.app/cafe\u0301", b"2"))
        )

    def test_case_alias_top_level(self):
        self.assert_rejected(lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "gents.APP/y")))

    def test_chained_symlinks_escape(self):
        # Lexically l2 stays inside the root; physically it climbs through l1.
        self.assert_rejected(
            lambda zf: (
                _zip_entry(zf, "Gents.app/x"),
                _zip_entry(zf, "Gents.app/l1", b"..", mode=0o120755),
                _zip_entry(zf, "Gents.app/l2", b"l1/x/../../..", mode=0o120755),
            )
        )

    def test_dotdot_symlink_inside_root_rejected_conservatively(self):
        self.assert_rejected(
            lambda zf: (
                _zip_entry(zf, "Gents.app/Contents/A/f", b"x"),
                _zip_entry(zf, "Gents.app/Contents/B/link", b"../A/f", mode=0o120755),
            )
        )

    def test_dot_component_symlink_rejected(self):
        self.assert_rejected(
            lambda zf: (_zip_entry(zf, "Gents.app/x"), _zip_entry(zf, "Gents.app/link", b"./x", mode=0o120755))
        )

    def test_chained_descending_symlinks_pass(self):
        def build(zf):
            _zip_entry(zf, "Gents.app/", mode=0o040755)
            _zip_entry(zf, "Gents.app/Contents/Frameworks/X.framework/Versions/A/X", b"bin")
            _zip_entry(zf, "Gents.app/Contents/Frameworks/X.framework/Versions/Current", b"A", mode=0o120755)
            _zip_entry(zf, "Gents.app/Contents/Frameworks/X.framework/X", b"Versions/Current/X", mode=0o120755)

        sha = self.stage_zip(build)
        self.assertEqual(self.run_phases(sha=sha, phases=("verify",)).returncode, 0)


class FilesystemEvidenceTests(unittest.TestCase):
    """Documents the property the alias checks defend against, on this host."""

    def test_host_volume_aliases_names(self):
        with tempfile.TemporaryDirectory() as directory:
            os.mkdir(os.path.join(directory, "Link"))
            if not os.path.exists(os.path.join(directory, "link")):
                self.skipTest("case-sensitive volume")
            Path(directory, "caf\u00e9").write_text("")
            self.assertTrue(os.path.exists(os.path.join(directory, "cafe\u0301")))
            self.assertEqual(nj.alias_key("Gents.app/Link"), nj.alias_key("gents.APP/link"))
            self.assertEqual(nj.alias_key("caf\u00e9"), nj.alias_key("CAFE\u0301"))


class DurableStateUnitTests(unittest.TestCase):
    SHA = "a" * 64

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="state-unit-"))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.tmp)])
        self.root = self.tmp / "state"

    def test_creation_order_flushes_namespace_before_intent_visible(self):
        events = []
        real_sync, real_link, real_mkdir = nj._sync_dir, os.link, os.mkdir

        def sync(path):
            events.append(("sync", os.path.basename(path)))
            real_sync(path)

        def link(src, dst):
            events.append(("link", os.path.basename(dst)))
            real_link(src, dst)

        def mkdir(path, mode=0o777):
            events.append(("mkdir", os.path.basename(path)))
            real_mkdir(path, mode)

        with mock.patch.object(nj, "_sync_dir", sync), mock.patch.object(nj.os, "link", link), \
                mock.patch.object(nj.os, "mkdir", mkdir):
            nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        self.assertEqual(
            events,
            [
                ("mkdir", "state"), ("sync", self.tmp.name), ("sync", "state"),
                ("mkdir", self.SHA), ("sync", "state"),
                ("link", "intent.json"), ("sync", self.SHA),
            ],
        )
        record = json.loads((self.root / self.SHA / "intent.json").read_text())
        self.assertEqual(record["state"], "submitting")
        self.assertEqual(sorted(os.listdir(self.root / self.SHA)), ["intent.json"])

    def test_file_flush_failure_never_publishes_intent(self):
        real = nj._full_sync

        def fail_on_files(fd):
            if stat.S_ISREG(os.fstat(fd).st_mode):
                raise OSError(5, "EIO")
            real(fd)

        with mock.patch.object(nj, "_full_sync", fail_on_files):
            with self.assertRaises(OSError):
                nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        self.assertEqual(os.listdir(self.root / self.SHA), [])  # no intent, no temp left

    def test_link_failure_never_publishes_intent(self):
        with mock.patch.object(nj.os, "link", side_effect=OSError(28, "ENOSPC")):
            with self.assertRaises(OSError):
                nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        self.assertEqual(os.listdir(self.root / self.SHA), [])

    def test_directory_flush_failure_is_an_error(self):
        real = nj._sync_dir
        calls = []

        def flaky(path):
            calls.append(path)
            if os.path.basename(path) == self.SHA:
                raise OSError(5, "EIO")
            real(path)

        with mock.patch.object(nj, "_sync_dir", flaky):
            with self.assertRaises(OSError):
                nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")

    def test_root_flush_failure_is_an_error(self):
        with mock.patch.object(nj, "_sync_dir", side_effect=OSError(5, "EIO")):
            with self.assertRaises(OSError):
                nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")

    def test_existing_state_refused_and_records_never_overwritten(self):
        nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        with self.assertRaises(nj.Refused):
            nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        state = str(self.root / self.SHA)
        nj.state_write(state, "receipt.json", "submission_id", "x")
        with self.assertRaises(FileExistsError):
            nj.state_write(state, "receipt.json", "submission_id", "y")
        self.assertEqual(json.loads(Path(state, "receipt.json").read_text())["submission_id"], "x")
        self.assertFalse([n for n in os.listdir(state) if n.startswith(".")])

    def test_symlinked_root_or_state_refused(self):
        real = self.tmp / "elsewhere"
        real.mkdir()
        os.symlink(real, self.root)
        with self.assertRaises(nj.Refused):
            nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")

    def test_full_fsync_used(self):
        with mock.patch.object(nj.fcntl, "fcntl") as fcntl_call:
            nj.state_create_intent(str(self.root), self.SHA, "state", "submitting")
        self.assertTrue(fcntl_call.call_args_list)
        self.assertTrue(all(c.args[1] == nj.fcntl.F_FULLFSYNC for c in fcntl_call.call_args_list))


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

    def unlock_state_on_cleanup(self):
        self.addCleanup(subprocess.run, ["chmod", "-R", "u+w", str(self.tmp)])

    def test_intent_persistence_failure_refuses_before_submit(self):
        locked = self.tmp / "locked"
        locked.mkdir()
        locked.chmod(0o500)
        self.unlock_state_on_cleanup()
        self.notary_state = locked / "state"
        result = self.run_phases()
        self.assertEqual(result.returncode, 15, result.stderr)
        self.assertIn("refusing to submit", result.stderr)
        self.assertEqual(self.called("notarytool submit"), [])
        self.assert_credentials_removed()

    def test_receipt_persistence_failure_after_submit(self):
        self.unlock_state_on_cleanup()
        result = self.run_phases(SHIM_LOCK_STATE_ON="submit")
        self.assertEqual(result.returncode, 16, result.stderr)
        self.assertIn(SUBMISSION, result.stderr)
        self.assertIn("mode=resume", result.stderr)
        self.assertIn(f"submission_id={SUBMISSION}", result.stdout)  # announced before persisting
        self.assertTrue((self.run_dir / "out" / "receipt.json").exists())
        self.assertEqual(self.called("notarytool wait"), [])
        (self.notary_state / self.sha).chmod(0o700)
        self.assertEqual(self.run_phases().returncode, 9)  # intent still blocks resubmission
        resumed = self.run_phases(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertEqual(len(self.called("notarytool submit")), 1)
        self.assertIn("receipt.json", self.state_files())

    def test_acceptance_persistence_failure_fails_closed(self):
        self.unlock_state_on_cleanup()
        result = self.run_phases(SHIM_LOCK_STATE_ON="log")
        self.assertEqual(result.returncode, 16, result.stderr)
        self.assertIn("mode=resume", result.stderr)

    def test_existing_record_values_are_sanitized(self):
        state = self.notary_state / self.sha
        state.mkdir(parents=True)
        (state / "receipt.json").write_text(json.dumps({"submission_id": "x\u001b[31mevil", "state": "ok"}))
        result = self.run_phases()
        self.assertEqual(result.returncode, 9)
        self.assertNotIn("\u001b", result.stderr)
        self.assertIn("submission_id=<redacted>", result.stderr)

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

    def test_resume_without_state_persists_host_guard(self):
        result = self.run_phases(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((self.notary_state / self.sha / "receipt.json").read_text())
        self.assertEqual(receipt["state"], "resumed")
        self.assertEqual(receipt["submission_id"], SUBMISSION)
        self.assertIn(f"accepted-{SUBMISSION}.json", self.state_files())
        self.assertEqual(self.run_phases().returncode, 9)  # later submit is blocked
        self.assertEqual(self.called("notarytool submit"), [])

    def test_resume_accepts_uppercase_id(self):
        result = self.run_phases(mode="resume", submission_id=SUBMISSION.upper())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.run_dir / "out" / "receipt.json").read_text())["submission_id"], SUBMISSION)

    def test_resume_after_uncertain_submit_reconciles(self):
        self.assertEqual(self.run_phases(SHIM_SUBMIT_RC="1").returncode, 21)
        result = self.run_phases(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.called("notarytool submit")), 1)


CLI_CDHASH = "595a5f783971fe0d9271b8357395d52114f51438"


class CliTests(Harness):
    CLI = {"ARTIFACT_KIND": "cli", "SHIM_IDENT": "gents", "EXPECTED_CLI_NAME": "gents", "EXPECTED_CLI_IDENTIFIER": "gents"}

    def setUp(self):
        super().setUp()
        self.binary = b"\xcf\xfa\xed\xfe fake signed mach-o, never executed\n"
        src = self.tmp / "clisrc"
        src.mkdir()
        (src / "gents").write_bytes(self.binary)
        (src / "gents").chmod(0o755)
        zip_path = self.tmp / "cli.zip"
        subprocess.run(["ditto", "-c", "-k", str(src / "gents"), str(zip_path)], check=True)
        with zipfile.ZipFile(zip_path) as zf:
            self.assertEqual(zf.namelist(), ["gents"])  # ditto layout the runbook relies on
        self.cli_sha = self.stage(zip_path.read_bytes())

    def run_cli(self, sha=None, **overrides):
        env = dict(self.CLI)
        env.update(overrides)
        return self.run_phases(sha=sha or self.cli_sha, **env)

    def test_cli_submit_success_without_stapling(self):
        result = self.run_cli()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.called("stapler"), [])
        self.assertEqual(self.called("spctl"), [])
        self.assertEqual(len(self.called("notarytool submit")), 1)
        out = self.run_dir / "out"
        self.assertEqual((out / "gents").read_bytes(), self.binary)  # byte-for-byte unchanged
        self.assertEqual(
            (out / "SHA256SUMS").read_text().split()[0], hashlib.sha256(self.binary).hexdigest()
        )
        self.assertIn(f"accepted-{SUBMISSION}.json", self.state_files(self.cli_sha))
        self.assertIn(CLI_CDHASH, result.stdout)
        verified = json.loads((self.run_dir / "verified.json").read_text())
        self.assertEqual(verified["artifact_kind"], "cli")
        self.assertEqual(verified["cdhash"], CLI_CDHASH)

    def test_cli_ticket_not_covering_cdhash_fails_closed(self):
        result = self.run_cli(SHIM_TICKET_CDHASH="f" * 40)
        self.assertEqual(result.returncode, 12)
        self.assertIn("does not cover", result.stderr)
        self.assertFalse((self.run_dir / "out" / "gents").exists())

    def test_cli_missing_ticket_contents_fails_closed(self):
        result = self.run_cli(SHIM_TICKET_NONE="1")
        self.assertEqual(result.returncode, 12)
        self.assertFalse((self.run_dir / "out" / "gents").exists())

    def test_cli_resume_never_submits(self):
        result = self.run_cli(mode="resume", submission_id=SUBMISSION)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.called("notarytool submit"), [])

    def test_cli_signature_failures_before_credentials(self):
        for override in (
            {"SHIM_IDENT": "com.source-inc.gents"},
            {"SHIM_FORMAT": "app bundle with Mach-O thin (arm64)"},
            {"SHIM_CDHASH": ""},
            {"SHIM_CDHASH": "XYZ"},
            {"SHIM_TEAM": "OTHERTEAM1"},
            {"SHIM_NO_RUNTIME": "1"},
        ):
            with self.subTest(override=override):
                result = self.run_cli(**override)
                self.assertEqual(result.returncode, 5, result.stderr)
                self.assertEqual(result.phase, "verify")
        self.assert_no_credentials_used()

    def test_cli_fat_binary_rejected_before_credentials(self):
        result = self.run_cli(SHIM_ARCHS="x86_64 arm64")
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assertIn("thin arm64", result.stderr)
        self.assert_no_credentials_used()

    def test_cli_universal_format_rejected(self):
        result = self.run_cli(SHIM_FORMAT="Mach-O universal (x86_64 arm64)")
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assert_no_credentials_used()

    def test_cli_not_mach_o_rejected(self):
        src = self.tmp / "notmacho"
        src.mkdir()
        (src / "gents").write_bytes(b"#!/bin/sh\necho hi\n")
        zip_path = self.tmp / "notmacho.zip"
        subprocess.run(["ditto", "-c", "-k", str(src / "gents"), str(zip_path)], check=True)
        result = self.run_cli(sha=self.stage(zip_path.read_bytes()))
        self.assertEqual(result.returncode, 5, result.stderr)
        self.assert_no_credentials_used()

    def test_cli_output_copy_mismatch_fails(self):
        result = self.run_cli(SHIM_CORRUPT_COPY="1")
        self.assertEqual(result.returncode, 14, result.stderr)
        self.assertIn("verified SHA-256", result.stderr)

    def assert_cli_rejected(self, build):
        sha = self.stage_zip(build)
        result = self.run_cli(sha=sha)
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assert_no_credentials_used()

    def test_cli_archive_two_entries(self):
        self.assert_cli_rejected(lambda zf: (_zip_entry(zf, "gents", b"x"), _zip_entry(zf, "extra", b"y")))

    def test_cli_archive_wrong_name(self):
        self.assert_cli_rejected(lambda zf: _zip_entry(zf, "other", b"x"))

    def test_cli_archive_nested_path(self):
        self.assert_cli_rejected(lambda zf: _zip_entry(zf, "dir/gents", b"x"))

    def test_cli_archive_symlink(self):
        self.assert_cli_rejected(lambda zf: _zip_entry(zf, "gents", b"/bin/sh", mode=0o120755))

    def test_cli_archive_directory(self):
        self.assert_cli_rejected(lambda zf: _zip_entry(zf, "gents/", mode=0o040755))

    def test_app_zip_rejected_as_cli(self):
        result = self.run_phases(**self.CLI)  # the default .app fixture
        self.assertEqual(result.returncode, 4)
        self.assert_no_credentials_used()

    def test_invalid_kind(self):
        self.assertEqual(self.run_cli(ARTIFACT_KIND="dmg").returncode, 2)

    def test_kind_mismatch_between_phases(self):
        self.assertEqual(self.run_cli(phases=("verify",)).returncode, 0)
        self.run_counter -= 1  # same run directory, as later steps of one run
        env_app = dict(self.CLI, ARTIFACT_KIND="app")
        result = self.run_phases(sha=self.cli_sha, phases=("submit",), **env_app)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(self.called("notarytool"), [])


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
        inputs = self.on["workflow_dispatch"]["inputs"]
        self.assertEqual(set(inputs), {"artifact_sha256", "artifact_kind", "mode", "submission_id"})
        self.assertEqual(inputs["artifact_kind"]["options"], ["app", "cli"])
        self.assertEqual(inputs["artifact_kind"]["default"], "app")

    def test_artifact_kind_passed_to_every_phase(self):
        for step_id in ("verify", "submit", "complete"):
            self.assertEqual(self.step(step_id)["env"]["ARTIFACT_KIND"], "${{ inputs.artifact_kind }}")
        self.assertEqual(self.wf["env"]["EXPECTED_CLI_NAME"], "gents")
        self.assertEqual(self.wf["env"]["EXPECTED_CLI_IDENTIFIER"], "gents")

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
