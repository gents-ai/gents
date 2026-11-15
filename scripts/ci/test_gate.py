#!/usr/bin/env python3
"""Selection and guard checks for gate.py against the real workspace graph; no builds."""

import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate  # noqa: E402

MEMBERS = gate.workspace()
CI = (gate.ROOT / ".github/workflows/ci.yml").read_text()
SCRIPT = re.compile(r"(?<![\w/.-])(?:\./)?((?:\.github/scripts|scripts)/[\w./-]+\.(?:sh|py|mjs|json))")


def workflow_jobs(text):
    """Job id -> its text, for the top-level jobs of a workflow file."""
    body = text.split("\njobs:\n", 1)[1]
    parts = re.split(r"^  ([a-z][\w-]*):[ \t]*$", body, flags=re.M)
    return dict(zip(parts[1::2], parts[2::2]))


def select(*files, **options):
    return gate.plan(list(files), MEMBERS, **options)


def groups(selection):
    return {entry["group"]: entry["packages"].split() for entry in selection["matrix"]}


class Selection(unittest.TestCase):
    def test_every_member_has_one_group(self):
        gate.check_groups(MEMBERS)

    def test_a_client_change_leaves_the_core_alone(self):
        selection = select("crates/gents-cli/src/lib.rs")
        selected = groups(selection)
        self.assertEqual(selected["cli"], ["gents-cli"])
        self.assertEqual(set(selected), {"cli", "desktop"})
        self.assertEqual(selected["desktop"], ["gents-desktop-bridge", "gents-client-host", "gents-fixture-host",
                                               "gents-desktop-tauri"])
        self.assertNotIn("gents", selection["built"])
        self.assertTrue(selection["contract"]["ratchets"])
        self.assertFalse(any(job.startswith("ts-") for job in selection["jobs"]))

    def test_a_store_change_tests_everything_above_it(self):
        selection = select("crates/gents-store/src/graphql.rs")
        selected = groups(selection)
        self.assertEqual(set(selected), {"store", "session", "runtime", "cli", "desktop"})
        self.assertEqual(selected["store"], gate.GROUPS[1].packages)
        self.assertNotIn("gents-protocol", selection["affected"])
        self.assertFalse(selection["contract"]["ratchets"])

    def test_proofs_replay_every_consumer_without_relinting(self):
        selection = select("crates/gents/proofs/Proofs/Request/CausalHop.lean")
        self.assertEqual(set(groups(selection)), {group.name for group in gate.GROUPS})
        self.assertEqual(selection["built"], [])
        self.assertIn("contract", selection["jobs"])
        self.assertNotIn("lint", selection["jobs"])
        self.assertTrue(selection["contract"]["ts_lean"])

    def test_a_draft_tests_what_it_changes_and_lints_what_depends_on_it(self):
        selection = select("crates/gents-store/src/graphql.rs", tier="draft")
        self.assertEqual(groups(selection), {"store": ["gents-store"]})
        self.assertIn("gents-cli", selection["built"])
        self.assertIn("lint", selection["jobs"])
        self.assertTrue(selection["contract"]["ratchets"])
        proofs = select("crates/gents/proofs/Proofs.lean", tier="draft")
        self.assertEqual(set(groups(proofs)), {group.name for group in gate.GROUPS})

    def test_prose_selects_nothing(self):
        selection = select("docs/upgrade-from-rc1.md", "CHANGELOG.md", "AGENTS.md")
        self.assertEqual(selection["jobs"], [])
        self.assertEqual(selection["matrix"], [])

    def test_frontend_changes_run_the_frontend_and_the_consumer_scan(self):
        selection = select("packages/gents-desktop-ui/src/index.ts")
        self.assertEqual(selection["jobs"], ["contract", "ts-browser", "ts-fuzz", "ts-packages", "ts-unit"])

    def test_build_inputs_and_unknown_paths_select_everything(self):
        for path in ("Cargo.lock", "scripts/ci/gate.py", "third_party/noq/src/lib.rs", "new-top-level/file"):
            selection = select(path)
            self.assertTrue(selection["everything"], path)
            self.assertEqual(len(selection["affected"]), len(MEMBERS))

    def test_core_pack_inputs_are_flagged(self):
        self.assertTrue(select("core-packs/web/pack.json")["core_packs"])
        self.assertTrue(select("crates/gents-pack/src/lib.rs")["core_packs"])
        self.assertFalse(select("crates/gents-cli/src/lib.rs")["core_packs"])

    def test_a_workflow_only_change_runs_only_the_jobs_every_event_runs(self):
        selection = select(".github/workflows/core-packs.yml", ".github/scripts/test-release-signing-mode.py")
        self.assertEqual(selection["jobs"], [])
        self.assertFalse(selection["everything"])

    def test_scripts_the_selected_jobs_run_are_never_inert(self):
        # A path that classifies as inert runs only the always-on jobs, so every
        # script a plan-selected job or the runner setup calls must select work.
        jobs = workflow_jobs(CI)
        sources = [jobs[name] for name in set(gate.WORKFLOW_JOBS.values()) | set(gate.MATRIX_JOBS)]
        sources += [(gate.ROOT / ".github/actions/rust-runner/action.yml").read_text(),
                    Path(gate.__file__).read_text()]
        referenced = {path for text in sources for path in SCRIPT.findall(text)}
        self.assertIn(".github/scripts/host-runtime-service.sh", referenced)
        for path in sorted(referenced):
            self.assertTrue((gate.ROOT / path).exists(), path)
            self.assertNotEqual(gate.classify(path, MEMBERS), {"inert"}, path)

    def test_overrides_must_be_label_lists(self):
        os.environ["GATE_RUNNER_OVERRIDES"] = '{"portable": ["self-hosted", "Linux", "X64", "ci-other"]}'
        try:
            self.assertEqual(gate.runner("portable", fork=False, fallback=False),
                             ["self-hosted", "Linux", "X64", "ci-other"])
            self.assertEqual(gate.runner("cli", fork=False, fallback=False), ROLES["cli"]["labels"])
        finally:
            del os.environ["GATE_RUNNER_OVERRIDES"]
        for value in ("ci-portable", '{"portable": "ci-portable"}', '{"portable": []}', '{"portable": [""]}',
                      '{"portable": [1]}', "[]"):
            os.environ["GATE_RUNNER_OVERRIDES"] = value
            try:
                with self.assertRaises(SystemExit, msg=value):
                    gate.runner("portable", fork=False, fallback=False)
            finally:
                del os.environ["GATE_RUNNER_OVERRIDES"]

    def test_every_named_test_target_exists(self):
        # Integration tests are folded into one `integration` binary per crate;
        # a stale `--test <name>` would fail to build only on the runner.
        metadata = json.loads(subprocess.run(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            cwd=gate.ROOT, check=True, stdout=subprocess.PIPE).stdout)
        targets = {target["name"] for package in metadata["packages"] for target in package["targets"]
                   if "test" in target["kind"]}
        named = set(re.findall(r'"--test", "([\w-]+)"', Path(gate.__file__).read_text()))
        self.assertEqual(named, {"integration", "write_owner_structure"})
        self.assertLessEqual(named, targets)

    def test_only_narrows_a_plan(self):
        selection = select(everything=True, only={"lint", "test:cli"})
        self.assertEqual(selection["jobs"], ["lint", "test:cli"])
        self.assertEqual(list(groups(selection)), ["cli"])
        self.assertEqual(selection["archives"], [])


def sharded(count, *files, **options):
    os.environ["GATE_PORTABLE_SHARDS"] = str(count)
    try:
        return select(*files, **options)
    finally:
        del os.environ["GATE_PORTABLE_SHARDS"]


def archived(selection):
    return [package for entry in selection["archives"] for package in entry["packages"].split()]


class Shards(unittest.TestCase):
    PORTABLE = [package for group in gate.GROUPS if group.runner == "portable" for package in group.packages]

    def test_the_portable_groups_build_once_and_run_as_slices(self):
        selection = sharded(4, everything=True)
        self.assertEqual(set(groups(selection)), {"runtime", "cli", "desktop"})
        self.assertEqual(archived(selection), self.PORTABLE)
        self.assertEqual([(entry["index"], entry["count"]) for entry in selection["shards"]],
                         [(1, 4), (2, 4), (3, 4), (4, 4)])
        self.assertIn("build:portable", selection["jobs"])
        self.assertIn("shard:portable", selection["jobs"])
        self.assertFalse(any(job in selection["jobs"] for job in ("test:foundation", "test:store", "test:session")))

    def test_zero_or_unset_keeps_one_job_per_group(self):
        for selection in (select(everything=True), sharded(0, everything=True)):
            self.assertEqual(set(groups(selection)), {group.name for group in gate.GROUPS})
            self.assertEqual((selection["archives"], selection["shards"]), ([], []))

    def test_never_more_slices_than_crates_with_tests(self):
        selection = sharded(6, "crates/gents-store/src/graphql.rs", tier="draft")
        self.assertEqual(archived(selection), ["gents-store"])
        self.assertEqual(len(selection["shards"]), 1)
        compile_only = sharded(6, "crates/gents-core-packs/src/lib.rs", tier="draft")
        self.assertEqual(archived(compile_only), ["gents-core-packs"])
        self.assertEqual(compile_only["shards"], [])
        self.assertIn("build:portable", compile_only["jobs"])
        self.assertNotIn("shard:portable", compile_only["jobs"])

    def test_only_selects_portable_groups_or_the_whole_archive(self):
        session = sharded(4, everything=True, only={"test:session"})
        self.assertEqual(archived(session), gate.GROUPS[2].packages)
        self.assertEqual(session["jobs"], ["build:portable", "shard:portable"])
        whole = sharded(4, everything=True, only={"shard:portable", "test:cli"})
        self.assertEqual(archived(whole), self.PORTABLE)
        self.assertEqual(list(groups(whole)), ["cli"])

    def test_the_slice_count_must_be_a_small_whole_number(self):
        for value in ("-1", "four", "1.5", "33"):
            with self.assertRaises(SystemExit, msg=value):
                sharded(value, everything=True)

    def test_an_archive_is_found_in_the_runner_temp_artifact_directory(self):
        saved = {name: os.environ.get(name) for name in ("GITHUB_ACTIONS", "RUNNER_TEMP")}
        os.environ.update(GITHUB_ACTIONS="true", RUNNER_TEMP="/runner/_temp")
        try:
            self.assertEqual(gate.archive_dir(), Path("/runner/_temp/gents-tests"))
        finally:
            for name, value in saved.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value

    def test_slices_cap_their_test_threads(self):
        captured = []
        original = gate.nextest
        gate.nextest = lambda arguments, **options: captured.append(arguments) or True
        saved = os.environ.get("RUST_TEST_THREADS")
        try:
            for threads in ("12", "4"):
                os.environ["RUST_TEST_THREADS"] = threads
                gate.run_shard(sharded(3, everything=True), "portable", 1)
        finally:
            gate.nextest = original
            os.environ.pop("GENTS_TEST_BINARIES_PREBUILT", None)
            if saved is None:
                os.environ.pop("RUST_TEST_THREADS", None)
            else:
                os.environ["RUST_TEST_THREADS"] = saved
        threads = [arguments[arguments.index("--test-threads") + 1] for arguments in captured]
        self.assertEqual(threads, [str(gate.SLICE_THREADS), "4"])

    def test_a_shard_job_names_its_slice(self):
        selection = sharded(3, everything=True)
        with self.assertRaises(SystemExit):
            gate.run_shard(selection, "portable", 4)

    def test_no_portable_crate_has_a_doctest(self):
        self.assertEqual(gate.doctests(self.PORTABLE), [])
        for line in ("/// ```", "    //! ```rust", '#[doc = "```"]', "/// ```no_run"):
            self.assertTrue(gate.DOC_FENCE.match(line), line)
        for line in ("/// ```text", "//! ```ignore", "// ```", "let fence = \"```\";"):
            self.assertFalse(gate.DOC_FENCE.match(line), line)

    def test_a_binary_holding_the_build_checkout_path_is_unrelocatable(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "src"
            portable = Path(directory) / "portable"
            portable.write_bytes(b"\0" + str(source / "target/debug/build/x/out").encode() + b"\0")
            pinned = Path(directory) / "pinned"
            pinned.write_bytes(b"\0" + str(source / "crates/gents-pack").encode() + b"\0")
            self.assertEqual(gate.unrelocatable([portable, pinned], source), ["pinned"])


ROLES = gate.inventory()["roles"]
HOSTED = re.compile(r"(ubuntu|macos|windows)-[\w.-]+")


def runs_on_values(text):
    """(job, runs-on as written) for every job of a workflow file."""
    for job, body in workflow_jobs(text).items():
        match = re.search(r"^    runs-on:[ \t]*(.*)\n((?:      .*\n)*)", body, flags=re.M)
        if match:
            yield job, " ".join([match.group(1).strip()] + [line.strip() for line in match.group(2).splitlines()])


def labels_of(value):
    """The literal labels a runs-on value can name: list items, `- item` lines
    and the quoted strings inside expressions."""
    value = value.strip()
    # In an expression, a runner is a result of `&&`/`||` or a fromJSON list;
    # other quoted strings are operands of comparisons.
    literals = re.findall(r"(?:&&|\|\|)\s*'([^'\[]*)'", value)
    for literal in re.findall(r"fromJSON\('(\[[^']*\])'\)", value):
        literals += json.loads(literal)
    plain = re.sub(r"\$\{\{.*?\}\}", "", value).strip()
    if plain.startswith(">-"):
        plain = ""
    plain = plain.strip("[]")
    items = [item.strip().strip('"') for item in re.split(r"[,\s-]+\s", " " + plain + " ") if item.strip()]
    return {label for label in literals + items if label}


class Runners(unittest.TestCase):
    """Jobs choose runners by role labels from .github/ci-runners.json, never by host."""

    def test_a_fork_never_resolves_to_a_self_hosted_runner(self):
        for role in ROLES:
            value = gate.runner(role, fork=True, fallback=False)
            self.assertIsInstance(value, str, role)
            self.assertRegex(value, HOSTED, role)

    def test_fallback_moves_the_shared_roles_off_our_linux_pool(self):
        self.assertEqual(gate.runner("linux", fork=False, fallback=True), "ubuntu-latest")
        self.assertEqual(gate.runner("browser", fork=False, fallback=True), "ubuntu-latest")

    def test_a_role_whose_pool_is_hosted_runs_hosted(self):
        for role, entry in ROLES.items():
            if entry.get("pool") == "hosted":
                self.assertEqual(gate.runner(role, fork=False, fallback=False), entry["hosted"], role)
            else:
                self.assertNotIn("pool", entry, role)
                self.assertEqual(gate.runner(role, fork=False, fallback=False), entry["labels"], role)
        self.assertEqual(gate.runner("portable", fork=False, fallback=True), ROLES["macos"]["labels"])
        self.assertEqual(gate.runner("cli", fork=False, fallback=True), ROLES["cli"]["labels"])

    def test_every_role_is_self_hosted_capability_labels(self):
        for role, entry in ROLES.items():
            labels = entry["labels"]
            self.assertEqual(labels[0], "self-hosted", role)
            self.assertIn(labels[1], ("Linux", "macOS"), role)
            self.assertIn(labels[2], ("X64", "ARM64"), role)
            self.assertTrue(all(label.startswith("ci-") for label in labels[3:]), role)
            fallback = entry.get("fallback")
            if fallback:
                self.assertTrue(fallback in ROLES or HOSTED.fullmatch(fallback), role)

    def test_every_building_role_names_its_host_cache(self):
        # A role whose jobs build Rust shares one host sccache server per
        # host; the setup step reaches it through the supervisor named here.
        for role, entry in ROLES.items():
            if "target" not in entry:
                continue
            with self.subTest(role=role):
                key = "systemd_unit" if entry["labels"][1] == "Linux" else "launchd_label"
                self.assertEqual(set(entry.get("cache", {})), {key})
                self.assertTrue(entry["cache"][key])

    def test_the_plan_routes_every_job_and_group(self):
        selection = select(everything=True)
        self.assertEqual(set(selection["runs_on"]), set(gate.WORKFLOW_ROLES))
        for entry in selection["matrix"]:
            self.assertEqual(entry["runs_on"], gate.runner(entry["role"]), entry["group"])
        jobs = workflow_jobs(CI)
        for job, value in runs_on_values(CI):
            with self.subTest(job=job):
                routed = re.findall(r"fromJSON\(needs\.plan\.outputs\.plan\)\.runs_on\.([\w-]+)", value)
                self.assertTrue(routed or "matrix.runs_on" in value or job == "plan", value)
                self.assertLessEqual(set(routed), set(gate.WORKFLOW_ROLES))
                if job not in ("plan", "gate"):
                    self.assertRegex(jobs[job], r"needs: (plan|\[plan)")

    def test_literal_linux_labels_are_the_linux_role(self):
        for path in sorted((gate.ROOT / ".github/workflows").glob("*.yml")):
            for job, value in runs_on_values(path.read_text()):
                for literal in re.findall(r"fromJSON\('(\[[^']*\])'\)", value):
                    self.assertEqual(json.loads(literal), ROLES["linux"]["labels"], f"{path.name} {job}")

    def test_no_workflow_selects_a_runner_by_host(self):
        vocabulary = {"self-hosted"} | {label for entry in ROLES.values() for label in entry["labels"]}
        role_sets = [entry["labels"] for entry in ROLES.values()]
        for path in sorted((gate.ROOT / ".github/workflows").glob("*.yml")):
            text = path.read_text()
            self.assertNotRegex(text, r"runner\.name\s*(==|!=)", path.name)
            for job, value in runs_on_values(text):
                with self.subTest(workflow=path.name, job=job):
                    labels = labels_of(value)
                    named = {label for label in labels if not HOSTED.fullmatch(label)}
                    self.assertLessEqual(named, vocabulary | {"plan", "runs_on"}, value)
                    if "${{" not in value and "self-hosted" in labels:
                        self.assertIn(sorted(labels), [sorted(labels) for labels in role_sets], value)

    def test_actionlint_knows_exactly_the_inventory_labels(self):
        config = (gate.ROOT / ".github/actionlint.yaml").read_text()
        listed = set(re.findall(r"^    - (\S+)$", config.split("paths:")[0], flags=re.M))
        self.assertEqual(listed, {label for entry in ROLES.values() for label in entry["labels"]} - {"self-hosted"})


class Verdict(unittest.TestCase):
    """The `gate` job: `needs` as GitHub passes it, for the plans each event makes."""

    def needs(self, selection, **results):
        names = set(gate.ALWAYS) | set(gate.WORKFLOW_JOBS.values()) | set(gate.MATRIX_JOBS)
        selected = {gate.workflow_job(job) for job in selection["jobs"]}
        needs = {name: {"result": "success" if name in selected or name in gate.ALWAYS else "skipped",
                        "outputs": {}} for name in names}
        needs["plan"]["outputs"] = {"plan": json.dumps(selection)}
        for name, result in results.items():
            needs[name.replace("_", "-")]["result"] = result
        return needs

    def test_the_gate_needs_every_job_it_judges(self):
        listed = re.search(r"^  gate:\n(?:    .*\n)*?    needs:\n((?:      - \S+\n)+)", CI, flags=re.M)
        self.assertIsNotNone(listed)
        needs = set(re.findall(r"- (\S+)", listed.group(1)))
        self.assertEqual(needs, set(gate.ALWAYS) | set(gate.WORKFLOW_JOBS.values()) | set(gate.MATRIX_JOBS))
        self.assertEqual(set(workflow_jobs(CI)) - {"gate"}, needs)
        self.assertEqual(gate.verdict({name: {"result": "success"} for name in needs - {"lint"}}),
                         ["lint: missing from the gate job's needs", "plan: produced no plan"])

    def test_every_event_passes_when_its_jobs_pass(self):
        plans = {
            "docs only": select("docs/upgrade-from-rc1.md", "AGENTS.md"),
            "workflow only": select(".github/workflows/core-packs.yml"),
            "one crate": select("crates/gents-cli/src/lib.rs"),
            "draft": select("crates/gents-store/src/graphql.rs", tier="draft"),
            "frontend": select("packages/gents-desktop-ui/src/index.ts"),
            "everything (push, merge queue, dispatch)": select(everything=True),
            "everything, sharded": sharded(4, everything=True),
            "draft, sharded": sharded(4, "crates/gents-store/src/graphql.rs", tier="draft"),
            "compile only, sharded": sharded(4, "crates/gents-core-packs/src/lib.rs", tier="draft"),
        }
        for event, selection in plans.items():
            self.assertEqual(gate.verdict(self.needs(selection)), [], event)

    def test_a_selected_job_that_was_skipped_fails(self):
        selection = select("crates/gents-cli/src/lib.rs")
        for job in ("lint", "test", "fmt", "contract"):
            problems = gate.verdict(self.needs(selection, **{job: "skipped"}))
            self.assertEqual(len(problems), 1, job)
            self.assertIn(f"{job}: the plan selected", problems[0])

    def test_a_selected_build_or_shard_that_was_skipped_fails(self):
        selection = sharded(4, everything=True)
        for job in ("build", "shard"):
            problems = gate.verdict(self.needs(selection, **{job: "skipped"}))
            self.assertEqual(problems, [f"{job}: the plan selected {job}:portable but it was skipped"], job)
        self.assertEqual(gate.verdict(self.needs(selection, shard="failure")), ["shard: failure"])

    def test_a_job_the_plan_left_out_may_be_skipped_but_not_fail(self):
        selection = select("docs/upgrade-from-rc1.md")
        self.assertEqual(gate.verdict(self.needs(selection, lint="skipped")), [])
        self.assertEqual(gate.verdict(self.needs(selection, desktop_ui="failure")), ["desktop-ui: failure"])

    def test_failed_or_cancelled_jobs_fail(self):
        selection = select(everything=True)
        for result in ("failure", "cancelled"):
            self.assertEqual(gate.verdict(self.needs(selection, test=result)), [f"test: {result}"])
            self.assertEqual(gate.verdict(self.needs(selection, repository_guard=result)),
                             [f"repository-guard: {result}"])

    def test_jobs_every_event_runs_must_succeed(self):
        selection = select("docs/upgrade-from-rc1.md")
        for name in gate.ALWAYS[1:]:
            problems = gate.verdict(self.needs(selection, **{name.replace("-", "_"): "skipped"}))
            self.assertEqual(problems, [f"{name}: runs on every event but was skipped"])

    def test_a_fork_is_told_why_its_selected_jobs_were_skipped(self):
        selection = select("crates/gents-cli/src/lib.rs")
        needs = self.needs(selection, lint="skipped", contract="skipped", test="skipped")
        problems = gate.verdict(needs, fork=True)
        self.assertEqual(len(problems), 3)
        self.assertTrue(all("comes from a fork" in problem for problem in problems))
        self.assertEqual(gate.verdict(self.needs(select("docs/upgrade-from-rc1.md")), fork=True), [])

    def test_no_plan_fails(self):
        needs = self.needs(select("docs/upgrade-from-rc1.md"), plan="failure")
        needs["plan"]["outputs"] = {}
        self.assertEqual(gate.verdict(needs), ["plan: failure"])


class Guards(unittest.TestCase):
    def run_fake(self, script, **options):
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "cargo"
            fake.write_text("#!/bin/sh\n" + script)
            fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
            path = os.environ["PATH"]
            os.environ["PATH"] = directory + os.pathsep + path
            gate.results.clear()
            try:
                return gate.cargo_test("fake", ["-p", "fake"], **options), gate.results[-1][1]
            finally:
                os.environ["PATH"] = path

    def test_passing_tests_pass(self):
        ok, verdict = self.run_fake(
            "echo '     Running unittests src/lib.rs'\necho 'test a ... ok'\n"
            "echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n")
        self.assertTrue(ok, verdict)

    def test_a_run_that_selects_zero_tests_fails(self):
        ok, verdict = self.run_fake(
            "echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out'\n")
        self.assertFalse(ok)
        self.assertEqual(verdict, "selected zero tests")

    def test_a_stale_no_tests_entry_fails(self):
        ok, verdict = self.run_fake(
            "echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n",
            expect_tests=False)
        self.assertFalse(ok)
        self.assertIn("NO_TESTS", verdict)

    def test_a_failing_run_fails(self):
        ok, verdict = self.run_fake(
            "echo 'test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'\nexit 101\n")
        self.assertFalse(ok)
        self.assertEqual(verdict, "exit 101")

    def test_a_hung_test_is_killed_and_named(self):
        timeout, gate.TEST_TIMEOUT = gate.TEST_TIMEOUT, 2
        try:
            ok, verdict = self.run_fake(
                "echo '     Running unittests src/lib.rs'\necho 'test fine ... ok'\n"
                "echo 'test stuck has been running for over 60 seconds'\nsleep 600\n")
        finally:
            gate.TEST_TIMEOUT = timeout
        self.assertFalse(ok)
        self.assertIn("stuck", verdict)
        self.assertNotIn("fine", verdict)


if __name__ == "__main__":
    unittest.main()
