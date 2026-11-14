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
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROOFS = "crates/gents/proofs"

STUDIO = ["self-hosted", "macOS", "ARM64", "studio"]


class Group:
    def __init__(self, name, runner, packages, threads=None, nocapture=False,
                 isolate_target=False, timeout=120):
        self.name = name
        self.runner = runner
        self.packages = packages
        self.threads = threads
        self.nocapture = nocapture
        self.isolate_target = isolate_target
        self.timeout = timeout


# Every workspace member belongs to exactly one group; `plan` fails otherwise.
# "portable" groups have no platform-specific code and move to Linux runners
# through GATE_PORTABLE_RUNNER; the others need macOS.
GROUPS = [
    Group("foundation", "portable", [
        "gents-protocol", "gents-schemas", "gents-loop", "gents-lean-contract",
        "gents-lean-vocab", "gents-migration", "gents-lens-fixture-add-label",
        "gents-callback-fixture-create-workspace", "gents-claude", "gents-claude-login",
        "gents-chatgpt-login", "gents-login-ui", "gents-fs-runner", "gents-shell",
        "gents-core-packs",
    ], timeout=60),
    Group("store", "portable", [
        "gents-store", "gents-documents", "gents-inference", "gents-p2p", "gents-pack",
    ], timeout=90),
    Group("session", "portable", [
        "gents-session", "gents-tools", "gents-toolcalls", "gents-automation", "gents-observe",
    ], timeout=120),
    Group("runtime", "runtime", ["gents", "gents-eval"], timeout=150),
    # CLI tests share fixed ports and spawn Cargo for plugin builds, which must
    # not inherit the job's target directory.
    Group("cli", "cli", ["gents-cli", "gents-service", "gents-host"],
          threads=1, nocapture=True, isolate_target=True, timeout=180),
    Group("desktop", "desktop", [
        "gents-desktop-core", "gents-desktop", "gents-desktop-bridge", "gents-client-host",
        "gents-media-host", "fixture-domain-plugin", "gents-fixture-host", "gents-desktop-tauri",
    ], timeout=150),
]

PACKAGE_FEATURES = {
    "gents-claude": "gents-claude/transport",
    "gents-protocol": "gents-protocol/typescript",
    "gents-desktop-bridge": "notify/macos_kqueue",
}

# Packages with no test of their own. The list only shrinks: a listed package
# that runs a test fails the gate, as does an unlisted one that runs none.
NO_TESTS = {
    "fixture-domain-plugin",
    "gents-callback-fixture-create-workspace",
    "gents-core-packs",
    "gents-desktop",
    "gents-lens-fixture-add-label",
}

# With GATE_PORTABLE_SHARDS above zero the portable groups' crates build once
# into one nextest archive (`build:portable`) and `shard:portable` runs it as
# that many slices on their own runners, compiling nothing. Test binaries must
# then find source files and binaries through the runtime CARGO_MANIFEST_DIR,
# NEXTEST_WORKSPACE_ROOT and CARGO_BIN_EXE_<name>: the build checkout is gone.
ARCHIVE = "portable"
ARCHIVE_SOURCE = "gents-archive-src"
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
    ".github/scripts/collect-migration-lens-diagnostics.py", ".github/scripts/host-runtime-service.sh",
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
    "scripts/packed-desktop-consumer/",
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
    ],
    "ts-browser": [
        "npm run build:packages",
        "npm --prefix apps/gents-desktop run build",
        "npm --prefix apps/gents-desktop run test:ui:e2e",
        "npm --prefix apps/gents-desktop run perf:mobile -- --runs=3 --output=test-results/mobile-performance/ci",
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
    raw = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT, check=True, stdout=subprocess.PIPE).stdout
    return parse_workspace(json.loads(raw))


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
    owner = max((name for name, (directory, _) in members.items() if path.startswith(directory + "/")),
                key=lambda name: len(members[name][0]), default=None)
    if owner:
        return tags | {f"crate:{owner}"}
    if under(path, TS_PATHS):
        return tags | {"ts"}
    if tags or under(path, CONTROL_PATHS) or under(path, INERT_PATHS) or ("/" not in path and path.endswith(".md")):
        return tags | {"inert"}
    return {"all"}


def runner(kind):
    if kind == "portable":
        override = os.environ.get("GATE_PORTABLE_RUNNER", "").strip()
        if not override:
            return STUDIO
        try:
            labels = json.loads(override)
        except json.JSONDecodeError:
            labels = None
        if not (isinstance(labels, list) and labels and all(isinstance(label, str) and label for label in labels)):
            fail(f"GATE_PORTABLE_RUNNER must be a JSON list of runner labels, got {override!r}")
        return labels
    if kind == "runtime":
        isolated = os.environ.get("GATE_RUNTIME_ISOLATED") == "true"
        return STUDIO + ["studio-1", "ci-runtime-isolated" if isolated else "ci-runtime"]
    if kind == "cli":
        return STUDIO + ["studio-2", "ci-cli"]
    if kind == "desktop":
        return STUDIO + ["studio-2", "ci-desktop"]
    return STUDIO


def portable_shards():
    """GATE_PORTABLE_SHARDS: 0 or empty keeps one job per portable group."""
    raw = os.environ.get("GATE_PORTABLE_SHARDS", "").strip()
    if not raw:
        return 0
    if not raw.isdigit() or int(raw) > 32:
        fail(f"GATE_PORTABLE_SHARDS must be a whole number from 0 to 32, got {raw!r}")
    return int(raw)


def plan(files, members, everything=False, only=None, tier="full"):
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
    jobs = {
        "fmt": rust,
        "lint": rust,
        "contract": rust or ts or proofs or "contract" in tags,
        "ts-packages": ts,
        "ts-unit": ts,
        "ts-browser": ts,
        "ts-fuzz": ts,
    }
    matrix = []
    for group in GROUPS:
        selected = [package for package in group.packages if package in tested]
        if not selected:
            continue
        jobs[f"test:{group.name}"] = True
        matrix.append({
            "group": group.name,
            "runs_on": runner(group.runner),
            "packages": " ".join(selected),
            "timeout": group.timeout,
            "go": group.name == "cli",
        })
    archives, shards = [], []
    count = portable_shards()
    if count:
        # `only` names a portable group as test:<group> or the whole archive.
        portable = {group.name for group in GROUPS if group.runner == "portable"}
        whole = only and {f"build:{ARCHIVE}", f"shard:{ARCHIVE}"} & only
        archived = [package for entry in matrix if entry["group"] in portable
                    and (not only or whole or f"test:{entry['group']}" in only)
                    for package in entry["packages"].split()]
        matrix = [entry for entry in matrix if entry["group"] not in portable]
        for group in portable:
            jobs.pop(f"test:{group}", None)
        if archived:
            # At most one slice per crate with tests, so every slice runs some.
            count = min(count, len([package for package in archived if package not in NO_TESTS]))
            archives.append({"archive": ARCHIVE, "runs_on": runner("portable"),
                             "packages": " ".join(archived), "timeout": 60})
            shards = [{"archive": ARCHIVE, "index": index, "count": count, "runs_on": runner("portable"),
                       "timeout": 60} for index in range(1, count + 1)]
    if only:
        jobs = {job: wanted and job in only for job, wanted in jobs.items()}
        matrix = [entry for entry in matrix if f"test:{entry['group']}" in only]
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
        },
        "core_packs": everything or "core-packs" in tags,
        "runs_on": {"studio": STUDIO},
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
    return plan(files, workspace(), everything, only, tier)


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
    # The product build: no test targets, so no dev-dependency features.
    sh(["cargo", "check", *packages, *profile])
    sh(["cargo", "clippy", *packages, "--all-targets", *profile, "--", "-D", "warnings"])
    # Live suites run after merge; they must still compile.
    for package in ("gents", "gents-cli"):
        if package in affected:
            sh(["cargo", "check", "-p", package, "--tests", "--features", "live-e2e", *profile])
    if "gents-loop" in affected:
        sh(["cargo", "build", "-p", "gents-loop", "--target", "wasm32-wasip1", *profile])
    if "gents-lens-fixture-add-label" in affected:
        sh(["cargo", "build", "-p", "gents-lens-fixture-add-label", "--target", "wasm32-unknown-unknown", *profile])
    if "gents-desktop-tauri" in affected:
        sh(["./scripts/check-native-e2e-capabilities.sh"])
        if sys.platform == "darwin":
            # iOS shares the desktop crates; desktop-only Tauri APIs break it silently (#890).
            sh("rustup target add aarch64-apple-ios-sim && cargo check -p gents-desktop-tauri --lib "
               "--target aarch64-apple-ios-sim " + shlex.join(profile))


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
    sh(["cargo", "run", *profile_flags(), "-p", "gents-lean-contract", "--example",
        "regenerate_household_policy", "--", "--check"])
    if selection["contract"]["ratchets"]:
        cargo_test("gents write_owner_structure", ["-p", "gents", "--test", "write_owner_structure"])
    if selection["contract"]["ts_lean"]:
        for command in TS_LEAN:
            sh(command)


def run_ts(job):
    for command in TS_JOBS[job]:
        sh(command)


def run_test(selection, name):
    group = next((group for group in GROUPS if group.name == name), None)
    entry = next((entry for entry in selection["matrix"] if entry["group"] == name), None)
    if group is None or entry is None:
        fail(f"test group {name} is not selected by this plan")
    packages = entry["packages"].split()
    harness = []
    if group.threads:
        harness.append(f"--test-threads={group.threads}")
    if group.nocapture:
        harness.append("--nocapture")
    options = {"harness": harness, "capture": not group.nocapture, "isolate_target": group.isolate_target}
    # One crate per build: a crate that compiles only through the features
    # another selected crate turns on must fail here.
    for package in packages:
        arguments = ["-p", package]
        if package in PACKAGE_FEATURES and (package != "gents-desktop-bridge" or sys.platform == "darwin"):
            arguments += ["--features", PACKAGE_FEATURES[package]]
        cargo_test(package, arguments, expect_tests=package not in NO_TESTS, **options)
    if name == "desktop" and {"gents-client-host", "gents-media-host"} & set(packages):
        if os.environ.get("GITHUB_ACTIONS") and os.environ.get("CARGO_TARGET_DIR"):
            sh(["python3", ".github/scripts/ensure-rust-build-space.py", os.environ["CARGO_TARGET_DIR"]])
        if sh([".github/scripts/desktop-runtime-journey.sh", "build"]):
            cargo_test("native client runtime journey", [
                "-p", "gents-cli", "-p", "gents-client-host", "-p", "gents-media-host",
                "--features", "gents-client-host/runtime-e2e", "--test", "runtime"])


SUMMARY = re.compile(r"^\s*Summary \[\s*([\d.]+)s\] (\d+) tests? run: (.*)$")


def archive_dir():
    """Where `build:` writes the archive and `shard:` reads it (the workflow's artifact directory)."""
    if os.environ.get("GATE_ARCHIVE_DIR"):
        return Path(os.environ["GATE_ARCHIVE_DIR"])
    if os.environ.get("GITHUB_ACTIONS") and os.environ.get("RUNNER_TEMP"):
        return Path(os.environ["RUNNER_TEMP"]) / "gents-tests"
    return ROOT / "target" / "gents-tests"


def nextest(arguments, cwd=ROOT, label=None, expect=None):
    """`cargo nextest`, recording the summary's counts; `expect` checks the listed tests per package."""
    command = ["cargo", "nextest", *arguments]
    env = dict(os.environ)
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


def listed_tests(archive, workspace):
    """Package -> (tests, ignored) in an archive, and the paths of its test binaries."""
    raw = subprocess.run(["cargo", "nextest", "list", "--archive-file", str(archive), "--workspace-remap",
                          str(workspace), "--message-format", "json", "--run-ignored", "all"],
                         cwd=workspace, check=True, stdout=subprocess.PIPE).stdout
    listing = json.loads(raw)
    tests, binaries = {}, []
    for suite in listing["rust-suites"].values():
        binaries.append(Path(suite["binary-path"]))
        package = suite["package-name"]
        total, ignored = tests.get(package, (0, 0))
        cases = suite["testcases"].values()
        tests[package] = (total + len(cases), ignored + sum(1 for case in cases if case.get("ignored")))
    return tests, binaries


def unrelocatable(binaries, source):
    """Binaries holding the build checkout's path: compile-time env!("CARGO_MANIFEST_DIR"),
    CARGO_BIN_EXE_<name> or CARGO_TARGET_TMPDIR, which a shard's checkout cannot satisfy."""
    needle = re.compile(re.escape(str(source).encode()) + rb"/(?!target/)")
    found = []
    for binary in binaries:
        if binary.is_file() and needle.search(binary.read_bytes()):
            found.append(binary.name)
    return sorted(found)


def run_build(selection, name):
    """Build every selected crate's tests once, in one Cargo invocation, into a nextest archive.

    In CI the build runs from a worktree under RUNNER_TEMP that the runner removes
    after the job, so a test binary can only reach source files through the
    runtime paths a shard remaps to its own checkout.
    """
    entry = next(entry for entry in selection["archives"] if entry["archive"] == name)
    packages = entry["packages"].split()
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
    arguments = ["archive", *[flag for package in packages for flag in ("-p", package)]]
    if features:
        arguments += ["--features", ",".join(features)]
    profile = os.environ.get("PROFILE", "")
    if profile:
        arguments += ["--cargo-profile", profile]
    arguments += ["--archive-file", str(archive)]
    if not nextest(arguments, cwd=source, label=f"build {name}: {len(packages)} crates"):
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
    if stuck := unrelocatable(binaries, source):
        problems.append(f"test binaries embed the build checkout's path {source}: {', '.join(stuck)}")
    (output / f"{name}.tests.json").write_text(json.dumps(tests, indent=2, sort_keys=True))
    total = sum(count for count, _ in tests.values())
    ignored = sum(count for _, count in tests.values())
    for problem in problems:
        print(f"::error::{problem}", flush=True)
    results.append((f"check {name} archive", "ok" if not problems else "; ".join(problems),
                    f"{total} tests listed, {ignored} ignored", time.time() - started))


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
    if threads:
        arguments += ["--test-threads", threads]
    # Tests that would otherwise `cargo build` a helper binary use the archived one.
    os.environ["GENTS_TEST_BINARIES_PREBUILT"] = "1"
    nextest(arguments, label=label)


def run(selection, job):
    if job.startswith("shard:") and job.count(":") == 2:
        _, name, index = job.split(":")
        if f"shard:{name}" not in selection["jobs"]:
            print(f"gate: shard:{name} is not selected by this plan; nothing to do")
            return
        run_shard(selection, name, int(index))
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


def verdict(needs):
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
            problems.setdefault(owner, f"the plan selected {job} but it was {result}")
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
                handle.write(f"- `test:{entry['group']}`: {entry['packages']}\n")
            for entry in selection["archives"]:
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
        problems = verdict(json.loads(os.environ["NEEDS"]))
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
