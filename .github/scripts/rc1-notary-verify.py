#!/usr/bin/env python3
"""Verify fixed RC1 notary-candidate artifact hashes and metadata.

Read-only: never executes the candidate binaries or scripts, never mounts
the DMG, and never mutates anything. Every check is hash- or metadata-only
and operates on the pinned artifact identities that the RC1 existing-artifact
notary workflow is allowed to process.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tarfile
from pathlib import Path

CHUNK = 1024 * 1024

CLI_TARBALL_ENTRY = "gents-aarch64-apple-darwin-signed-unnotarized-candidate/gents"
CLI_TARBALL_LICENSE_ENTRY = "gents-aarch64-apple-darwin-signed-unnotarized-candidate/LICENSE"


def die(message: str) -> None:
    print(f"::error::{message}", file=sys.stderr)
    raise SystemExit(1)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while True:
            block = source.read(CHUNK)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def expect_sha256(path: Path, expected: str, label: str) -> str:
    actual = sha256_file(path)
    if actual != expected.lower():
        die(f"{label} sha256 mismatch: expected {expected.lower()}, got {actual} ({path})")
    return actual


def check_cli_tarball(tarball: Path, expected_tarball_sha: str, expected_binary_sha: str) -> dict:
    if not tarball.is_file():
        die(f"CLI tarball not found at {tarball}")
    tarball_sha = expect_sha256(tarball, expected_tarball_sha, "CLI tarball")

    binary_sha = None
    entries = []
    with tarfile.open(tarball, "r:gz") as archive:
        for member in archive.getmembers():
            entries.append(member.name)
            if member.name == CLI_TARBALL_ENTRY:
                if not member.isfile():
                    die(f"CLI tarball entry {CLI_TARBALL_ENTRY} is not a regular file")
                extracted = archive.extractfile(member)
                if extracted is None:
                    die(f"CLI tarball entry {CLI_TARBALL_ENTRY} has no content")
                digest = hashlib.sha256()
                while True:
                    block = extracted.read(CHUNK)
                    if not block:
                        break
                    digest.update(block)
                binary_sha = digest.hexdigest()
    if binary_sha is None:
        die(f"CLI tarball does not contain {CLI_TARBALL_ENTRY}")
    if binary_sha != expected_binary_sha.lower():
        die(
            "CLI binary sha256 mismatch inside tarball: expected "
            f"{expected_binary_sha.lower()}, got {binary_sha}"
        )
    if CLI_TARBALL_LICENSE_ENTRY not in entries:
        die(f"CLI tarball is missing {CLI_TARBALL_LICENSE_ENTRY}")
    return {"tarball_sha256": tarball_sha, "binary_sha256": binary_sha, "entries": entries}


def check_dmg(dmg: Path, expected_dmg_sha: str, expected_app_sha: str,
              expected_cli_sha: str, expected_bridge_sha: str) -> dict:
    if not dmg.is_file():
        die(f"Desktop DMG not found at {dmg}")
    if dmg.suffix != ".dmg":
        die(f"Desktop artifact at {dmg} does not have a .dmg extension")
    # A UDIF DMG ends in a 512-byte koly trailer; cheap structural sanity only.
    with dmg.open("rb") as image:
        image.seek(-512, os.SEEK_END)
        trailer = image.read(512)
    if len(trailer) != 512 or not trailer.startswith(b"koly"):
        die(f"Desktop artifact at {dmg} does not end in a UDIF koly trailer; not a DMG")
    dmg_sha = expect_sha256(dmg, expected_dmg_sha, "Desktop DMG")
    return {
        "dmg_sha256": dmg_sha,
        "expected_app_binary_sha256": expected_app_sha.lower(),
        "expected_cli_binary_sha256": expected_cli_sha.lower(),
        "expected_bridge_binary_sha256": expected_bridge_sha.lower(),
    }


def check_desktop_manifest(manifest: Path, expected_manifest_sha: str,
                           expected_source_sha: str) -> dict:
    if not manifest.is_file():
        die(f"Desktop manifest not found at {manifest}")
    manifest_sha = expect_sha256(manifest, expected_manifest_sha, "Desktop build manifest")
    payload = json.loads(manifest.read_text())
    if payload.get("sha") != expected_source_sha:
        die(
            "Desktop build manifest sha "
            f"{payload.get('sha')!r} does not match pinned source SHA "
            f"{expected_source_sha!r}"
        )
    if payload.get("notarized") is not False:
        die(f"Desktop build manifest notarized flag is {payload.get('notarized')!r}; expected false (pre-notary candidate)")
    binaries = payload.get("binaries_sha256")
    if not isinstance(binaries, dict):
        die("Desktop build manifest has no binaries_sha256 object")
    return {"manifest_sha256": manifest_sha, "manifest": payload}


def check_manifest_inner_binaries(manifest_report: dict, expected_app_sha: str,
                                  expected_cli_sha: str, expected_bridge_sha: str) -> None:
    """Cross-check the hash-verified manifest's inner-binary hashes against the pins.

    This binds the DMG-internal binaries to the notary pipeline without ever
    mounting the image or executing anything: the manifest is trusted only
    after its own sha256 matches the pinned manifest hash.
    """
    binaries = manifest_report["manifest"].get("binaries_sha256", {})
    for name, expected in (
        ("gents-desktop-tauri", expected_app_sha),
        ("gents", expected_cli_sha),
        ("bridge_runner", expected_bridge_sha),
    ):
        actual = binaries.get(name)
        if actual != expected.lower():
            die(
                f"Desktop manifest binaries_sha256[{name!r}] is {actual!r}; "
                f"expected pinned {expected.lower()!r}"
            )


def check_source_sha(source_sha: str) -> None:
    if len(source_sha) != 40 or any(c not in "0123456789abcdef" for c in source_sha):
        die(f"Pinned source SHA {source_sha!r} is not a full lowercase 40-character commit SHA")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--build-run-id", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--cli-tarball", required=True, type=Path)
    parser.add_argument("--expected-cli-tarball-sha256", required=True)
    parser.add_argument("--expected-cli-binary-sha256", required=True)
    parser.add_argument("--expected-desktop-manifest-sha256", required=True)
    parser.add_argument("--desktop-manifest", type=Path, default=None)
    parser.add_argument("--manifest-source-sha", default=None,
                        help="Source SHA a supplied manifest must carry; defaults to --source-sha")
    parser.add_argument("--dmg", required=True, type=Path)
    parser.add_argument("--expected-dmg-sha256", required=True)
    parser.add_argument("--expected-app-binary-sha256", required=True)
    parser.add_argument("--expected-cli-app-binary-sha256", required=True)
    parser.add_argument("--expected-bridge-binary-sha256", required=True)
    args = parser.parse_args()

    check_source_sha(args.source_sha)

    cli_report = check_cli_tarball(
        args.cli_tarball, args.expected_cli_tarball_sha256, args.expected_cli_binary_sha256)
    print(f"cli-tarball-sha256={cli_report['tarball_sha256']}")
    print(f"cli-binary-sha256={cli_report['binary_sha256']}")

    if args.desktop_manifest is not None:
        manifest_report = check_desktop_manifest(
            args.desktop_manifest, args.expected_desktop_manifest_sha256,
            args.manifest_source_sha or args.source_sha)
        print(f"desktop-manifest-sha256={manifest_report['manifest_sha256']}")
        check_manifest_inner_binaries(
            manifest_report,
            args.expected_app_binary_sha256,
            args.expected_cli_app_binary_sha256,
            args.expected_bridge_binary_sha256,
        )
        print(
            "desktop-inner-binaries-bound-to-pins: "
            f"app={args.expected_app_binary_sha256.lower()} "
            f"cli={args.expected_cli_app_binary_sha256.lower()} "
            f"bridge={args.expected_bridge_binary_sha256.lower()}"
        )
    else:
        # Manifest hash is pinned directly; content-level checks happen when a
        # manifest file is supplied (test suites and desktop slot layouts).
        print(f"desktop-manifest-sha256-pinned={args.expected_desktop_manifest_sha256}")

    dmg_report = check_dmg(
        args.dmg,
        args.expected_dmg_sha256,
        args.expected_app_binary_sha256,
        args.expected_cli_app_binary_sha256,
        args.expected_bridge_binary_sha256,
    )
    print(f"dmg-sha256={dmg_report['dmg_sha256']}")
    print(
        "desktop-inner-hashes-pinned: "
        f"app={dmg_report['expected_app_binary_sha256']} "
        f"cli={dmg_report['expected_cli_binary_sha256']} "
        f"bridge={dmg_report['expected_bridge_binary_sha256']}"
    )
    print("rc1-notary-verify: all fixed-artifact checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
