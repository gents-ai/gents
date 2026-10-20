#!/usr/bin/env python3
"""Stdlib-only helpers for notarize-staged-artifact.sh.

Subcommands either succeed (exit 0, printing only the requested non-secret
value) or fail (non-zero) so the calling script can fail closed. Exception
messages are fixed strings; input contents are never echoed.
"""

import json
import os
import posixpath
import re
import stat
import sys
import zipfile
from datetime import datetime, timezone

UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)
# Provider-derived values are only ever printed if they match this.
SAFE_RE = re.compile(r"^[A-Za-z0-9._:+ -]{1,128}$")

MAX_ENTRIES = 50000
MAX_TOTAL_BYTES = 4 * 1024 * 1024 * 1024
ALLOWED_TYPES = (0, stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK)


class Refused(Exception):
    """A fail-closed refusal with a fixed, non-provider message."""


def safe(value):
    return value if isinstance(value, str) and SAFE_RE.match(value) else "<redacted>"


def _load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


# --------------------------------------------------------------- archives
def zip_inspect(path):
    """Validate every archive entry before extraction; print the .app root.

    Requirements: no absolute paths, no empty/'.'/'..' components, no
    backslashes or NUL, no duplicate names, only regular/dir/symlink types,
    every entry under exactly one top-level '<Name>.app' directory, the root
    itself is not a symlink, no entry lives beneath a symlink entry, and every
    symlink target is relative and lexically resolves inside the root.
    """
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        if not infos:
            raise Refused("archive is empty")
        if len(infos) > MAX_ENTRIES:
            raise Refused("archive has too many entries")
        names = [info.filename for info in infos]
        if len(set(names)) != len(names):
            raise Refused("archive has duplicate entry names")
        total = 0
        roots = set()
        links = set()
        for info in infos:
            name = info.filename
            if "\x00" in name or "\\" in name or name.startswith("/"):
                raise Refused("archive entry has an unsafe path")
            parts = name.rstrip("/").split("/")
            if any(part in ("", ".", "..") for part in parts):
                raise Refused("archive entry has an unsafe path component")
            ftype = stat.S_IFMT(info.external_attr >> 16)
            if ftype not in ALLOWED_TYPES:
                raise Refused("archive entry has an unsupported file type")
            roots.add(parts[0])
            total += info.file_size
            if total > MAX_TOTAL_BYTES:
                raise Refused("archive expands beyond the size limit")
            if ftype == stat.S_IFLNK:
                if len(parts) == 1:
                    raise Refused("archive root is a symlink")
                target = archive.read(info).decode("utf-8", errors="strict")
                if not target or target.startswith("/") or "\x00" in target:
                    raise Refused("archive symlink has an unsafe target")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname("/".join(parts)), target))
                if resolved != parts[0] and not resolved.startswith(parts[0] + "/"):
                    raise Refused("archive symlink escapes the app root")
                links.add("/".join(parts))
        if len(roots) != 1:
            raise Refused("archive must contain exactly one top-level entry")
        root = roots.pop()
        if not root.endswith(".app") or root == ".app":
            raise Refused("archive top-level entry is not a .app")
        for name in names:
            parts = name.rstrip("/").split("/")
            for depth in range(1, len(parts)):
                if "/".join(parts[:depth]) in links:
                    raise Refused("archive entry lies beneath a symlink")
    print(root)


def tree_check(extract_dir, root):
    """Post-extraction physical check of the extracted tree."""
    extract_real = os.path.realpath(extract_dir)
    if sorted(os.listdir(extract_dir)) != [root]:
        raise Refused("extraction produced unexpected top-level entries")
    app = os.path.join(extract_dir, root)
    if os.path.islink(app) or not os.path.isdir(app):
        raise Refused("extracted app root is not a real directory")
    app_real = os.path.realpath(app)
    if os.path.dirname(app_real) != extract_real:
        raise Refused("extracted app root is outside the work directory")
    for dirpath, dirnames, filenames in os.walk(app, followlinks=False):
        for entry in dirnames + filenames:
            full = os.path.join(dirpath, entry)
            mode = os.lstat(full).st_mode
            if not (stat.S_ISREG(mode) or stat.S_ISDIR(mode) or stat.S_ISLNK(mode)):
                raise Refused("extracted tree has an unsupported file type")
            if stat.S_ISLNK(mode):
                real = os.path.realpath(full)
                if real != app_real and not real.startswith(app_real + os.sep):
                    raise Refused("extracted symlink escapes the app root")


# ----------------------------------------------------------- notary JSON
def history_matches(path, name):
    """Strict history parse; print `id status createdDate` for name matches."""
    data = _load(path)
    entries = data.get("history") if isinstance(data, dict) else None
    if not isinstance(entries, list):
        raise Refused("history field missing or not a list")
    for entry in entries:
        if not isinstance(entry, dict):
            raise Refused("history entry is not an object")
        for field in ("id", "name", "status"):
            if not isinstance(entry.get(field), str) or not entry.get(field):
                raise Refused("history entry is missing a required field")
        if entry["name"] == name:
            print(
                "{} {} {}".format(
                    safe(entry["id"]),
                    safe(entry["status"]).replace(" ", "_"),
                    safe(entry.get("createdDate")),
                )
            )


def submit_id(path):
    data = _load(path)
    value = data.get("id") if isinstance(data, dict) else None
    if not isinstance(value, str) or not UUID_RE.match(value):
        raise Refused("submission id missing or malformed")
    print(value)


def info_check(path, expected_id, expected_name):
    """Require the submission info to name this id and artifact; print status."""
    data = _load(path)
    if not isinstance(data, dict):
        raise Refused("info is not an object")
    if data.get("id") != expected_id:
        raise Refused("submission info id does not match")
    if data.get("name") != expected_name:
        raise Refused("submission info name does not match this artifact")
    status = data.get("status")
    if not isinstance(status, str) or not SAFE_RE.match(status):
        raise Refused("submission info status missing or malformed")
    print(status)


def log_check(path, expected_id, expected_sha256):
    """Require Apple's log to bind this job id to the full archive SHA-256."""
    data = _load(path)
    if not isinstance(data, dict):
        raise Refused("log is not an object")
    if data.get("jobId") != expected_id:
        raise Refused("log jobId does not match the submission id")
    sha = data.get("sha256")
    if not isinstance(sha, str) or sha.lower() != expected_sha256:
        raise Refused("log sha256 does not match the staged artifact")
    if data.get("status") != "Accepted":
        raise Refused("log status is not Accepted")
    issues = data.get("issues")
    if issues is None:
        print(0)
    elif isinstance(issues, list):
        print(len(issues))
    else:
        raise Refused("log issues field malformed")


def json_field(path, field):
    data = _load(path)
    value = data.get(field) if isinstance(data, dict) else None
    if not isinstance(value, str) or not value:
        raise Refused("field missing")
    print(value)


def write_json_excl(path, *pairs):
    """Atomically create a JSON record; fail if it already exists."""
    if len(pairs) % 2:
        raise Refused("record fields must be key/value pairs")
    record = dict(zip(pairs[0::2], pairs[1::2]))
    record["recorded_at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(record, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())


def is_uuid(value):
    if not UUID_RE.match(value):
        raise Refused("not a uuid")


def print_safe(value):
    print(safe(value))


COMMANDS = {
    "zip-inspect": (zip_inspect, 1),
    "tree-check": (tree_check, 2),
    "history-matches": (history_matches, 2),
    "submit-id": (submit_id, 1),
    "info-check": (info_check, 3),
    "log-check": (log_check, 3),
    "json-field": (json_field, 2),
    "is-uuid": (is_uuid, 1),
    "safe": (print_safe, 1),
}


def main(argv):
    if len(argv) >= 3 and argv[1] == "write-json-excl":
        write_json_excl(argv[2], *argv[3:])
        return 0
    if len(argv) < 2 or argv[1] not in COMMANDS:
        print("notary_json.py: usage error", file=sys.stderr)
        return 64
    func, arity = COMMANDS[argv[1]]
    if len(argv) - 2 != arity:
        print("notary_json.py: wrong argument count", file=sys.stderr)
        return 64
    func(*argv[2:])
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except Refused as exc:
        print("notary_json.py: refused: {}".format(exc), file=sys.stderr)
        sys.exit(65)
    except (OSError, ValueError, UnicodeError, zipfile.BadZipFile, json.JSONDecodeError) as exc:
        print("notary_json.py: failure ({})".format(type(exc).__name__), file=sys.stderr)
        sys.exit(65)
