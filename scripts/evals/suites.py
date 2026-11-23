#!/usr/bin/env python3
"""The native Engineer eval suites and the subject each one runs as.

  suites.py list TRIALS                 split, stage and check counts per suite
  suites.py select SELECTION SHARD      suite ids SELECTION names (`all` is the default list)
  suites.py field SUITE KEY             one field of a suite (path, definition_id, subject)
  suites.py prepare-subject SUITE DEST  the suite's subject, prepared for a trial, at DEST
  suites.py command-execution           the mode GENTS_EVAL_COMMAND_EXECUTION requests

A suite is a `suite.json` beside its definition's `pack_config.json` under
the configurator eval fixtures: its `id`, `title`, `coverage`, the `subject`
pack directory (relative to the fixtures) and whether the shipped Engineer
self-config preset replaces its Engineer's grant (`preset_self_config`).
`scripts/evals/matrix.json` keeps only the `default` list `all` runs.

Preparing a subject copies it, writes the shipped Engineer prompt over its
Engineer's system prompt, applies the preset when the suite asks, then
applies the suite's `subject_patch.json` when one sits beside its
definition. A patch is a JSON merge patch (RFC 7386) over the subject's
pack_config.json, except that a list of documents named in DOCUMENT_KEYS is
patched as an object keyed by each document's id: `{"tools": {"engineer-tools":
{"built_ins": {"enable_goal_creation": true}}}}` edits one Tools document, a
new key adds a document and `null` removes one. Patched content is inline;
a patch adds no files. GENTS_EVAL_SUBJECT replaces the subject directory of
every suite prepared.

Every prepared subject's Engineer runs commands in the mode
GENTS_EVAL_COMMAND_EXECUTION names (default `off`): `off` leaves it no host
bash; `unrestricted` grants host bash as `gents init --tool-package yolo`
does, which a trial holds only inside an isolating container
(scripts/evals/run-container.sh); the launcher sets the definition's
`subject.host_bash` to match.
"""
import collections
import json
import os
import pathlib
import shutil
import sys

FIXTURES = pathlib.PurePosixPath("crates/gents/tests/fixtures/configurator_evals")
MATRIX = pathlib.PurePosixPath("scripts/evals/matrix.json")
PROMPT = pathlib.PurePosixPath("crates/gents-protocol/prompts/engineer.md")
PRESET = pathlib.PurePosixPath("crates/gents-protocol/presets/engineer-self-config.json")
CELL_AGENT = "engineer"
DOCUMENT_KEYS = {
    "agents": "agent_slot",
    "contexts": "context_id",
    "tools": "tools_id",
    "skills": "skill_id",
    "datastore_tool_surfaces": "surface_id",
    "agent_targets": "target_id",
}
SUITE_KEYS = {"id", "title", "coverage", "subject", "preset_self_config", "blocked_reason"}
BASH_MODES = {"off": "Off", "unrestricted": "Unrestricted"}


def suites(root):
    """Every suite under ROOT's fixtures, ordered by path, with its definition id."""
    fixtures = pathlib.Path(root) / FIXTURES
    found, ids, definitions = [], set(), set()
    for path in sorted(fixtures.rglob("suite.json")):
        suite = json.loads(path.read_text())
        relative = path.parent.relative_to(fixtures).as_posix()
        unknown = set(suite) - SUITE_KEYS
        missing = {"id", "title", "coverage", "subject"} - set(suite)
        if unknown or missing:
            raise SystemExit(f"{relative}/suite.json: unknown keys {sorted(unknown)}, missing {sorted(missing)}")
        if not (fixtures / suite["subject"] / "pack_config.json").is_file():
            raise SystemExit(f"{relative}/suite.json: subject {suite['subject']} has no pack_config.json")
        definition = json.loads((path.parent / "pack_config.json").read_text())["eval_definitions"][0]
        suite = dict(suite, path=relative, definition_id=definition["definition_id"])
        if suite["id"] in ids or suite["definition_id"] in definitions:
            raise SystemExit(f"{relative}/suite.json: suite {suite['id']} or definition {suite['definition_id']} is registered twice")
        ids.add(suite["id"])
        definitions.add(suite["definition_id"])
        found.append(suite)
    return found


def matrix(root):
    """The default list and every suite; a default id must name a suite."""
    default = json.loads((pathlib.Path(root) / MATRIX).read_text())["default"]
    found = suites(root)
    known = {suite["id"] for suite in found}
    if unknown := [suite for suite in default if suite not in known]:
        raise SystemExit(f"matrix.json default names no suite.json: {unknown}")
    return {"default": default, "suites": found}


def suite(root, suite_id):
    matches = [found for found in suites(root) if found["id"] == suite_id]
    if len(matches) != 1:
        raise SystemExit(f"no suite {suite_id}")
    return matches[0]


def select(root, selection, shard):
    m = matrix(root)
    try:
        index, total = map(int, shard.split("/"))
        if not 1 <= index <= total:
            raise ValueError()
    except ValueError:
        raise SystemExit("GENTS_EVAL_SHARD must be INDEX/COUNT with 1 <= INDEX <= COUNT")
    if selection == "all":
        matches = m["default"]
    else:
        matches = [s["id"] for s in m["suites"] if selection in [s["id"], s["id"].split("-")[0]]]
        if len(matches) != 1:
            raise SystemExit("unknown or ambiguous suite: " + selection)
    selected = matches[index - 1::total]
    if not selected:
        raise SystemExit("this shard selects no suites")
    return selected


def listing(root, trials):
    fixtures = pathlib.Path(root) / FIXTURES
    lines = [f"{'Suite':<20} {'Train':>5} {'Valid':>5} {'Held':>5} {'Stages':>6} {'Checks':>6}  Coverage"]
    for found in matrix(root)["suites"]:
        directory = fixtures / found["path"]
        definition = json.loads((directory / "pack_config.json").read_text())["eval_definitions"][0]
        cases = [json.loads((directory / p).read_text()) for p in definition["cases"]]
        splits = collections.Counter(c["split"] for c in cases)
        stages = [s for c in cases for s in c["stages"]]
        lines.append(f"{found['id']:<20} {splits['train']:>5} {splits['validation']:>5} {splits['held_out']:>5} {len(stages):>6} {sum(len(s['checks']) for s in stages):>6}  {', '.join(found['coverage'])}")
        if "blocked_reason" in found:
            lines.append("  " + found["blocked_reason"])
    lines.append(f"Stage/check totals include held-out cases; each selected case runs {trials} trials. Capstone stops at configuration.")
    return "\n".join(lines)


def merge(target, patch):
    if not isinstance(patch, dict):
        return patch
    merged = dict(target) if isinstance(target, dict) else {}
    for key, value in patch.items():
        if value is None:
            merged.pop(key, None)
        else:
            merged[key] = merge(merged.get(key), value)
    return merged


def apply_patch(config, patch):
    """CONFIG with PATCH applied as the module docstring describes."""
    config = dict(config)
    for key, value in patch.items():
        id_key = DOCUMENT_KEYS.get(key)
        if id_key is None or not isinstance(value, dict):
            if value is None:
                config.pop(key, None)
            else:
                config[key] = merge(config.get(key), value)
            continue
        rows = {row[id_key]: row for row in config.get(key, [])}
        for document_id, document_patch in value.items():
            if document_patch is None:
                if rows.pop(document_id, None) is None:
                    raise SystemExit(f"subject_patch.json removes {key} {document_id}, which the subject does not have")
            else:
                rows[document_id] = merge(rows.get(document_id, {id_key: document_id}), document_patch)
        config[key] = list(rows.values())
    return config


def engineer_parts(config):
    """The cell agent's prompt path and Tools document id in a subject's pack_config."""
    agents = [a for a in config.get("agents", []) if a.get("agent_slot") == CELL_AGENT]
    if len(agents) != 1:
        raise SystemExit(f"a subject needs exactly one {CELL_AGENT} agent slot")
    contexts = [c for c in config.get("contexts", []) if c.get("context_id") == agents[0].get("context_id")]
    if len(contexts) != 1 or not str(contexts[0].get("system_prompt", "")).startswith("./"):
        raise SystemExit(f"the {CELL_AGENT} agent's Context must name its system prompt as a ./ sidecar")
    return contexts[0]["system_prompt"][2:], contexts[0]["tools_id"]


def command_execution():
    mode = os.environ.get("GENTS_EVAL_COMMAND_EXECUTION") or "off"
    if mode not in BASH_MODES:
        raise SystemExit(f"GENTS_EVAL_COMMAND_EXECUTION must be one of {', '.join(sorted(BASH_MODES))}, not {mode!r}")
    return mode


def prepare_subject(root, suite_id, destination):
    root = pathlib.Path(root)
    found = suite(root, suite_id)
    source = os.environ.get("GENTS_EVAL_SUBJECT") or str(root / FIXTURES / found["subject"])
    destination = pathlib.Path(destination)
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination)
    config_path = destination / "pack_config.json"
    config = json.loads(config_path.read_text())
    prompt, tools_id = engineer_parts(config)
    shutil.copy(root / PROMPT, destination / prompt)
    if found.get("preset_self_config"):
        engineer_tools(config, suite_id, tools_id)["self_config"] = json.loads((root / PRESET).read_text())
    patch_path = root / FIXTURES / found["path"] / "subject_patch.json"
    if patch_path.is_file():
        config = apply_patch(config, json.loads(patch_path.read_text()))
        _, tools_id = engineer_parts(config)
    host = engineer_tools(config, suite_id, tools_id).setdefault("host", {})
    host["bash"] = dict(host.get("bash") or {}, mode=BASH_MODES[command_execution()])
    json.dump(config, open(config_path, "w"), indent=2)
    return destination


def engineer_tools(config, suite_id, tools_id):
    tools = [t for t in config.get("tools", []) if t.get("tools_id") == tools_id]
    if len(tools) != 1:
        raise SystemExit(f"{suite_id}: the {CELL_AGENT} agent's Tools {tools_id} is not in its subject")
    return tools[0]


def main(argv):
    root = pathlib.Path(__file__).resolve().parents[2]
    command, *arguments = argv or ["help"]
    if command == "list" and len(arguments) == 1:
        print(listing(root, int(arguments[0])))
    elif command == "select" and len(arguments) == 2:
        print("\n".join(select(root, *arguments)))
    elif command == "field" and len(arguments) == 2:
        print(suite(root, arguments[0])[arguments[1]])
    elif command == "prepare-subject" and len(arguments) == 2:
        print(prepare_subject(root, *arguments))
    elif command == "command-execution" and not arguments:
        print(command_execution())
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
