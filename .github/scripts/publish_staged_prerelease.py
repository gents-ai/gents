#!/usr/bin/env python3
"""Publish a macOS prerelease from notarize-staged-artifact.yml outputs.

Used by .github/workflows/publish-staged-prerelease.yml. Every GitHub call
goes through `gh api` / `gh release` authenticated with the workflow's
GITHUB_TOKEN. Events created with GITHUB_TOKEN do not start new workflow
runs (except workflow_dispatch/repository_dispatch), so publishing the tag
does not start the tag-triggered release rebuild workflows; `publish` checks
that afterwards.

Subcommands (configuration comes only from environment variables):

  collect  Read-only. Validate inputs; require the target commit to exist and
           the tag and any release for it to be absent; require each notarize
           run to be a successful workflow_dispatch run of
           notarize-staged-artifact.yml on the default branch; download that
           run's single output artifact; re-check Apple's log binding offline
           (jobId == receipt id, status Accepted, sha256 == staged sha256);
           require the output hashes to equal the operator-pinned hashes; build
           the release assets (the notarized app zip byte-for-byte, a
           deterministic CLI tarball, checksum files) and the release notes.
  publish  Re-check that the tag/release are still absent; create a DRAFT
           prerelease targeting the exact commit with the assets; require the
           draft's asset names and GitHub-computed digests to equal the local
           manifest exactly; only then publish; confirm the tag points at the
           target commit and that no push-triggered runs were started.
"""

import calendar
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile
from pathlib import Path

NOTARIZE_WORKFLOW_PATH = ".github/workflows/notarize-staged-artifact.yml"
TAG_RE = re.compile(r"^v([0-9]+\.[0-9]+\.[0-9]+-rc[0-9]+)$")
SHA40_RE = re.compile(r"^[0-9a-f]{40}$")
SHA64_RE = re.compile(r"^[0-9a-f]{64}$")
RUN_ID_RE = re.compile(r"^[1-9][0-9]{0,19}$")
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
NOTE_RE = re.compile(r"^[^\x00-\x08\x0b-\x1f\x7f]{0,2000}$")
OUTPUT_NAME_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
MAX_RELEASE_PAGES = 50
CLI_DIR = "gents-aarch64-apple-darwin"
CLI_TARBALL = CLI_DIR + ".tar.gz"


class Refused(Exception):
    """Fail-closed refusal with a fixed message."""


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    with open(path, "rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


class Gh:
    """Thin wrapper over the gh CLI; never echoes response bodies on error."""

    def __init__(self, repo):
        self.repo = repo

    def run(self, args, stdin=None):
        result = subprocess.run(["gh"] + args, input=stdin, capture_output=True)
        return result.returncode, result.stdout, result.stderr

    def api(self, path, method="GET", body=None, accept=None, allow_404=False):
        args = ["api", "-X", method, path]
        if accept:
            args += ["-H", "Accept: " + accept]
        stdin = None
        if body is not None:
            args += ["--input", "-"]
            stdin = json.dumps(body).encode()
        code, out, err = self.run(args, stdin)
        if code != 0:
            if allow_404 and b"HTTP 404" in err:
                return None
            raise Refused("GitHub API {} {} failed".format(method, path.split("?")[0]))
        return out

    def json(self, path, **kwargs):
        raw = self.api(path, **kwargs)
        return None if raw is None else json.loads(raw)


def config():
    env = os.environ
    cfg = {
        "repo": env.get("GITHUB_REPOSITORY", ""),
        "default_branch": env.get("TRUSTED_DEFAULT_BRANCH", ""),
        "tag": env.get("RELEASE_TAG", ""),
        "target_sha": env.get("TARGET_SHA", ""),
        "app_run_id": env.get("APP_NOTARIZE_RUN_ID", ""),
        "cli_run_id": env.get("CLI_NOTARIZE_RUN_ID", ""),
        "app_sha256": env.get("APP_ASSET_SHA256", ""),
        "cli_sha256": env.get("CLI_BINARY_SHA256", ""),
        "provenance": env.get("PROVENANCE_NOTE", ""),
        "work": env.get("PUBLISH_WORK_DIR", ""),
        "run_url": env.get("RUN_URL", ""),
    }
    checks = (
        ("repo", re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")),
        ("default_branch", re.compile(r"^[A-Za-z0-9_./-]+$")),
        ("tag", TAG_RE),
        ("target_sha", SHA40_RE),
        ("app_run_id", RUN_ID_RE),
        ("cli_run_id", RUN_ID_RE),
        ("app_sha256", SHA64_RE),
        ("cli_sha256", SHA64_RE),
        ("provenance", NOTE_RE),
    )
    for key, pattern in checks:
        if not pattern.match(cfg[key]):
            raise Refused("input {} is missing or malformed".format(key))
    if cfg["app_run_id"] == cfg["cli_run_id"]:
        raise Refused("app and cli notarize runs must differ")
    if not cfg["work"]:
        raise Refused("PUBLISH_WORK_DIR is empty")
    cfg["version"] = TAG_RE.match(cfg["tag"]).group(1)
    cfg["app_asset"] = "gents-desktop_{}_aarch64.app.zip".format(cfg["version"])
    return cfg


# ------------------------------------------------------------------ checks
def check_target(gh, cfg):
    commit = gh.json("repos/{}/commits/{}".format(cfg["repo"], cfg["target_sha"]))
    if not isinstance(commit, dict) or commit.get("sha") != cfg["target_sha"]:
        raise Refused("target commit not found")
    date = commit["commit"]["committer"]["date"]
    return calendar.timegm(time.strptime(date, "%Y-%m-%dT%H:%M:%SZ"))


def list_releases(gh, cfg):
    """All releases, drafts included (visible to a contents: write token)."""
    found = []
    for page in range(1, MAX_RELEASE_PAGES + 1):
        releases = gh.json("repos/{}/releases?per_page=100&page={}".format(cfg["repo"], page))
        if not isinstance(releases, list):
            raise Refused("release listing is malformed")
        if not releases:
            return found
        found += releases
    raise Refused("release listing exceeded the page limit; refusing")


def check_unpublished(gh, cfg):
    if gh.api("repos/{}/git/ref/tags/{}".format(cfg["repo"], cfg["tag"]), allow_404=True) is not None:
        raise Refused("tag {} already exists; refusing to modify it".format(cfg["tag"]))
    for release in list_releases(gh, cfg):
        if release.get("tag_name") == cfg["tag"]:
            raise Refused(
                "a release (draft={}) already exists for {}; remove it deliberately before retrying".format(
                    release.get("draft"), cfg["tag"]
                )
            )


def check_run(gh, cfg, run_id):
    run = gh.json("repos/{}/actions/runs/{}".format(cfg["repo"], run_id))
    expected = {
        "path": NOTARIZE_WORKFLOW_PATH,
        "event": "workflow_dispatch",
        "head_branch": cfg["default_branch"],
        "status": "completed",
        "conclusion": "success",
    }
    for field, value in expected.items():
        if run.get(field) != value:
            raise Refused("notarize run {} has unexpected {}".format(run_id, field))
    if (run.get("repository") or {}).get("full_name") != cfg["repo"]:
        raise Refused("notarize run {} belongs to another repository".format(run_id))
    attempt = run.get("run_attempt")
    if not isinstance(attempt, int) or attempt < 1:
        raise Refused("notarize run {} has no valid attempt".format(run_id))
    return attempt


def fetch_outputs(gh, cfg, run_id, attempt, dest):
    name = "notarize-staged-{}-{}".format(run_id, attempt)
    listing = gh.json("repos/{}/actions/runs/{}/artifacts?per_page=100".format(cfg["repo"], run_id))
    matches = [a for a in listing.get("artifacts", []) if a.get("name") == name]
    if len(matches) != 1 or matches[0].get("expired"):
        raise Refused("notarize run {} has no single unexpired output artifact {}".format(run_id, name))
    data = gh.api("repos/{}/actions/artifacts/{}/zip".format(cfg["repo"], matches[0]["id"]))
    dest.mkdir(parents=True)
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for info in archive.infolist():
            if info.is_dir() or not OUTPUT_NAME_RE.match(info.filename):
                raise Refused("notarize output artifact has an unexpected entry")
            (dest / info.filename).write_bytes(archive.read(info))


def _load_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        raise Refused("{} missing or unparseable".format(path.name))


def verify_outputs(dest, kind, expected_sha):
    receipt = _load_json(dest / "receipt.json")
    submission = receipt.get("submission_id")
    staged = receipt.get("artifact_sha256")
    if not isinstance(submission, str) or not UUID_RE.match(submission):
        raise Refused("{} receipt has no valid submission id".format(kind))
    if not isinstance(staged, str) or not SHA64_RE.match(staged):
        raise Refused("{} receipt has no valid staged sha256".format(kind))
    log = _load_json(dest / "notary-log.json")
    if log.get("jobId") != submission:
        raise Refused("{} notary log jobId does not match receipt".format(kind))
    if log.get("status") != "Accepted":
        raise Refused("{} notary log status is not Accepted".format(kind))
    if not isinstance(log.get("sha256"), str) or log["sha256"].lower() != staged:
        raise Refused("{} notary log sha256 does not match staged artifact".format(kind))
    try:
        lines = (dest / "SHA256SUMS").read_text().splitlines()
    except OSError:
        raise Refused("{} SHA256SUMS missing".format(kind))
    if len(lines) != 1 or not re.match(r"^[0-9a-f]{64}  [A-Za-z0-9._-]+$", lines[0]):
        raise Refused("{} SHA256SUMS malformed".format(kind))
    digest, name = lines[0].split("  ", 1)
    if kind == "app":
        if not name.endswith("-notarized.zip") or (dest / "gents").exists():
            raise Refused("app outputs are not an app notarization result")
    elif name != "gents":
        raise Refused("cli outputs are not a cli notarization result")
    actual = sha256_file(dest / name)
    if actual != digest or actual != expected_sha:
        raise Refused("{} output sha256 does not match the pinned value".format(kind))
    return {"file": dest / name, "submission_id": submission, "staged_sha256": staged, "sha256": actual}


# ------------------------------------------------------------------ assets
def build_cli_tarball(binary, license_bytes, mtime, path):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as tar:

        def add(name, data, mode, kind=tarfile.REGTYPE):
            info = tarfile.TarInfo(name)
            info.type = kind
            info.mode = mode
            info.mtime = mtime
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            if kind == tarfile.REGTYPE:
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            else:
                tar.addfile(info)

        add(CLI_DIR, b"", 0o755, tarfile.DIRTYPE)
        add(CLI_DIR + "/LICENSE", license_bytes, 0o644)
        add(CLI_DIR + "/gents", binary, 0o755)
    with open(path, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as gz:
        gz.write(buffer.getvalue())


def check_cli_tarball(path, expected_sha):
    with tarfile.open(path, "r:gz") as tar:
        names = tar.getnames()
        if names != [CLI_DIR, CLI_DIR + "/LICENSE", CLI_DIR + "/gents"]:
            raise Refused("CLI tarball layout is unexpected")
        member = tar.getmember(CLI_DIR + "/gents")
        if not member.isreg() or sha256_bytes(tar.extractfile(member).read()) != expected_sha:
            raise Refused("CLI tarball does not contain the pinned binary")


def release_notes(cfg, app, cli, assets):
    repo_url = "https://github.com/{}".format(cfg["repo"])
    lines = [
        "## macOS prerelease: notarized desktop app and CLI (Apple silicon only)",
        "",
        "This prerelease contains **only** the macOS arm64 desktop app and the macOS arm64 CLI.",
        "There is no DMG, no Linux package and no x86_64 build in this release.",
        "",
        "- **Desktop app** (`{}`): Developer ID signed (team NXJ753WW5X), notarized by Apple, "
        "ticket stapled. Notary submission `{}`.".format(cfg["app_asset"], app["submission_id"]),
        "- **CLI** (`{}`): Developer ID signed (team NXJ753WW5X), notarized by Apple "
        "(submission `{}`). Command-line binaries cannot carry a stapled ticket; "
        "Gatekeeper checks the ticket online on first run.".format(CLI_TARBALL, cli["submission_id"]),
        "",
        "**Source:** commit [`{0}`]({1}/commit/{0}).".format(cfg["target_sha"], repo_url),
    ]
    if cfg["provenance"]:
        lines += ["", "**Provenance:** " + cfg["provenance"]]
    lines += ["", "### SHA-256", "", "| Asset | SHA-256 |", "|---|---|"]
    for name, digest in assets.items():
        lines.append("| `{}` | `{}` |".format(name, digest))
    lines += [
        "",
        "CLI binary inside the tarball: `{}`.".format(cli["sha256"]),
        "",
        "Notarization runs: [{0}]({2}/actions/runs/{0}) (app), [{1}]({2}/actions/runs/{1}) (CLI).".format(
            cfg["app_run_id"], cfg["cli_run_id"], repo_url
        ),
    ]
    if cfg["run_url"]:
        lines.append("Published by {}.".format(cfg["run_url"]))
    return "\n".join(lines) + "\n"


def collect(gh, cfg):
    work = Path(cfg["work"])
    if work.exists():
        raise Refused("work directory already exists")
    work.mkdir(parents=True)
    mtime = check_target(gh, cfg)
    check_unpublished(gh, cfg)
    results = {}
    for kind, run_id, pinned in (
        ("app", cfg["app_run_id"], cfg["app_sha256"]),
        ("cli", cfg["cli_run_id"], cfg["cli_sha256"]),
    ):
        attempt = check_run(gh, cfg, run_id)
        fetch_outputs(gh, cfg, run_id, attempt, work / kind)
        results[kind] = verify_outputs(work / kind, kind, pinned)

    assets_dir = work / "assets"
    assets_dir.mkdir()
    shutil.copyfile(results["app"]["file"], assets_dir / cfg["app_asset"])
    license_bytes = gh.api(
        "repos/{}/contents/LICENSE?ref={}".format(cfg["repo"], cfg["target_sha"]),
        accept="application/vnd.github.raw+json",
    )
    build_cli_tarball(results["cli"]["file"].read_bytes(), license_bytes, mtime, assets_dir / CLI_TARBALL)
    check_cli_tarball(assets_dir / CLI_TARBALL, cfg["cli_sha256"])

    digests = {
        cfg["app_asset"]: sha256_file(assets_dir / cfg["app_asset"]),
        CLI_TARBALL: sha256_file(assets_dir / CLI_TARBALL),
    }
    if digests[cfg["app_asset"]] != cfg["app_sha256"]:
        raise Refused("app asset copy does not match the pinned value")
    (assets_dir / "SHA256SUMS-desktop-macos.txt").write_text(
        "{}  {}\n".format(digests[cfg["app_asset"]], cfg["app_asset"])
    )
    (assets_dir / "SHA256SUMS-cli-macos.txt").write_text("{}  {}\n".format(digests[CLI_TARBALL], CLI_TARBALL))
    for name in ("SHA256SUMS-desktop-macos.txt", "SHA256SUMS-cli-macos.txt"):
        digests[name] = sha256_file(assets_dir / name)

    (work / "notes.md").write_text(release_notes(cfg, results["app"], results["cli"], digests))
    manifest = {
        "tag": cfg["tag"],
        "target_sha": cfg["target_sha"],
        "assets": digests,
        "app_submission_id": results["app"]["submission_id"],
        "cli_submission_id": results["cli"]["submission_id"],
    }
    (work / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    for name, digest in digests.items():
        print("asset {} sha256 {}".format(name, digest))
    return manifest


def find_release(gh, cfg):
    return [r for r in list_releases(gh, cfg) if r.get("tag_name") == cfg["tag"]]


def publish(gh, cfg, settle_seconds=20):
    work = Path(cfg["work"])
    manifest = _load_json(work / "manifest.json")
    if manifest.get("tag") != cfg["tag"] or manifest.get("target_sha") != cfg["target_sha"]:
        raise Refused("collected manifest does not match inputs")
    expected = manifest["assets"]
    assets_dir = work / "assets"
    for name, digest in expected.items():
        if sha256_file(assets_dir / name) != digest:
            raise Refused("local asset {} changed since collection".format(name))
    check_unpublished(gh, cfg)

    args = [
        "release", "create", cfg["tag"],
        "--repo", cfg["repo"],
        "--draft", "--prerelease", "--latest=false",
        "--target", cfg["target_sha"],
        "--title", cfg["tag"],
        "--notes-file", str(work / "notes.md"),
    ] + [str(assets_dir / name) for name in sorted(expected)]
    code, _, _ = gh.run(args)
    if code != 0:
        raise Refused("creating the draft release failed; inspect drafts before any retry")

    drafts = find_release(gh, cfg)
    if len(drafts) != 1 or not drafts[0].get("draft"):
        raise Refused("expected exactly one draft release for the tag")
    release = drafts[0]
    if release.get("target_commitish") != cfg["target_sha"] or not release.get("prerelease"):
        raise Refused("draft release target or prerelease flag is wrong; left as draft")
    remote = {a.get("name"): a for a in release.get("assets", [])}
    if set(remote) != set(expected):
        raise Refused("draft release asset names differ from the manifest; left as draft")
    for name, digest in expected.items():
        if remote[name].get("digest") != "sha256:" + digest:
            raise Refused("draft asset {} digest differs from the manifest; left as draft".format(name))
    print("draft {} verified: {} assets match the manifest".format(release.get("id"), len(expected)))

    published = gh.json(
        "repos/{}/releases/{}".format(cfg["repo"], release["id"]), method="PATCH", body={"draft": False}
    )
    if published.get("draft") is not False:
        raise Refused("publishing the release failed")
    ref = gh.json("repos/{}/git/ref/tags/{}".format(cfg["repo"], cfg["tag"]))
    obj = ref.get("object") or {}
    if obj.get("type") != "commit" or obj.get("sha") != cfg["target_sha"]:
        raise Refused("published tag does not point at the target commit")
    print("published {} at {}".format(published.get("html_url"), cfg["target_sha"]))

    time.sleep(settle_seconds)
    runs = gh.json("repos/{}/actions/runs?event=push&branch={}".format(cfg["repo"], cfg["tag"]))
    if runs.get("total_count", 0) != 0:
        raise Refused(
            "push-triggered workflow runs exist for {}; inspect and cancel them before they modify assets".format(
                cfg["tag"]
            )
        )
    print("no push-triggered workflow runs for {}".format(cfg["tag"]))
    return published


def main(argv):
    if len(argv) != 2 or argv[1] not in ("collect", "publish"):
        print("usage: publish_staged_prerelease.py collect|publish", file=sys.stderr)
        return 64
    cfg = config()
    gh = Gh(cfg["repo"])
    if argv[1] == "collect":
        collect(gh, cfg)
    else:
        publish(gh, cfg)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except Refused as exc:
        print("::error::publish-staged-prerelease: {}".format(exc))
        sys.exit(1)
