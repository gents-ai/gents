#!/usr/bin/env python3
"""Selection and guard checks for gate.py against the real workspace graph; no builds."""

import json
import os
import re
import stat
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

    def test_a_portable_runner_must_be_a_label_list(self):
        for value, expected in (("", gate.STUDIO), ('["self-hosted","Linux","ci-portable"]',
                                                    ["self-hosted", "Linux", "ci-portable"])):
            os.environ["GATE_PORTABLE_RUNNER"] = value
            try:
                self.assertEqual(gate.runner("portable"), expected)
            finally:
                del os.environ["GATE_PORTABLE_RUNNER"]
        for value in ("ci-portable", '"ci-portable"', "[]", '[""]', "[1]"):
            os.environ["GATE_PORTABLE_RUNNER"] = value
            try:
                with self.assertRaises(SystemExit, msg=value):
                    gate.runner("portable")
            finally:
                del os.environ["GATE_PORTABLE_RUNNER"]

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

    def test_slices_receive_the_archive_by_artifact_unless_kept_on_the_host(self):
        self.assertEqual({entry["transfer"] for entry in sharded(4, everything=True)["shards"]}, {"artifact"})
        os.environ["GATE_ARCHIVE_TRANSFER"] = "host"
        try:
            selection = sharded(4, everything=True)
            self.assertEqual({entry["transfer"] for entry in selection["archives"] + selection["shards"]}, {"host"})
            os.environ["GATE_ARCHIVE_TRANSFER"] = "nfs"
            with self.assertRaises(SystemExit):
                sharded(4, everything=True)
        finally:
            del os.environ["GATE_ARCHIVE_TRANSFER"]

    def test_a_kept_archive_is_found_by_run_not_attempt(self):
        saved = {name: os.environ.get(name) for name in ("GITHUB_ACTIONS", "RUNNER_TEMP", "GITHUB_RUN_ID")}
        os.environ.update(GITHUB_ACTIONS="true", RUNNER_TEMP="/runner/_temp", GITHUB_RUN_ID="42")
        try:
            self.assertEqual(gate.archive_dir("host"), gate.HOST_ARCHIVES / "42")
            self.assertEqual(gate.archive_dir("artifact"), Path("/runner/_temp/gents-tests"))
        finally:
            for name, value in saved.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value

    def test_a_shard_job_names_its_slice(self):
        selection = sharded(3, everything=True)
        with self.assertRaises(SystemExit):
            gate.run_shard(selection, "portable", 4)

    def test_a_binary_holding_the_build_checkout_path_is_unrelocatable(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "src"
            portable = Path(directory) / "portable"
            portable.write_bytes(b"\0" + str(source / "target/debug/build/x/out").encode() + b"\0")
            pinned = Path(directory) / "pinned"
            pinned.write_bytes(b"\0" + str(source / "crates/gents-pack").encode() + b"\0")
            self.assertEqual(gate.unrelocatable([portable, pinned], source), ["pinned"])


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
