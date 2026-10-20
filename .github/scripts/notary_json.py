#!/usr/bin/env python3
"""Stdlib-only JSON helpers for notarize-staged-artifact.sh.

Every subcommand reads notarytool JSON output that the calling script captured
privately. Only non-secret fields (submission ids, names, statuses, dates,
issue counts) are ever printed. Exit status 0 means the input parsed and the
requested value was produced; any other status means the caller must fail
closed.
"""

import json
import re
import sys
from datetime import datetime, timezone

UUID_RE = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)


def _load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def history_matches(path, name):
    """Print `id status createdDate` for each history entry named `name`."""
    data = _load(path)
    entries = data.get("history") if isinstance(data, dict) else None
    if not isinstance(entries, list):
        raise ValueError("history field missing or not a list")
    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("history entry is not an object")
        if entry.get("name") == name:
            print(
                "{} {} {}".format(
                    entry.get("id", "?"),
                    str(entry.get("status", "?")).replace(" ", "_"),
                    entry.get("createdDate", "?"),
                )
            )


def submit_id(path):
    data = _load(path)
    value = data.get("id") if isinstance(data, dict) else None
    if not isinstance(value, str) or not UUID_RE.match(value):
        raise ValueError("submission id missing or malformed")
    print(value)


def info_field(path, field):
    data = _load(path)
    value = data.get(field) if isinstance(data, dict) else None
    if not isinstance(value, str) or not value:
        raise ValueError("field missing")
    print(value)


def log_issue_count(path):
    data = _load(path)
    issues = data.get("issues") if isinstance(data, dict) else None
    if issues is None:
        print(0)
    elif isinstance(issues, list):
        print(len(issues))
    else:
        raise ValueError("issues field malformed")


def write_receipt(path, *pairs):
    if len(pairs) % 2:
        raise ValueError("receipt fields must be key/value pairs")
    receipt = dict(zip(pairs[0::2], pairs[1::2]))
    receipt["recorded_at"] = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with open(path, "x", encoding="utf-8") as handle:
        json.dump(receipt, handle, indent=2, sort_keys=True)
        handle.write("\n")


def is_uuid(value):
    if not UUID_RE.match(value):
        raise ValueError("not a uuid")


COMMANDS = {
    "history-matches": (history_matches, 2),
    "submit-id": (submit_id, 1),
    "info-field": (info_field, 2),
    "log-issue-count": (log_issue_count, 1),
    "is-uuid": (is_uuid, 1),
}


def main(argv):
    if len(argv) >= 2 and argv[1] == "write-receipt":
        write_receipt(argv[2], *argv[3:])
        return 0
    if len(argv) < 2 or argv[1] not in COMMANDS:
        print("usage: notary_json.py <command> ...", file=sys.stderr)
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
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        # Never echo input contents; only the exception class.
        print("notary_json.py: parse failure ({})".format(type(exc).__name__), file=sys.stderr)
        sys.exit(65)
