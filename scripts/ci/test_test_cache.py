#!/usr/bin/env python3
"""The test-result cache against its Lean model, and its tracer against strace logs; no builds."""

import contextlib
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_cache  # noqa: E402

PROOFS = Path(__file__).resolve().parents[2] / "crates/gents/proofs"
TESTS = [["b1", "t1"], ["b1", "t2"], ["b2", "t1"]]
KEYS = ["k1", "k2"]
PATHS = ["ws:a", "ws:b", "host:/c", "tree:p"]
ANSWERS = [{"file": "1"}, {"file": "2"}, {"dir": ["x", "y"]}, "absent"]


def random_fs(rng):
    return [[path, rng.choice(ANSWERS)] for path in PATHS if rng.random() < 0.8]


def lookup(fs):
    table = {path: answer for path, answer in fs}
    return lambda path: table.get(path, "absent")


def random_scenario(rng):
    policy = {"full": rng.random() < 0.15, "excluded": [test for test in TESTS if rng.random() < 0.2]}
    # A few file systems per scenario, so recorded traces are often met again.
    systems = [random_fs(rng) for _ in range(2)]
    steps = []
    for index in range(rng.randint(1, 12)):
        test, key, fs = rng.choice(TESTS), rng.choice(KEYS), rng.choice(systems)
        if rng.random() < 0.5:
            steps.append({"decide": {"test": test, "key": key, "fs": fs}})
        else:
            trace = [entry for entry in fs if rng.random() < 0.6]
            steps.append({"observe": {
                "test": test, "key": key, "fs": fs, "passed": rng.random() < 0.7, "trace": trace,
                "trusted": rng.random() < 0.85, "traceComplete": rng.random() < 0.85,
                "checkoutClean": rng.random() < 0.9, "run": f"r{index}"}})
    return {"policy": policy, "store": {"records": [], "contradicted": []}, "steps": steps}


def to_lean(scenario):
    def test(value):
        return {"binary": value[0], "name": value[1]}

    steps = []
    for step in scenario["steps"]:
        kind, body = next(iter(step.items()))
        steps.append({kind: {**body, "test": test(body["test"])}})
    policy = scenario["policy"]
    return {"policy": {"full": policy["full"], "excluded": [test(value) for value in policy["excluded"]]},
            "store": scenario["store"], "steps": steps}


def from_lean(result):
    """The model's output in the implementation's shapes."""
    if "records" in result:
        return {"records": [{"test": [record["test"]["binary"], record["test"]["name"]], "key": record["key"],
                             "trace": record["trace"], "run": record["run"]} for record in result["records"]],
                "contradicted": [[[test["binary"], test["name"]], key] for test, key in result["contradicted"]]}
    return result


def implementation(scenario):
    policy, store, results = scenario["policy"], scenario["store"], []
    for step in scenario["steps"]:
        kind, body = next(iter(step.items()))
        if kind == "decide":
            outcome, detail = test_cache.decide(policy, store, body["test"], body["key"], lookup(body["fs"]))
            results.append({"skip": detail["run"]} if outcome == "skip" else {"run": detail})
        else:
            store = test_cache.observe(policy, store, {**body, "fs": lookup(body["fs"])})
            results.append(store)
    return results


class Conformance(unittest.TestCase):
    """decide and observe agree with TestCache.lean's executable definitions."""

    @unittest.skipUnless(shutil.which("lake"), "lake is not installed")
    def test_random_scenarios_match_the_model(self):
        subprocess.run(["lake", "build", "Proofs.Ci.TestCache"], cwd=PROOFS, check=True, stdout=subprocess.DEVNULL)
        rng = random.Random(int(os.environ.get("TEST_CACHE_SEED", "3869")))
        scenarios = [random_scenario(rng) for _ in range(400)]
        output = subprocess.run(["lake", "env", "lean", "--run", "TestCacheConformance.lean"], cwd=PROOFS, check=True,
                                input=json.dumps([to_lean(scenario) for scenario in scenarios]),
                                stdout=subprocess.PIPE, text=True).stdout
        expected = json.loads(output)
        skips = contradictions = 0
        for scenario, model in zip(scenarios, expected):
            ours = implementation(scenario)
            self.assertEqual([from_lean(result) for result in model], ours, json.dumps(scenario))
            skips += sum(1 for result in ours if "skip" in result)
            contradictions += sum(1 for result in ours if result.get("contradicted"))
        # The generator reaches every branch the theorems cover.
        self.assertGreater(skips, 20)
        self.assertGreater(contradictions, 5)


ROOT = "/w/checkout"
EXTRACT = "/w/temp/extract"
BINARY = f"{EXTRACT}/target/debug/deps/gents_store-abc"
HOME = "/home/runner"


def context():
    return test_cache.Context(ROOT, EXTRACT, home=HOME, derived=[(f"{ROOT}/crates/gents/proofs/.lake", "crates/gents/proofs"),
                                                                  ("/home/runner/.cache/gents-lean", "crates/gents/proofs")],
                              refused=["/home/runner/.cache/gents-ci-target"])


def parse(lines, derived_pids=()):
    """Parse "PID line" strings as per-process trace files: each process's lines together, in order."""
    by_pid = {}
    for line in lines:
        pid, text = line.split(" ", 1)
        by_pid.setdefault(int(pid), []).append(text)
    records = [(pid, text) for pid, texts in by_pid.items() for text in texts]
    return test_cache.parse_trace(lambda: iter(records), context(), {BINARY}, derived_pids, "crates/gents/proofs")


def run_test(*lines, pid=100):
    """A test process: nextest's double spawn executes the binary, then the lines."""
    head = [
        f'{pid} chdir("{ROOT}/crates/gents-store") = 0',
        f'{pid} execve("{BINARY}", ["{BINARY}", "--exact", "local::home::tests::scan", "--nocapture"], '
        f'0x7ffd /* 30 vars */) = 0',
    ]
    return head + [f"{pid} {line}" for line in lines]


class Tracer(unittest.TestCase):
    def queries(self, *lines):
        tests, dirty = parse(run_test(*lines))
        trace = tests[(BINARY, "local::home::tests::scan")]
        return trace, dirty

    def test_reads_in_the_checkout_are_workspace_queries(self):
        trace, dirty = self.queries(
            f'openat(AT_FDCWD<{ROOT}/crates/gents-store>, "src/lib.rs", O_RDONLY|O_CLOEXEC) = 3<{ROOT}/crates/gents-store/src/lib.rs>',
            f'openat(AT_FDCWD<{ROOT}/crates/gents-store>, "{ROOT}/crates", O_RDONLY|O_CLOEXEC|O_DIRECTORY) = 4<{ROOT}/crates>',
            f'statx(AT_FDCWD<{ROOT}/crates/gents-store>, "missing.json", AT_STATX_SYNC_AS_STAT, STATX_ALL, 0x7f) = -1 ENOENT (No such file or directory)',
        )
        self.assertEqual(trace.queries, {"ws:crates/gents-store/src/lib.rs": "any", "ws:crates": "dir",
                                         "ws:crates/gents-store/missing.json": "absent"})
        self.assertEqual(trace.problems, [])
        self.assertEqual(dirty, {})

    def test_children_belong_to_their_test_and_inherit_its_directory(self):
        lines = run_test('clone3({flags=CLONE_VM|CLONE_VFORK, exit_signal=SIGCHLD}, 88) = 101') + [
            '101 execve("/usr/bin/git", ["git", "status"], 0x7ffd /* 30 vars */) = 0',
            '101 openat(AT_FDCWD</w/checkout/crates/gents-store>, "Cargo.toml", O_RDONLY) = 3</w/checkout/crates/gents-store/Cargo.toml>',
        ]
        tests, _ = parse(lines)
        trace = tests[(BINARY, "local::home::tests::scan")]
        self.assertEqual(trace.queries["host:/usr/bin/git"], "file")
        self.assertEqual(trace.queries["ws:crates/gents-store/Cargo.toml"], "any")

    def test_a_child_seen_before_its_parents_clone_result_is_attributed(self):
        lines = run_test('clone3({flags=CLONE_VM|CLONE_VFORK, exit_signal=SIGCHLD}, 88 <unfinished ...>') + [
            '102 execve("/usr/bin/lake", ["lake", "build"], 0x7ffd /* 30 vars */) = 0',
            '100 <... clone3 resumed>) = 102',
        ]
        tests, _ = parse(lines)
        self.assertIn("host:/usr/bin/lake", tests[(BINARY, "local::home::tests::scan")].queries)

    def test_a_relative_access_follows_the_process_directory(self):
        trace, _ = self.queries(
            f'chdir("{ROOT}/crates/gents/proofs") = 0',
            'access("lakefile.lean", F_OK) = 0',
        )
        self.assertEqual(trace.queries["ws:crates/gents/proofs/lakefile.lean"], "any")

    def test_split_lines_are_joined(self):
        trace, _ = self.queries(
            f'openat(AT_FDCWD<{ROOT}>, "README.md", O_RDONLY <unfinished ...>',
            f'<... openat resumed>) = 5<{ROOT}/README.md>',
        )
        self.assertEqual(trace.queries, {"ws:README.md": "any"})

    def test_lake_output_is_keyed_by_the_proofs_sources(self):
        trace, dirty = self.queries(
            f'openat(AT_FDCWD<{ROOT}>, "{ROOT}/crates/gents/proofs/.lake/build/lib/lean/X.olean", O_RDONLY) = 3</home/runner/.cache/gents-lean/k/build/lib/lean/X.olean>',
            f'openat(AT_FDCWD<{ROOT}>, "{ROOT}/crates/gents/proofs/.lake/lock", O_RDWR|O_CREAT, 0644) = 4<{ROOT}/crates/gents/proofs/.lake/lock>',
        )
        self.assertEqual(trace.queries, {"tree:crates/gents/proofs": "any"})
        self.assertEqual(dirty, {})

    def test_a_process_whose_lake_lines_were_dropped_queries_the_proofs(self):
        tests, _ = parse(run_test('clone3({flags=CLONE_VM}, 88) = 103') +
                         ['103 execve("/usr/bin/lake", ["lake", "build"], 0x7f /* 1 var */) = 0'], derived_pids={103})
        self.assertEqual(tests[(BINARY, "local::home::tests::scan")].queries["tree:crates/gents/proofs"], "any")

    def test_archive_binaries_and_toolchains_are_digested(self):
        trace, _ = self.queries(
            f'execve("{EXTRACT}/target/debug/gents-fs-runner", ["gents-fs-runner"], 0x7f /* 1 var */) = 0',
            f'openat(AT_FDCWD<{ROOT}>, "/home/runner/.elan/toolchains/v4/bin/lean", O_RDONLY) = 3</home/runner/.elan/toolchains/v4/bin/lean>',
        )
        self.assertIn("target:target/debug/gents-fs-runner", trace.queries)
        self.assertIn("host:~/.elan/toolchains/v4/bin/lean", trace.queries)

    def test_scratch_space_is_not_an_input(self):
        trace, _ = self.queries(
            'openat(AT_FDCWD</tmp/x>, "/tmp/.tmpA1/node/db", O_RDWR|O_CREAT, 0644) = 3</tmp/.tmpA1/node/db>',
            'openat(AT_FDCWD</tmp/x>, "/proc/self/status", O_RDONLY) = 3</proc/1/status>',
            f'openat(AT_FDCWD<{ROOT}>, "{ROOT}/target/nextest/ci/junit.xml", O_RDONLY) = 3<{ROOT}/target/nextest/ci/junit.xml>',
        )
        self.assertEqual(trace.queries, {})
        self.assertEqual(trace.problems, [])

    def test_elan_bookkeeping_is_not_an_input(self):
        trace, _ = self.queries('openat(AT_FDCWD</w>, "/home/runner/.elan/known-projects", O_RDONLY) = 3</home/runner/.elan/known-projects>')
        self.assertEqual(trace.queries, {})

    def test_unkeyed_inputs_refuse_the_record(self):
        for line, words in [
            ('connect(5<socket:[1]>, {sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr("140.82.112.3")}, 16) = 0',
             "the network"),
            ('connect(5<socket:[1]>, {sa_family=AF_INET, sin_port=htons(53), sin_addr=inet_addr("127.0.0.53")}, 16) = 0',
             "resolver"),
            ('connect(5<socket:[1]>, {sa_family=AF_UNIX, sun_path="/run/dbus/system_bus_socket"}, 110) = 0',
             "host service"),
            ('execve("/usr/bin/systemctl", ["systemctl", "--user", "stop", "x"], 0x7f /* 1 var */) = 0',
             "service manager"),
            ('openat(AT_FDCWD</w>, "/home/runner/.gents/home", O_RDONLY) = 3</home/runner/.gents/home>',
             "home"),
            ('openat(AT_FDCWD</w>, "/home/runner/.cache/gents-ci-target/debug/gents-fs-runner", O_RDONLY) = 3</home/runner/.cache/gents-ci-target/debug/gents-fs-runner>',
             "outside the run"),
            (f'openat(AT_FDCWD<{ROOT}>, "{ROOT}/target/debug/stale", O_RDONLY) = 3<{ROOT}/target/debug/stale>',
             "not the commit"),
            ('openat(AT_FDCWD</w>, "relative", O_RDONLY) = -1 EPERM (Operation not permitted)', "EPERM"),
            (f'openat(AT_FDCWD<{ROOT}>, ".git/HEAD", O_RDONLY) = 3<{ROOT}/.git/HEAD>', "not the commit"),
        ]:
            with self.subTest(words):
                trace, _ = self.queries(line)
                self.assertTrue(trace.problems, line)
                self.assertIn(words, " ".join(trace.problems))

    def test_loopback_connections_are_allowed(self):
        trace, _ = self.queries(
            'connect(5<socket:[1]>, {sa_family=AF_INET, sin_port=htons(40000), sin_addr=inet_addr("127.0.0.1")}, 16) = 0',
            'connect(5<socket:[1]>, {sa_family=AF_INET6, sin6_port=htons(40000), sin6_flowinfo=htonl(0), inet_pton(AF_INET6, "::1", &sin6_addr), sin6_scope_id=0}, 28) = 0',
            'connect(5<socket:[1]>, {sa_family=AF_UNIX, sun_path="/tmp/.tmpB/sock"}, 110) = 0',
        )
        self.assertEqual(trace.problems, [])

    def test_writes_in_the_checkout_are_attributed_to_their_test(self):
        trace, written = self.queries(
            f'openat(AT_FDCWD<{ROOT}>, "crates/gents-lean-vocab/families.schema.json", O_WRONLY|O_CREAT|O_TRUNC, 0666) = 3<{ROOT}/crates/gents-lean-vocab/families.schema.json>',
            f'renameat2(AT_FDCWD<{ROOT}>, "/tmp/x", AT_FDCWD<{ROOT}>, "crates/a.rs", RENAME_NOREPLACE) = 0',
            f'symlink("../README.md", "{ROOT}/docs/link") = 0',
        )
        key = (BINARY, "local::home::tests::scan")
        self.assertEqual(written, {"ws:crates/gents-lean-vocab/families.schema.json": {key},
                                   "ws:crates/a.rs": {key}, "ws:docs/link": {key}})
        self.assertEqual(trace.wrote, set(written))

    def test_a_probe_that_finds_nothing_in_home_is_keyed(self):
        trace, _ = self.queries('newfstatat(AT_FDCWD</w>, "/home/runner/.ghrunner/a/_work/_tool/nextest/git", 0x7f, 0) = -1 ENOENT (No such file or directory)',
                                'statx(AT_FDCWD</w>, "/home/runner/.cache", 0, STATX_ALL, {stx_mode=S_IFDIR|0755}) = 0')
        self.assertEqual(trace.queries, {"host:~/.ghrunner/a/_work/_tool/nextest/git": "absent",
                                         "up:/home/runner/.cache": "dir"})
        self.assertEqual(trace.problems, [])

    def test_ancestors_of_the_checkout_are_keyed_by_kind(self):
        trace, _ = self.queries('statx(AT_FDCWD</w>, "/w", 0, STATX_ALL, {stx_mode=S_IFDIR|0755}) = 0')
        self.assertEqual(trace.queries, {"up:/w": "dir"})

    def test_the_repository_marker_is_a_query_not_a_refusal(self):
        trace, _ = self.queries(f'statx(AT_FDCWD<{ROOT}>, "{ROOT}/.git", 0, STATX_ALL, {{stx_mode=S_IFDIR|0755}}) = 0')
        self.assertEqual(trace.queries, {"ws:.git": "any"})
        self.assertEqual(trace.problems, [])

    def test_processes_outside_a_test_are_not_attributed(self):
        tests, _ = parse(['50 openat(AT_FDCWD</w>, "/w/checkout/Cargo.toml", O_RDONLY) = 3</w/checkout/Cargo.toml>'])
        self.assertEqual(tests, {})


class Recording(unittest.TestCase):
    def setUp(self):
        self.temporary = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.temporary)
        self.store = self.temporary / "store"

    def plan(self):
        return {"mode": "on", "keys": {"b": "k"}, "runner": {}, "decisions": {}}

    def trace(self, kind="any"):
        trace = test_cache.TestTrace("/bin/b", "t")
        trace.query("ws:a", kind)
        return trace

    def record(self, passed, trace, trusted=True, dirty=()):
        answers = {"ws:a": {"file": "1"}}
        # A contradiction prints a workflow error annotation; keep it out of CI's log.
        with contextlib.redirect_stdout(io.StringIO()):
            return self.record_quietly(passed, trace, trusted, dirty, answers)

    def record_quietly(self, passed, trace, trusted, dirty, answers):
        return test_cache.record_run(self.plan(), {("b", "t"): passed}, {("/bin/b", "t"): trace},
                                     {path: {None} for path in dirty},
                                     lambda path: answers.get(path, "absent"), {"b": "/bin/b"}, trusted,
                                     root=self.store)

    def test_a_vouched_pass_is_found_by_the_next_lookup(self):
        notes, _ = self.record(True, self.trace())
        self.assertEqual(notes, {"b t": "recorded"})
        store = test_cache.read_entry(self.store, ["b", "t"], "k")
        self.assertEqual(test_cache.decide({"full": False, "excluded": []}, store, ["b", "t"], "k",
                                           lambda path: {"file": "1"})[0], "skip")
        self.assertEqual(test_cache.decide({"full": False, "excluded": []}, store, ["b", "t"], "k",
                                           lambda path: {"file": "2"}), ("run", "miss"))

    def test_untrusted_dirty_or_incomplete_runs_record_nothing(self):
        incomplete = self.trace()
        incomplete.refuse("connects to 1.2.3.4:443: the network")
        for notes, _ in [self.record(True, self.trace(), trusted=False),
                         self.record(True, self.trace(), dirty={"ws:a"}),
                         self.record(True, self.trace(), dirty={"ws:a/new-file"}),
                         self.record(True, incomplete)]:
            self.assertTrue(notes["b t"].startswith("not recorded"), notes)
        self.assertEqual(test_cache.read_entry(self.store, ["b", "t"], "k")["records"], [])

    def test_a_failure_where_a_pass_vouches_is_a_permanent_contradiction(self):
        self.record(True, self.trace())
        notes, _ = self.record(False, self.trace())
        self.assertIn("CONTRADICTED", notes["b t"])
        self.record(True, self.trace())
        store = test_cache.read_entry(self.store, ["b", "t"], "k")
        self.assertEqual(test_cache.decide({"full": False, "excluded": []}, store, ["b", "t"], "k",
                                           lambda path: {"file": "1"}), ("run", "contradicted"))

    def test_an_answer_that_changed_during_the_test_is_not_recorded(self):
        notes, _ = self.record(True, self.trace("absent"))
        self.assertIn("was absent during the test", notes["b t"])


class Broker(unittest.TestCase):
    def test_a_shim_runs_its_program_through_the_broker_with_its_descriptors_and_status(self):
        import test_cache_broker
        directory = Path(tempfile.mkdtemp(prefix="broker-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, directory)
        socket_path = str(directory / "socket")
        broker = subprocess.Popen([sys.executable, str(Path(test_cache.__file__).with_name("test_cache_broker.py")),
                                   "serve", socket_path, "/bin/sh"])
        self.addCleanup(broker.kill)
        for _ in range(100):
            if os.path.exists(socket_path):
                break
            import time
            time.sleep(0.05)
        shim = test_cache_broker.shim(str(directory / "bin"), socket_path, "sh-via-broker")
        result = subprocess.run([shim, "-c", 'read line; echo "$line in $(pwd) $GATE_PROBE"; echo err >&2; exit 3'],
                                input="hello\n", capture_output=True, text=True, cwd=str(directory),
                                env={**os.environ, "GATE_PROBE": "env"}, timeout=20)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout.strip(), f"hello in {os.path.realpath(directory)} env")
        self.assertEqual(result.stderr.strip(), "err")
        # A CI job's environment (its gate plan) spans many socket reads.
        large = "x" * (1 << 18)
        result = subprocess.run([shim, "-c", 'printf %s "${#GATE_LARGE}"'], capture_output=True, text=True,
                                env={**os.environ, "GATE_LARGE": large}, timeout=20)
        self.assertEqual((result.returncode, result.stdout), (0, str(len(large))), result.stderr)
        # Many concurrent calls each return: a reply must not wait for the broker's watcher.
        import concurrent.futures
        with concurrent.futures.ThreadPoolExecutor(16) as pool:
            codes = list(pool.map(lambda code: subprocess.run([shim, "-c", f"exit {code}"], timeout=20).returncode,
                                  range(48)))
        self.assertEqual(codes, list(range(48)))


class ArchiveAnswers(unittest.TestCase):
    def test_archive_directories_are_keyed_by_kind_and_files_by_content(self):
        extract = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, extract)
        (extract / "target/debug/build/x/out").mkdir(parents=True)
        (extract / "target/debug/build/x/out/leftover.o").write_bytes(b"runner a")
        answers = test_cache.Answers(test_cache.ROOT, extract)
        self.assertEqual(answers("target:target/debug/build/x/out"), {"dir": []})
        self.assertEqual(answers("target:target/debug/build/x/out/leftover.o"),
                         {"file": test_cache.sha256_file(extract / "target/debug/build/x/out/leftover.o")})
        self.assertEqual(answers("target:target/debug/missing"), "absent")


class Policy(unittest.TestCase):
    def test_the_repository_policy_loads_and_names_issues(self):
        policy = test_cache.load_policy()
        self.assertTrue(policy["excluded"] or policy["excluded_binaries"])
        for entry in list(policy["excluded"].values()) + list(policy["excluded_binaries"].values()):
            self.assertIsInstance(entry["issue"], int)

    def test_ci_plumbing_does_not_enter_the_key(self):
        policy = test_cache.load_policy()
        keyed = test_cache.keyed_environment(policy, {
            "GENTS_TEST_BINARIES_PREBUILT": "1", "RUST_MIN_STACK": "16777216", "GENTS_CI_KEEP_TARGET_TREE": "true",
            "GITHUB_RUN_ID": "1", "CARGO_TARGET_DIR": "/runner/a", "HOME": "/home/x"})
        self.assertEqual(keyed, {"GENTS_TEST_BINARIES_PREBUILT": "1", "RUST_MIN_STACK": "16777216"})

    def test_the_platform_enters_the_key(self):
        policy = test_cache.load_policy()
        linux = test_cache.platform_inputs("linux", "x86_64-unknown-linux-gnu")
        self.assertEqual(linux, {"os": "linux", "target": "x86_64-unknown-linux-gnu"})
        mac = test_cache.platform_inputs("darwin", "aarch64-apple-darwin", "25A100")
        self.assertEqual(mac["os_build"], "25A100")
        with self.assertRaises(ValueError):
            test_cache.platform_inputs("darwin", "aarch64-apple-darwin", "")

        def key(platform):
            return test_cache.static_key(policy, "b", "digest", test_cache.runner_inputs(policy, "nextest 1", platform))

        updated = test_cache.platform_inputs("darwin", "aarch64-apple-darwin", "25B200")
        other_arch = test_cache.platform_inputs("linux", "aarch64-unknown-linux-gnu")
        keys = [key(linux), key(mac), key(updated), key(other_arch)]
        self.assertEqual(len(set(keys)), len(keys))
        self.assertEqual(key(linux), key(dict(linux)))
        # This host's own platform is read from the toolchain and the OS.
        here = test_cache.platform_inputs()
        self.assertEqual(here["os"], sys.platform)
        self.assertEqual("os_build" in here, sys.platform == "darwin")

    def test_filters_select_exactly_the_tests_that_run(self):
        listing = {"a": ["x", "y"], "b": ["z"], "c": ["u", "v", "w"]}
        decisions = {"a x": {"run": "miss"}, "a y": {"run": "miss"}, "b z": {"skip": {}},
                     "c u": {"skip": {}}, "c v": {"run": "excluded"}, "c w": {"skip": {}}}
        self.assertEqual(test_cache.filters({"decisions": decisions}, listing),
                         ["-E", "binary_id(=a)", "-E", "binary_id(=c) & (test(=v))"])
        everything_skipped = {name: {"skip": {}} for name in decisions}
        self.assertEqual(test_cache.filters({"decisions": everything_skipped}, listing), ["-E", "none()"])


if __name__ == "__main__":
    unittest.main()
