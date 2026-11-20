#!/usr/bin/env python3
"""The pull request gate, shared by CI and `make gate`.

  gate.py plan [--base REF | --files FILE]   select jobs for a change
  gate.py run JOB                            run one job (fmt, lint, contract, ts-*, test:<group>,
                                             build:<archive>, shard:<archive>[:<index>])
  gate.py all [--base REF]                   plan, then run every selected job
  gate.py packages GROUP...                  list the crates of test groups
  gate.py verdict                            decide the `gate` job from NEEDS

`run` reads the plan from GATE_PLAN (CI passes the plan job's output) or
computes it against --base.
"""

import argparse
import functools
import importlib.util
import json
import os
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from xml.etree import ElementTree

ROOT = Path(__file__).resolve().parents[2]
PROOFS = "crates/gents/proofs"
TLA_CHECK = ROOT / PROOFS / "tla/scripts/tla_check.py"


@functools.cache
def load_tla_check():
    """Loaded on first use: the verdict job checks out only scripts/ci."""
    spec = importlib.util.spec_from_file_location("tla_check", TLA_CHECK)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def __getattr__(name):
    if name == "tla_check":
        return load_tla_check()
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")

# Roles -> runner labels: the one inventory of CI runners. Jobs select runners
# by these capability labels only, never by host name, so new machines join by
# registering a runner with a role's labels.
INVENTORY = ROOT / ".github/ci-runners.json"
# Roles the CI workflow's non-matrix jobs run on.
WORKFLOW_ROLES = ("linux", "browser", "macos")
HOSTED_LINUX = "ubuntu-latest"


class Group:
    def __init__(self, name, runner, packages, threads=None, nocapture=False,
                 isolate_target=False, timeout=120, macos=None):
        self.name = name
        self.runner = runner
        self.packages = packages
        self.threads = threads
        self.nocapture = nocapture
        self.isolate_target = isolate_target
        self.timeout = timeout
        # The macOS role the scheduled run also runs this portable group on, uncached.
        self.macos = macos


# Every workspace member belongs to exactly one group; `plan` fails otherwise.
# "portable" groups run on Linux, where the archived run joins the test-result
# cache. A portable crate's tests that compile only on macOS run in the
# `native` group (`macos_only_tests`), and the scheduled run also runs the
# groups that have `macos` there, uncached, so filesystem and path differences
# between the platforms still surface daily. `native` holds the crates that need
# macOS: the service manager owner, whose tests drive the real one, and the
# Tauri crates, which need WebKitGTK on Linux.
GROUPS = [
    Group("foundation", "portable", [
        "gents-protocol", "gents-schemas", "gents-loop", "gents-lean-contract",
        "gents-lean-vocab",
        "gents-callback-fixture-create-workspace", "gents-claude", "gents-claude-login",
        "gents-chatgpt-login", "gents-login-ui", "gents-fs-runner", "gents-shell",
        "gents-core-packs", "gents-workspace-hack",
    ], timeout=60),
    Group("store", "portable", [
        "gents-store", "gents-documents", "gents-inference", "gents-p2p", "gents-pack",
    ], timeout=90),
    Group("session", "portable", [
        "gents-session", "gents-tools", "gents-toolcalls", "gents-automation", "gents-observe",
    ], timeout=120),
    Group("runtime", "portable", ["gents", "gents-eval"], timeout=150, macos="runtime"),
    # CLI tests spawn Cargo for plugin builds, which must not inherit the
    # job's target directory. They run at the default thread count (#3778).
    Group("cli", "portable", ["gents-cli", "gents-host"],
          nocapture=True, isolate_target=True, timeout=180, macos="cli"),
    Group("desktop", "portable", [
        "gents-desktop-core", "gents-desktop", "gents-client-host", "gents-media-host", "fixture-domain-plugin",
    ], timeout=150, macos="desktop"),
    Group("native", "desktop", [
        "gents-service", "gents-desktop-bridge", "gents-fixture-host", "gents-desktop-tauri",
    ], nocapture=True, isolate_target=True, timeout=150),
]
NATIVE = "native"
# The `native` group's second job: the macOS-only tests of portable crates and
# the native client journey, beside the job that tests the `native` crates.
MACOS_ONLY = "macos-only"
# Crates whose change selects the native client journey, which runs in `native`.
JOURNEY = {"gents-cli", "gents-client-host", "gents-media-host"}
# Portable crates whose tests need the Afterburner compilers (Go, Javy, Binaryen).
AFTERBURNER = {"gents-cli"}

# The system libraries a Linux build of the portable crates links, by the
# pkg-config module that finds each (module -> Ubuntu package): Tauri's GTK and
# WebKitGTK (gents-client-host, fixture-domain-plugin), ALSA (gents-media-host)
# and the rest of the portable hosts' provisioning list (amygdala
# infra/services/github-runners/provision-ghrunner-root.sh). `system_preflight`
# names any that is missing before a build, instead of a -sys crate's build
# script failing deep inside it.
LINUX_SYSTEM_LIBRARIES = {
    "gtk+-3.0": "libgtk-3-dev",
    "webkit2gtk-4.1": "libwebkit2gtk-4.1-dev",
    "javascriptcoregtk-4.1": "libjavascriptcoregtk-4.1-dev",
    "libsoup-3.0": "libsoup-3.0-dev",
    "librsvg-2.0": "librsvg2-dev",
    "ayatana-appindicator3-0.1": "libayatana-appindicator3-dev",
    "alsa": "libasound2-dev",
    "libudev": "libudev-dev",
    "libxml-2.0": "libxml2-dev",
    "zlib": "zlib1g-dev",
    "ncurses": "libncurses-dev",
}

PACKAGE_FEATURES = {
    "gents-claude": "gents-claude/transport",
    "gents-protocol": "gents-protocol/typescript",
    "gents-desktop-bridge": "notify/macos_kqueue",
}

# Tests behind a crate's `vera` feature: package -> libtest name filter for
# its lib target. The feature brings in a Vera node and changes what other
# crates see of gents-store, so these run in their own build beside the
# package's or the archive's, never by enabling it there.
VERA_TESTS = {
    "gents-store": "local::document_acp::",
    "gents-tools": "surface::source_policy::embedded_vera_tests::",
    "gents-toolcalls": "call::lifecycle::recovery::vera_tests::",
}

# Packages with no test of their own. The list only shrinks: a listed package
# that runs a test fails the gate, as does an unlisted one that runs none.
NO_TESTS = {
    "fixture-domain-plugin",
    "gents-callback-fixture-create-workspace",
    "gents-core-packs",
    "gents-desktop",
    "gents-workspace-hack",
}

# With GATE_PORTABLE_SHARDS above zero the portable groups' crates build once
# into one nextest archive (`build:portable`) and `shard:portable` runs it as
# that many slices on their own runners, compiling nothing. Test binaries must
# then find source files and binaries through the runtime CARGO_MANIFEST_DIR,
# NEXTEST_WORKSPACE_ROOT and CARGO_BIN_EXE_<name>: the build checkout is gone.
ARCHIVE = "portable"
ARCHIVE_SOURCE = "gents-archive-src"
# The archive's Cargo profile unless PROFILE names another: workspace crates at
# opt-level 0, dependencies at 1. Against `test` (opt-level 1 throughout) a cold
# build of gents, gents-cli and gents-desktop-core spent 629 rather than 1,165
# unit-seconds on workspace crates and compiled the gents lib test target in 51
# rather than 140 s, while gents-desktop-core's 363 tests took 122.9 s against
# 122.4 s (M-series laptop, 10 jobs, 2026-11-17).
ARCHIVE_PROFILE = "ci-native"
NEXTEST = "cargo-nextest"

# A change here can alter any build or the gate itself, so it selects everything.
# Every script a selected job runs is here, under CONTRACT_PATHS or TS_PATHS;
# test_gate.py fails when one would classify as inert.
GLOBAL_PATHS = (
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "clippy.toml", "rustfmt.toml",
    ".cargo/", "Makefile", "scripts/ci/", ".github/workflows/ci.yml", ".github/actions/",
    ".github/scripts/setup-", ".github/scripts/rustc-direct.sh",
    ".github/scripts/ensure-rust-build-space.py", ".github/scripts/desktop-runtime-journey.sh",
    ".github/scripts/show-sccache-stats.sh", ".github/scripts/check-isolated-runtime-runner.py",
    ".github/scripts/host-runtime-service.sh",
    "scripts/macos-clang-lld.sh", "scripts/linux-lld.sh", "scripts/measure-gents-binary.sh",
    "scripts/check-native-e2e-capabilities.sh", "third_party/", "examples/",
)
CONTRACT_PATHS = (
    "contracts/", ".github/scripts/check-lean-", ".github/scripts/test-lean-",
    ".github/scripts/lean-import-layers.json", ".github/scripts/report-lean-import-layers.py",
    ".github/scripts/check-canonical-output-map.py", ".github/scripts/test-canonical-output-map.py",
)
TS_PATHS = (
    "apps/", "packages/", "package.json", "package-lock.json", "scripts/check-desktop-package-boundaries.mjs",
    "scripts/clean-desktop-package-dist.mjs", "scripts/pack-desktop-packages.sh",
    "scripts/packed-desktop-consumer/", ".github/scripts/install-browser-deps.sh",
)
CORE_PACK_PATHS = ("core-packs/", "crates/gents-core-packs/", "crates/gents-pack/")
# Checked by the hosted control jobs, which run on every pull request.
CONTROL_PATHS = (".github/", "scripts/")
INERT_PATHS = ("docs/", "release/", "LICENSE", ".gitignore", ".gitattributes", ".gate/")

TS_JOBS = {
    "ts-packages": [
        "npm run check:package-boundaries",
        "npm run build:packages",
        "npm run test --workspace @source-inc/gents-desktop-client",
        "npm run test --workspace @source-inc/gents-desktop-ui",
        "npm run test --workspace @source-inc/gents-desktop-fleet",
        "npm run test --workspace @source-inc/gents-desktop-operations",
        "npm run build --workspace @source-inc/fixture-host-ui",
        "./scripts/pack-desktop-packages.sh target/desktop-npm-ci",
    ],
    "ts-unit": [
        "npm run build:packages",
        "npm --prefix apps/gents-desktop run format:check",
        "npm --prefix apps/gents-desktop run typecheck",
        "npm --prefix apps/gents-desktop run build",
        "npm --prefix apps/gents-desktop test",
        # the interaction budgets drive Chromium; they run here, beside the
        # browser lane's shards, rather than after one of them
        "npm --prefix apps/gents-desktop run perf:mobile -- --runs=3 --output=test-results/mobile-performance/ci",
    ],
    "ts-browser": [
        "npm run build:packages",
        "npm --prefix apps/gents-desktop run build",
        "npm --prefix apps/gents-desktop run test:ui:e2e",
    ],
    "ts-fuzz": [
        "npm run build:packages",
        "npm --prefix apps/gents-desktop run test:ui:fuzz -- --time-limit 30s",
    ],
}
TS_LEAN = [
    "npm run build --workspace @source-inc/gents-desktop-client",
    "npm run build --workspace @source-inc/gents-desktop-ui",
    "npm run build --workspace @source-inc/gents-desktop-chat",
    "npm run build --workspace @source-inc/gents-desktop-fleet",
    "npm test --workspace @source-inc/gents-desktop-chat",
    "npm run test:lean-conformance --workspace @source-inc/gents-desktop-app",
]


def fail(message):
    print(f"gate: {message}", file=sys.stderr)
    sys.exit(2)


def workspace():
    """Member name -> (directory, workspace dependencies of every kind)."""
    return parse_workspace(workspace_metadata())


@functools.cache
def workspace_metadata():
    raw = subprocess.run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
                         cwd=ROOT, check=True, stdout=subprocess.PIPE).stdout
    return json.loads(raw)


def parse_workspace(metadata):
    root = metadata["workspace_root"].rstrip("/") + "/"
    names = {package["name"] for package in metadata["packages"]}
    members = {}
    for package in metadata["packages"]:
        directory = os.path.dirname(package["manifest_path"])[len(root):]
        deps = {dep["name"] for dep in package["dependencies"]
                if dep.get("path") and dep["name"] in names and dep["name"] != package["name"]}
        members[package["name"]] = (directory, deps)
    return members


def check_groups(members):
    assigned = [package for group in GROUPS for package in group.packages]
    duplicates = sorted({package for package in assigned if assigned.count(package) > 1})
    missing = sorted(set(members) - set(assigned))
    unknown = sorted(set(assigned) - set(members))
    if duplicates or missing or unknown:
        fail(f"GROUPS in scripts/ci/gate.py must hold each workspace member once: "
             f"missing {missing}, unknown {unknown}, duplicated {duplicates}")
    stale = sorted(NO_TESTS - set(members))
    if stale:
        fail(f"NO_TESTS names packages outside the workspace: {stale}")


def dependents(members, changed):
    reverse = {name: set() for name in members}
    for name, (_, deps) in members.items():
        for dep in deps:
            reverse[dep].add(name)
    affected, queue = set(changed), list(changed)
    while queue:
        for user in reverse[queue.pop()]:
            if user not in affected:
                affected.add(user)
                queue.append(user)
    return affected


def under(path, prefixes):
    return any(path == prefix or path.startswith(prefix) for prefix in prefixes)


def crate_of(path, members):
    """The workspace member whose directory holds `path`, if any."""
    return max((name for name, (directory, _) in members.items() if path.startswith(directory + "/")),
               key=lambda name: len(members[name][0]), default=None)


def classify(path, members):
    """One file -> the tags it selects."""
    tags = set()
    if under(path, CORE_PACK_PATHS):
        tags.add("core-packs")
    if under(path, GLOBAL_PATHS):
        return tags | {"all"}
    if path.startswith(PROOFS + "/"):
        return tags | {"proofs"}
    if under(path, CONTRACT_PATHS):
        return tags | {"contract"}
    owner = crate_of(path, members)
    if owner:
        return tags | {f"crate:{owner}"}
    if under(path, TS_PATHS):
        return tags | {"ts"}
    if tags or under(path, CONTROL_PATHS) or under(path, INERT_PATHS) or ("/" not in path and path.endswith(".md")):
        return tags | {"inert"}
    return {"all"}


def inventory():
    return json.loads(INVENTORY.read_text())


def runner(role, roles=None, fork=None, fallback=None, host_group=None):
    """The runs-on value for a role: its labels, a hosted label for a fork pull
    request (never a self-hosted runner) or a role whose pool is still hosted,
    or the role's fallback while the repository variable GENTS_CI_FALLBACK is
    `hosted`. `host_group` adds one of the role's `host_groups` labels to its
    own labels, pinning the job to that group's hosts."""
    return placement(role, roles, fork, fallback, host_group)["runs_on"]


def placement(role, roles=None, fork=None, fallback=None, host_group=None):
    """Where a job of `role` runs (`runs_on`) and the Cargo build jobs and test
    threads its runner may use (empty: the workflow's defaults). The role's
    `jobs` divide a host's cores between the runners it registers, so its
    concurrent jobs cannot oversubscribe it."""
    roles = roles or inventory()["roles"]
    labels = route(role, roles, fork, fallback)
    entry = roles.get(role, {})
    if labels == entry.get("labels"):
        groups = entry.get("host_groups", [])
        if host_group is not None:
            if host_group not in groups:
                fail(f"role {role!r} has no host group {host_group!r} in {INVENTORY.relative_to(ROOT)}")
            labels = [*labels, host_group]
        jobs = entry.get("jobs", {})
    else:
        target = entry.get("fallback")
        jobs = roles[target].get("jobs", {}) if labels == roles.get(target, {}).get("labels") else {}
    return {"runs_on": labels, "build_jobs": str(jobs.get("build", "")), "test_threads": str(jobs.get("test", ""))}


def host_groups(role, roles=None, fork=None, fallback=None):
    """The role's host groups when its jobs run on its own runners, else none."""
    roles = roles or inventory()["roles"]
    entry = roles[role]
    return entry.get("host_groups", []) if route(role, roles, fork, fallback) == entry["labels"] else []


def partitions(role, roles=None, fork=None, fallback=None):
    """How many hash partitions the role's in-build slice runs as: its
    `partitions` on its own runners, else one."""
    roles = roles or inventory()["roles"]
    entry = roles[role]
    return entry.get("partitions", 1) if route(role, roles, fork, fallback) == entry["labels"] else 1


def route(role, roles, fork=None, fallback=None):
    """The labels a role's job runs on, before any host group."""
    fork = os.environ.get("GATE_FORK") == "true" if fork is None else fork
    fallback = os.environ.get("GATE_FALLBACK") == "hosted" if fallback is None else fallback
    if role not in roles:
        fail(f"no runner role {role!r} in {INVENTORY.relative_to(ROOT)}")
    entry = roles[role]
    # `"pool": "hosted"` keeps a role on its hosted runner until runners with
    # its labels exist (the browser role waits for its hosts' system libraries).
    if fork or entry.get("pool") == "hosted":
        return entry.get("hosted", HOSTED_LINUX)
    overrides = os.environ.get("GATE_RUNNER_OVERRIDES", "").strip()
    if overrides:
        try:
            override = json.loads(overrides).get(role)
        except (json.JSONDecodeError, AttributeError):
            override = None
            fail(f"GATE_RUNNER_OVERRIDES must be a JSON object of role to label list, got {overrides!r}")
        if override is not None:
            if not (isinstance(override, list) and override and all(isinstance(label, str) and label for label in override)):
                fail(f"GATE_RUNNER_OVERRIDES[{role!r}] must be a non-empty JSON list of labels")
            return override
    if fallback and "fallback" in entry:
        target = entry["fallback"]
        return route(target, roles, fork, False) if target in roles else target
    return entry["labels"]


# ---- tests that compile only on macOS ----

CFG_TOKEN = re.compile(r'\s*(?:([A-Za-z_]\w*)\s*(?:=\s*"([^"]*)")?|([(),]))')


def cfg_holds(expression, platform, features):
    """Evaluate a `cfg(...)` predicate for `linux` (the portable runners) or
    `macos` (the Studios). Features are on only when the gate enables them."""
    tokens, position = [], 0
    while position < len(expression):
        match = CFG_TOKEN.match(expression, position)
        if not match or match.end() == position:
            break
        tokens.append(match.groups())
        position = match.end()
    tokens.reverse()

    def value():
        name, literal, punctuation = tokens.pop()
        if name in ("any", "all", "not") and tokens and tokens[-1][2] == "(":
            tokens.pop()
            items = []
            while tokens and tokens[-1][2] != ")":
                items.append(value())
                if tokens and tokens[-1][2] == ",":
                    tokens.pop()
            if tokens:
                tokens.pop()
            return any(items) if name == "any" else all(items) if name == "all" else not items[0]
        if literal is not None:
            if name == "target_os":
                return literal == platform
            if name == "target_vendor":
                return (literal == "apple") == (platform == "macos")
            if name == "target_arch":
                return literal == ("aarch64" if platform == "macos" else "x86_64")
            if name == "target_family":
                return literal == "unix"
            if name == "feature":
                return literal in features
            return False
        return name not in ("windows", "miri")

    return value() if tokens else True


CFG_ATTRIBUTE = re.compile(r"#!?\[cfg\((.*)\)\]\s*$", re.S)
PATH_ATTRIBUTE = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')
TEST_ATTRIBUTE = re.compile(r"#\[(?:\w+::)*test\b")
ITEM = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*(fn|mod)\s+(\w+)\s*([;{(<])?")


def cfg_of(attribute):
    match = CFG_ATTRIBUTE.match(" ".join(attribute.split()))
    return match.group(1) if match else None


def compiled(attributes, platform, features):
    return all(cfg_holds(condition, platform, features)
               for condition in map(cfg_of, attributes) if condition is not None)


@functools.cache
def macos_only_tests(package):
    """The tests of a crate that compile only on macOS: cargo test target
    arguments -> libtest name filters (an empty list runs the whole target).

    Each test target's module tree is walked from its root, as rustc would on
    macOS with the gate's features: a test function, a module or a file under
    `cfg` that holds on macOS and not on Linux is selected. A filter is a name
    substring, so it may also select a same-named test elsewhere in the target.
    """
    package_info = next(entry for entry in workspace_metadata()["packages"] if entry["name"] == package)
    features = {feature.split("/", 1)[1] for feature in PACKAGE_FEATURES.get(package, "").split(",")
                if feature.startswith(package + "/")}
    found = {}
    for target in package_info["targets"]:
        kinds = set(target["kind"])
        if not kinds & {"lib", "bin", "test"} or not set(target.get("required-features") or []) <= features:
            continue
        arguments = ("--lib",) if "lib" in kinds else ("--bin", target["name"]) if "bin" in kinds \
            else ("--test", target["name"])
        filters = set()
        walk_module(Path(target["src_path"]), True, features, filters, set())
        if filters:
            found[arguments] = [] if "" in filters else sorted(filters)
    return found


def walk_module(path, owns_directory, features, filters, seen):
    """Collect the macOS-only tests of one module file and the modules it declares."""
    if path in seen or not path.is_file():
        return
    seen.add(path)
    lines = path.read_text(errors="replace").splitlines()
    inner = [line.strip() for line in lines[:40] if line.strip().startswith("#![cfg(")]
    if not compiled(inner, "macos", features):
        return
    if not compiled(inner, "linux", features):
        filters.add("" if owns_directory and path.name != "mod.rs" and path.parent.name in ("tests", "src")
                    else (path.parent.name if path.name == "mod.rs" else path.stem) + "::")
        return
    directory = path.parent if owns_directory or path.name == "mod.rs" else path.parent / path.stem
    attributes, pending = [], ""
    for line in lines:
        text = line.strip()
        if pending or (text.startswith("#[") and not text.startswith("#![")):
            pending += " " + text
            if pending.count("[") <= pending.count("]"):
                attributes.append(pending.strip())
                pending = ""
            continue
        if not text or text.startswith("//"):
            continue
        item = ITEM.match(line)
        if item and compiled(attributes, "macos", features):
            kind, name, end = item.groups()
            only = not compiled(attributes, "linux", features)
            if kind == "mod" and only:
                filters.add(name + "::")
            elif kind == "mod" and end == ";":
                explicit = next((match.group(1) for match in map(PATH_ATTRIBUTE.match, attributes) if match), None)
                if explicit:
                    walk_module(path.parent / explicit, False, features, filters, seen)
                else:
                    child = directory / f"{name}.rs"
                    walk_module(child if child.is_file() else directory / name / "mod.rs", False, features,
                                filters, seen)
            elif kind == "fn" and only and any(TEST_ATTRIBUTE.match(attribute) for attribute in attributes):
                filters.add(name)
        attributes = []


def tla_specs(files, members, everything):
    """The TLA+ specs a change selects by their headers: every spec when it
    changes everything or a header does not parse (the runner then reports it)."""
    tla_check = load_tla_check()
    try:
        nodes = tla_check.graph()
    except tla_check.HeaderError:
        return ["*"]
    if everything:
        return sorted(nodes)
    return tla_check.select(files, nodes, lambda path: crate_of(path, members))


def portable_shards():
    """GATE_PORTABLE_SHARDS: 0 or empty keeps one job per portable group."""
    raw = os.environ.get("GATE_PORTABLE_SHARDS", "").strip()
    if not raw:
        return 0
    if not raw.isdigit() or int(raw) > 32:
        fail(f"GATE_PORTABLE_SHARDS must be a whole number from 0 to 32, got {raw!r}")
    return int(raw)


def vera_tests(packages):
    """The packages whose `vera` tests a job runs, space-separated: none on a
    pull request, whose runs leave the Vera node's build to the merge queue."""
    if os.environ.get("GATE_EVENT") == "pull_request":
        return ""
    return " ".join(package for package in packages if package in VERA_TESTS)


def test_entry(group, job, role, packages, go, **extra):
    """One `test` matrix job: `job` is the group, or `<group>@macos` for a portable
    group's scheduled macOS run."""
    if role == "runtime" and os.environ.get("GATE_RUNTIME_ISOLATED") == "true":
        role = "runtime-isolated"
    return {"job": f"test:{job}", "group": group.name, "role": role, **placement(role),
            "packages": " ".join(packages), "vera": vera_tests(packages), "timeout": group.timeout, "go": go,
            **extra}


def balance(entries):
    """Spread portable test jobs over the role's host groups, longest first onto
    the group with the least work (by timeout): GitHub hands queued jobs to any
    matching runner, which can put a whole run on one host while another idles."""
    groups = host_groups("portable")
    if not groups:
        return
    load = {group: 0 for group in groups}
    for entry in sorted(entries, key=lambda entry: (-entry["timeout"], entry["job"])):
        group = min(groups, key=lambda name: (load[name], groups.index(name)))
        load[group] += entry["timeout"]
        entry.update(placement("portable", host_group=group))


def plan(files, members, everything=False, only=None, tier="full", mirrors=False):
    check_groups(members)
    tags, reasons = set(), {}
    for path in files:
        for tag in classify(path, members):
            tags.add(tag)
            reasons.setdefault(tag, path)
    everything = everything or "all" in tags
    changed = set(members) if everything else {tag[6:] for tag in tags if tag.startswith("crate:")}
    built = dependents(members, changed)
    rust = bool(changed)
    # Proofs change what the contract's consumers replay, not what compiles.
    affected = dependents(members, changed | {"gents-lean-contract"}) if "proofs" in tags else built
    # A draft tests the crates it changes; their dependents are still compiled
    # and linted, and tested once the pull request is ready for review.
    tested = affected if tier == "full" or "proofs" in tags else changed
    ts = everything or "ts" in tags
    proofs = everything or "proofs" in tags
    tla = tla_specs(files, members, everything)
    jobs = {
        "fmt": rust,
        "lint": rust,
        "contract": rust or ts or proofs or "contract" in tags or bool(tla),
        "ts-packages": ts,
        "ts-unit": ts,
        "ts-browser": ts,
        "ts-fuzz": ts,
    }
    portable = [group for group in GROUPS if group.runner == "portable"]
    portable_tested = [package for group in portable for package in group.packages if package in tested]
    matrix = []
    for group in GROUPS:
        selected = [package for package in group.packages if package in tested]
        if selected:
            matrix.append(test_entry(group, group.name, group.runner, selected, bool(AFTERBURNER & set(selected))))
        if group.name == NATIVE:
            # A second macOS job, so the native crates and the ported tests build and run side by side.
            macos = [package for package in portable_tested if macos_only_tests(package)]
            journey = bool(JOURNEY & set(tested))
            if macos or journey:
                matrix.append(test_entry(group, MACOS_ONLY, group.runner, [], bool(AFTERBURNER & set(macos)),
                                         macos_tests=macos, journey=journey))
    for group in portable:
        selected = [package for package in group.packages if package in tested]
        if mirrors and group.macos and selected:
            matrix.append(test_entry(group, f"{group.name}@macos", group.macos, selected,
                                     bool(AFTERBURNER & set(selected))))
    for entry in matrix:
        jobs[entry["job"]] = True
    archives, shards = [], []
    count = portable_shards()
    portable_names = {group.name for group in portable}
    if count:
        # `only` names a portable group as test:<group> or the whole archive.
        whole = only and {f"build:{ARCHIVE}", f"shard:{ARCHIVE}"} & only
        archived = [package for entry in matrix if entry["job"] == f"test:{entry['group']}"
                    and entry["group"] in portable_names
                    and (not only or whole or entry["job"] in only)
                    for package in entry["packages"].split()]
        for entry in matrix:
            if entry["job"] == f"test:{entry['group']}" and entry["group"] in portable_names:
                jobs.pop(entry["job"], None)
        matrix = [entry for entry in matrix if entry["job"] in jobs]
        if archived:
            # At most one slice per crate with tests, so every slice runs some.
            tested_crates = len([package for package in archived if package not in NO_TESTS])
            count = min(count, tested_crates)
            # One slice runs in the build job itself: no archive transfer, which
            # from the minisforums costs more than the tests (1.24 GB: 3.5 min
            # up, 14 min down in dispatch 43775099812).
            single = count == 1
            groups = host_groups("portable")
            go = bool(AFTERBURNER & set(archived))
            entry = {"archive": ARCHIVE, "run": single, "packages": " ".join(archived),
                     "timeout": 150 if single else 90, "go": go}
            vera = vera_tests(archived)
            # The slice that runs in the build job splits into the role's
            # `partitions` hash partitions; each job builds the archive from
            # its runner's compiler cache and kept target tree and runs its
            # partition there, on any free portable runner. The Vera feature
            # tests run once, in the first job.
            count_partitions = min(partitions("portable"), tested_crates) if single else 0
            if count_partitions > 1:
                archives += [{**entry, **placement("portable"),
                              "vera": vera if index == 1 else "",
                              "partition": index, "partitions": count_partitions}
                             for index in range(1, count_partitions + 1)]
            else:
                archives.append({**entry, **placement("portable"), "vera": vera})
            # Slices alternate between host groups, so one run's slices split across hosts.
            shards = [] if single else [
                {"archive": ARCHIVE, "index": index, "count": count, "timeout": 90, "go": go,
                 **placement("portable", host_group=groups[(index - 1) % len(groups)] if groups else None)}
                for index in range(1, count + 1)]
    balance([entry for entry in matrix if entry["role"] == "portable"])
    if only:
        jobs = {job: wanted and job in only for job, wanted in jobs.items()}
        matrix = [entry for entry in matrix if entry["job"] in only]
    if archives:
        jobs[f"build:{ARCHIVE}"] = True
        jobs[f"shard:{ARCHIVE}"] = bool(shards)
    return {
        "tier": tier,
        "everything": everything,
        "reason": reasons.get("all", ""),
        "changed": sorted(changed),
        "built": sorted(built),
        "affected": sorted(affected),
        "jobs": sorted(job for job, wanted in jobs.items() if wanted),
        "matrix": matrix,
        "archives": archives,
        "shards": shards,
        "contract": {
            # write_owner_structure scans every crate's sources but is a
            # gents test target, which the runtime group runs when selected.
            "ratchets": rust and "gents" not in tested,
            "ts_lean": ts or proofs,
            # TLA+ specs whose header names a changed crate or Lean owner, or
            # whose own files or imported generated modules changed.
            "tla": tla,
        },
        "core_packs": everything or "core-packs" in tags,
        # The lint job's iOS check of gents-desktop-tauri: only when it or a
        # workspace crate it depends on changed (the toolchain and lock change
        # everything).
        "ios_check": "gents-desktop-tauri" in built,
        # The live suites (`live-e2e`) run after merge; lint them when they or a
        # crate they use changed.
        "live_e2e_lint": [package for package in ("gents", "gents-cli") if package in built],
        "runs_on": {role: runner(role) for role in WORKFLOW_ROLES},
    }


def changed_files(base):
    merge_base = subprocess.run(["git", "merge-base", base, "HEAD"], cwd=ROOT, check=True,
                                stdout=subprocess.PIPE, text=True).stdout.strip()
    committed = subprocess.run(["git", "diff", "--name-only", merge_base], cwd=ROOT, check=True,
                               stdout=subprocess.PIPE, text=True).stdout.split("\n")
    untracked = subprocess.run(["git", "ls-files", "--others", "--exclude-standard"], cwd=ROOT, check=True,
                               stdout=subprocess.PIPE, text=True).stdout.split("\n")
    return sorted({path for path in committed + untracked if path})


def resolve_plan(arguments):
    if os.environ.get("GATE_PLAN"):
        return json.loads(os.environ["GATE_PLAN"])
    only = set(filter(None, os.environ.get("GATE_ONLY", "").replace(",", " ").split())) or None
    everything = getattr(arguments, "everything", False) or os.environ.get("GATE_EVERYTHING") == "true"
    if everything:
        files = []
    elif getattr(arguments, "files", None):
        files = [line.strip() for line in Path(arguments.files).read_text().splitlines() if line.strip()]
    else:
        files = changed_files(arguments.base)
    tier = "draft" if os.environ.get("GATE_TIER") == "draft" else "full"
    mirrors = os.environ.get("GATE_EVENT") == "schedule"
    if mirrors:
        # The scheduled run is the portable groups that have a macOS role, on
        # macOS: platform differences the Linux runs cannot see surface daily.
        only = {f"test:{group.name}@macos" for group in GROUPS if group.macos}
    return plan(files, workspace(), everything, only, tier, mirrors)


# ---- running ----

RESULT = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;")
STARTED = re.compile(r"^test (\S+)(?: - should panic)? \.\.\. ")
FINISHED = re.compile(r"^test (\S+)(?: - should panic)? \.\.\. (?:ok|FAILED|ignored|failed)\b")
SLOW = re.compile(r"^test (\S+) has been running for over \d+ seconds")
TEST_TIMEOUT = int(os.environ.get("GATE_TEST_TIMEOUT", "600"))

results = []


def profile_flags():
    profile = os.environ.get("PROFILE", "")
    return ["--profile", profile] if profile else []


def sh(command, cwd=ROOT, env=None):
    shown = command if isinstance(command, str) else shlex.join(command)
    print(f"\n$ {shown}", flush=True)
    started = time.time()
    status = subprocess.run(command, cwd=cwd, env=env, shell=isinstance(command, str)).returncode
    results.append((shown, "ok" if status == 0 else f"exit {status}", "", time.time() - started))
    return status == 0


def cargo_test(label, arguments, harness=(), expect_tests=True, capture=True, isolate_target=False):
    """`cargo test` that fails when it runs no test or when a test hangs.

    libtest has no per-test deadline. It reports a test still running after 60
    seconds, so a captured run is killed once such a test has not finished
    TEST_TIMEOUT seconds later, and any run is killed after TEST_TIMEOUT
    seconds without output once tests have started, naming the tests in flight.
    """
    command = ["cargo", "test", *profile_flags(), *arguments, "--no-fail-fast"]
    env = dict(os.environ)
    env.setdefault("RUST_MIN_STACK", "16777216")
    if isolate_target and env.get("CARGO_TARGET_DIR"):
        command += ["--target-dir", env.pop("CARGO_TARGET_DIR")]
    if harness:
        command += ["--", *harness]
    print(f"\n$ {shlex.join(command)}", flush=True)
    started = time.time()
    process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True)
    state = {"last": time.time(), "passed": 0, "failed": 0, "ignored": 0, "running": {}, "slow": {},
             "testing": False}

    def read():
        for raw in process.stdout:
            line = raw.decode("utf-8", "replace")
            sys.stdout.write(line)
            sys.stdout.flush()
            state["last"] = time.time()
            text = line.strip()
            if text.startswith(("Running ", "Doc-tests ")):
                state["testing"] = True
            if match := RESULT.search(text):
                for key, value in zip(("passed", "failed", "ignored"), match.groups()):
                    state[key] += int(value)
                state["running"].clear()
                state["slow"].clear()
            elif match := FINISHED.match(text):
                state["running"].pop(match.group(1), None)
                state["slow"].pop(match.group(1), None)
            elif match := SLOW.match(text):
                state["slow"].setdefault(match.group(1), time.time())
            elif match := STARTED.match(text):
                if not capture:
                    state["running"].clear()
                state["running"][match.group(1)] = time.time()

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    hung = []
    while process.poll() is None:
        time.sleep(1)
        now = time.time()
        overdue = [name for name, since in state["slow"].items() if now - since > TEST_TIMEOUT] if capture else []
        if overdue or (state["testing"] and now - state["last"] > TEST_TIMEOUT):
            hung = sorted(overdue or state["slow"] or state["running"]) or ["<no test in flight: build or harness>"]
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
    process.wait()
    reader.join(timeout=10)
    process.stdout.close()
    ran = state["passed"] + state["failed"]
    if hung:
        verdict = f"hung after {TEST_TIMEOUT}s: {', '.join(hung)}"
    elif process.returncode != 0:
        verdict = f"exit {process.returncode}"
    elif expect_tests and ran == 0:
        verdict = "selected zero tests"
    elif not expect_tests and ran > 0:
        verdict = "ran tests but is listed in NO_TESTS; remove it from the list"
    else:
        verdict = "ok"
    counts = f"{state['passed']} passed, {state['failed']} failed, {state['ignored']} ignored"
    results.append((label, verdict, counts, time.time() - started))
    if verdict != "ok":
        print(f"::error::{label}: {verdict} ({counts})", flush=True)
    return verdict == "ok"


def report():
    lines = ["| step | result | tests | seconds |", "|---|---|---|---|"]
    lines += [f"| `{label}` | {verdict} | {counts} | {seconds:.0f} |" for label, verdict, counts, seconds in results]
    table = "\n".join(lines)
    print("\n" + table, flush=True)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write(table + "\n")
    return all(verdict == "ok" for _, verdict, _, _ in results)


def package_flags(selection):
    if selection["everything"]:
        return ["--workspace"]
    return [flag for package in selection["built"] for flag in ("-p", package)]


def run_fmt(selection):
    sh(["cargo", "fmt", "--all", "--check"])


def run_lint(selection):
    packages, affected = package_flags(selection), set(selection["built"])
    profile = profile_flags()
    # gents-workspace-hack keeps per-crate and joint test builds on one feature
    # set (the archive builds every portable crate in one invocation). `verify`
    # and `manage-deps` do not fit its dev-dependency-only, unify-target-host =
    # "none" setup, so the check is that the generated hack is current.
    sh(["cargo", "hakari", "generate", "--diff"])
    # The product build: no test targets, so no dev-dependency features.
    sh(["cargo", "check", *packages, *profile])
    sh(["cargo", "clippy", *packages, "--all-targets", *profile, "--", "-D", "warnings"])
    # Live suites run after merge; they must still compile and lint clean.
    for package in selection.get("live_e2e_lint", []):
        sh(["cargo", "clippy", "-p", package, "--tests", "--features", "live-e2e", *profile, "--", "-D", "warnings"],
           env={**os.environ, **side_tree_env("live-e2e")})
    if "gents-cli" in affected and os.environ.get("GITHUB_EVENT_NAME") != "pull_request":
        sh(["cargo", "check", "-p", "gents-cli", "--tests", "--features", "vera", *profile])
    if "gents-loop" in affected:
        sh(["cargo", "build", "-p", "gents-loop", "--target", "wasm32-wasip1", *profile])
    if "gents-desktop-tauri" in affected:
        sh(["./scripts/check-native-e2e-capabilities.sh"])
    if selection.get("ios_check") and sys.platform == "darwin":
        # iOS shares the desktop crates; desktop-only Tauri APIs break it silently (#890).
        sh(["rustup", "target", "add", "aarch64-apple-ios-sim"])
        sh(["cargo", "check", "-p", "gents-desktop-tauri", "--lib", "--target", "aarch64-apple-ios-sim", *profile],
           env={**os.environ, **side_tree_env("ios-check")})


def run_contract(selection):
    proofs = ROOT / PROOFS
    lake = os.environ.get("LAKE", "lake")
    sh(r"! grep -rn -E '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' --include='*.lean' --exclude-dir=.lake .", cwd=proofs)
    sh("python3 .github/scripts/test-lean-import-closure.py")
    sh([lake, "build", "importClosure"], cwd=proofs)
    sh("python3 .github/scripts/test-lean-import-layers.py")
    sh([lake, "build", "importLayers"], cwd=proofs)
    sh([lake, "build"], cwd=proofs)
    sh("python3 .github/scripts/test-canonical-output-map.py && "
       "python3 .github/scripts/check-canonical-output-map.py --check-export")
    sh("python3 .github/scripts/test-lean-rust-decoders.py && python3 .github/scripts/check-lean-rust-decoders.py")
    # Generated decoders, the family schema and the one-consumer-per-family scan.
    cargo_test("gents-lean-vocab", ["-p", "gents-lean-vocab"])
    # The `cargo test` above already built this example; under `dev` it would
    # build its dependencies again.
    sh(["cargo", "run", *(profile_flags() or ["--profile", "test"]), "-p", "gents-lean-vocab", "--example",
        "regenerate_household_policy", "--", "--check"])
    if selection["contract"]["ratchets"]:
        cargo_test("gents write_owner_structure", ["-p", "gents", "--test", "write_owner_structure"])
    tla = selection["contract"].get("tla", [])
    if tla:
        sh(["python3", str(TLA_CHECK), "check-headers"])
        selected = ["--all"] if tla == ["*"] else [flag for spec in tla for flag in ("--spec", spec)]
        sh(["python3", str(TLA_CHECK), "run", *selected])
    if selection["contract"]["ts_lean"]:
        for command in TS_LEAN:
            sh(command)


def run_ts(job):
    for command in TS_JOBS[job]:
        sh(command)


def group_options(group):
    harness = []
    if group.threads:
        harness.append(f"--test-threads={group.threads}")
    if group.nocapture:
        harness.append("--nocapture")
    return {"harness": harness, "capture": not group.nocapture, "isolate_target": group.isolate_target}


def feature_flags(package):
    if package in PACKAGE_FEATURES and (package != "gents-desktop-bridge" or sys.platform == "darwin"):
        return ["--features", PACKAGE_FEATURES[package]]
    return []


def system_preflight(platform=None, exists=None):
    """False, with the missing packages named, when this Linux host lacks a
    system library the portable crates link."""
    if (platform or sys.platform) != "linux":
        return True
    started = time.time()

    def found(name):
        try:
            return subprocess.run(["pkg-config", "--exists", name]).returncode == 0
        except FileNotFoundError:
            return False

    missing = [package for name, package in LINUX_SYSTEM_LIBRARIES.items() if not (exists or found)(name)]
    if not missing:
        return True
    problem = (f"this runner lacks the system packages {' '.join(missing)}, which the portable crates link "
               "on Linux; install them on every host of the runner's role "
               "(amygdala infra/services/github-runners/provision-ghrunner-root.sh)")
    print(f"::error::{problem}", flush=True)
    results.append(("system library preflight", problem, "", time.time() - started))
    return False


def run_test(selection, name):
    entry = next((entry for entry in selection["matrix"] if entry["job"] == f"test:{name}"), None)
    group = next((group for group in GROUPS if entry and group.name == entry["group"]), None)
    if group is None or entry is None:
        fail(f"test group {name} is not selected by this plan")
    packages = entry["packages"].split()
    if group.name == NATIVE:
        run_native(entry, packages)
        return
    if not system_preflight():
        return
    options = group_options(group)
    # One crate per build: a crate that compiles only through the features
    # another selected crate turns on must fail here.
    for package in packages:
        cargo_test(package, ["-p", package, *feature_flags(package)], expect_tests=package not in NO_TESTS,
                   **options)
    for package in entry.get("vera", "").split():
        cargo_test(f"{package} (vera)", ["-p", package, "--lib", "--features", "vera"],
                   **{**options, "harness": [VERA_TESTS[package], *options["harness"]]})


def target_arguments(package):
    """Cargo's target flags for each lib, bin and test target of a package."""
    info = next(entry for entry in workspace_metadata()["packages"] if entry["name"] == package)
    found = set()
    for target in info["targets"]:
        kinds = set(target["kind"])
        if "lib" in kinds:
            found.add(("--lib",))
        elif "bin" in kinds:
            found.add(("--bin", target["name"]))
        elif "test" in kinds:
            found.add(("--test", target["name"]))
    return found


def macos_invocations(packages):
    """The macOS-only test targets as few `cargo nextest run` invocations as select
    no other target: Cargo applies an invocation's target flags to every package
    in it, so `gents --lib` and `gents-toolcalls --test integration` together
    would also build gents' own integration tests. Each item is (packages, flags)."""
    invocations = []
    for package in packages:
        wanted = set(macos_only_tests(package))
        for invocation in invocations:
            flags = invocation[1] | wanted
            if all(target_arguments(member) & flags == set(macos_only_tests(member))
                   for member in [*invocation[0], package]):
                invocation[0].append(package)
                invocation[1] = flags
                break
        else:
            invocations.append([[package], wanted])
    return [(members, sorted(flags)) for members, flags in invocations]


def binary_id(package, target):
    """nextest's binary id for a package's target flags."""
    if target == ("--lib",):
        return package
    if target[0] == "--bin":
        return f"{package}::bin/{target[1]}"
    return f"{package}::{target[1]}"


def macos_filterset(packages):
    """A nextest filterset for the macOS-only tests: libtest's name filters are substrings, as `test(~...)` is."""
    terms = []
    for package in packages:
        for target, filters in sorted(macos_only_tests(package).items()):
            selected = f"binary_id(={binary_id(package, target)})"
            if filters:
                selected += " & (" + " | ".join(f"test(~{name})" for name in filters) + ")"
            terms.append(f"({selected})")
    return " | ".join(terms)


def run_vera_tests(packages):
    """The `vera` tests of an archive's packages, built apart from the archive in
    one `cargo nextest run`: enabling the feature in the archive would build
    every archived crate against a gents-store with the Vera node."""
    if not packages:
        return
    os.environ.pop("GENTS_TEST_BINARIES_PREBUILT", None)
    features = ",".join(f"{package}/vera" for package in packages)
    filterset = " | ".join(f"(binary_id(={package}) & test(~{VERA_TESTS[package]}))" for package in packages)
    # The archive's profile and target tree: in the `test` profile the feature
    # build recompiled about 1,083 dependencies the archive had just built.
    native_tests(f"{' '.join(packages)} --lib (vera)", packages, ["--lib", "--features", features], filterset,
                 set(packages), profile=os.environ.get("PROFILE") or ARCHIVE_PROFILE)


def native_tests(label, packages, flags, filterset, expected, profile=None):
    """One `cargo nextest run`: one build, one parallel pool across its crates and binaries.

    Its JUnit report then fails a crate, or a macOS-only target, that ran no
    test, as `cargo_test` did. `expected` holds binary ids, or packages for
    every binary of one.
    """
    arguments = [*[flag for package in packages for flag in ("-p", package)], *flags]
    features = sorted({feature for package in packages for feature in feature_flags(package)[1:]})
    if features:
        arguments += ["--features", ",".join(features)]
    profile = profile or os.environ.get("PROFILE")
    if profile:
        arguments += ["--cargo-profile", profile]
    # Tests that spawn Cargo for plugin builds must not inherit the job's target directory.
    drop = ()
    if os.environ.get("CARGO_TARGET_DIR"):
        arguments += ["--target-dir", os.environ["CARGO_TARGET_DIR"]]
        drop = ("CARGO_TARGET_DIR",)
    if filterset:
        arguments += ["-E", filterset]
    arguments += ["--profile", "ci", "--no-tests", "fail"]
    if os.environ.get("RUST_TEST_THREADS"):
        arguments += ["--test-threads", os.environ["RUST_TEST_THREADS"]]
    junit = ROOT / "target" / "nextest" / "ci" / "junit.xml"
    junit.unlink(missing_ok=True)
    nextest(["run", *arguments], label=label, drop_env=drop)
    if not junit.exists():
        return
    ran = junit_outcomes(junit)
    problems = []
    for key in sorted(expected):
        count = sum(1 for binary, _ in ran if binary == key or binary.startswith(key + "::"))
        if key in NO_TESTS and count:
            problems.append(f"{key} ran tests but is listed in NO_TESTS; remove it from the list")
        elif key not in NO_TESTS and not count:
            problems.append(f"{key} selected zero tests")
    for problem in problems:
        print(f"::error::{label}: {problem}", flush=True)
    results.append((f"{label}: selection", "ok" if not problems else "; ".join(problems), f"{len(ran)} ran", 0))


def run_native(entry, packages):
    if sys.platform != "darwin":
        fail(f"test:{entry['job']} runs tests that compile only on macOS; this host is {sys.platform}")
    if packages:
        native_tests(" ".join(packages), packages, [], None, set(packages))
    for members, flags in macos_invocations(entry.get("macos_tests", [])):
        expected = {binary_id(package, target) for package in members for target in macos_only_tests(package)}
        native_tests(f"{' '.join(members)} {' '.join(flag for pair in flags for flag in pair)} (macOS only)",
                     members, [flag for pair in flags for flag in pair], macos_filterset(members), expected)
    if entry.get("journey"):
        if os.environ.get("GITHUB_ACTIONS") and os.environ.get("CARGO_TARGET_DIR"):
            sh(["python3", ".github/scripts/ensure-rust-build-space.py", os.environ["CARGO_TARGET_DIR"]])
        if sh([".github/scripts/desktop-runtime-journey.sh", "build"]):
            # The folded `runtime` suite; the CLI's integration binary is built
            # too, and its cli_runtime:: suite also matches `runtime::`.
            cargo_test("native client runtime journey", [
                "-p", "gents-cli", "-p", "gents-client-host", "-p", "gents-media-host",
                "--features", "gents-client-host/runtime-e2e", "--test", "integration"],
                harness=("runtime::", "--skip", "cli_runtime::"))


SUMMARY = re.compile(r"^\s*Summary \[\s*([\d.]+)s\] (\d+) tests? run: (.*)$")


# Slices of one run can share a host, and each test process starts its own
# embedded node: 3 slices at 12 threads took a 32-core minisforum from load
# 20 to over 100, where 8 threads each kept it under 80 (dispatch 43739322437).
SLICE_THREADS = 8


def archive_dir():
    """Where `build:` writes the archive and `shard:` reads it (the workflow's artifact directory)."""
    if os.environ.get("GATE_ARCHIVE_DIR"):
        return Path(os.environ["GATE_ARCHIVE_DIR"])
    if os.environ.get("GITHUB_ACTIONS") and os.environ.get("RUNNER_TEMP"):
        return Path(os.environ["RUNNER_TEMP"]) / "gents-tests"
    return ROOT / "target" / "gents-tests"


def nextest(arguments, cwd=ROOT, label=None, expect=None, extra_env=None, drop_env=()):
    """`cargo nextest`, recording the summary's counts; `expect` checks the listed tests per package."""
    command = ["cargo", "nextest", *arguments]
    env = {**os.environ, **(extra_env or {})}
    for name in drop_env:
        env.pop(name, None)
    env.setdefault("RUST_MIN_STACK", "16777216")
    print(f"\n$ {shlex.join(command)}", flush=True)
    started = time.time()
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    counts = ""
    for raw in process.stdout:
        line = raw.decode("utf-8", "replace")
        sys.stdout.write(line)
        sys.stdout.flush()
        if match := SUMMARY.match(line.rstrip()):
            counts = f"{match.group(2)} run: {match.group(3).strip()}"
    process.wait()
    verdict = "ok" if process.returncode == 0 else f"exit {process.returncode}"
    results.append((label or shlex.join(command), verdict, counts, time.time() - started))
    if verdict != "ok":
        print(f"::error::{label}: {verdict} ({counts})", flush=True)
    return verdict == "ok"


def partition_arguments(entry):
    """The nextest partition of a partitioned build's own slice, else none."""
    return ["--partition", f"hash:{entry['partition']}/{entry['partitions']}"] if entry.get("partition") else []


def listed_tests(archive, workspace):
    """Package -> (tests, ignored) in an archive, and the paths of its test binaries."""
    raw = subprocess.run(["cargo", "nextest", "list", "--archive-file", str(archive), "--workspace-remap",
                          str(workspace), "--message-format", "json", "--run-ignored", "all"],
                         cwd=workspace, check=True, stdout=subprocess.PIPE).stdout
    tests, binaries = {}, []
    for suite in json.loads(raw)["rust-suites"].values():
        binaries.append(Path(suite["binary-path"]))
        package = suite["package-name"]
        total, ignored = tests.get(package, (0, 0))
        cases = suite["testcases"].values()
        tests[package] = (total + len(cases), ignored + sum(1 for case in cases if case.get("ignored")))
    return tests, binaries


def junit_outcomes(path):
    """(binary id, test name) -> passed, from nextest's JUnit report."""
    outcomes = {}
    if not Path(path).exists():
        return outcomes
    for case in ElementTree.parse(path).getroot().iter("testcase"):
        failed = any(child.tag in ("failure", "error", "flakyFailure", "rerunFailure") for child in case)
        skipped = any(child.tag == "skipped" for child in case)
        if not skipped:
            outcomes[(case.get("classname"), case.get("name"))] = not failed
    return outcomes


def unrelocatable(binaries, source):
    """Binaries holding the build checkout's path: compile-time env!("CARGO_MANIFEST_DIR"),
    CARGO_BIN_EXE_<name> or CARGO_TARGET_TMPDIR, which a shard's checkout cannot satisfy."""
    needle = re.compile(re.escape(str(source).encode()) + rb"/(?!target/)")
    found = []
    for binary in binaries:
        if binary.is_file() and needle.search(binary.read_bytes()):
            found.append(binary.name)
    return sorted(found)


DOC_FENCE = re.compile(r"^\s*(?://[/!]|#!?\[doc\s*=).*```(?!\s*(?:text|ignore)\b)")


def doctests(packages):
    """Doc comments that rustdoc would test. nextest does not run doctests and
    none exist today, so the archive job refuses the first one instead of
    letting it go unrun; add a `cargo test --doc` step when one is wanted."""
    members = workspace()
    found = []
    for package in packages:
        for path in sorted((ROOT / members[package][0]).rglob("*.rs")):
            if "target" in path.relative_to(ROOT).parts:
                continue
            for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
                if DOC_FENCE.match(line):
                    found.append(f"{path.relative_to(ROOT)}:{number}")
    return found


def run_build(selection, name, partition=None):
    """Build every selected crate's tests once, in one Cargo invocation, into a nextest archive.

    In CI the build runs from a worktree under RUNNER_TEMP that the runner removes
    after the job, so a test binary can only reach source files through the
    runtime paths a shard remaps to its own checkout. `build:<name>:<partition>`
    runs only that partition of the tests; `build:<name>` (`make gate`) runs them all.
    """
    entries = [entry for entry in selection["archives"] if entry["archive"] == name]
    if partition is None:
        entry = {key: value for key, value in entries[0].items() if key not in ("partition", "partitions")}
    else:
        entry = next((entry for entry in entries if entry.get("partition") == partition), None)
        if entry is None:
            fail(f"build {name}:{partition} is not selected by this plan")
    lean = LeanContract.start() if entry.get("run") or entry.get("vera") else None
    try:
        build_and_run(entry, name, lean)
    finally:
        if lean:
            lean.stop()


def show_timings(report, count=15):
    """The compile's slowest units from Cargo's --timings report, so a job's
    log says where its compile went."""
    try:
        text = report.read_text()
        units = json.loads(re.search(r"const UNIT_DATA = (\[.*?\]);\n", text, re.S).group(1))
    except (OSError, AttributeError, ValueError):
        return
    end = max((unit["start"] + unit["duration"] for unit in units), default=0)
    print(f"\nCompile: {len(units)} units in {end:.0f} s; the slowest:", flush=True)
    for unit in sorted(units, key=lambda unit: -unit["duration"])[:count]:
        print(f"  {unit['duration']:7.1f} s  from {unit['start']:6.1f} s  {unit['name']} {unit['target'].strip()}",
              flush=True)


class LeanContract:
    """The Lean conformance contract the Lean-backed tests load, built beside
    the Rust compile rather than before it (rust-runner `lean-contract: deferred`):
    it took 2.5 to 10 minutes ahead of the compile in PR #3926's runs. Tests
    must not start before it is built: they would each build it on first use
    under a shared lock, with their output captured."""

    TARGET = "Proofs.Conformance.Contracts:olean"

    def __init__(self, process, log):
        self.process, self.log, self.started = process, log, time.time()

    @classmethod
    def start(cls):
        if not shutil.which("lake"):
            return None
        log = tempfile.NamedTemporaryFile(prefix="gate-lean-contract-", suffix=".log", delete=False)
        print(f"\n$ lake build {cls.TARGET} (beside the Rust compile; output in {log.name})", flush=True)
        process = subprocess.Popen(["lake", "build", cls.TARGET], cwd=ROOT / PROOFS, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        return cls(process, log)

    def wait(self):
        """True once the contract is built; on failure, its output and the gate's error."""
        code = self.process.wait()
        self.log.close()
        verdict = "ok" if code == 0 else f"exit {code}"
        results.append((f"lake build {self.TARGET}", verdict, "", time.time() - self.started))
        if code != 0:
            sys.stdout.write(Path(self.log.name).read_text(errors="replace")[-20000:])
            print(f"::error::lake build {self.TARGET}: {verdict}", flush=True)
        return code == 0

    def stop(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            self.process.wait()
        self.log.close()
        Path(self.log.name).unlink(missing_ok=True)


# Packages whose tests run another package's binary from beside the test
# binary when GENTS_TEST_BINARIES_PREBUILT is set: the archive must build that
# package too, or a plan that tests only the first (a draft, a partition) finds
# no helper (#3950).
TEST_HELPER_PACKAGES = {"gents-tools": ("gents-fs-runner",)}


def archive_packages(packages):
    """The packages an archive builds: the tested ones and the helpers their tests run."""
    helpers = [helper for package in packages for helper in TEST_HELPER_PACKAGES.get(package, ())]
    return list(dict.fromkeys([*packages, *helpers]))


def build_and_run(entry, name, lean):
    packages = entry["packages"].split()
    if not system_preflight():
        return
    source = ROOT
    if os.environ.get("GITHUB_ACTIONS") and os.environ.get("RUNNER_TEMP"):
        source = Path(os.environ["RUNNER_TEMP"]) / ARCHIVE_SOURCE
        subprocess.run(["git", "worktree", "remove", "--force", str(source)], cwd=ROOT,
                       stderr=subprocess.DEVNULL, check=False)
        subprocess.run(["git", "worktree", "prune"], cwd=ROOT, check=False)
        if not sh(["git", "worktree", "add", "--detach", str(source), "HEAD"]):
            return
    output = archive_dir()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{name}.tar.zst"
    features = [PACKAGE_FEATURES[package] for package in packages
                if package in PACKAGE_FEATURES and package != "gents-desktop-bridge"]
    arguments = ["archive", *[flag for package in archive_packages(packages) for flag in ("-p", package)]]
    if features:
        arguments += ["--features", ",".join(features)]
    arguments += ["--cargo-profile", os.environ.get("PROFILE") or ARCHIVE_PROFILE]
    arguments += ["--archive-file", str(archive), "--timings"]
    built = nextest(arguments, cwd=source, label=f"build {name}: {len(packages)} crates")
    show_timings(Path(os.environ.get("CARGO_TARGET_DIR") or source / "target") / "cargo-timings" / "cargo-timing.html")
    if not built:
        return
    started = time.time()
    try:
        tests, binaries = listed_tests(archive, source)
    except subprocess.CalledProcessError as error:
        results.append((f"check {name} archive", f"listing failed: exit {error.returncode}", "",
                        time.time() - started))
        return
    problems = []
    for package in packages:
        total, _ = tests.get(package, (0, 0))
        if package in NO_TESTS and total:
            problems.append(f"{package} has {total} tests but is listed in NO_TESTS; remove it from the list")
        elif package not in NO_TESTS and not total:
            problems.append(f"{package} selected zero tests")
    if fences := doctests(packages):
        problems.append(f"doc comments hold code blocks rustdoc would test, which nextest does not run "
                        f"(tag them text or ignore, or add a doctest step): {', '.join(fences[:10])}")
    if stuck := unrelocatable(binaries, source):
        problems.append(f"test binaries embed the build checkout's path {source}: {', '.join(stuck)}")
    (output / f"{name}.tests.json").write_text(json.dumps(tests, indent=2, sort_keys=True))
    total = sum(count for count, _ in tests.values())
    ignored = sum(count for _, count in tests.values())
    for problem in problems:
        print(f"::error::{problem}", flush=True)
    results.append((f"check {name} archive", "ok" if not problems else "; ".join(problems),
                    f"{total} tests listed, {ignored} ignored", time.time() - started))
    built = lean is None or lean.wait()
    if not problems and entry.get("run") and built:
        run_archive(entry, archive, name)
    if built:
        run_vera_tests(entry.get("vera", "").split())


def run_archive(entry, archive, name):
    """The whole archive, on this runner, from this job's own checkout."""
    os.environ["GENTS_TEST_BINARIES_PREBUILT"] = "1"
    arguments = ["run", "--archive-file", str(archive), "--workspace-remap", str(ROOT), "--profile", "ci",
                 *partition_arguments(entry)]
    if os.environ.get("RUST_TEST_THREADS"):
        arguments += ["--test-threads", os.environ["RUST_TEST_THREADS"]]
    nextest(arguments, label=f"run {name}")


def run_shard(selection, name, index=None):
    """Run one slice of an archive against this checkout; without an index, all of it."""
    archive = archive_dir() / f"{name}.tar.zst"
    arguments = ["run", "--archive-file", str(archive), "--workspace-remap", str(ROOT), "--profile", "ci"]
    label = f"shard {name}"
    if index is not None:
        entry = next((entry for entry in selection["shards"]
                      if entry["archive"] == name and entry["index"] == index), None)
        if entry is None:
            fail(f"shard {name}:{index} is not selected by this plan")
        arguments += ["--partition", f"slice:{index}/{entry['count']}"]
        label = f"shard {name} {index}/{entry['count']}"
    threads = os.environ.get("RUST_TEST_THREADS")
    if index is not None:
        threads = str(min(int(threads or SLICE_THREADS), SLICE_THREADS))
    if threads:
        arguments += ["--test-threads", threads]
    # Tests that would otherwise `cargo build` a helper binary use the archived one.
    os.environ["GENTS_TEST_BINARIES_PREBUILT"] = "1"
    nextest(arguments, label=label)


def side_tree_env(name):
    """A lint build's own target tree inside the kept one, built incrementally.

    The iOS check and the live-e2e lints build every workspace crate under a
    target or feature set no other build shares (38 and 11 min from scratch on
    a Studio, #3926), which sccache does not serve. Each gets its own
    incremental tree: incremental units in the shared tree would replace the
    non-incremental ones the other builds reuse. Inside the kept tree, it
    survives the checkout's `git clean` and counts toward the runner's
    reclamation.
    """
    target = os.environ.get("CARGO_TARGET_DIR")
    if not target:
        return {"CARGO_INCREMENTAL": "1"}
    return {"CARGO_TARGET_DIR": str(Path(target) / name), "CARGO_INCREMENTAL": "1", "RUSTC_WRAPPER": ""}


def keep_core_packs_fresh():
    """Give gents-core-packs' build script a core packs directory that outlives the checkout.

    The build script creates and watches its directory (default
    target/core-packs). The checkout's `git clean` deletes that directory
    before every job, and a directory created during a build reruns the script
    once more, which recompiles gents-cli and gents-client-host above it. A CI
    runner therefore keeps an empty directory beside its Cargo target. Empty,
    it embeds the same no archives a missing directory does.
    """
    if os.environ.get("GENTS_CORE_PACKS_DIR"):
        return
    if os.environ.get("GITHUB_ACTIONS") and os.environ.get("CARGO_TARGET_DIR"):
        directory = Path(os.environ["CARGO_TARGET_DIR"]) / "core-packs-none"
        os.environ["GENTS_CORE_PACKS_DIR"] = str(directory)
    else:
        directory = ROOT / "target" / "core-packs"
    directory.mkdir(parents=True, exist_ok=True)


def run(selection, job):
    keep_core_packs_fresh()
    if job.startswith(("shard:", "build:")) and job.count(":") == 2:
        prefix, name, index = job.split(":")
        if f"{prefix}:{name}" not in selection["jobs"]:
            print(f"gate: {prefix}:{name} is not selected by this plan; nothing to do")
            return
        (run_shard if prefix == "shard" else run_build)(selection, name, int(index))
        return
    if job not in selection["jobs"]:
        print(f"gate: {job} is not selected by this plan; nothing to do")
        return
    if job == "fmt":
        run_fmt(selection)
    elif job == "lint":
        run_lint(selection)
    elif job == "contract":
        run_contract(selection)
    elif job in TS_JOBS:
        run_ts(job)
    elif job.startswith("test:"):
        run_test(selection, job[5:])
    elif job.startswith("build:"):
        run_build(selection, job[6:])
    elif job.startswith("shard:"):
        run_shard(selection, job[6:])
    else:
        fail(f"unknown job {job}")


# The workflow job that runs each plan job, and the jobs every event runs.
WORKFLOW_JOBS = {
    "fmt": "fmt", "lint": "lint", "contract": "contract", "ts-packages": "desktop-packages",
    "ts-unit": "desktop-ui", "ts-browser": "desktop-ui", "ts-fuzz": "desktop-ui-bombadil",
}
# Matrix jobs: a plan job `<prefix>:<name>` runs in the workflow job `<prefix>`.
MATRIX_JOBS = ("test", "build", "shard")
ALWAYS = ("plan", "repository-guard", "publish-control-tests", "triage-hygiene", "notary-control-tests")


def workflow_job(job):
    prefix = job.split(":", 1)[0]
    return prefix if prefix in MATRIX_JOBS and ":" in job else WORKFLOW_JOBS[job]


def verdict(needs, fork=False):
    """The `gate` job's decision from its `toJSON(needs)`: a list of problems, empty to pass.

    A job is skipped both when the plan did not select it and when a job it
    needs failed, so a skip passes only for a job the plan left out.
    """
    problems = {}
    known = set(ALWAYS) | set(WORKFLOW_JOBS.values()) | set(MATRIX_JOBS)
    for name in sorted(set(needs) - known):
        problems[name] = "gate needs it but gate.py does not know it; add it to WORKFLOW_JOBS or ALWAYS"
    for name in sorted(known - set(needs)):
        problems[name] = "missing from the gate job's needs"
    for name, job in sorted(needs.items()):
        if job.get("result") not in ("success", "skipped"):
            problems[name] = job.get("result")
    for name in ALWAYS:
        result = needs.get(name, {}).get("result")
        if result != "success":
            problems.setdefault(name, f"runs on every event but was {result}")
    raw = needs.get("plan", {}).get("outputs", {}).get("plan")
    if not raw:
        problems.setdefault("plan", "produced no plan")
        return [f"{name}: {problem}" for name, problem in problems.items()]
    for job in json.loads(raw)["jobs"]:
        owner = workflow_job(job)
        result = needs.get(owner, {}).get("result")
        if result != "success":
            reason = (" because this pull request comes from a fork: self-hosted jobs never run fork code. "
                      "A maintainer pushes the change to a branch of this repository to test it"
                      if fork and result == "skipped" else "")
            problems.setdefault(owner, f"the plan selected {job} but it was {result}{reason}")
    return [f"{name}: {problem}" for name, problem in problems.items()]


def emit(selection):
    print(json.dumps(selection, indent=2))
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as handle:
            handle.write(f"plan={json.dumps(selection)}\n")
            handle.write(f"matrix={json.dumps({'include': selection['matrix']})}\n")
            handle.write(f"jobs={json.dumps(selection['jobs'])}\n")
            handle.write(f"tests={'true' if selection['matrix'] else 'false'}\n")
            handle.write(f"archives={json.dumps({'include': selection['archives']})}\n")
            handle.write(f"shards={json.dumps({'include': selection['shards']})}\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as handle:
            scope = f"everything ({selection['reason'] or 'not a pull request'})" if selection["everything"] else \
                f"{len(selection['affected'])} affected crates from {len(selection['changed'])} changed"
            if selection["tier"] == "draft":
                scope += "; draft: tests cover the changed crates, dependents are tested when ready for review"
            handle.write(f"### Gate plan\n\nScope: {scope}\n\nJobs: {', '.join(selection['jobs']) or 'none'}\n\n")
            for entry in selection["matrix"]:
                extra = f"; macOS-only tests of {' '.join(entry['macos_tests'])}" if entry.get("macos_tests") else ""
                extra += "; the native client journey" if entry.get("journey") else ""
                handle.write(f"- `{entry['job']}`: {entry['packages'] or 'no crates of its own'}{extra}\n")
            for entry in selection["archives"]:
                if entry.get("partition"):
                    handle.write(f"- `build:{entry['archive']}:{entry['partition']}`, running partition "
                                 f"{entry['partition']} of {entry['partitions']} on {entry['runs_on'][-1]}: "
                                 f"{entry['packages']}\n")
                    continue
                handle.write(f"- `build:{entry['archive']}`, then {len(selection['shards'])} "
                             f"`shard:{entry['archive']}` slices: {entry['packages']}\n")


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("plan", "run", "all"):
        command = commands.add_parser(name)
        command.add_argument("--base", default=os.environ.get("BASE") or "origin/main")
        command.add_argument("--files")
        command.add_argument("--everything", action="store_true")
        if name == "run":
            command.add_argument("job")
    commands.add_parser("packages").add_argument("groups", nargs="+")
    commands.add_parser("verdict", help="decide the gate job from NEEDS (its toJSON(needs))")
    arguments = parser.parse_args()
    if arguments.command == "verdict":
        problems = verdict(json.loads(os.environ["NEEDS"]), os.environ.get("GATE_FORK") == "true")
        for problem in problems:
            print(f"::error::{problem}")
        sys.exit(1 if problems else 0)
    if arguments.command == "packages":
        print(" ".join(package for group in GROUPS if group.name in arguments.groups for package in group.packages))
        return
    selection = resolve_plan(arguments)
    if arguments.command == "plan":
        emit(selection)
        return
    for job in [arguments.job] if arguments.command == "run" else selection["jobs"]:
        run(selection, job)
    if not report():
        sys.exit(1)


if __name__ == "__main__":
    main()
