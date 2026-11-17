#!/usr/bin/env python3
"""Selection and guard checks for gate.py against the real workspace graph; no builds."""

import contextlib
import hashlib
import io
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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
    return {entry["job"][len("test:"):]: entry["packages"].split() for entry in selection["matrix"]}


class TestCacheMode(unittest.TestCase):
    def mode(self, single=True, **environment):
        names = ("GATE_TEST_CACHE", "GATE_TEST_CACHE_INPUT", "GATE_TEST_CACHE_MAIN", "GATE_EVENT", "GATE_LABELS")
        saved = {name: os.environ.pop(name, None) for name in names}
        os.environ.update(environment)
        try:
            return gate.test_cache_mode(single)
        finally:
            for name in names:
                os.environ.pop(name, None)
                if saved[name] is not None:
                    os.environ[name] = saved[name]

    def test_off_until_the_repository_turns_it_on(self):
        self.assertEqual(self.mode(GATE_EVENT="pull_request"), "off")
        self.assertEqual(self.mode(GATE_EVENT="push"), "off")

    def test_pull_requests_use_it_and_main_runs_everything(self):
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="pull_request"), "on")
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="merge_group"), "on")
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="push"), "full")
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="push", GATE_TEST_CACHE_MAIN="on"), "on")
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="schedule"), "full")

    def test_a_label_or_a_dispatch_forces_a_full_run(self):
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="pull_request",
                                   GATE_LABELS='["test-cache: full"]'), "full")
        self.assertEqual(self.mode(GATE_EVENT="workflow_dispatch", GATE_TEST_CACHE_INPUT="full"), "full")
        self.assertEqual(self.mode(GATE_TEST_CACHE="on", GATE_EVENT="workflow_dispatch",
                                   GATE_TEST_CACHE_INPUT="off"), "off")

    def scheduled(self, **environment):
        names = ("GATE_EVENT", "GATE_TEST_CACHE", "GATE_PLAN", "GATE_PORTABLE_SHARDS")
        saved = {name: os.environ.pop(name, None) for name in names}
        os.environ.update(GATE_EVENT="schedule", **environment)
        try:
            arguments = type("Arguments", (), {"everything": True, "files": None, "base": "HEAD"})()
            return gate.resolve_plan(arguments)
        finally:
            for name in names:
                os.environ.pop(name, None)
            os.environ.update({name: value for name, value in saved.items() if value is not None})

    def test_the_scheduled_run_runs_the_moved_groups_on_macos_uncached(self):
        mirrored = {f"test:{group.name}@macos" for group in gate.GROUPS if group.macos}
        self.assertEqual(mirrored, {"test:runtime@macos", "test:cli@macos", "test:desktop@macos"})
        selection = self.scheduled()
        self.assertEqual(set(selection["jobs"]), mirrored)
        for entry in selection["matrix"]:
            group = next(group for group in gate.GROUPS if group.name == entry["group"])
            self.assertEqual(entry["role"], group.macos)
            self.assertEqual(entry["runs_on"], gate.runner(group.macos))
            self.assertEqual(entry["packages"].split(), group.packages)
        self.assertEqual(selection["archives"], [])
        cached = self.scheduled(GATE_TEST_CACHE="on", GATE_PORTABLE_SHARDS="1")
        self.assertEqual(set(cached["jobs"]), mirrored | {"build:portable"})
        self.assertEqual(cached["archives"][0]["test_cache"], "full")
        # Pull requests and pushes never run the macOS copies.
        self.assertFalse(any("@" in job for job in select(everything=True)["jobs"]))

    def test_it_needs_the_single_slice_in_the_build_job(self):
        self.assertEqual(self.mode(False, GATE_TEST_CACHE="on", GATE_EVENT="pull_request"), "off")


class Selection(unittest.TestCase):
    def test_every_member_has_one_group(self):
        gate.check_groups(MEMBERS)

    def test_a_client_change_leaves_the_core_alone(self):
        selection = select("crates/gents-cli/src/lib.rs")
        selected = groups(selection)
        self.assertEqual(selected["cli"], ["gents-cli"])
        self.assertEqual(set(selected), {"cli", "desktop", "native", gate.MACOS_ONLY})
        self.assertEqual(selected["desktop"], ["gents-client-host"])
        self.assertEqual(selected["native"], ["gents-desktop-bridge", "gents-fixture-host", "gents-desktop-tauri"])
        self.assertEqual(selected[gate.MACOS_ONLY], [])
        native = next(entry for entry in selection["matrix"] if entry["job"] == f"test:{gate.MACOS_ONLY}")
        self.assertEqual(native["macos_tests"], ["gents-cli", "gents-client-host"])
        self.assertTrue(native["journey"])
        self.assertTrue(native["go"])
        self.assertNotIn("gents", selection["built"])
        self.assertTrue(selection["contract"]["ratchets"])
        self.assertFalse(any(job.startswith("ts-") for job in selection["jobs"]))

    def test_a_store_change_tests_everything_above_it(self):
        selection = select("crates/gents-store/src/graphql.rs")
        selected = groups(selection)
        self.assertEqual(set(selected), {"store", "session", "runtime", "cli", "desktop", "native", gate.MACOS_ONLY})
        self.assertEqual(selected["store"], gate.GROUPS[1].packages)
        self.assertNotIn("gents-protocol", selection["affected"])
        self.assertFalse(selection["contract"]["ratchets"])

    def test_proofs_replay_every_consumer_without_relinting(self):
        selection = select("crates/gents/proofs/Proofs/Request/CausalHop.lean")
        self.assertEqual(set(groups(selection)), {group.name for group in gate.GROUPS} | {gate.MACOS_ONLY})
        self.assertEqual(selection["built"], [])
        self.assertIn("contract", selection["jobs"])
        self.assertNotIn("lint", selection["jobs"])
        self.assertTrue(selection["contract"]["ts_lean"])

    def test_the_test_cache_model_check_runs_only_for_the_caches_own_files(self):
        for path in ("crates/gents/proofs/Proofs/Ci/TestCache.lean", "scripts/ci/test_cache.py",
                     ".github/scripts/check-test-cache-model.sh", "crates/gents/proofs/lean-toolchain"):
            with self.subTest(path=path):
                selection = select(path)
                self.assertIn("contract", selection["jobs"])
                self.assertTrue(selection["contract"]["test_cache_model"])
        for path in ("crates/gents/proofs/Proofs/Request/CausalHop.lean", "scripts/ci/gate.py",
                     "crates/gents-store/src/graphql.rs", "Cargo.lock"):
            with self.subTest(path=path):
                self.assertFalse(select(path)["contract"]["test_cache_model"])

    def test_a_draft_tests_what_it_changes_and_lints_what_depends_on_it(self):
        selection = select("crates/gents-store/src/graphql.rs", tier="draft")
        self.assertEqual(groups(selection), {"store": ["gents-store"]})
        self.assertIn("gents-cli", selection["built"])
        self.assertIn("lint", selection["jobs"])
        self.assertTrue(selection["contract"]["ratchets"])
        proofs = select("crates/gents/proofs/Proofs.lean", tier="draft")
        self.assertEqual(set(groups(proofs)), {group.name for group in gate.GROUPS} | {gate.MACOS_ONLY})

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
        self.assertEqual(set(groups(selection)), {"native", gate.MACOS_ONLY})
        self.assertEqual(archived(selection), self.PORTABLE)
        self.assertTrue(selection["archives"][0]["go"])
        self.assertEqual([(entry["index"], entry["count"]) for entry in selection["shards"]],
                         [(1, 4), (2, 4), (3, 4), (4, 4)])
        self.assertIn("build:portable", selection["jobs"])
        self.assertIn("shard:portable", selection["jobs"])
        self.assertFalse(any(job in selection["jobs"] for job in ("test:foundation", "test:store", "test:session")))

    def test_one_slice_runs_in_the_build_job(self):
        selection = sharded(1, everything=True)
        self.assertEqual(selection["shards"], [])
        self.assertTrue(selection["archives"][0]["run"])
        self.assertIn("build:portable", selection["jobs"])
        self.assertNotIn("shard:portable", selection["jobs"])
        self.assertFalse(sharded(3, everything=True)["archives"][0]["run"])

    def test_zero_or_unset_keeps_one_job_per_group(self):
        for selection in (select(everything=True), sharded(0, everything=True)):
            self.assertEqual(set(groups(selection)), {group.name for group in gate.GROUPS} | {gate.MACOS_ONLY})
            self.assertEqual((selection["archives"], selection["shards"]), ([], []))

    def test_never_more_slices_than_crates_with_tests(self):
        selection = sharded(6, "crates/gents-store/src/graphql.rs", tier="draft")
        self.assertEqual(archived(selection), ["gents-store"])
        self.assertEqual(selection["shards"], [])
        self.assertTrue(selection["archives"][0]["run"])
        compile_only = sharded(6, "crates/gents-core-packs/src/lib.rs", tier="draft")
        self.assertEqual(archived(compile_only), ["gents-core-packs"])
        self.assertEqual(compile_only["shards"], [])
        self.assertIn("build:portable", compile_only["jobs"])
        self.assertNotIn("shard:portable", compile_only["jobs"])

    def test_only_selects_portable_groups_or_the_whole_archive(self):
        session = sharded(4, everything=True, only={"test:session"})
        self.assertEqual(archived(session), gate.GROUPS[2].packages)
        self.assertEqual(session["jobs"], ["build:portable", "shard:portable"])
        whole = sharded(4, everything=True, only={"shard:portable", "test:native"})
        self.assertEqual(archived(whole), self.PORTABLE)
        self.assertEqual(list(groups(whole)), ["native"])

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


class RepositoryVariables(unittest.TestCase):
    """The plan a push to main and a pull request make with GENTS_CI_PORTABLE_SHARDS
    and GENTS_CI_TEST_CACHE set, through the plan step's environment and outputs.

    GitHub reads `vars` when it creates a run: a run created before a variable
    changed plans with the old value, however late its plan job starts."""

    NAMES = ("GATE_EVENT", "GATE_LABELS", "GATE_TEST_CACHE", "GATE_TEST_CACHE_INPUT", "GATE_TEST_CACHE_MAIN",
             "GATE_PLAN", "GATE_PORTABLE_SHARDS", "GATE_EVERYTHING", "GATE_ONLY", "GATE_TIER", "GITHUB_OUTPUT",
             "GITHUB_STEP_SUMMARY")

    def planned(self, files=None, **environment):
        saved = {name: os.environ.pop(name, None) for name in self.NAMES}
        os.environ.update({"GATE_TEST_CACHE": "on", "GATE_PORTABLE_SHARDS": "1", "GATE_LABELS": "[]", **environment})
        try:
            with tempfile.TemporaryDirectory() as directory:
                listed = None
                if files is not None:
                    listed = Path(directory) / "changed-files.txt"
                    listed.write_text("\n".join(files) + "\n")
                os.environ["GITHUB_OUTPUT"] = str(Path(directory) / "output")
                arguments = type("Arguments", (), {"everything": False, "files": listed and str(listed),
                                                   "base": "HEAD"})()
                with contextlib.redirect_stdout(io.StringIO()):
                    gate.emit(gate.resolve_plan(arguments))
                lines = Path(os.environ["GITHUB_OUTPUT"]).read_text().splitlines()
                return {key: json.loads(value) if key != "tests" else value
                        for key, value in (line.split("=", 1) for line in lines)}
        finally:
            for name in self.NAMES:
                os.environ.pop(name, None)
            os.environ.update({name: value for name, value in saved.items() if value is not None})

    def assert_cached_build(self, outputs, mode):
        portable = {f"test:{group.name}" for group in gate.GROUPS if group.runner == "portable"}
        self.assertIn("build:portable", outputs["jobs"])
        self.assertNotIn("shard:portable", outputs["jobs"])
        self.assertFalse(portable & set(outputs["jobs"]))
        self.assertFalse(portable & {entry["job"] for entry in outputs["matrix"]["include"]})
        [archive] = outputs["archives"]["include"]
        self.assertTrue(archive["run"])
        self.assertEqual(archive["test_cache"], mode)
        self.assertEqual(outputs["shards"]["include"], [])

    def test_the_plan_step_reads_the_repository_variables(self):
        step = CI.split("- name: Select gate jobs", 1)[1].split("\n      - ", 1)[0]
        for line in ("GATE_EVENT: ${{ github.event_name }}",
                     "GATE_TEST_CACHE: ${{ vars.GENTS_CI_TEST_CACHE }}",
                     "GATE_PORTABLE_SHARDS: ${{ inputs.portable_shards || vars.GENTS_CI_PORTABLE_SHARDS }}"):
            self.assertIn(line, step)

    def test_build_and_shard_read_the_plan_outputs_gate_writes(self):
        jobs = workflow_jobs(CI)
        for name in ("jobs", "archives", "shards", "matrix", "plan", "tests"):
            self.assertIn(f"{name}: ${{{{ steps.plan.outputs.{name} }}}}", jobs["plan"])
        self.assertIn("contains(fromJSON(needs.plan.outputs.jobs), 'build:portable')", jobs["build"])
        self.assertIn("matrix: ${{ fromJSON(needs.plan.outputs.archives) }}", jobs["build"])
        self.assertIn("contains(fromJSON(needs.plan.outputs.jobs), 'shard:portable')", jobs["shard"])
        self.assertIn("matrix: ${{ fromJSON(needs.plan.outputs.shards) }}", jobs["shard"])

    def test_a_push_to_main_builds_once_and_runs_the_cached_slice_in_full(self):
        outputs = self.planned(GATE_EVENT="push", GATE_EVERYTHING="true", GATE_TIER="full")
        self.assert_cached_build(outputs, "full")
        self.assertEqual(outputs["archives"]["include"][0]["packages"].split(), Shards.PORTABLE)
        self.assertIn(f"test:{gate.NATIVE}", outputs["jobs"])
        self.assertIn(f"test:{gate.MACOS_ONLY}", outputs["jobs"])

    def test_a_pull_request_builds_once_and_uses_the_cache(self):
        outputs = self.planned(["crates/gents-store/src/graphql.rs"], GATE_EVENT="pull_request", GATE_TIER="full")
        self.assert_cached_build(outputs, "on")
        self.assertIn("gents-store", outputs["archives"]["include"][0]["packages"].split())

    def test_more_slices_build_once_and_shard_uncached(self):
        outputs = self.planned(GATE_EVENT="push", GATE_EVERYTHING="true", GATE_PORTABLE_SHARDS="3")
        self.assertIn("build:portable", outputs["jobs"])
        self.assertIn("shard:portable", outputs["jobs"])
        self.assertEqual(len(outputs["shards"]["include"]), 3)
        self.assertEqual(outputs["archives"]["include"][0]["test_cache"], "off")

    def test_with_the_variables_unset_each_portable_group_is_its_own_job(self):
        outputs = self.planned(GATE_EVENT="push", GATE_EVERYTHING="true", GATE_PORTABLE_SHARDS="0",
                               GATE_TEST_CACHE="off")
        self.assertNotIn("build:portable", outputs["jobs"])
        self.assertIn("test:foundation", outputs["jobs"])


class NativeNextest(unittest.TestCase):
    def test_macos_only_targets_build_in_invocations_that_select_no_other_target(self):
        packages = [package for group in gate.GROUPS if group.runner == "portable" for package in group.packages
                    if gate.macos_only_tests(package)]
        invocations = gate.macos_invocations(packages)
        self.assertEqual(sorted(package for members, _ in invocations for package in members), sorted(packages))
        for members, flags in invocations:
            for package in members:
                self.assertEqual(gate.target_arguments(package) & set(flags), set(gate.macos_only_tests(package)))
        self.assertLess(len(invocations), len(packages))

    def test_the_filterset_names_each_binary_and_filter(self):
        filterset = gate.macos_filterset(["gents", "gents-cli"])
        self.assertIn("(binary_id(=gents-cli::plugin_build_sandbox))", filterset)
        self.assertIn("test(~a_skipped_gate_releases_the_bound_workspace)", filterset)
        self.assertEqual(gate.binary_id("gents-client-host", ("--bin", "gents-client-host")),
                         "gents-client-host::bin/gents-client-host")
        self.assertEqual(gate.binary_id("gents", ("--lib",)), "gents")


    def test_a_crate_or_macos_target_that_ran_no_test_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def run(arguments, **_):
                report = root / "target/nextest/ci/junit.xml"
                report.parent.mkdir(parents=True, exist_ok=True)
                report.write_text('<testsuites><testsuite name="gents-service">'
                                  '<testcase classname="gents-service::native_service" name="a"/>'
                                  '</testsuite></testsuites>')
                return True

            with mock.patch.object(gate, "ROOT", root), mock.patch.object(gate, "nextest", run), \
                    mock.patch.object(gate, "results", []), contextlib.redirect_stdout(io.StringIO()):
                gate.native_tests("native", ["gents-service", "gents-fixture-host"], [], None,
                                  {"gents-service", "gents-fixture-host"})
                self.assertEqual(gate.results[-1][1], "gents-fixture-host selected zero tests")


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
            self.assertEqual(entry["runs_on"][:len(gate.runner(entry["role"]))], gate.runner(entry["role"]),
                             entry["job"])
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


class HostGroups(unittest.TestCase):
    GROUPS = ROLES["portable"]["host_groups"]

    def test_portable_jobs_split_across_the_host_groups(self):
        selection = select(everything=True)
        portable = [entry for entry in selection["matrix"] if entry["role"] == "portable"]
        placed = {entry["runs_on"][-1] for entry in portable}
        self.assertEqual(placed, set(self.GROUPS))
        load = {group: sum(entry["timeout"] for entry in portable if entry["runs_on"][-1] == group)
                for group in self.GROUPS}
        self.assertLessEqual(max(load.values()) - min(load.values()), max(entry["timeout"] for entry in portable))
        for entry in portable:
            self.assertEqual(entry["runs_on"], [*ROLES["portable"]["labels"], entry["runs_on"][-1]])

    def test_slices_alternate_between_the_host_groups(self):
        selection = sharded(4, everything=True)
        self.assertEqual([entry["runs_on"][-1] for entry in selection["shards"]],
                         [self.GROUPS[index % len(self.GROUPS)] for index in range(4)])

    def test_a_cached_build_may_plan_on_either_host_group(self):
        # Each host keeps its own test-result store (TestCache.lean
        # `skip_sound_on_any_host`), so the cached build names no group and
        # a runner of every group matches it.
        saved = os.environ.get("GATE_EVENT")
        os.environ["GATE_TEST_CACHE"] = "on"
        try:
            for event, mode in (("pull_request", "on"), ("push", "full")):
                os.environ["GATE_EVENT"] = event
                [archive] = sharded(1, everything=True)["archives"]
                with self.subTest(event=event):
                    self.assertEqual(archive["test_cache"], mode)
                    self.assertEqual(archive["runs_on"], ROLES["portable"]["labels"])
                    for group in self.GROUPS:
                        self.assertLessEqual(set(archive["runs_on"]), {*ROLES["portable"]["labels"], group})
        finally:
            del os.environ["GATE_TEST_CACHE"]
            os.environ.pop("GATE_EVENT", None)
            if saved is not None:
                os.environ["GATE_EVENT"] = saved

    def test_a_runner_takes_its_roles_compile_and_test_parallelism(self):
        jobs = ROLES["portable"]["jobs"]
        here = gate.placement("portable", fork=False, fallback=False, host_group=self.GROUPS[1])
        self.assertEqual((here["build_jobs"], here["test_threads"]), (str(jobs["build"]), str(jobs["test"])))
        self.assertEqual(gate.placement("macos", fork=False, fallback=False)["build_jobs"], "")
        with self.assertRaises(SystemExit):
            gate.placement("macos", fork=False, fallback=False, host_group=self.GROUPS[0])

    def test_forks_and_fallbacks_never_take_a_host_group(self):
        fork = gate.placement("portable", fork=True, fallback=False, host_group=self.GROUPS[0])
        self.assertEqual(fork["runs_on"], ROLES["portable"].get("hosted", gate.HOSTED_LINUX))
        self.assertEqual(fork["build_jobs"], "")
        self.assertEqual(gate.host_groups("portable", fork=True, fallback=False), [])
        fallback = gate.placement("portable", fork=False, fallback=True, host_group=self.GROUPS[0])
        self.assertNotIn(self.GROUPS[0], fallback["runs_on"])

    def test_host_groups_are_labels_only_their_role_uses(self):
        for role, entry in ROLES.items():
            for group in entry.get("host_groups", []):
                self.assertTrue(group.startswith(entry["labels"][-1] + "-"), (role, group))
                self.assertFalse(any(group in other["labels"] for other in ROLES.values()), group)


class MacosOnlyTests(unittest.TestCase):
    def test_cfg_predicates_evaluate_per_platform(self):
        cases = {
            'target_os = "macos"': (True, False),
            'any(target_os = "linux", target_os = "macos")': (True, True),
            'all(unix, not(target_os = "macos"))': (False, True),
            'all(target_os = "macos", feature = "editor-e2e")': (False, False),
            'target_vendor = "apple"': (True, False),
            "test": (True, True),
            "windows": (False, False),
        }
        for expression, expected in cases.items():
            with self.subTest(expression=expression):
                self.assertEqual((gate.cfg_holds(expression, "macos", set()), gate.cfg_holds(expression, "linux", set())),
                                 expected)
        self.assertTrue(gate.cfg_holds('feature = "x"', "linux", {"x"}))

    def test_the_walk_finds_gated_tests_modules_and_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src/nested").mkdir(parents=True)
            (root / "src/lib.rs").write_text(
                '#[cfg(target_os = "macos")]\nmod sandbox;\nmod nested;\n'
                '#[cfg(all(target_os = "macos", feature = "off"))]\nmod featured;\n'
                '#[cfg(test)]\nmod tests {\n    #[test]\n    #[cfg(target_os = "macos")]\n'
                '    fn only_here() {}\n    #[cfg(any(target_os = "linux", target_os = "macos"))]\n'
                '    #[test]\n    fn both() {}\n}\n')
            (root / "src/nested/mod.rs").write_text('#[cfg(\n    target_os = "macos"\n)]\n#[tokio::test]\n'
                                                     'async fn multi_line() {}\nmod deeper;\n')
            (root / "src/nested/deeper.rs").write_text('#![cfg(target_os = "macos")]\n#[test]\nfn inside() {}\n')
            filters = set()
            gate.walk_module(root / "src/lib.rs", True, set(), filters, set())
            self.assertEqual(filters, {"sandbox::", "only_here", "multi_line", "deeper::"})
            (root / "tests").mkdir()
            (root / "tests/sandbox.rs").write_text('#![cfg(target_os = "macos")]\n#[test]\nfn a() {}\n')
            whole = set()
            gate.walk_module(root / "tests/sandbox.rs", True, set(), whole, set())
            self.assertEqual(whole, {""})

    def test_the_repository_gated_tests_are_found(self):
        # A whole-file gate runs the whole target.
        self.assertEqual(gate.macos_only_tests("gents-cli")[("--test", "plugin_build_sandbox")], [])
        self.assertIn("a_skipped_gate_releases_the_bound_workspace", gate.macos_only_tests("gents")[("--lib",)])
        # Compiled on both platforms, or on neither with the gate's features: not macOS-only.
        self.assertEqual(gate.macos_only_tests("gents-host"), {})
        self.assertNotIn("editor_store_installed::",
                         gate.macos_only_tests("gents-client-host")[("--test", "integration")])
        native = next(entry for entry in select(everything=True)["matrix"] if entry["job"] == f"test:{gate.MACOS_ONLY}")
        portable = {package for group in gate.GROUPS if group.runner == "portable" for package in group.packages}
        self.assertEqual(set(native["macos_tests"]), {package for package in portable if gate.macos_only_tests(package)})


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

    def test_a_cache_extract_directory_starts_empty(self):
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.dict(os.environ, {"GATE_ARCHIVE_DIR": str(Path(directory) / "archive"),
                                             "GITHUB_RUN_ID": "7"}):
            first = gate.extract_dir("plan")
            (first / "stale").write_text("left by an earlier slice")
            again = gate.extract_dir("plan")
            self.assertEqual(again, Path(directory) / "extract-7-plan")
            self.assertEqual(list(again.iterdir()), [])

    def test_verdict_runs_from_a_checkout_of_scripts_ci_alone(self):
        # The gate job's sparse checkout; it also holds the TLA+ runner, which
        # verdict must not need.
        with tempfile.TemporaryDirectory() as directory:
            sparse = Path(directory) / "scripts/ci"
            sparse.parent.mkdir()
            shutil.copytree(gate.ROOT / "scripts/ci", sparse, ignore=shutil.ignore_patterns("__pycache__"))
            needs = self.needs(select("crates/gents-cli/src/lib.rs"))
            result = subprocess.run([sys.executable, str(sparse / "gate.py"), "verdict"], capture_output=True,
                                    text=True, env={**os.environ, "NEEDS": json.dumps(needs), "GATE_FORK": "false"},
                                    timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

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


def tlc_message(code, kind, body):
    return f"@!@!@STARTMSG {code}:{kind} @!@!@\n{body}\n@!@!@ENDMSG {code} @!@!@\n"


class TlaSelection(unittest.TestCase):
    """The planner picks TLA+ specs by the crates and Lean owners their headers name."""

    def test_a_crate_change_selects_the_specs_that_name_it(self):
        tla = select("crates/gents-desktop-core/src/lib.rs")["contract"]["tla"]
        self.assertIn("RuntimeConnectionTransport", tla)
        self.assertNotIn("ClientSubmissionContention", tla)
        for name in tla:
            self.assertIn("gents-desktop-core", gate.tla_check.spec_header(gate.tla_check.specs()[name])[1])

    def test_a_lean_change_selects_the_specs_that_name_its_owner(self):
        selection = select("crates/gents/proofs/Proofs/Store/TransactionReplay.lean")
        self.assertIn("ClientSubmissionContention", selection["contract"]["tla"])
        self.assertNotIn("PackUxWatch", selection["contract"]["tla"])

    def test_a_generated_decision_selects_the_specs_that_import_it(self):
        importers = {name for name, node in gate.tla_check.graph().items()
                     if "LeanDecision_TransactionReplay" in node["generated"]}
        self.assertTrue(importers)
        tla = select("crates/gents/proofs/tla/generated/LeanDecision_TransactionReplay.tla")["contract"]["tla"]
        self.assertEqual(set(tla), importers)

    def test_a_config_selects_its_spec_and_the_runner_selects_every_spec(self):
        self.assertEqual(select("crates/gents/proofs/tla/MCPackUxWatchTransient.cfg")["contract"]["tla"],
                         ["PackUxWatch"])
        everything = sorted(gate.tla_check.specs())
        self.assertEqual(select("crates/gents/proofs/tla/scripts/tla_check.py")["contract"]["tla"], everything)
        self.assertEqual(select("Cargo.lock")["contract"]["tla"], everything)

    def test_selection_follows_the_header_not_the_file_name(self):
        nodes = {"A": {"path": Path("A.tla"), "lean": ["X.decide"], "crates": ["gents-p2p"], "configs": [],
                       "generated": []},
                 "B": {"path": Path("B.tla"), "lean": [], "crates": ["gents-store"], "configs": [],
                       "generated": ["LeanDecision_X"]}}
        owner = lambda path: gate.crate_of(path, MEMBERS)  # noqa: E731
        check = gate.tla_check
        self.assertEqual(check.select(["crates/gents-p2p/src/lib.rs"], nodes, owner), ["A"])
        self.assertEqual(check.select(["crates/gents-store/src/lib.rs"], nodes, owner), ["B"])
        self.assertEqual(check.select(["crates/gents/proofs/tla/generated/LeanDecision_X.tla"], nodes, owner),
                         ["B"])
        self.assertEqual(check.select(["crates/gents-cli/src/lib.rs", "docs/a.md"], nodes, owner), [])

    def test_a_conformance_export_selects_the_spec_that_names_its_owner(self):
        """Lean exports live in `namespace Conformance.<Owner>` or name the owner in full."""
        check = gate.tla_check
        cases = "crates/gents/proofs/Proofs/Conformance/ClientDataPublicationCases.lean"
        self.assertIn("ObserverResync", select(cases)["contract"]["tla"])
        resources = "crates/gents/proofs/Proofs/Conformance/PluginResources.lean"
        nodes = {"A": {"path": Path("A.tla"), "lean": ["PluginResources.computeAllowances"], "crates": [],
                       "configs": [], "generated": []}}
        self.assertEqual(check.select([resources], nodes, lambda path: None), ["A"])
        self.assertFalse(check.lean_declares("namespace Conformance.Other\ndef computeAllowances := 1\n",
                                             "PluginResources.computeAllowances"))
        self.assertTrue(check.lean_declares("attribute [conformance_function] A.B.decide\n", "A.B.decide"))

    def test_code_outside_a_crate_selects_the_spec_whose_paths_name_it(self):
        observation = "apps/gents-desktop/src/hooks/clientObservation.ts"
        self.assertIn("DesktopUpdateListener", select(observation)["contract"]["tla"])
        self.assertNotIn("DesktopUpdateListener", select("apps/gents-desktop/src/hooks/chatStore.ts")["contract"]["tla"])
        check = gate.tla_check
        nodes = {"A": {"path": Path("A.tla"), "lean": [], "crates": [], "paths": ["packages/ui/src"],
                       "configs": [], "generated": []}}
        self.assertEqual(check.select(["packages/ui/src/view.ts"], nodes, lambda path: None), ["A"])
        self.assertEqual(check.select(["packages/ui/srcx/view.ts"], nodes, lambda path: None), [])
        with tempfile.TemporaryDirectory() as scratch:
            spec = Path(scratch) / "A.tla"
            spec.write_text("---- MODULE A ----\n\\* lean: none\n\\* crates: none\n"
                            "\\* paths: apps/x.ts, packages/y\n====\n")
            self.assertEqual(check.spec_header(spec).paths, ["apps/x.ts", "packages/y"])
            spec.write_text("---- MODULE A ----\n\\* lean: none\n\\* crates: none\n"
                            "\\* paths: apps/x.ts\n\\* models: A.b\n====\n")
            with self.assertRaises(check.HeaderError):
                check.spec_header(spec)

    def test_lean_a_spec_restates_selects_it(self):
        """Specs that copied Lean by hand name it, as a generated table or on `models:`."""
        proofs = "crates/gents/proofs/Proofs/"
        for source, spec in [("Session/AgentSession.lean", "SessionHeadObservation"),
                             ("Clients/ClientShell/PackWatch.lean", "PackUxWatch"),
                             ("Store/NativeCompletion.lean", "NativeCompletionHandshake"),
                             ("Tools/ToolPolicy/PluginCalls.lean", "GuestAllowance")]:
            self.assertIn(spec, select(proofs + source)["contract"]["tla"], source)
        node = gate.tla_check.graph()["SessionHeadObservation"]
        self.assertEqual(node["lean"], ["AgentSession.newerByKeys"])
        self.assertEqual(node["generated"], ["LeanDecision_AgentSession"])

    def test_prose_selects_no_spec(self):
        self.assertEqual(select("docs/guide.md")["contract"]["tla"], [])


class TlaStructure(unittest.TestCase):
    """Every spec names its Lean owners and crates, every config its TLC result."""

    def test_headers_owners_crates_and_generated_imports(self):
        schema = json.loads((gate.ROOT / "crates/gents-lean-vocab/families.schema.json").read_text())
        problems = gate.tla_check.structure_problems(set(schema["functions"]), set(MEMBERS))
        self.assertEqual(problems, [])

    def test_a_missing_or_unnamed_generated_module_is_a_problem_and_still_selects(self):
        """Imports come from the EXTENDS text, so a deleted module neither hides
        its importers from selection nor passes the structure check."""
        check = gate.tla_check
        with tempfile.TemporaryDirectory() as scratch:
            tla = Path(scratch)
            (tla / "generated").mkdir()
            (tla / "generated/LeanDecision_Here.tla").write_text(
                "---- MODULE LeanDecision_Here ----\nEXTENDS TLC\nkeep(a) == a\n====\n")
            (tla / "A.tla").write_text("---- MODULE A ----\n\\* lean: Gone.decide\n\\* crates: none\n"
                                       "EXTENDS Naturals, LeanDecision_Gone\n====\n")
            (tla / "B.tla").write_text("---- MODULE B ----\n\\* lean: none\n\\* crates: none\n"
                                       "\\* models: Nowhere.decide\nEXTENDS LeanDecision_Here\n====\n")
            (tla / "C.tla").write_text("---- MODULE C ----\n\\* lean: Here.keep\n\\* crates: none\n"
                                       "EXTENDS Naturals\n====\n")
            for spec in "ABC":
                (tla / f"{spec}.cfg").write_text("\\* expect: holds\nSPECIFICATION Spec\n")
            nodes = check.graph(tla)
            self.assertEqual(nodes["A"]["generated"], ["LeanDecision_Gone"])
            self.assertEqual(nodes["B"]["models"], ["Nowhere.decide"])
            self.assertEqual(check.select(["crates/gents/proofs/tla/generated/LeanDecision_Gone.tla"], nodes,
                                          lambda path: None), ["A"])
            problems = check.structure_problems({"gone_decide", "here_keep"}, set(), tla, sources={})
        self.assertEqual(problems, [
            "A.tla: extends LeanDecision_Gone, which is not generated",
            "B.tla: models Nowhere.decide, which no Lean source declares",
            "B.tla: extends LeanDecision_Here but names none of its owners on its lean line",
            "C.tla: names Here.keep but does not extend LeanDecision_Here",
        ])

    def test_check_headers_reports_every_structure_problem(self):
        """Not only the expect lines: owners, crates and imports too."""
        check = gate.tla_check
        with tempfile.TemporaryDirectory() as scratch:
            tla = Path(scratch)
            (tla / "A.tla").write_text("---- MODULE A ----\n\\* lean: Ns.decide\n\\* crates: no-such-crate\n"
                                       "EXTENDS Naturals\n====\n")
            (tla / "A.cfg").write_text("\\* expect: holds\nSPECIFICATION Spec\n")
            errors = io.StringIO()
            with contextlib.redirect_stderr(errors):
                status = check.check_headers(tla, functions=set(), members={"gents"})
        self.assertEqual(status, 1)
        self.assertIn("no-such-crate is not a workspace package", errors.getvalue())
        self.assertIn("Ns.decide is not in the conformance snapshot", errors.getvalue())
        self.assertIn("does not extend LeanDecision_Ns", errors.getvalue())
        self.assertEqual(check.check_headers(functions=set(json.loads(
            (gate.ROOT / "crates/gents-lean-vocab/families.schema.json").read_text())["functions"]),
            members=set(MEMBERS)), 0)

    def test_lean_owner_keys_follow_lean_wire_names(self):
        check = gate.tla_check
        self.assertEqual(check.wire_key("TransactionReplay.disposition"), "transaction_replay_disposition")
        self.assertEqual(check.wire_key("Conformance.PluginResources.computeAllowances"),
                         "plugin_resources_compute_allowances")
        self.assertEqual(check.wire_name("MCPHealth"), "mcp_health")

    def test_tlc_results_parse(self):
        parse = gate.tla_check.parse
        stats = tlc_message(2199, 0, "1,234 states generated, 1,045 distinct states found, 0 states left on queue.")
        self.assertEqual(parse(tlc_message(2193, 0, "Model checking completed. No error has been found.") + stats, 0),
                         ("holds", 1045))
        self.assertEqual(parse(tlc_message(2110, 1, "Invariant NoLostWrite is violated.") + stats, 12),
                         ("violates NoLostWrite", 1045))
        self.assertEqual(parse(tlc_message(2116, 1, "Temporal property EventualResume was violated."), 13).verdict,
                         "violates EventualResume")
        self.assertEqual(parse(tlc_message(2116, 1, "Temporal properties were violated."), 13).verdict, "violates ?")
        self.assertEqual(parse(tlc_message(2114, 1, "Deadlock reached."), 11).verdict, "deadlock")
        self.assertTrue(parse(tlc_message(1000, 1, "Unknown operator Foo"), 255).verdict
                        .startswith("error: Unknown operator"))
        self.assertTrue(parse("", 1).verdict.startswith("error:"))

    def test_a_model_without_states_or_violated_initially_does_not_match(self):
        """TLC reports `Init == FALSE` as holding over zero states and an initial
        state's violation like any other; neither is the recorded result."""
        check = gate.tla_check
        empty = (tlc_message(2190, 0, "Finished computing initial states: 0 distinct states generated at t.")
                 + tlc_message(2193, 0, "Model checking completed. No error has been found.")
                 + tlc_message(2199, 0, "0 states generated, 0 distinct states found, 0 states left on queue."))
        self.assertEqual(check.parse(empty, 0), ("holds", 0))
        self.assertEqual(check.judge(check.Expect("holds", 2), check.parse(empty, 0)), "FEW STATES")
        self.assertEqual(check.judge(check.Expect("holds", 2), check.parse(
            tlc_message(2193, 0, "Model checking completed. No error has been found."), 0)), "FEW STATES")
        initial = tlc_message(2107, 1, "Invariant Inv is violated by the initial state:\nx = 0")
        self.assertEqual(check.parse(initial, 12).verdict, "violates Inv initially")
        self.assertEqual(check.judge(check.Expect("violates Inv", 2), check.parse(initial, 12)), "MISMATCH")
        self.assertEqual(check.judge(check.Expect("violates Inv initially", 2), check.parse(initial, 12)), "ok")
        early = tlc_message(2116, 1, "Temporal properties were violated.") + tlc_message(
            2199, 0, "3 states generated, 3 distinct states found, 0 states left on queue.")
        self.assertEqual(check.judge(check.Expect("violates ?", 2), check.parse(early, 13)), "ok")
        self.assertEqual(check.judge(check.Expect("violates ?", 40), check.parse(early, 13)), "FEW STATES")
        with tempfile.TemporaryDirectory() as scratch:
            cfg = Path(scratch) / "MCX.cfg"
            cfg.write_text("\\* expect: violates Live\n\\* states: 40\nSPECIFICATION Spec\nPROPERTY Live\n")
            self.assertEqual(check.expectation(cfg), ("violates Live", 40))
            cfg.write_text("\\* expect: violates Inv initially\nSPECIFICATION Spec\nINVARIANT Inv\n")
            self.assertEqual(check.expectation(cfg), ("violates Inv initially", 2))
            cfg.write_text("\\* expect: holds\n\\* states: many\nSPECIFICATION Spec\n")
            with self.assertRaises(check.HeaderError):
                check.expectation(cfg)

    def test_tlc_runs_with_a_large_thread_stack(self):
        """A default Java thread stack overflows on a large generated table."""
        check = gate.tla_check
        command = check.tlc_command("java", "tla2tools.jar", "4g", "/tmp/x", "MCX.cfg", "MCX")
        self.assertIn("-Xss16m", command)
        self.assertLess(command.index("-Xss16m"), command.index("tlc2.TLC"))
        script = (check.TLA / "scripts/run-tlc.sh").read_text()
        self.assertIn("s/^jvm: //p", script)
        self.assertIn('"${JVM[@]}"', script)

    def test_a_cached_jdk_is_reinstalled_unless_its_stamp_names_the_pinned_archive(self):
        check = gate.tla_check
        key = (platform.system(), platform.machine())
        if key not in check.JDKS:
            self.skipTest(f"no pinned JDK for {key}")
        archive, digest = check.JDKS[key]
        inside = "Contents/Home/bin/java" if key[0] == "Darwin" else "bin/java"
        fetched = []

        def fetch(url, expected, destination):
            fetched.append(url.rsplit("/", 1)[1])
            destination.parent.mkdir(parents=True, exist_ok=True)
            if url == check.TLA_TOOLS["url"]:
                destination.write_bytes(b"jar")
                return
            with tarfile.open(destination, "w:gz") as bundle:
                data = b"#!/bin/sh\n"
                entry = tarfile.TarInfo(f"{check.JDK_RELEASE}/{inside}")
                entry.size = len(data)
                bundle.addfile(entry, io.BytesIO(data))

        with tempfile.TemporaryDirectory() as scratch, \
                mock.patch.dict(os.environ, {"XDG_CACHE_HOME": scratch}), \
                mock.patch.object(check, "fetch", fetch), \
                mock.patch.dict(check.TLA_TOOLS, {"sha256": hashlib.sha256(b"jar").hexdigest()}):
            home = Path(scratch) / "gents-tla" / check.JDK_RELEASE
            (home / inside).parent.mkdir(parents=True)
            (home / inside).write_text("left over by an interrupted install")
            java, _ = check.install()
            self.assertEqual(fetched, ["tla2tools.jar", archive])
            self.assertEqual(java, home / inside)
            self.assertEqual(java.read_text(), "#!/bin/sh\n")
            self.assertEqual((home / check.JDK_STAMP).read_text().strip(), digest)
            check.install()
            self.assertEqual(fetched, ["tla2tools.jar", archive])
            (home / check.JDK_STAMP).write_text("another archive\n")
            check.install()
            self.assertEqual(fetched, ["tla2tools.jar", archive, archive])

    def test_expectations_and_isolated_properties(self):
        check = gate.tla_check
        with tempfile.TemporaryDirectory() as scratch:
            cfg = Path(scratch) / "MCX.cfg"
            cfg.write_text("\\* expect: violates Live\nSPECIFICATION Spec\nINVARIANT TypeOK\nPROPERTY Live\n")
            self.assertEqual(check.expectation(cfg), ("violates Live", 2))
            cfg.write_text("\\* expect: violates Missing\nSPECIFICATION Spec\nINVARIANT TypeOK\n")
            with self.assertRaises(check.HeaderError):
                check.expectation(cfg)
            cfg.write_text("\\* expect: deadlock\nSPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n")
            with self.assertRaises(check.HeaderError):
                check.expectation(cfg)
            cfg.write_text("SPECIFICATION Spec\n")
            with self.assertRaises(check.HeaderError):
                check.expectation(cfg)
        text = "\\* note\nCONSTANTS\n  N = 2\nSPECIFICATION Spec\nINVARIANT TypeOK\nPROPERTY A\nPROPERTY B\n"
        self.assertEqual(check.cfg_sections(text)["PROPERTY"], ["A", "B"])
        alone = check.without_checks(text, "B")
        self.assertEqual(check.cfg_sections(alone), {"CONSTANTS": ["N", "=", "2"], "SPECIFICATION": ["Spec"],
                                                     "PROPERTY": ["B"]})


if __name__ == "__main__":
    unittest.main()
