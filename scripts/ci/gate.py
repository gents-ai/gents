#!/usr/bin/env python3
"""The pull request gate, shared by CI and `make gate`.

  gate.py plan [--base REF | --files FILE]   select jobs for a change
  gate.py run JOB                            run one job (fmt, lint, contract, ts-*, test:<group>)
  gate.py all [--base REF]                   plan, then run every selected job

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
    def __init__(self, name, runner, packages, joint=False, threads=None, nocapture=False,
                 isolate_target=False, timeout=120):
        self.name = name
        self.runner = runner
        self.packages = packages
        self.joint = joint
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
    # One feature graph: notify/macos_kqueue reaches every desktop test only
    # when gents-desktop-bridge is built with them.
    Group("desktop", "desktop", [
        "gents-desktop-core", "gents-desktop", "gents-desktop-bridge", "gents-client-host",
        "gents-media-host", "fixture-domain-plugin", "gents-fixture-host", "gents-desktop-tauri",
    ], joint=True, timeout=150),
]

PACKAGE_FEATURES = {
    "gents-claude": "gents-claude/transport",
    "gents-protocol": "gents-protocol/typescript",
}
JOINT_FEATURES = {"desktop": "notify/macos_kqueue"}

# Packages with no test of their own. The list only shrinks: a listed package
# that runs a test fails the gate, as does an unlisted one that runs none.
NO_TESTS = {
    "fixture-domain-plugin",
    "gents-callback-fixture-create-workspace",
    "gents-core-packs",
    "gents-desktop",
    "gents-lens-fixture-add-label",
}

# A change here can alter any build or the gate itself, so it selects everything.
GLOBAL_PATHS = (
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "clippy.toml", "rustfmt.toml",
    ".cargo/", "Makefile", "scripts/ci/", ".github/workflows/ci.yml", ".github/actions/",
    ".github/scripts/setup-", ".github/scripts/rustc-direct.sh",
    ".github/scripts/ensure-rust-build-space.py", ".github/scripts/desktop-runtime-journey.sh",
    "scripts/macos-clang-lld.sh", "scripts/linux-lld.sh", "third_party/", "examples/",
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
        return json.loads(override) if override else STUDIO
    if kind == "runtime":
        isolated = os.environ.get("GATE_RUNTIME_ISOLATED") == "true"
        return STUDIO + ["studio-1", "ci-runtime-isolated" if isolated else "ci-runtime"]
    if kind == "cli":
        return STUDIO + ["studio-2", "ci-cli"]
    if kind == "desktop":
        return STUDIO + ["studio-2", "ci-desktop"]
    return STUDIO


def plan(files, members, everything=False, only=None):
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
        selected = [package for package in group.packages if package in affected]
        if not selected:
            continue
        jobs[f"test:{group.name}"] = True
        matrix.append({
            "group": group.name,
            "runs_on": runner(group.runner),
            "packages": " ".join(group.packages if group.joint else selected),
            "timeout": group.timeout,
            "go": group.name == "cli",
        })
    if only:
        jobs = {job: wanted and job in only for job, wanted in jobs.items()}
        matrix = [entry for entry in matrix if f"test:{entry['group']}" in only]
    return {
        "everything": everything,
        "reason": reasons.get("all", ""),
        "changed": sorted(changed),
        "built": sorted(built),
        "affected": sorted(affected),
        "jobs": sorted(job for job, wanted in jobs.items() if wanted),
        "matrix": matrix,
        "contract": {
            # write_owner_structure scans every crate's sources but is a
            # gents test target, which the runtime group runs when selected.
            "ratchets": rust and "gents" not in affected,
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
    if getattr(arguments, "files", None):
        files = [line.strip() for line in Path(arguments.files).read_text().splitlines() if line.strip()]
    elif everything:
        files = []
    else:
        files = changed_files(arguments.base)
    return plan(files, workspace(), everything, only)


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
    if group.joint:
        arguments = [flag for package in packages for flag in ("-p", package)]
        if name in JOINT_FEATURES:
            arguments += ["--features", JOINT_FEATURES[name]]
        cargo_test(name, arguments, **options)
    else:
        for package in packages:
            arguments = ["-p", package]
            if package in PACKAGE_FEATURES:
                arguments += ["--features", PACKAGE_FEATURES[package]]
            cargo_test(package, arguments, expect_tests=package not in NO_TESTS, **options)
    if name == "desktop":
        if sh([".github/scripts/desktop-runtime-journey.sh", "build"]):
            cargo_test("native client runtime journey", [
                "-p", "gents-cli", "-p", "gents-client-host", "-p", "gents-media-host",
                "--features", "gents-client-host/runtime-e2e", "--test", "runtime"])


def run(selection, job):
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
    else:
        fail(f"unknown job {job}")


def emit(selection):
    print(json.dumps(selection, indent=2))
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as handle:
            handle.write(f"plan={json.dumps(selection)}\n")
            handle.write(f"matrix={json.dumps({'include': selection['matrix']})}\n")
            handle.write(f"jobs={json.dumps(selection['jobs'])}\n")
            handle.write(f"tests={'true' if selection['matrix'] else 'false'}\n")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a") as handle:
            scope = f"everything ({selection['reason'] or 'not a pull request'})" if selection["everything"] else \
                f"{len(selection['affected'])} affected crates from {len(selection['changed'])} changed"
            handle.write(f"### Gate plan\n\nScope: {scope}\n\nJobs: {', '.join(selection['jobs']) or 'none'}\n\n")
            for entry in selection["matrix"]:
                handle.write(f"- `test:{entry['group']}`: {entry['packages']}\n")


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
    arguments = parser.parse_args()
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
