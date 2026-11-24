#!/usr/bin/env python3
"""The native Engineer eval suites and the one subject they all run.

  suites.py list TRIALS                 split, stage and check counts per suite
  suites.py select SELECTION SHARD      suite ids SELECTION names (`all` is the default list)
  suites.py field SUITE KEY             one field of a suite (path, definition_id)
  suites.py prepare-subject SUITE DEST  the subject a suite's trials run, at DEST
  suites.py command-execution           the mode GENTS_EVAL_COMMAND_EXECUTION requests
  suites.py launcher-divergences OUT    what only the launcher sees sets this launch apart from a production node
  suites.py banner FILE...              the block that says how a launch differs, from those lists and subject receipts

A suite is a `suite.json` beside its definition's `pack_config.json` under
the configurator eval fixtures: its `id`, `title`, `coverage` and the
`subject_fixtures` it layers on the subject. `scripts/evals/matrix.json`
keeps only the `default` list `all` runs.

Every suite runs the Engineer `gents init --engineer` makes on a fresh home
at the commit under test: `$GENTS_BIN utils eval subject` writes it from
init's own code, and each trial installs the definition's core packs on it
as init installs them. GENTS_EVAL_SUBJECT replaces it with a subject
directory for every suite prepared.

A subject fixture adds to that Engineer what a user's node could plausibly
hold, and the suite's README says why; `gents utils eval subject --suite`
layers it and refuses one that would change or drop anything. Each fixture
has a `name`, a `why` and any of
  documents     {key: [document, ...]}: new agents, contexts, tools, skills,
                datastore_tool_surfaces or agent_targets, each under an id the
                subject does not have;
  engineer      {"context"|"tools": {...}}: fields added to the Engineer's
                own Context or Tools, each one the document leaves unset or
                items appended to a list it has;
  slot_agents   [agent_slot, ...]: new agents run by the Engineer's
                inference slot;
  assets        [path, ...]: files or directories under the suite directory
                the subject carries.
A core pack init installs but leaves off is turned on by the definition's
`subject.enabled_core_packs` instead, as `gents config pack starter --enable`
does.

Every prepared subject's Engineer runs commands in the mode
GENTS_EVAL_COMMAND_EXECUTION names (default `off`): `off` leaves it no host
bash and so no background processes; `unrestricted` keeps the bash and
background processes init gives it (a GENTS_EVAL_SUBJECT keeps its own),
which a trial holds only inside an isolating container
(scripts/evals/run-container.sh); the launcher sets the definition's
`subject.host_bash` to match.

Every run records how its subjects and conditions differ from a production
node running the Engineer `gents init --engineer` makes: the run finds what it
can in its subjects and definition, and `--divergences` adds what only the
launcher sees. Each is `narrows` (removes, narrows or replaces what ships) or
`adds` (a fixture a user's node could plausibly hold).
"""
import collections
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

FIXTURES = pathlib.PurePosixPath("crates/gents/tests/fixtures/configurator_evals")
MATRIX = pathlib.PurePosixPath("scripts/evals/matrix.json")
SUITE_KEYS = {"id", "title", "coverage", "subject_fixtures", "blocked_reason"}
FIXTURE_KEYS = {"name", "why", "documents", "engineer", "slot_agents", "assets"}
COMMAND_EXECUTION = ("off", "unrestricted")


def suites(root):
    """Every suite under ROOT's fixtures, ordered by path, with its definition id."""
    fixtures = pathlib.Path(root) / FIXTURES
    found, ids, definitions = [], set(), set()
    for path in sorted(fixtures.rglob("suite.json")):
        suite = json.loads(path.read_text())
        relative = path.parent.relative_to(fixtures).as_posix()
        unknown = set(suite) - SUITE_KEYS
        missing = {"id", "title", "coverage"} - set(suite)
        if unknown or missing:
            raise SystemExit(f"{relative}/suite.json: unknown keys {sorted(unknown)}, missing {sorted(missing)}")
        names = set()
        for fixture in suite.get("subject_fixtures", []):
            name = fixture.get("name")
            if set(fixture) - FIXTURE_KEYS or not name or not fixture.get("why") or name in names:
                raise SystemExit(f"{relative}/suite.json: a subject fixture needs a distinct name and a why, "
                                 f"and takes only {sorted(FIXTURE_KEYS)}: {fixture}")
            names.add(name)
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
    """A named suite needs only the registry: a container's fixtures hold the running suite alone."""
    try:
        index, total = map(int, shard.split("/"))
        if not 1 <= index <= total:
            raise ValueError()
    except ValueError:
        raise SystemExit("GENTS_EVAL_SHARD must be INDEX/COUNT with 1 <= INDEX <= COUNT")
    if selection == "all":
        matches = matrix(root)["default"]
    else:
        matches = [s["id"] for s in suites(root) if selection in [s["id"], s["id"].split("-")[0]]]
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
    lines.append(f"Stage/check totals include held-out cases; each selected case runs {trials} trials. The capstone configures, then runs one request.")
    return "\n".join(lines)


def command_execution():
    mode = os.environ.get("GENTS_EVAL_COMMAND_EXECUTION") or "off"
    if mode not in COMMAND_EXECUTION:
        raise SystemExit(f"GENTS_EVAL_COMMAND_EXECUTION must be one of {', '.join(COMMAND_EXECUTION)}, not {mode!r}")
    return mode


def gents_binary():
    binary = os.environ.get("GENTS_BIN") or shutil.which("gents")
    if not binary:
        raise SystemExit("set GENTS_BIN to the gents binary of the commit under test: it writes the subject")
    return binary


def prepare_subject(root, suite_id, destination):
    """The subject SUITE_ID's trials run, at DESTINATION: the Engineer the gents binary's init makes
    (or GENTS_EVAL_SUBJECT) in the requested command execution mode, with the suite's fixtures."""
    root = pathlib.Path(root)
    found = suite(root, suite_id)
    destination = pathlib.Path(destination)
    if destination.exists():
        shutil.rmtree(destination)
    command = [gents_binary(), "utils", "eval", "subject", "--out", str(destination),
               "--command-execution", command_execution(), "--suite", str(root / FIXTURES / found["path"])]
    if override := os.environ.get("GENTS_EVAL_SUBJECT"):
        command += ["--from", override]
    written = subprocess.run(command, capture_output=True, text=True)
    if written.returncode != 0:
        raise SystemExit(f"gents utils eval subject failed: {written.stderr.strip()}")
    receipt(destination).write_text(written.stdout)
    return destination


def receipt(subject):
    return subject.with_name(subject.name + ".receipt.json")


def git(root, *arguments):
    try:
        found = subprocess.run(["git", "-C", str(root), *arguments], capture_output=True, text=True)
    except OSError:
        return None
    return found.stdout.strip() if found.returncode == 0 else None


def launcher_divergences(root, built_here):
    """What only the launcher sees sets this launch apart from a production node."""
    root = pathlib.Path(root)
    found = []

    def narrows(kind, detail):
        found.append({"severity": "narrows", "kind": kind, "detail": detail})

    if os.environ.get("GENTS_EVAL_DEFINITION_SOURCE"):
        narrows("definition_override", f"the definitions come from GENTS_EVAL_DEFINITION_SOURCE="
                f"{os.environ['GENTS_EVAL_DEFINITION_SOURCE']}, not this checkout's suites")
    version = subprocess.run([gents_binary(), "runtime", "version"], capture_output=True, text=True).stdout
    built = re.search(r"\(([0-9a-f]{7,40})(?:-dirty)?\)", version)
    binary = built.group(1) if built else None
    image = os.environ.get("GENTS_IMAGE_COMMIT")
    checkout = git(root, "rev-parse", "HEAD") or image
    same = binary and checkout and (binary.startswith(checkout) or checkout.startswith(binary))
    if not binary or not checkout:
        narrows("binary_unchecked", f"the commit of the gents binary ({binary or 'unknown'}) or of the suites "
                f"({checkout or 'unknown'}) is unknown, so the binary is not checked against the suites it runs")
    elif not same:
        narrows("binary_not_checkout", f"gents was built from {binary}, the suites come from {checkout}")
    if git(root, "status", "--porcelain", "--", str(FIXTURES), "scripts/evals"):
        narrows("checkout_dirty", "the suites or the launcher have uncommitted changes")
    if image and binary and (image.startswith(binary) or binary.startswith(image)):
        pass
    elif built_here:
        packs = pathlib.Path(os.environ.get("GENTS_CORE_PACKS_DIR") or root / "target/core-packs")
        stamp = (packs / "SOURCE").read_text().strip() if (packs / "SOURCE").is_file() else None
        tree = git(root, "rev-parse", "HEAD:core-packs")
        current = tree and tree + ("-dirty" if git(root, "status", "--porcelain", "--", "core-packs") else "")
        if not stamp or stamp != current:
            narrows("core_packs_not_rebuilt", f"the core packs this gents embeds were built from core-packs "
                    f"{stamp or 'of unknown source'}, not this checkout's {current or 'unknown'}; run "
                    f"`make core-packs` before building")
    else:
        narrows("core_packs_unchecked", "the core packs GENTS_BIN embeds are not checked against this checkout")
    return found


def read_divergences(paths):
    """The divergences in launcher lists and subject receipts, each receipt's under its suite."""
    divergences = []
    for path in map(pathlib.Path, paths):
        loaded = json.loads(path.read_text() or "[]")
        if isinstance(loaded, dict):
            suite_id = path.name.removesuffix(".receipt.json")
            loaded = [dict(item, cell=item.get("cell") or suite_id) for item in loaded.get("divergences", [])]
        divergences += [item for item in loaded if item not in divergences]
    return divergences


def banner(divergences, color=False):
    """The block that opens a launch's output, or one plain line when it has no divergence."""
    if not divergences:
        return ("divergences: none; the subject and conditions are the Engineer `gents init --engineer` makes, "
                "as a production node runs it")
    narrowing = sum(item["severity"] == "narrows" for item in divergences)
    rule = "!" * 78
    lines = [rule, f"!! NOT THE SHIPPED ENGINEER: {narrowing} difference(s) that remove, narrow or replace what it "
                   f"ships, {len(divergences) - narrowing} addition(s)"]
    for item in sorted(divergences, key=lambda item: (item["severity"] != "narrows", item.get("cell") or "", item["kind"])):
        where = f"[{item['cell']}] " if item.get("cell") else ""
        lines.append(f"!! {item['severity'].upper():<7} {where}{item['kind']}: {item['detail']}")
    lines.append(rule)
    text = "\n".join(lines)
    return f"\033[1;31m{text}\033[0m" if color else text


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
    elif command == "launcher-divergences" and len(arguments) == 1:
        found = launcher_divergences(root, os.environ.get("GENTS_EVAL_BUILT") == "1")
        pathlib.Path(arguments[0]).write_text(json.dumps(found, indent=2) + "\n")
    elif command == "banner" and arguments:
        print(banner(read_divergences(arguments), color=sys.stderr.isatty()))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
