#!/usr/bin/env python3
"""Stdlib-only helpers for notarize-staged-artifact.sh.

Subcommands either succeed (exit 0, printing only the requested non-secret
value) or fail (non-zero) so the calling script can fail closed. Exception
messages are fixed strings; input contents are never echoed.
"""

import fcntl
import json
import os
import posixpath
import re
import stat
import sys
import unicodedata
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


def alias_key(name):
    """Filesystem-equivalence key for the runner's case- and
    normalization-insensitive APFS volume: NFD + casefold, re-normalized."""
    return unicodedata.normalize("NFD", unicodedata.normalize("NFD", name).casefold())


def safe(value):
    return value if isinstance(value, str) and SAFE_RE.match(value) else "<redacted>"


def _load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


# --------------------------------------------------------------- archives
def zip_inspect(path):
    """Validate every archive entry before extraction; print the .app root.

    Names are compared by alias_key, because the runner's APFS volume treats
    case variants and Unicode-equivalent spellings as the same file.
    Requirements: no absolute paths, no empty/'.'/'..' components, no
    backslashes or NUL, no two entries with the same alias key, only
    regular/dir/symlink types, every entry under exactly one top-level
    '<Name>.app' directory, the root itself is not a symlink, and no entry
    lies beneath (an alias of) a symlink entry.

    Symlinks are handled conservatively: a target must be relative and must
    not contain empty, '.' or '..' components. Such a target only ever
    descends from the link's own directory, which is inside the root, so even
    chains of links cannot leave the root. Legitimate links that climb with
    '..' are therefore rejected rather than resolved.
    """
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        if not infos:
            raise Refused("archive is empty")
        if len(infos) > MAX_ENTRIES:
            raise Refused("archive has too many entries")
        total = 0
        roots = set()
        keys = set()
        link_keys = set()
        entries = []
        for info in infos:
            name = info.filename
            if "\x00" in name or "\\" in name or name.startswith("/"):
                raise Refused("archive entry has an unsafe path")
            parts = name.rstrip("/").split("/")
            if any(part in ("", ".", "..") for part in parts):
                raise Refused("archive entry has an unsafe path component")
            key = alias_key("/".join(parts))
            if key in keys:
                raise Refused("archive has duplicate or filesystem-equivalent entry names")
            keys.add(key)
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
                if not target or target.startswith("/") or "\x00" in target or "\\" in target:
                    raise Refused("archive symlink has an unsafe target")
                if any(part in ("", ".", "..") for part in target.split("/")):
                    raise Refused("archive symlink target uses '..', '.' or empty components")
                link_keys.add(key)
            entries.append(parts)
        if len(roots) != 1 or len({alias_key(r) for r in roots}) != 1:
            raise Refused("archive must contain exactly one top-level entry")
        root = roots.pop()
        if not root.endswith(".app") or root == ".app":
            raise Refused("archive top-level entry is not a .app")
        for parts in entries:
            for depth in range(1, len(parts)):
                if alias_key("/".join(parts[:depth])) in link_keys:
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


# ------------------------------------------------------- durable host state
def _full_sync(fd):
    """fsync, then F_FULLFSYNC so the data reaches stable storage on macOS."""
    os.fsync(fd)
    fcntl.fcntl(fd, fcntl.F_FULLFSYNC)


def _sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        _full_sync(fd)
    finally:
        os.close(fd)


def _publish_record(directory, name, record):
    """Durably publish directory/name exactly once.

    The JSON is written to a temporary file and flushed to stable storage,
    then hard-linked to its final name (which fails if the name exists, so a
    record is never overwritten or seen half-written), and the directory is
    flushed so the new name itself survives a host crash.
    """
    final = os.path.join(directory, name)
    tmp = os.path.join(directory, ".{}.{}.tmp".format(name, os.getpid()))
    data = (json.dumps(record, indent=2, sort_keys=True) + "\n").encode("utf-8")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        try:
            view = memoryview(data)
            while view:
                view = view[os.write(fd, view):]
            _full_sync(fd)
        finally:
            os.close(fd)
        os.link(tmp, final)
    finally:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
    _sync_dir(directory)


def _record(pairs):
    if len(pairs) % 2:
        raise Refused("record fields must be key/value pairs")
    record = dict(zip(pairs[0::2], pairs[1::2]))
    record["recorded_at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return record


def _ensure_root(root):
    parent = os.path.dirname(os.path.abspath(root))
    try:
        os.mkdir(root, 0o700)
    except FileExistsError:
        pass
    else:
        _sync_dir(parent)
    if os.path.islink(root) or not os.path.isdir(root):
        raise Refused("state root is not a real directory")
    _sync_dir(root)


def state_create_intent(root, sha, *pairs):
    """Create <root>/<sha>/ (exclusive) holding intent.json, all durably,
    before any Apple contact. Any failure is a refusal (fail closed)."""
    if not re.match(r"^[0-9a-f]{64}$", sha):
        raise Refused("state key is not a sha256")
    _ensure_root(root)
    state = os.path.join(root, sha)
    try:
        os.mkdir(state, 0o700)
    except FileExistsError:
        raise Refused("state for this artifact already exists")
    _sync_dir(root)
    _publish_record(state, "intent.json", _record(pairs))


def state_ensure(root, sha):
    """Durably ensure <root>/<sha>/ exists (resume path)."""
    if not re.match(r"^[0-9a-f]{64}$", sha):
        raise Refused("state key is not a sha256")
    _ensure_root(root)
    state = os.path.join(root, sha)
    try:
        os.mkdir(state, 0o700)
    except FileExistsError:
        if os.path.islink(state) or not os.path.isdir(state):
            raise Refused("state path is not a real directory")
    _sync_dir(state)
    _sync_dir(root)


def state_write(state, name, *pairs):
    """Durably publish one new record in an existing state directory."""
    if not re.match(r"^[A-Za-z0-9._-]{1,128}$", name) or name.startswith("."):
        raise Refused("record name is invalid")
    _publish_record(state, name, _record(pairs))


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
    "state-ensure": (state_ensure, 2),
}


def main(argv):
    if len(argv) >= 3 and argv[1] == "write-json-excl":
        write_json_excl(argv[2], *argv[3:])
        return 0
    if len(argv) >= 4 and argv[1] == "state-create-intent":
        state_create_intent(argv[2], argv[3], *argv[4:])
        return 0
    if len(argv) >= 4 and argv[1] == "state-write":
        state_write(argv[2], argv[3], *argv[4:])
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
