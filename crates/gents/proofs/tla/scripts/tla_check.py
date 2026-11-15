#!/usr/bin/env python3
"""Model-check TLA+ configurations and compare TLC's result with each header.

  tla_check.py run (--all | --spec S | --config C | --crate C | --lean O | --files F)...
  tla_check.py select --files F      print the specs a change selects
  tla_check.py check-headers         validate every spec and config header
  tla_check.py install               install the pinned JDK and tla2tools.jar

Header grammar (README.md). A config `MC*.cfg` (or `Sanity.cfg`) starts with
exactly one of

  \\* expect: holds
  \\* expect: deadlock
  \\* expect: violates <Name>

and a spec `X.tla` follows its `---- MODULE X ----` line with

  \\* lean: none | <Lean.owner>[, <Lean.owner>]...
  \\* crates: none | <crate>[, <crate>]...

TLC runs in breadth-first model-checking mode with one worker and a fixed
fingerprint polynomial, so the first violation it reports is reproducible.
"""

import argparse
import hashlib
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
EXPECT = re.compile(rf"\\\* expect: (holds|deadlock|violates ({IDENT}))")
MODULE = re.compile(rf"-{{4,}} MODULE ({IDENT}) -{{4,}}")
LEAN = re.compile(rf"\\\* lean: (none|{LEAN_NAME}(?:, {LEAN_NAME})*)")
CRATES = re.compile(rf"\\\* crates: (none|{CRATE}(?:, {CRATE})*)")

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


def expectation(cfg):
    lines = cfg.read_text().splitlines()
    match = EXPECT.fullmatch(lines[0]) if lines else None
    if not match:
        raise HeaderError(f"{cfg.name}: first line must be `\\* expect: holds|deadlock|violates <Name>`")
    sections = cfg_sections(cfg.read_text())
    if match.group(2):
        checked = sections.get("INVARIANT", []) + sections.get("PROPERTY", [])
        if match.group(2) not in checked:
            raise HeaderError(f"{cfg.name}: expects {match.group(2)} violated but does not check it")
        return f"violates {match.group(2)}"
    if match.group(1) == "deadlock" and sections.get("CHECK_DEADLOCK") == ["FALSE"]:
        raise HeaderError(f"{cfg.name}: expects a deadlock but sets CHECK_DEADLOCK FALSE")
    return match.group(1)


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
    return names(lean), names(crates)


def extends(module_file):
    """The modules `module_file` extends or instantiates."""
    text = strip_comments(module_file.read_text())
    names = []
    for match in re.finditer(rf"\bEXTENDS\s+({IDENT}(?:\s*,\s*{IDENT})*)", text):
        names += re.findall(IDENT, match.group(1))
    return names + re.findall(rf"\bINSTANCE\s+({IDENT})", text)


def specs():
    """Spec name -> path: every module that is not a model config's wrapper."""
    return {path.stem: path for path in sorted(TLA.glob("*.tla")) if not path.stem.startswith("MC")}


def configs():
    return sorted(TLA.glob("*.cfg"))


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
    return [name for name in extends(spec) if (GENERATED / f"{name}.tla").exists()]


def graph():
    """Spec name -> {path, lean, crates, configs, generated}."""
    known = specs()
    nodes = {}
    for name, path in known.items():
        lean, crates = spec_header(path)
        nodes[name] = {"path": path, "lean": lean, "crates": crates, "configs": [],
                       "generated": generated_imports(path)}
    for cfg in configs():
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


def structure_problems(functions, members):
    """Header and import problems; `functions` are the snapshot's family keys."""
    try:
        nodes = graph()
    except HeaderError as error:
        return [str(error)]
    problems = []
    for name, node in sorted(nodes.items()):
        for owner in node["lean"]:
            if wire_key(owner) not in functions:
                problems.append(f"{name}.tla: Lean owner {owner} is not in the conformance snapshot "
                                f"(no family {wire_key(owner)})")
        for crate in node["crates"]:
            if crate not in members:
                problems.append(f"{name}.tla: {crate} is not a workspace package")
        for module in node["generated"]:
            clash = definitions(node["path"]) & definitions(GENERATED / f"{module}.tla")
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
    """Whether Lean source `text` declares or exports `owner` (namespace then name)."""
    parts = owner.split(".")
    if not re.search(rf"^\s*namespace\s+{re.escape(parts[0])}(\.|\s|$)", text, re.M):
        return False
    tails = {".".join(parts[index:]) for index in range(1, len(parts))}
    for tail in tails:
        name = re.escape(tail)
        if re.search(rf"^\s*(?:@\[[^\]]*\]\s*)?(?:private\s+|protected\s+|noncomputable\s+)*"
                     rf"(?:def|abbrev|theorem|lemma|inductive|structure)\s+{name}(?![\w.'?!])", text, re.M):
            return True
        if re.search(rf"^\s*attribute\s+\[conformance_function\][^\n]*(?<![\w.]){name}(?![\w.'?!])", text, re.M):
            return True
    return False


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
        if path.startswith(tla + "generated/"):
            module = Path(path).stem
            chosen |= {name for name, node in nodes.items() if module in node["generated"]}
        elif path.startswith(tla) and "/" not in path[len(tla):]:
            chosen |= by_file.get(path[len(tla):], set())
        elif path.startswith(lean) and path.endswith(".lean"):
            source = ROOT / path
            text = source.read_text() if source.exists() else ""
            chosen |= {name for name, node in nodes.items()
                       if any(lean_declares(text, owner) for owner in node["lean"])}
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


def install():
    """The pinned java binary and tla2tools.jar, installed under the user cache when absent."""
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
    if not java.exists():
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
        if home.exists():
            shutil.rmtree(staging)
        else:
            unpacked.rename(home)
            shutil.rmtree(staging)
        tarball.unlink()
    return java, jar


# ---- running TLC ----

MESSAGE = re.compile(r"@!@!@STARTMSG (\d+):(\d+) @!@!@\n(.*?)@!@!@ENDMSG \1 @!@!@", re.S)
INVARIANT = re.compile(rf"Invariant ({IDENT}) is violated")
ACTION = re.compile(rf"Action property ({IDENT}) is violated")
TEMPORAL = re.compile(rf"Temporal property ({IDENT}) was violated")
SUCCESS = 2193
DEADLOCK = 2114
TEMPORAL_UNNAMED = 2116
ERROR_CLASS = 1


def parse(output, status):
    """TLC's `-tool` output -> holds | deadlock | violates <Name> | violates ? | error: <text>."""
    messages = [(int(code), int(kind), body.strip()) for code, kind, body in MESSAGE.findall(output)]
    for code, _, body in messages:
        for pattern in (INVARIANT, ACTION, TEMPORAL):
            match = pattern.search(body)
            if match:
                return f"violates {match.group(1)}"
        if code == TEMPORAL_UNNAMED:
            return "violates ?"
        if code == DEADLOCK:
            return "deadlock"
    if status == 0 and any(code == SUCCESS for code, _, _ in messages):
        return "holds"
    errors = [body for _, kind, body in messages if kind == ERROR_CLASS and body]
    plain = [line for line in MESSAGE.sub("", output).splitlines() if line.strip()]
    detail = errors[-1] if errors else "\n".join(plain[-3:]) or f"TLC exited {status} with no output"
    return "error: " + " ".join(detail.split())[:240]


def without_checks(text, keep_property):
    """`text` with its INVARIANT and PROPERTY sections replaced by one PROPERTY."""
    keywords = "|".join(CFG_KEYWORDS)
    body = strip_comments(text)
    body = re.sub(rf"(?ms)^\s*(?:INVARIANTS?|PROPERTY|PROPERTIES)\b.*?(?=^\s*(?:{keywords})\b|\Z)", "", body)
    return body.rstrip() + f"\nPROPERTY {keep_property}\n"


class Tlc:
    def __init__(self, java, jar, timeout, heap):
        self.java, self.jar, self.timeout, self.heap = java, jar, timeout, heap

    def run(self, cfg, text=None):
        """Model-check one config; `text` replaces its contents. Returns (result, seconds)."""
        with tempfile.TemporaryDirectory(prefix="tlc-") as scratch:
            scratch = Path(scratch)
            config = cfg
            if text is not None:
                config = scratch / cfg.name
                config.write_text(text)
            command = [str(self.java), f"-Xmx{self.heap}", "-XX:+UseParallelGC",
                       f"-Djava.io.tmpdir={scratch}", f"-DTLA-Library={GENERATED}",
                       "-cp", str(self.jar), "tlc2.TLC", "-tool", "-workers", "1", "-fp", "1",
                       "-metadir", str(scratch / "states"), "-config", str(config), cfg.stem]
            started = time.time()
            try:
                done = subprocess.run(command, cwd=TLA, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      text=True, timeout=self.timeout)
            except subprocess.TimeoutExpired:
                return f"timeout after {self.timeout}s", time.time() - started
            return parse(done.stdout, done.returncode), time.time() - started

    def check(self, cfg):
        result, seconds = self.run(cfg)
        if result == "violates ?":
            # TLC 1.7.4 does not name a violated temporal property: check each
            # alone, in config order, and report the first that fails.
            text = cfg.read_text()
            properties = cfg_sections(text).get("PROPERTY", [])
            if len(properties) == 1:
                return f"violates {properties[0]}", seconds
            result = "error: a temporal property was violated but none fails alone"
            for name in properties:
                alone, extra = self.run(cfg, without_checks(text, name))
                seconds += extra
                if alone.startswith("violates"):
                    result = f"violates {name}"
                    break
        return result, seconds


def report(rows):
    lines = ["| config | spec | expected | actual | seconds | verdict |", "|---|---|---|---|---|---|"]
    lines += [f"| {cfg} | {spec} | {expected} | {actual} | {seconds:.0f} | {verdict} |"
              for cfg, spec, expected, actual, seconds, verdict in rows]
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
    chosen |= {name for name, node in nodes.items() if set(arguments.lean or []) & set(node["lean"])}
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
    java, jar = install()
    tlc = Tlc(java, jar, arguments.timeout, arguments.heap)
    spec_for = {cfg: name for name, node in nodes.items() for cfg in node["configs"]}

    def one(cfg):
        try:
            expected = expectation(cfg)
        except HeaderError as error:
            return cfg.stem, spec_for[cfg], "missing", str(error), 0.0, "NO HEADER"
        actual, seconds = tlc.check(cfg)
        return cfg.stem, spec_for[cfg], expected, actual, seconds, "ok" if actual == expected else "MISMATCH"

    started = time.time()
    with ThreadPoolExecutor(max_workers=arguments.jobs) as pool:
        futures = [pool.submit(one, cfg) for cfg in targets]
        for future in as_completed(futures):
            row = future.result()
            print(f"tla: {row[0]}: {row[3]} ({row[4]:.0f}s, {row[5]})", flush=True)
        rows = [future.result() for future in futures]
    report(rows)
    failed = [row for row in rows if row[5] != "ok"]
    print(f"tla: {len(rows) - len(failed)} of {len(rows)} configurations match their headers "
          f"in {time.time() - started:.0f}s", flush=True)
    return 1 if failed else 0


def check_headers():
    problems = []
    try:
        nodes = graph()
    except HeaderError as error:
        print(f"tla: {error}", file=sys.stderr)
        return 1
    for node in nodes.values():
        for cfg in node["configs"]:
            try:
                expectation(cfg)
            except HeaderError as error:
                problems.append(str(error))
    for problem in problems:
        print(f"tla: {problem}", file=sys.stderr)
    return 1 if problems else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    runner = commands.add_parser("run")
    runner.add_argument("--all", action="store_true", help="every configuration")
    runner.add_argument("--spec", action="append", help="every configuration of this spec")
    runner.add_argument("--config", action="append", help="one configuration, by its file stem")
    runner.add_argument("--crate", action="append", help="specs whose header names this crate")
    runner.add_argument("--lean", action="append", help="specs whose header names this Lean owner")
    runner.add_argument("--files", help="specs a change to the listed repository paths selects")
    runner.add_argument("--timeout", type=int, default=int(os.environ.get("TLA_TIMEOUT", "1800")),
                        help="seconds per configuration; a timeout fails it")
    runner.add_argument("--jobs", type=int, default=int(os.environ.get("TLA_JOBS", "4")),
                        help="configurations checked at once, one TLC worker each")
    runner.add_argument("--heap", default=os.environ.get("TLA_HEAP", "4g"), help="TLC heap per configuration")
    chooser = commands.add_parser("select")
    chooser.add_argument("--files", required=True)
    commands.add_parser("check-headers")
    commands.add_parser("install")
    arguments = parser.parse_args()
    if arguments.command == "run":
        sys.exit(run(arguments))
    if arguments.command == "select":
        files = [line.strip() for line in Path(arguments.files).read_text().splitlines() if line.strip()]
        print("\n".join(select(files)))
    elif arguments.command == "check-headers":
        sys.exit(check_headers())
    elif arguments.command == "install":
        java, jar = install()
        print(f"java: {java}\ntla2tools: {jar}")


if __name__ == "__main__":
    main()
