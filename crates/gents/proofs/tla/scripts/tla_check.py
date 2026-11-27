#!/usr/bin/env python3
"""Model-check TLA+ configurations and compare TLC's result with each header.

  tla_check.py run (--all | --spec S | --config C | --crate C | --lean O | --files F)... [--fresh]
  tla_check.py select --files F      print the specs a change selects
  tla_check.py check-headers         validate every spec and config header
  tla_check.py install               install the pinned JDK and tla2tools.jar

Header grammar (AGENTS.md). A config `MC*.cfg` (or `Sanity.cfg`) starts with
exactly one of

  \\* expect: holds
  \\* expect: deadlock
  \\* expect: violates <Name>
  \\* expect: violates <Name> initially

optionally followed by `\\* states: <N>`, the fewest distinct states TLC must
find (default 2, so a model whose initial predicate admits nothing or nothing
past its first state does not pass). A spec `X.tla` follows its `---- MODULE X ----` line with

  \\* lean: none | <Lean.owner>[, <Lean.owner>]...
  \\* crates: none | <crate>[, <crate>]...

and optionally, in this order,

  \\* models: <Lean.declaration>[, <Lean.declaration>]...
  \\* paths: <repository path>[, <repository path>]...

A `lean:` owner is a conformance function the spec reads from its generated
`LeanDecision_<Namespace>` module; a `models:` declaration is Lean the spec
restates by hand, named so that changing it selects the spec. A `paths:` entry
is a file or directory outside any crate the spec models; a change to it, or
under it, selects the spec.

TLC runs in breadth-first model-checking mode with one worker and a fixed
fingerprint polynomial, so the first violation it reports is reproducible.
Its outcome is therefore a function of the bytes it reads, and `run` records
each outcome under a digest of those bytes (see `result_key`) in the user
cache, shared by every checkout on the host. A later run with the same digest
reuses the outcome, judged against the header again; `--fresh` runs TLC anyway.
"""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
from collections import namedtuple
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

TLA = Path(__file__).resolve().parents[1]
ROOT = TLA.parents[3]
GENERATED = TLA / "generated"
PROOFS_LEAN = ROOT / "crates/gents/proofs/Proofs"

# tla2tools v1.8.0 is a nightly rebuilt in place; v1.7.4 is the newest
# release whose jar does not change under its URL.
TLA_TOOLS = {
    "version": "1.7.4",
    "url": "https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar",
    "sha256": "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88",
}
# TLC's parser and evaluator recurse per nested operand; Java's default thread
# stack (512k on some platforms) is too small for the larger specs.
JVM_OPTIONS = ["-Xss16m", "-XX:+UseParallelGC"]
JDK_RELEASE = "jdk-17.0.21+9"
JDK_BASE = "https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.21%2B9/"
JDKS = {
    ("Linux", "x86_64"): ("OpenJDK17U-jdk_x64_linux_hotspot_17.0.21_9.tar.gz",
                          "8b9b611d917e3dfd8419d3566b5949226f69b052fed7e109c1368add1dfe8711"),
    ("Linux", "aarch64"): ("OpenJDK17U-jdk_aarch64_linux_hotspot_17.0.21_9.tar.gz",
                           "3e4eca7ead088f080af1476007decf010a240a1eb4e1e78c54fa32fbcc347084"),
    ("Darwin", "arm64"): ("OpenJDK17U-jdk_aarch64_mac_hotspot_17.0.21_9.tar.gz",
                          "78b08c7f5a0598242d5719c022e19479cb4a3aa45c37c19726a757a469ed1c15"),
    ("Darwin", "x86_64"): ("OpenJDK17U-jdk_x64_mac_hotspot_17.0.21_9.tar.gz",
                           "868f5c672b3389fdbe48e44f94ae170f01668b1401360944fc7159d7cc9772cc"),
}

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"
LEAN_NAME = r"[A-Za-z_][A-Za-z0-9_'?!]*(?:\.[A-Za-z_][A-Za-z0-9_'?!]*)+"
CRATE = r"[a-z][a-z0-9_-]*"
EXPECT = re.compile(rf"\\\* expect: (holds|deadlock|violates ({IDENT})( initially)?)")
STATES = re.compile(r"\\\* states: ([1-9][0-9]*)")
DEFAULT_FLOOR = 2
MODULE = re.compile(rf"-{{4,}} MODULE ({IDENT}) -{{4,}}")
LEAN = re.compile(rf"\\\* lean: (none|{LEAN_NAME}(?:, {LEAN_NAME})*)")
CRATES = re.compile(rf"\\\* crates: (none|{CRATE}(?:, {CRATE})*)")
REPO_PATH = r"[A-Za-z0-9_.@+-]+(?:/[A-Za-z0-9_.@+-]+)*"
OPTIONAL = {"models": re.compile(rf"\\\* models: ({LEAN_NAME}(?:, {LEAN_NAME})*)"),
            "paths": re.compile(rf"\\\* paths: ({REPO_PATH}(?:, {REPO_PATH})*)")}
MODULE_PREFIX = "LeanDecision_"

CFG_KEYWORDS = ("CONSTANT", "CONSTANTS", "INIT", "NEXT", "SPECIFICATION", "INVARIANT", "INVARIANTS",
                "PROPERTY", "PROPERTIES", "CHECK_DEADLOCK", "SYMMETRY", "VIEW", "CONSTRAINT",
                "CONSTRAINTS", "ACTION_CONSTRAINT", "ACTION_CONSTRAINTS", "POSTCONDITION", "ALIAS")


class HeaderError(Exception):
    pass


# ---- headers and the spec graph ----

def strip_comments(text):
    text = re.sub(r"\(\*.*?\*\)", " ", text, flags=re.S)
    return re.sub(r"\\\*[^\n]*", "", text)


def cfg_sections(text):
    """Keyword -> the names listed under it, for a TLC configuration."""
    canonical = {"CONSTANT": "CONSTANTS", "INVARIANTS": "INVARIANT", "PROPERTIES": "PROPERTY",
                 "CONSTRAINTS": "CONSTRAINT", "ACTION_CONSTRAINTS": "ACTION_CONSTRAINT"}
    sections, current = {}, None
    for token in strip_comments(text).split():
        if token in CFG_KEYWORDS:
            current = canonical.get(token, token)
            sections.setdefault(current, [])
        elif current is not None:
            sections[current].append(token)
    return sections


class Expect(namedtuple("Expect", "verdict floor")):
    """A config's recorded TLC result and the fewest distinct states it must reach."""


def expectation(cfg):
    lines = cfg.read_text().splitlines()
    match = EXPECT.fullmatch(lines[0]) if lines else None
    if not match:
        raise HeaderError(f"{cfg.name}: first line must be "
                          "`\\* expect: holds|deadlock|violates <Name>[ initially]`")
    floor = DEFAULT_FLOOR
    if len(lines) > 1 and lines[1].startswith("\\* states:"):
        states = STATES.fullmatch(lines[1])
        if not states:
            raise HeaderError(f"{cfg.name}: second line must be `\\* states: <N>` when it names states")
        floor = int(states.group(1))
    sections = cfg_sections(cfg.read_text())
    if match.group(2):
        checked = sections.get("INVARIANT", []) + sections.get("PROPERTY", [])
        if match.group(2) not in checked:
            raise HeaderError(f"{cfg.name}: expects {match.group(2)} violated but does not check it")
    if match.group(1) == "deadlock" and sections.get("CHECK_DEADLOCK") == ["FALSE"]:
        raise HeaderError(f"{cfg.name}: expects a deadlock but sets CHECK_DEADLOCK FALSE")
    return Expect(match.group(1), floor)


class Header(namedtuple("Header", "lean crates models paths")):
    """A spec's header lines; each field is a list of names."""


def spec_header(spec):
    lines = spec.read_text().splitlines()
    if len(lines) < 3 or not MODULE.fullmatch(lines[0]):
        raise HeaderError(f"{spec.name}: must start with its MODULE line, then the lean and crates lines")
    lean, crates = LEAN.fullmatch(lines[1]), CRATES.fullmatch(lines[2])
    if not lean:
        raise HeaderError(f"{spec.name}: line 2 must be `\\* lean: none` or `\\* lean: <Owner>, ...`")
    if not crates:
        raise HeaderError(f"{spec.name}: line 3 must be `\\* crates: none` or `\\* crates: <crate>, ...`")

    def names(match):
        return [] if match.group(1) == "none" else match.group(1).split(", ")
    optional = {key: [] for key in OPTIONAL}
    keys = list(OPTIONAL)
    for line in lines[3:]:
        key = next((key for key in OPTIONAL if line.startswith(f"\\* {key}:")), None)
        if key is None:
            break
        match = OPTIONAL[key].fullmatch(line)
        if not match or keys.index(key) < max((keys.index(seen) for seen in optional if optional[seen]),
                                              default=-1) or optional[key]:
            raise HeaderError(f"{spec.name}: `{line}` must be one `\\* {key}: <name>, ...` line, "
                              f"in the order {', '.join(keys)}")
        optional[key] = names(match)
    return Header(names(lean), names(crates), optional["models"], optional["paths"])


def extends(module_file):
    """The modules `module_file` extends or instantiates."""
    text = strip_comments(module_file.read_text())
    names = []
    for match in re.finditer(rf"\bEXTENDS\s+({IDENT}(?:\s*,\s*{IDENT})*)", text):
        names += re.findall(IDENT, match.group(1))
    return names + re.findall(rf"\bINSTANCE\s+({IDENT})", text)


def specs(tla=TLA):
    """Spec name -> path: every module that is not a model config's wrapper."""
    return {path.stem: path for path in sorted(tla.glob("*.tla")) if not path.stem.startswith("MC")}


def configs(tla=TLA):
    return sorted(tla.glob("*.cfg"))


def spec_of(cfg, known):
    if cfg.stem in known:
        return cfg.stem
    module = cfg.with_suffix(".tla")
    if not module.exists():
        raise HeaderError(f"{cfg.name}: no module {module.name}")
    named = [name for name in extends(module) if name in known]
    if len(named) != 1:
        raise HeaderError(f"{module.name}: must extend exactly one spec, extends {named}")
    return named[0]


def generated_imports(spec):
    """The generated modules `spec` extends, by name, whether or not they exist."""
    return [name for name in extends(spec) if name.startswith(MODULE_PREFIX)]


def generated_module(owner):
    """The generated module that holds a `lean:` owner's table."""
    return MODULE_PREFIX + "_".join(owner.removeprefix("Conformance.").split(".")[:-1])


def graph(tla=TLA):
    """Spec name -> {path, lean, crates, models, paths, configs, generated}."""
    known = specs(tla)
    nodes = {}
    for name, path in known.items():
        header = spec_header(path)
        nodes[name] = {"path": path, "lean": header.lean, "crates": header.crates, "models": header.models,
                       "paths": header.paths, "configs": [], "generated": generated_imports(path)}
    for cfg in configs(tla):
        nodes[spec_of(cfg, known)]["configs"].append(cfg)
    return nodes


def wire_name(lean):
    """Lean `Conformance.wireName`: camelCase to snake_case; a run of capitals is one word."""
    out = []
    for index, char in enumerate(lean):
        previous = lean[index - 1] if index else ""
        following = lean[index + 1] if index + 1 < len(lean) else ""
        starts = char.isascii() and char.isupper() and bool(previous) and (
            previous.islower() or previous.isdigit() or (previous.isupper() and following.islower()))
        out.append(("_" if starts else "") + char.lower())
    return "".join(out)


def wire_key(owner):
    """The conformance snapshot key of a Lean owner; a family's key drops `Conformance.`."""
    owner = owner.removeprefix("Conformance.")
    return "_".join(wire_name(part) for part in owner.split("."))


def definitions(module_file):
    """Operator names a module defines at its top level."""
    text = strip_comments(module_file.read_text())
    return set(re.findall(rf"^({IDENT})\s*(?:\([^)]*\)|\[[^\]]*\])?\s*==", text, re.M))


def lean_sources():
    """Path -> text of every Lean source under the proofs."""
    return {path: path.read_text() for path in sorted(PROOFS_LEAN.rglob("*.lean"))}


def structure_problems(functions, members, tla=TLA, sources=None):
    """Header and import problems; `functions` are the snapshot's family keys,
    `members` the workspace packages and `sources` the Lean sources by path."""
    try:
        nodes = graph(tla)
    except HeaderError as error:
        return [str(error)]
    sources = lean_sources() if sources is None else sources
    generated = tla / "generated"
    problems = []
    for name, node in sorted(nodes.items()):
        for owner in node["lean"]:
            if wire_key(owner) not in functions:
                problems.append(f"{name}.tla: Lean owner {owner} is not in the conformance snapshot "
                                f"(no family {wire_key(owner)})")
            if generated_module(owner) not in node["generated"]:
                problems.append(f"{name}.tla: names {owner} but does not extend {generated_module(owner)}")
        for owner in node.get("models", []):
            if not any(lean_declares(text, owner) for text in sources.values()):
                problems.append(f"{name}.tla: models {owner}, which no Lean source declares")
        for crate in node["crates"]:
            if crate not in members:
                problems.append(f"{name}.tla: {crate} is not a workspace package")
        for path in node.get("paths", []):
            if not (ROOT / path).exists():
                problems.append(f"{name}.tla: models {path}, which does not exist")
            elif crate_of(f"{path}/-" if (ROOT / path).is_dir() else path):
                problems.append(f"{name}.tla: {path} is in a crate; name the crate instead")
        named = {generated_module(owner) for owner in node["lean"]}
        for module in node["generated"]:
            path = generated / f"{module}.tla"
            if not path.exists():
                problems.append(f"{name}.tla: extends {module}, which is not generated")
                continue
            if module not in named:
                problems.append(f"{name}.tla: extends {module} but names none of its owners on its lean line")
            clash = definitions(node["path"]) & definitions(path)
            if clash:
                problems.append(f"{name}.tla: redefines {sorted(clash)} from generated {module}")
        if not node["configs"]:
            problems.append(f"{name}.tla: no model config checks it")
        for cfg in node["configs"]:
            try:
                expectation(cfg)
            except HeaderError as error:
                problems.append(str(error))
    return problems


# ---- selection ----

def crate_of(path):
    """The Cargo package that owns a repository path, if any."""
    directory = (ROOT / path).parent
    while directory != ROOT and ROOT in directory.parents:
        manifest = directory / "Cargo.toml"
        if manifest.exists():
            match = re.search(r'^\[package\][^\[]*?^name\s*=\s*"([^"]+)"', manifest.read_text(), re.M | re.S)
            if match:
                return match.group(1)
        directory = directory.parent
    return None


def lean_declares(text, owner):
    """Whether Lean source `text` declares or exports `owner`: an attribute naming
    it in full, or its namespace (or `Conformance.` and that namespace, where the
    conformance exports live) and then its name."""
    parts = owner.split(".")
    exported = rf"^\s*attribute\s+\[conformance_function\][^\n]*(?<![\w.]){{}}(?![\w.'?!])"
    if re.search(exported.format(re.escape(owner)), text, re.M):
        return True
    if not re.search(rf"^\s*namespace\s+(?:Conformance\.)?{re.escape(parts[0])}(\.|\s|$)", text, re.M):
        return False
    tails = {".".join(parts[index:]) for index in range(1, len(parts))}
    for tail in tails:
        name = re.escape(tail)
        if re.search(rf"^\s*(?:@\[[^\]]*\]\s*)?(?:private\s+|protected\s+|noncomputable\s+)*"
                     rf"(?:def|abbrev|theorem|lemma|inductive|structure)\s+{name}(?![\w.'?!])", text, re.M):
            return True
        if re.search(exported.format(name), text, re.M):
            return True
    return False


def within(path, modeled):
    """Whether repository `path` is the modeled file or directory or under it."""
    modeled = modeled.rstrip("/")
    return path == modeled or path.startswith(modeled + "/")


def select(files, nodes=None, crate_for=crate_of):
    """The specs a change to `files` selects, as a sorted list of names."""
    nodes = graph() if nodes is None else nodes
    tla = TLA.relative_to(ROOT).as_posix() + "/"
    lean = PROOFS_LEAN.relative_to(ROOT).as_posix() + "/"
    by_file = {}
    for name, node in nodes.items():
        by_file[node["path"].name] = {name}
        for cfg in node["configs"]:
            by_file.setdefault(cfg.name, set()).add(name)
            by_file.setdefault(cfg.with_suffix(".tla").name, set()).add(name)
    chosen = set()
    for path in files:
        if path.startswith(tla + "scripts/"):
            return sorted(nodes)
        chosen |= {name for name, node in nodes.items()
                   if any(within(path, modeled) for modeled in node.get("paths", []))}
        if path.startswith(tla + "generated/traces/") and path.endswith(".json"):
            chosen |= by_file.get(Path(path).with_suffix(".cfg").name, set(nodes))
        elif path.startswith(tla + "generated/"):
            module = Path(path).stem
            chosen |= {name for name, node in nodes.items() if module in node["generated"]}
        elif path.startswith(tla) and "/" not in path[len(tla):]:
            chosen |= by_file.get(path[len(tla):], set())
        elif path.startswith(lean) and path.endswith(".lean"):
            source = ROOT / path
            text = source.read_text() if source.exists() else ""
            chosen |= {name for name, node in nodes.items()
                       if any(lean_declares(text, owner) for owner in node["lean"] + node.get("models", []))}
        else:
            owner = crate_for(path)
            if owner:
                chosen |= {name for name, node in nodes.items() if owner in node["crates"]}
    return sorted(chosen)


# ---- tools ----

def cache_dir():
    return Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "gents-tla"


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch(url, digest, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    staged = destination.with_name(destination.name + f".{os.getpid()}.partial")
    print(f"tla: downloading {url}", file=sys.stderr, flush=True)
    with urllib.request.urlopen(url, timeout=300) as response, open(staged, "wb") as handle:
        shutil.copyfileobj(response, handle)
    actual = sha256(staged)
    if actual != digest:
        staged.unlink()
        raise SystemExit(f"tla: {url} has SHA-256 {actual}, expected {digest}")
    staged.replace(destination)


JDK_STAMP = ".gents-archive-sha256"


def install():
    """The pinned java binary and tla2tools.jar, installed under the user cache.

    The jar is hashed on every call. An extracted JDK carries a stamp with the
    SHA-256 of the archive it came from, written only after a verified
    extraction; a JDK whose stamp is missing or names another archive (an
    interrupted install, an older pin) is removed and installed again."""
    cache = cache_dir()
    jar = cache / f"tla2tools-{TLA_TOOLS['version']}.jar"
    if not jar.exists() or sha256(jar) != TLA_TOOLS["sha256"]:
        fetch(TLA_TOOLS["url"], TLA_TOOLS["sha256"], jar)
    key = (platform.system(), platform.machine())
    if key not in JDKS:
        raise SystemExit(f"tla: no pinned JDK for {key}")
    archive, digest = JDKS[key]
    home = cache / JDK_RELEASE
    java = home / ("Contents/Home/bin/java" if key[0] == "Darwin" else "bin/java")
    stamp = home / JDK_STAMP
    stamped = stamp.exists() and stamp.read_text().strip() == digest
    if not (stamped and java.exists()):
        tarball = cache / archive
        if not tarball.exists() or sha256(tarball) != digest:
            fetch(JDK_BASE + archive, digest, tarball)
        staging = Path(tempfile.mkdtemp(dir=cache, prefix="jdk-"))
        with tarfile.open(tarball) as bundle:
            if hasattr(tarfile, "data_filter"):
                bundle.extractall(staging, filter="tar")
            else:
                bundle.extractall(staging)
        unpacked = staging / JDK_RELEASE
        (unpacked / JDK_STAMP).write_text(digest + "\n")
        if home.exists():
            # Out of the way first, so a concurrent reader never sees half a JDK under `home`.
            stale = Path(tempfile.mkdtemp(dir=cache, prefix="jdk-stale-"))
            home.rename(stale / JDK_RELEASE)
            shutil.rmtree(stale)
        unpacked.rename(home)
        shutil.rmtree(staging)
        tarball.unlink()
    return java, jar


# ---- running TLC ----

MESSAGE = re.compile(r"@!@!@STARTMSG (\d+):(\d+) @!@!@\n(.*?)@!@!@ENDMSG \1 @!@!@", re.S)
INITIAL = re.compile(rf"(?:Invariant|Property) ({IDENT}) is violated by the initial state")
INVARIANT = re.compile(rf"Invariant ({IDENT}) is violated")
ACTION = re.compile(rf"Action property ({IDENT}) is violated")
TEMPORAL = re.compile(rf"Temporal property ({IDENT}) was violated")
SUCCESS = 2193
DEADLOCK = 2114
TEMPORAL_UNNAMED = 2116
STATS = 2199
ERROR_CLASS = 1
DISTINCT = re.compile(r"([\d,]+) distinct states? found")


class Outcome(namedtuple("Outcome", "verdict distinct")):
    """TLC's result and the distinct states it found (None when it printed no count)."""


def parse(output, status):
    """TLC's `-tool` output -> its verdict (holds | deadlock | violates <Name>[ initially] |
    violates ? | error: <text>) and the distinct states it reports."""
    messages = [(int(code), int(kind), body.strip()) for code, kind, body in MESSAGE.findall(output)]
    counts = [DISTINCT.search(body) for code, _, body in messages if code == STATS]
    distinct = int(counts[-1].group(1).replace(",", "")) if counts and counts[-1] else None
    for code, _, body in messages:
        match = INITIAL.search(body)
        if match:
            return Outcome(f"violates {match.group(1)} initially", distinct)
        for pattern in (INVARIANT, ACTION, TEMPORAL):
            match = pattern.search(body)
            if match:
                return Outcome(f"violates {match.group(1)}", distinct)
        if code == TEMPORAL_UNNAMED:
            return Outcome("violates ?", distinct)
        if code == DEADLOCK:
            return Outcome("deadlock", distinct)
    if status == 0 and any(code == SUCCESS for code, _, _ in messages):
        return Outcome("holds", distinct)
    errors = [body for _, kind, body in messages if kind == ERROR_CLASS and body]
    plain = [line for line in MESSAGE.sub("", output).splitlines() if line.strip()]
    detail = errors[-1] if errors else "\n".join(plain[-3:]) or f"TLC exited {status} with no output"
    return Outcome("error: " + " ".join(detail.split())[:240], distinct)


def judge(expected, outcome):
    """ok, MISMATCH, or FEW STATES when TLC gave the recorded result from fewer
    distinct states than the floor. A violation in the initial state has no
    count and matches only a header that records it."""
    if outcome.verdict != expected.verdict:
        return "MISMATCH"
    if expected.verdict.endswith(" initially"):
        return "ok"
    if outcome.distinct is None or outcome.distinct < expected.floor:
        return "FEW STATES"
    return "ok"


def without_checks(text, keep_property):
    """`text` with its INVARIANT and PROPERTY sections replaced by one PROPERTY."""
    keywords = "|".join(CFG_KEYWORDS)
    body = strip_comments(text)
    body = re.sub(rf"(?ms)^\s*(?:INVARIANTS?|PROPERTY|PROPERTIES)\b.*?(?=^\s*(?:{keywords})\b|\Z)", "", body)
    return body.rstrip() + f"\nPROPERTY {keep_property}\n"


def tlc_command(java, jar, heap, scratch, config, module):
    """One TLC run: breadth-first, one worker, fixed fingerprint, machine-readable output."""
    return [str(java), f"-Xmx{heap}", *JVM_OPTIONS, f"-Djava.io.tmpdir={scratch}",
            f"-DTLA-Library={GENERATED}", "-cp", str(jar), "tlc2.TLC", "-tool", "-workers", "1",
            "-fp", "1", "-metadir", str(Path(scratch) / "states"), "-config", str(config), module]


class Tlc:
    def __init__(self, java, jar, timeout, heap):
        self.java, self.jar, self.timeout, self.heap = java, jar, timeout, heap

    def execute(self, cfg, text=None):
        """Model-check one config and retain its numbered state messages."""
        with tempfile.TemporaryDirectory(prefix="tlc-") as scratch:
            scratch = Path(scratch)
            config = cfg
            if text is not None:
                config = scratch / cfg.name
                config.write_text(text)
            command = tlc_command(self.java, self.jar, self.heap, scratch, config, cfg.stem)
            started = time.time()
            try:
                done = subprocess.run(command, cwd=TLA, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      text=True, timeout=self.timeout)
            except subprocess.TimeoutExpired:
                return Outcome(f"timeout after {self.timeout}s", None), time.time() - started, ""
            return parse(done.stdout, done.returncode), time.time() - started, done.stdout

    def run(self, cfg, text=None):
        outcome, seconds, _ = self.execute(cfg, text)
        return outcome, seconds

    def check(self, cfg):
        """The config's Outcome and seconds; a violated temporal property is named,
        and the state count is the full run's."""
        outcome, seconds = self.run(cfg)
        if outcome.verdict == "violates ?":
            # TLC 1.7.4 does not name a violated temporal property: check each
            # alone, in config order, and report the first that fails.
            text = cfg.read_text()
            properties = cfg_sections(text).get("PROPERTY", [])
            if len(properties) == 1:
                return Outcome(f"violates {properties[0]}", outcome.distinct), seconds
            verdict = "error: a temporal property was violated but none fails alone"
            for name in properties:
                alone, extra = self.run(cfg, without_checks(text, name))
                seconds += extra
                if alone.verdict.startswith("violates"):
                    verdict = f"violates {name}"
                    break
            outcome = Outcome(verdict, outcome.distinct)
        return outcome, seconds


# ---- recorded outcomes ----

RESULT_FORMAT = "1"


def module_files(cfg, tla=TLA):
    """The module files TLC reads to check `cfg`: the module named after it and,
    transitively, every module it extends or instantiates that resolves beside
    it or under generated/, TLC's search order. Other names are standard
    modules inside tla2tools.jar, which the key covers by the jar's digest."""
    files, seen, pending = [], set(), [cfg.stem]
    while pending:
        name = pending.pop()
        if name in seen:
            continue
        seen.add(name)
        path = next((directory / f"{name}.tla" for directory in (tla, tla / "generated")
                     if (directory / f"{name}.tla").exists()), None)
        if path is not None:
            files.append(path)
            pending += extends(path)
    return sorted(files)


def result_key(cfg, tla=TLA):
    """SHA-256 of everything TLC's outcome for `cfg` depends on: the pinned
    tools and JVM options, this script (which runs and parses TLC), the config
    and every module file it reads, each named relative to `tla`."""
    digest = hashlib.sha256()

    def feed(label, data):
        for part in (label.encode(), data):
            digest.update(len(part).to_bytes(8, "big"))
            digest.update(part)
    feed("format", RESULT_FORMAT.encode())
    feed("tools", f"{TLA_TOOLS['sha256']} {JDK_RELEASE} {' '.join(JVM_OPTIONS)}".encode())
    feed("checker", Path(__file__).read_bytes())
    for path in [cfg, *module_files(cfg, tla)]:
        feed(path.relative_to(tla).as_posix(), path.read_bytes())
    return digest.hexdigest()


def results_dir():
    return cache_dir() / "results"


def recall(key, directory=None):
    """The outcome recorded under `key`, or None when there is none readable."""
    path = (directory or results_dir()) / f"{key}.json"
    try:
        data = json.loads(path.read_text())
        return Outcome(data["verdict"], data["distinct"])
    except (OSError, ValueError, KeyError, TypeError):
        return None


def record(key, outcome, directory=None):
    """Record a TLC verdict under `key`; errors and timeouts are not verdicts.

    Concurrent jobs on one host share the directory. A record is written to a
    private file and renamed over its key, so a reader sees a whole record or
    none, and writers of one key write the same outcome."""
    if outcome.verdict.startswith(("error:", "timeout")):
        return
    directory = directory or results_dir()
    directory.mkdir(parents=True, exist_ok=True)
    handle, staged = tempfile.mkstemp(dir=directory, prefix=f".{key}.", suffix=".partial")
    try:
        with os.fdopen(handle, "w") as stream:
            json.dump({"verdict": outcome.verdict, "distinct": outcome.distinct}, stream)
        os.replace(staged, directory / f"{key}.json")
    except BaseException:
        Path(staged).unlink(missing_ok=True)
        raise


def report(rows):
    lines = ["| config | spec | expected | actual | distinct states | seconds | source | verdict |",
             "|---|---|---|---|---|---|---|---|"]
    lines += [f"| {cfg} | {spec} | {expected} | {actual} | {states} | {seconds:.0f} | {source} | {verdict} |"
              for cfg, spec, expected, actual, states, seconds, source, verdict in rows]
    table = "\n".join(lines)
    print(table, flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as handle:
            handle.write("### TLA+ model checking\n\n" + table + "\n")


def run(arguments):
    nodes = graph()
    chosen = set()
    if arguments.all:
        chosen = set(nodes)
    chosen |= set(arguments.spec or [])
    chosen |= {name for name, node in nodes.items() if set(arguments.crate or []) & set(node["crates"])}
    chosen |= {name for name, node in nodes.items()
               if set(arguments.lean or []) & set(node["lean"] + node["models"])}
    if arguments.files:
        files = [line.strip() for line in Path(arguments.files).read_text().splitlines() if line.strip()]
        chosen |= set(select(files, nodes))
    unknown = sorted(chosen - set(nodes))
    if unknown:
        raise SystemExit(f"tla: unknown specs {unknown}")
    targets = [cfg for name in sorted(chosen) for cfg in nodes[name]["configs"]]
    by_name = {cfg.stem: (name, cfg) for name, node in nodes.items() for cfg in node["configs"]}
    for stem in arguments.config or []:
        if stem not in by_name:
            raise SystemExit(f"tla: unknown config {stem}")
        if by_name[stem][1] not in targets:
            targets.append(by_name[stem][1])
    if not targets:
        print("tla: no configuration selected")
        return 0
    keys = {cfg: result_key(cfg) for cfg in targets}
    recalled = {} if arguments.fresh else {cfg: recall(key) for cfg, key in keys.items()}
    tlc = None
    if any(recalled.get(cfg) is None for cfg in targets):
        java, jar = install()
        tlc = Tlc(java, jar, arguments.timeout, arguments.heap)
    spec_for = {cfg: name for name, node in nodes.items() for cfg in node["configs"]}

    def one(cfg):
        try:
            expected = expectation(cfg)
        except HeaderError as error:
            return cfg.stem, spec_for[cfg], "missing", str(error), "", 0.0, "-", "NO HEADER"
        outcome, seconds, source = recalled.get(cfg), 0.0, "recorded"
        if outcome is None:
            outcome, seconds = tlc.check(cfg)
            source = "TLC"
            record(keys[cfg], outcome)
        floor = f" (>= {expected.floor} states)" if expected.floor != DEFAULT_FLOOR else ""
        states = "" if outcome.distinct is None else outcome.distinct
        return (cfg.stem, spec_for[cfg], expected.verdict + floor, outcome.verdict, states, seconds, source,
                judge(expected, outcome))

    started = time.time()
    with ThreadPoolExecutor(max_workers=arguments.jobs) as pool:
        futures = [pool.submit(one, cfg) for cfg in targets]
        for future in as_completed(futures):
            row = future.result()
            print(f"tla: {row[0]}: {row[3]}, {row[4]} distinct states ({row[5]:.0f}s {row[6]}, {row[7]})",
                  flush=True)
        rows = [future.result() for future in futures]
    report(rows)
    failed = [row for row in rows if row[7] != "ok"]
    checked = sum(1 for row in rows if row[6] == "TLC")
    print(f"tla: {len(rows) - len(failed)} of {len(rows)} configurations match their headers "
          f"({checked} checked by TLC, {len(rows) - checked} recorded) in {time.time() - started:.0f}s",
          flush=True)
    return 1 if failed else 0


def trace_bytes(tlc, spec, cfg):
    import tlc_trace

    outcome, _, output = tlc.execute(cfg)
    expected = expectation(cfg)
    if judge(expected, outcome) != "ok":
        raise ValueError(f"{cfg.name}: expected {expected}, got {outcome}")
    constants = cfg_sections(cfg.read_text())["CONSTANTS"]
    bindings = {}
    for name, equals, raw in zip(*[iter(constants)] * 3, strict=True):
        if equals != "=" or name in bindings:
            raise ValueError(f"{cfg.name}: unsupported constant {name} {equals} {raw}")
        bindings[name] = tlc_trace.value(raw)
    messages = []
    for code, kind, body in MESSAGE.findall(output):
        if kind == "4":
            if code != "2217":
                raise ValueError(f"{cfg.name}: unsupported TLC trace message {code}")
            messages.append(body)
    trace = dict(spec=spec, config=cfg.stem, expect=expected.verdict, constants=bindings,
                 steps=tlc_trace.steps(messages))
    return (json.dumps(trace, indent=2, sort_keys=True) + "\n").encode()


def traces(check):
    nodes = graph()
    tlc = Tlc(*install(), int(os.getenv("TLA_TIMEOUT", "1800")), os.getenv("TLA_HEAP", "4g"))
    directory = GENERATED / "traces"
    def generate(entry):
        spec, cfg = entry
        data = trace_bytes(tlc, spec, cfg)
        if not check and trace_bytes(tlc, spec, cfg) != data:
            raise ValueError(f"{cfg.name}: two TLC runs produced different traces")
        return directory / (cfg.stem + ".json"), data

    configs = [(spec, cfg) for spec in ("ClientSubmissionContention", "InterruptLatchContention")
               for cfg in nodes[spec]["configs"] if expectation(cfg).verdict.startswith("violates ")]
    with ThreadPoolExecutor(max_workers=int(os.getenv("TLA_JOBS", "4"))) as pool:
        outputs = dict(pool.map(generate, configs))
    obsolete = set(directory.glob("*.json")) - outputs.keys()
    if check:
        stale = obsolete | {p for p, data in outputs.items() if not p.exists() or p.read_bytes() != data}
        if stale:
            raise SystemExit(f"stale TLC traces: {sorted(p.name for p in stale)}; run make tla-traces")
    else:
        directory.mkdir(exist_ok=True)
        for path, data in outputs.items():
            path.write_bytes(data)
        for path in obsolete:
            path.unlink()
    print(f"tla: {len(outputs)} traces {'checked' if check else 'regenerated twice and compared'}")


def snapshot_functions():
    """The conformance snapshot's family keys, from the checked-in schema."""
    schema = json.loads((ROOT / "crates/gents-lean-vocab/families.schema.json").read_text())
    return set(schema["functions"])


def workspace_members():
    """The workspace's package names, from `cargo metadata`."""
    raw = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT,
                         check=True, stdout=subprocess.PIPE).stdout
    return {package["name"] for package in json.loads(raw)["packages"]}


def check_headers(tla=TLA, functions=None, members=None):
    """Every structure problem of the specs in `tla`; 1 when there is any."""
    functions = snapshot_functions() if functions is None else functions
    members = workspace_members() if members is None else members
    problems = structure_problems(functions, members, tla)
    for problem in problems:
        print(f"tla: {problem}", file=sys.stderr)
    specs = [path for path in tla.glob("*.tla") if not path.name.startswith("MC")]
    configurations = list(tla.glob("MC*.cfg"))
    print(f"tla: {len(specs)} specs and {len(configurations)} configurations checked, "
          f"{len(problems)} problems")
    return 1 if problems else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    runner = commands.add_parser("run")
    runner.add_argument("--all", action="store_true", help="every configuration")
    runner.add_argument("--spec", action="append", help="every configuration of this spec")
    runner.add_argument("--config", action="append", help="one configuration, by its file stem")
    runner.add_argument("--crate", action="append", help="specs whose header names this crate")
    runner.add_argument("--lean", action="append", help="specs whose lean or models line names this Lean owner")
    runner.add_argument("--files", help="specs a change to the listed repository paths selects")
    runner.add_argument("--timeout", type=int, default=int(os.environ.get("TLA_TIMEOUT", "1800")),
                        help="seconds per configuration; a timeout fails it")
    runner.add_argument("--jobs", type=int, default=int(os.environ.get("TLA_JOBS", "4")),
                        help="configurations checked at once, one TLC worker each")
    runner.add_argument("--heap", default=os.environ.get("TLA_HEAP", "4g"), help="TLC heap per configuration")
    runner.add_argument("--fresh", action="store_true",
                        help="run TLC on every selected configuration, ignoring recorded outcomes")
    chooser = commands.add_parser("select")
    chooser.add_argument("--files", required=True)
    commands.add_parser("check-headers")
    commands.add_parser("traces").add_argument("--check", action="store_true")
    commands.add_parser("install")
    arguments = parser.parse_args()
    if arguments.command == "run":
        sys.exit(run(arguments))
    if arguments.command == "select":
        files = [line.strip() for line in Path(arguments.files).read_text().splitlines() if line.strip()]
        print("\n".join(select(files)))
    elif arguments.command == "check-headers":
        sys.exit(check_headers())
    elif arguments.command == "traces":
        traces(arguments.check)
    elif arguments.command == "install":
        java, jar = install()
        print(f"java: {java}\ntla2tools: {jar}\njvm: {' '.join(JVM_OPTIONS)}")


if __name__ == "__main__":
    main()
