#!/usr/bin/env python3
"""Tests for publish_staged_prerelease.py and its workflow.

All GitHub interaction goes through a fake `gh` (FakeGh) so these tests never
touch the network. Run: python3 .github/scripts/test-publish-staged-prerelease.py
(static workflow checks need PyYAML and are skipped without it).
"""

import hashlib
import importlib.util
import io
import json
import os
import shutil
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github" / "scripts" / "publish_staged_prerelease.py"
WORKFLOW = ROOT / ".github" / "workflows" / "publish-staged-prerelease.yml"

spec = importlib.util.spec_from_file_location("publish_staged_prerelease", SCRIPT)
pub = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pub)

REPO = "gents-ai/gents"
TARGET = "039e2299879565d602eaaa889908e16950015f06"
TAG = "v0.21.0-rc1"
APP_RUN, CLI_RUN = "111", "222"
APP_ID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
CLI_ID = "11111111-2222-3333-4444-555555555555"
APP_STAGED = "c" * 64
CLI_STAGED = "d" * 64
APP_ZIP = b"PK stapled notarized app zip bytes"
CLI_BIN = b"\xcf\xfa\xed\xfe signed cli bytes"
LICENSE = b"MIT License text\n"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def outputs_zip(files):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as zf:
        for name, data in files.items():
            zf.writestr(name, data)
    return buffer.getvalue()


def app_outputs(**over):
    name = "Gents-" + APP_STAGED[:16] + "-notarized.zip"
    files = {
        "receipt.json": json.dumps({"submission_id": APP_ID, "artifact_sha256": APP_STAGED}),
        "notary-log.json": json.dumps({"jobId": APP_ID, "status": "Accepted", "sha256": APP_STAGED}),
        "notary-info.json": "{}",
        name: APP_ZIP,
        "SHA256SUMS": "{}  {}\n".format(sha(APP_ZIP), name),
    }
    files.update(over)
    return {k: v for k, v in files.items() if v is not None}


def cli_outputs(**over):
    files = {
        "receipt.json": json.dumps({"submission_id": CLI_ID, "artifact_sha256": CLI_STAGED}),
        "notary-log.json": json.dumps({"jobId": CLI_ID, "status": "Accepted", "sha256": CLI_STAGED}),
        "gents": CLI_BIN,
        "SHA256SUMS": "{}  gents\n".format(sha(CLI_BIN)),
    }
    files.update(over)
    return {k: v for k, v in files.items() if v is not None}


class FakeGh(pub.Gh):
    def __init__(self):
        super().__init__(REPO)
        self.tags = {}
        self.releases = []
        self.calls = []
        self.push_runs = 0
        self.tamper_digest = None
        self.extra_asset = False
        self.wrong_target = False
        self.create_fails = False
        self.runs = {
            APP_RUN: self.run_record(APP_RUN),
            CLI_RUN: self.run_record(CLI_RUN),
        }
        self.artifacts = {
            APP_RUN: [{"id": 9001, "name": "notarize-staged-111-1", "expired": False}],
            CLI_RUN: [{"id": 9002, "name": "notarize-staged-222-1", "expired": False}],
        }
        self.blobs = {9001: outputs_zip(app_outputs()), 9002: outputs_zip(cli_outputs())}

    @staticmethod
    def run_record(run_id):
        return {
            "id": int(run_id),
            "path": ".github/workflows/notarize-staged-artifact.yml",
            "event": "workflow_dispatch",
            "head_branch": "main",
            "status": "completed",
            "conclusion": "success",
            "run_attempt": 1,
            "repository": {"full_name": REPO},
        }

    def run(self, args, stdin=None):
        self.calls.append(("run", tuple(args)))
        if args[:2] != ["release", "create"] or self.create_fails:
            return 1, b"", b"failed"
        option_values = {args[i + 1] for i, a in enumerate(args) if a in ("--notes-file", "--target", "--title", "--repo")}
        files = [a for a in args[3:] if not a.startswith("-") and a not in option_values]
        assets = [
            {"name": os.path.basename(f), "digest": "sha256:" + pub.sha256_file(f)} for f in files
        ]
        if self.tamper_digest:
            assets[0]["digest"] = "sha256:" + "0" * 64
        if self.extra_asset:
            assets.append({"name": "surprise.bin", "digest": "sha256:" + "1" * 64})
        target = args[args.index("--target") + 1]
        self.releases.append(
            {
                "id": 77,
                "tag_name": args[2],
                "draft": "--draft" in args,
                "prerelease": "--prerelease" in args,
                "target_commitish": "main" if self.wrong_target else target,
                "assets": assets,
                "html_url": "https://github.com/{}/releases/tag/{}".format(REPO, args[2]),
            }
        )
        return 0, b"", b""

    def api(self, path, method="GET", body=None, accept=None, allow_404=False):
        self.calls.append((method, path))
        base = "repos/{}/".format(REPO)
        assert path.startswith(base), path
        rest = path[len(base):]

        def missing():
            if allow_404:
                return None
            raise pub.Refused("GitHub API {} {} failed".format(method, path.split("?")[0]))

        def ok(value):
            return value if isinstance(value, bytes) else json.dumps(value).encode()

        if rest == "commits/" + TARGET:
            return ok({"sha": TARGET, "commit": {"committer": {"date": "2026-10-19T18:30:00Z"}}})
        if rest.startswith("commits/"):
            return missing()
        if rest.startswith("git/ref/tags/"):
            tag = rest[len("git/ref/tags/"):]
            if tag in self.tags:
                return ok({"object": {"type": "commit", "sha": self.tags[tag]}})
            return missing()
        if rest.startswith("releases?"):
            page = int(rest.rsplit("&page=", 1)[1])
            return ok(self.releases if page == 1 else [])
        if rest.startswith("releases/") and method == "PATCH":
            release = self.releases[0]
            release["draft"] = body["draft"]
            self.tags[release["tag_name"]] = TARGET
            return ok(release)
        if rest.startswith("actions/runs?event=push"):
            return ok({"total_count": self.push_runs})
        if rest.startswith("actions/runs/") and "/artifacts" in rest:
            run_id = rest.split("/")[2]
            return ok({"artifacts": self.artifacts.get(run_id, [])})
        if rest.startswith("actions/runs/"):
            run_id = rest.split("/")[2]
            return ok(self.runs[run_id]) if run_id in self.runs else missing()
        if rest.startswith("actions/artifacts/"):
            return self.blobs[int(rest.split("/")[2])]
        if rest == "contents/LICENSE?ref=" + TARGET:
            return LICENSE
        raise AssertionError("unexpected API path " + path)

    def api_calls(self, method):
        return [c for c in self.calls if c[0] == method]


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="publish-test-"))
        self.addCleanup(shutil.rmtree, self.tmp)
        self.counter = 0
        self.gh = FakeGh()
        self.env = {
            "GITHUB_REPOSITORY": REPO,
            "TRUSTED_DEFAULT_BRANCH": "main",
            "RELEASE_TAG": TAG,
            "TARGET_SHA": TARGET,
            "APP_NOTARIZE_RUN_ID": APP_RUN,
            "CLI_NOTARIZE_RUN_ID": CLI_RUN,
            "APP_ASSET_SHA256": sha(APP_ZIP),
            "CLI_BINARY_SHA256": sha(CLI_BIN),
            "PROVENANCE_NOTE": "Build stamp and receipts identify this commit; manifest source_sha is null.",
            "RUN_URL": "https://github.com/gents-ai/gents/actions/runs/999",
        }

    def cfg(self, **over):
        self.counter += 1
        env = dict(self.env, PUBLISH_WORK_DIR=str(self.tmp / "work{}".format(self.counter)))
        env.update(over)
        with mock.patch.dict(os.environ, env, clear=True):
            return pub.config()

    def collect(self, cfg=None):
        cfg = cfg or self.cfg()
        return cfg, pub.collect(self.gh, cfg)

    def assert_refused(self, func, *args, contains=None):
        with self.assertRaises(pub.Refused) as ctx:
            func(*args)
        if contains:
            self.assertIn(contains, str(ctx.exception))


class HappyPath(Base):
    def test_collect_then_publish(self):
        cfg, manifest = self.collect()
        assets = Path(cfg["work"]) / "assets"
        app_asset = "gents-desktop_0.21.0-rc1_aarch64.app.zip"
        self.assertEqual(
            sorted(manifest["assets"]),
            sorted([app_asset, "gents-aarch64-apple-darwin.tar.gz", "SHA256SUMS-cli-macos.txt",
                    "SHA256SUMS-desktop-macos.txt"]),
        )
        self.assertEqual((assets / app_asset).read_bytes(), APP_ZIP)
        with tarfile.open(assets / "gents-aarch64-apple-darwin.tar.gz") as tar:
            self.assertEqual(
                tar.getnames(),
                ["gents-aarch64-apple-darwin", "gents-aarch64-apple-darwin/LICENSE",
                 "gents-aarch64-apple-darwin/gents"],
            )
            member = tar.getmember("gents-aarch64-apple-darwin/gents")
            self.assertEqual(tar.extractfile(member).read(), CLI_BIN)
            self.assertEqual(member.mode, 0o755)
            self.assertEqual(member.uid, 0)
        self.assertIn(app_asset, (assets / "SHA256SUMS-desktop-macos.txt").read_text())
        notes = (Path(cfg["work"]) / "notes.md").read_text()
        for text in ("no DMG", APP_ID, CLI_ID, TARGET, "source_sha is null", sha(CLI_BIN), "cannot carry a stapled"):
            self.assertIn(text, notes)

        release = pub.publish(self.gh, cfg, settle_seconds=0)
        self.assertFalse(release["draft"])
        create = [c for c in self.gh.calls if c[0] == "run"][0][1]
        for flag in ("--draft", "--prerelease", "--latest=false"):
            self.assertIn(flag, create)
        self.assertEqual(create[create.index("--target") + 1], TARGET)
        self.assertEqual(len(self.gh.api_calls("PATCH")), 1)
        self.assertEqual(self.gh.tags[TAG], TARGET)
        # No write API calls other than the single publish PATCH.
        self.assertEqual({c[0] for c in self.gh.calls}, {"GET", "PATCH", "run"})

    def test_cli_tarball_is_deterministic(self):
        cfg1, m1 = self.collect()
        cfg2, m2 = self.collect()
        self.assertEqual(m1["assets"], m2["assets"])


class InputValidation(Base):
    def test_bad_inputs(self):
        for override in (
            {"RELEASE_TAG": "v0.21.0"},
            {"RELEASE_TAG": "v0.21.0-rc1;rm"},
            {"TARGET_SHA": TARGET.upper()},
            {"TARGET_SHA": TARGET[:39]},
            {"APP_NOTARIZE_RUN_ID": "0"},
            {"CLI_NOTARIZE_RUN_ID": APP_RUN},
            {"APP_ASSET_SHA256": "x" * 64},
            {"PROVENANCE_NOTE": "line\x01break"},
            {"GITHUB_REPOSITORY": "a b/c"},
        ):
            with self.subTest(override=override):
                with self.assertRaises(pub.Refused):
                    self.cfg(**override)


class Preconditions(Base):
    def test_missing_target_commit(self):
        self.assert_refused(self.collect, self.cfg(TARGET_SHA="e" * 40), contains="GitHub API")

    def test_existing_tag_refused(self):
        self.gh.tags[TAG] = TARGET
        self.assert_refused(self.collect, None, contains="already exists")

    def test_existing_draft_refused(self):
        self.gh.releases.append({"tag_name": TAG, "draft": True})
        self.assert_refused(self.collect, None, contains="already exists")

    def test_release_listing_page_cap_and_shape(self):
        original = self.gh.api

        def endless(path, **kwargs):
            if "/releases?" in path:
                return json.dumps([{"tag_name": "v0.0.1"}]).encode()
            return original(path, **kwargs)

        self.gh.api = endless
        self.assert_refused(self.collect, None, contains="page limit")

        def malformed(path, **kwargs):
            if "/releases?" in path:
                return json.dumps({"message": "odd"}).encode()
            return original(path, **kwargs)

        self.gh.api = malformed
        self.assert_refused(self.collect, None, contains="malformed")

    def test_run_provenance_checks(self):
        for field, value in (
            ("path", ".github/workflows/release-macos.yml"),
            ("event", "push"),
            ("head_branch", "feature"),
            ("conclusion", "failure"),
            ("status", "in_progress"),
            ("repository", {"full_name": "someone/fork"}),
            ("run_attempt", 0),
        ):
            with self.subTest(field=field):
                self.gh = FakeGh()
                self.gh.runs[APP_RUN][field] = value
                self.assert_refused(self.collect, None)

    def test_artifact_must_be_single_and_unexpired(self):
        for artifacts in (
            [],
            [{"id": 9001, "name": "notarize-staged-111-1", "expired": True}],
            [{"id": 9001, "name": "notarize-staged-111-1", "expired": False}] * 2,
            [{"id": 9001, "name": "notarize-staged-receipt-111-1", "expired": False}],
        ):
            with self.subTest(artifacts=artifacts):
                self.gh = FakeGh()
                self.gh.artifacts[APP_RUN] = artifacts
                self.assert_refused(self.collect, None)

    def test_artifact_attempt_must_match_run(self):
        self.gh.runs[APP_RUN]["run_attempt"] = 2
        self.assert_refused(self.collect, None, contains="no single unexpired")


class OutputTampering(Base):
    def assert_tampered(self, blob_id, files, contains=None):
        self.gh.blobs[blob_id] = outputs_zip(files)
        self.assert_refused(self.collect, None, contains=contains)

    def test_app_hash_differs_from_pin(self):
        name = "Gents-" + APP_STAGED[:16] + "-notarized.zip"
        other = b"different stapled zip"
        self.assert_tampered(
            9001, app_outputs(**{name: other, "SHA256SUMS": "{}  {}\n".format(sha(other), name)}), "pinned"
        )

    def test_sums_do_not_match_file(self):
        self.assert_tampered(9002, cli_outputs(SHA256SUMS="{}  gents\n".format("0" * 64)), "pinned")

    def test_log_job_id_mismatch(self):
        log = json.dumps({"jobId": CLI_ID, "status": "Accepted", "sha256": APP_STAGED})
        self.assert_tampered(9001, app_outputs(**{"notary-log.json": log}), "jobId")

    def test_log_not_accepted(self):
        log = json.dumps({"jobId": APP_ID, "status": "Invalid", "sha256": APP_STAGED})
        self.assert_tampered(9001, app_outputs(**{"notary-log.json": log}), "Accepted")

    def test_log_sha_mismatch(self):
        log = json.dumps({"jobId": APP_ID, "status": "Accepted", "sha256": "e" * 64})
        self.assert_tampered(9001, app_outputs(**{"notary-log.json": log}), "staged")

    def test_missing_log(self):
        self.assert_tampered(9002, cli_outputs(**{"notary-log.json": None}), "notary-log.json")

    def test_kind_swap(self):
        self.gh.blobs[9001] = outputs_zip(cli_outputs())
        self.env["APP_ASSET_SHA256"] = sha(CLI_BIN)
        self.assert_refused(self.collect, None, contains="not an app")

    def test_unsafe_entry_in_outputs(self):
        files = app_outputs()
        files["../escape"] = b"x"
        self.assert_tampered(9001, files, "unexpected entry")


class PublishSafety(Base):
    def test_draft_digest_mismatch_left_as_draft(self):
        cfg, _ = self.collect()
        self.gh.tamper_digest = True
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="left as draft")
        self.assertEqual(self.gh.api_calls("PATCH"), [])

    def test_extra_draft_asset_left_as_draft(self):
        cfg, _ = self.collect()
        self.gh.extra_asset = True
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="left as draft")
        self.assertEqual(self.gh.api_calls("PATCH"), [])

    def test_wrong_target_left_as_draft(self):
        cfg, _ = self.collect()
        self.gh.wrong_target = True
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="left as draft")
        self.assertEqual(self.gh.api_calls("PATCH"), [])

    def test_create_failure(self):
        cfg, _ = self.collect()
        self.gh.create_fails = True
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="inspect drafts")
        self.assertEqual(self.gh.api_calls("PATCH"), [])

    def test_local_asset_changed_after_collect(self):
        cfg, _ = self.collect()
        (Path(cfg["work"]) / "assets" / "SHA256SUMS-cli-macos.txt").write_text("tampered\n")
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="changed since collection")
        self.assertEqual([c for c in self.gh.calls if c[0] == "run"], [])

    def test_tag_created_concurrently_before_publish(self):
        cfg, _ = self.collect()
        self.gh.tags[TAG] = TARGET
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="already exists")
        self.assertEqual([c for c in self.gh.calls if c[0] == "run"], [])

    def test_push_triggered_runs_reported(self):
        cfg, _ = self.collect()
        self.gh.push_runs = 1
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="push-triggered")

    def test_publish_requires_matching_manifest(self):
        cfg, _ = self.collect()
        cfg = dict(cfg, tag="v0.21.0-rc2")
        self.assert_refused(pub.publish, self.gh, cfg, 0, contains="does not match inputs")


class WorkflowStatic(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import yaml
        except ImportError:  # pragma: no cover
            raise unittest.SkipTest("PyYAML not installed")
        cls.wf = yaml.safe_load(WORKFLOW.read_text())
        cls.on = cls.wf.get(True, cls.wf.get("on"))
        cls.steps = cls.wf["jobs"]["publish"]["steps"]

    def test_dispatch_only(self):
        self.assertEqual(list(self.on), ["workflow_dispatch"])
        self.assertEqual(
            set(self.on["workflow_dispatch"]["inputs"]),
            {"tag", "target_sha", "app_notarize_run_id", "cli_notarize_run_id", "app_asset_sha256",
             "cli_binary_sha256", "provenance_note"},
        )

    def test_permissions_exact(self):
        self.assertEqual(self.wf["permissions"], {"contents": "write", "actions": "read"})

    def test_guard_first_and_full_ref(self):
        self.assertIn("pre-checkout", self.steps[0]["name"])
        self.assertIn("refs/heads/${TRUSTED_DEFAULT_BRANCH}", self.steps[0]["run"])

    def test_pinned_actions(self):
        uses = [s["uses"] for s in self.steps if "uses" in s]
        self.assertEqual(
            uses,
            ["actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
             "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"],
        )
        self.assertFalse(self.steps[1]["with"]["persist-credentials"])

    def test_no_expression_interpolation_in_run(self):
        for step in self.steps:
            self.assertNotIn("${{", step.get("run", ""))

    def test_no_repository_secrets(self):
        self.assertNotIn("secrets.", json.dumps(self.wf))

    def test_hosted_runner_and_concurrency(self):
        self.assertEqual(self.wf["jobs"]["publish"]["runs-on"], "ubuntu-latest")
        self.assertFalse(self.wf["concurrency"]["cancel-in-progress"])

    def test_collect_before_publish(self):
        runs = [s.get("run", "") for s in self.steps]
        collect = next(i for i, r in enumerate(runs) if r.endswith("collect"))
        publish = next(i for i, r in enumerate(runs) if r.endswith("publish"))
        self.assertLess(collect, publish)


if __name__ == "__main__":
    unittest.main()
