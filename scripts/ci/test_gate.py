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
        sources = [jobs[name] for name in set(gate.WORKFLOW_JOBS.values()) | {"test"}]
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


class Verdict(unittest.TestCase):
    """The `gate` job: `needs` as GitHub passes it, for the plans each event makes."""

    def needs(self, selection, **results):
        names = set(gate.ALWAYS) | set(gate.WORKFLOW_JOBS.values()) | {"test"}
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
        self.assertEqual(needs, set(gate.ALWAYS) | set(gate.WORKFLOW_JOBS.values()) | {"test"})
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
        }
        for event, selection in plans.items():
            self.assertEqual(gate.verdict(self.needs(selection)), [], event)

    def test_a_selected_job_that_was_skipped_fails(self):
        selection = select("crates/gents-cli/src/lib.rs")
        for job in ("lint", "test", "fmt", "contract"):
            problems = gate.verdict(self.needs(selection, **{job: "skipped"}))
            self.assertEqual(len(problems), 1, job)
            self.assertIn(f"{job}: the plan selected", problems[0])

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

    def test_prose_selects_no_spec(self):
        self.assertEqual(select("docs/guide.md")["contract"]["tla"], [])


class TlaStructure(unittest.TestCase):
    """Every spec names its Lean owners and crates, every config its TLC result."""

    def test_headers_owners_crates_and_generated_imports(self):
        schema = json.loads((gate.ROOT / "crates/gents-lean-vocab/families.schema.json").read_text())
        problems = gate.tla_check.structure_problems(set(schema["functions"]), set(MEMBERS))
        self.assertEqual(problems, [])

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
