#!/usr/bin/env python3
import importlib.util
import os
from pathlib import Path
from collections import namedtuple
import json
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "build_space", Path(__file__).with_name("ensure-rust-build-space.py"))
build_space = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_space)
Usage = namedtuple("Usage", "total used free")
run_command = subprocess.run
GIB = 1024**3
WORKFLOW = Path(__file__).resolve().parents[1] / "workflows" / "ci.yml"


class BuildSpaceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.target = self.root / "current-runner"
        self.target.mkdir()
        self.other = self.root / "other-runner"
        self.other.mkdir()
        self.marker = self.other / "active-build"
        self.marker.write_text("keep")
        patch.dict(os.environ, {"CARGO_TARGET_ROOT": str(self.root),
                                "RUNNER_NAME": "current-runner"}).start()
        self.clean = patch.object(build_space.subprocess, "run").start()
        self.addCleanup(patch.stopall)

    def run_with_free_space(self, *gib):
        with patch.object(build_space.shutil, "disk_usage",
                          side_effect=[Usage(100, 0, free * 1024**3) for free in gib]):
            build_space.ensure_build_space(self.target, 64)
        self.assertEqual(self.marker.read_text(), "keep")

    def test_warm_cache_survives_when_space_is_available(self):
        self.run_with_free_space(64)
        self.clean.assert_not_called()

    def test_reclaims_workspace_artifacts_before_dependencies(self):
        self.run_with_free_space(10, 70)
        self.clean.assert_called_once_with(
            ["cargo", "clean", "--workspace", "--target-dir", str(self.target)], check=True)

    def test_reclaims_only_current_runner_when_workspace_cleanup_is_insufficient(self):
        self.run_with_free_space(10, 30, 70)
        self.assertEqual(self.clean.call_args_list, [
            unittest.mock.call(["cargo", "clean", "--workspace", "--target-dir",
                                str(self.target)], check=True),
            unittest.mock.call(["cargo", "clean", "--target-dir", str(self.target)], check=True),
        ])

    def run_with_target_size(self, *gib):
        with patch.object(build_space.shutil, "disk_usage",
                          return_value=Usage(100, 0, 1000 * GIB)), \
                patch.object(build_space, "tree_size",
                             side_effect=[size * GIB for size in gib]):
            build_space.ensure_build_space(self.target, 64, 150)
        self.assertEqual(self.marker.read_text(), "keep")

    def test_kept_tree_under_its_cap_survives(self):
        self.run_with_target_size(150)
        self.clean.assert_not_called()

    def test_kept_tree_over_its_cap_reclaims_workspace_artifacts_first(self):
        self.run_with_target_size(151, 90)
        self.clean.assert_called_once_with(
            ["cargo", "clean", "--workspace", "--target-dir", str(self.target)], check=True)

    def test_kept_tree_still_over_its_cap_is_emptied(self):
        self.run_with_target_size(400, 200, 0)
        self.assertEqual(self.clean.call_args_list, [
            unittest.mock.call(["cargo", "clean", "--workspace", "--target-dir",
                                str(self.target)], check=True),
            unittest.mock.call(["cargo", "clean", "--target-dir", str(self.target)], check=True),
        ])

    def keep_with(self, free, *used):
        with patch.object(build_space.shutil, "disk_usage",
                          return_value=Usage(100, 0, free * GIB)), \
                patch.object(build_space, "tree_size",
                             side_effect=[size * GIB for size in used]):
            kept = build_space.keep_target(self.target, 70, 150)
        self.assertEqual(self.marker.read_text(), "keep")
        return kept

    def test_kept_tree_is_used_while_the_host_keeps_its_floor_at_the_cap(self):
        # 300 GiB free, 50 used of a 70 GiB cap: 280 GiB remain at the cap.
        self.assertTrue(self.keep_with(300, 50))
        self.clean.assert_not_called()

    def test_floor_counts_only_the_room_left_below_the_cap(self):
        self.assertTrue(self.keep_with(150, 70))
        self.assertTrue(self.keep_with(150, 200))
        self.clean.assert_not_called()

    def test_short_host_reclaims_the_kept_tree_before_giving_it_up(self):
        # 200 GiB free with an empty tree leaves 130 at the cap, under the floor.
        self.assertFalse(self.keep_with(200, 0, 0, 0))
        self.assertEqual(self.clean.call_args_list, [
            unittest.mock.call(["cargo", "clean", "--workspace", "--target-dir",
                                str(self.target)], check=True),
            unittest.mock.call(["cargo", "clean", "--target-dir", str(self.target)], check=True),
        ])

    def test_reclaiming_an_over_cap_tree_can_restore_the_floor(self):
        # 100 GiB used of a 70 GiB cap with 140 free misses the floor; emptying
        # the workspace artifacts leaves 40 used and 200 free, 170 at the cap.
        with patch.object(build_space.shutil, "disk_usage",
                          side_effect=[Usage(100, 0, 140 * GIB), Usage(100, 0, 200 * GIB)]), \
                patch.object(build_space, "tree_size", side_effect=[100 * GIB, 40 * GIB]):
            self.assertTrue(build_space.keep_target(self.target, 70, 150))
        self.clean.assert_called_once_with(
            ["cargo", "clean", "--workspace", "--target-dir", str(self.target)], check=True)

    def test_keep_rejects_other_runners_trees(self):
        with self.assertRaisesRegex(ValueError, "isolated cache"):
            build_space.keep_target(self.other, 70, 150)
        self.clean.assert_not_called()

    def test_tree_size_counts_hard_links_once_and_does_not_follow_symlinks(self):
        artifact = self.target / "debug" / "deps" / "artifact"
        artifact.parent.mkdir(parents=True)
        artifact.write_bytes(b"x" * 65536)
        os.link(artifact, self.target / "debug" / "artifact")
        (self.target / "debug" / "outside").symlink_to(self.other, target_is_directory=True)
        allocated = os.lstat(artifact).st_blocks * 512
        self.assertGreaterEqual(allocated, 65536)
        self.assertLess(build_space.tree_size(self.target), 2 * allocated)
        self.assertGreaterEqual(build_space.tree_size(self.target), allocated)

    INVENTORY = {"roles": {"capped": {"labels": [], "target": {"max_gib": 70, "floor_gib": 50}},
                           "floor-only": {"labels": [], "target": {"floor_gib": 120}},
                           "uncapped": {"labels": []}}}

    def limits(self, role, inventory=None, runner="runner-a"):
        return build_space.target_limits({"RUNNER_NAME": runner, "GENTS_CI_RUNNER_ROLE": role},
                                         self.INVENTORY if inventory is None else inventory)

    def test_unset_limits_use_safe_defaults(self):
        self.assertEqual(self.limits(""), (60, 150))
        self.assertEqual(self.limits("uncapped"), (60, 150))

    def test_limits_are_chosen_per_role(self):
        self.assertEqual(self.limits("capped"), (70, 50))
        self.assertEqual(self.limits("floor-only"), (60, 120))

    def test_rejects_unknown_roles_and_malformed_limits(self):
        with self.assertRaisesRegex(ValueError, "not a role"):
            self.limits("someone-else")
        for value in (0, -5, 1.5, True, "70", None):
            inventory = {"roles": {"bad": {"labels": [], "target": {"max_gib": value}}}}
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.limits("bad", inventory)
        with self.assertRaisesRegex(ValueError, "RUNNER_NAME"):
            self.limits("capped", runner="")

    def test_the_checked_in_inventory_gives_every_kept_role_valid_limits(self):
        inventory = json.loads(build_space.INVENTORY.read_text())
        for role in inventory["roles"]:
            with self.subTest(role=role):
                self.limits(role, inventory)

    def test_limits_command_reads_the_role_from_the_inventory(self):
        environ = {**os.environ, "RUNNER_NAME": "any-runner", "GENTS_CI_RUNNER_ROLE": "portable"}
        output = run_command([sys.executable, str(Path(build_space.__file__)), "--limits"],
                             env=environ, check=True, capture_output=True, text=True)
        roles = json.loads(build_space.INVENTORY.read_text())["roles"]
        target = roles["portable"]["target"]
        self.assertEqual(output.stdout.split(), [str(target["max_gib"]), str(target["floor_gib"])])

    def test_insufficient_host_space_fails_before_compilation(self):
        with self.assertRaisesRegex(RuntimeError, "free additional space"):
            self.run_with_free_space(10, 30, 40)

    def test_recreates_target_removed_by_cargo_clean(self):
        def remove_target(*args, **kwargs):
            (self.target / "CACHEDIR.TAG").unlink()
            self.target.rmdir()
        self.clean.side_effect = remove_target
        self.run_with_free_space(10, 30, 70)
        self.assertTrue(self.target.is_dir())
        self.assertEqual((self.target / "CACHEDIR.TAG").read_text(),
                         build_space.CACHE_SIGNATURE)

    def test_legacy_cache_receives_a_tag_accepted_by_cargo_clean(self):
        workspace = self.root / "workspace"
        workspace.mkdir()
        manifest = workspace / "Cargo.toml"
        manifest.write_text('[package]\nname = "cache-fixture"\nversion = "0.1.0"\n'
                            '[lib]\npath = "lib.rs"\n')
        (workspace / "lib.rs").write_text("")
        artifact = self.target / "debug" / "deps" / "old-artifact"
        artifact.parent.mkdir(parents=True)
        artifact.write_text("old build output")
        build_space.initialize_cache_tag(self.target)
        run_command(["cargo", "clean", "--offline", "--manifest-path", str(manifest),
                     "--target-dir", str(self.target)], check=True, cwd=workspace,
                    env={**os.environ, "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": ""})
        self.assertFalse(artifact.exists())
        self.assertEqual(self.marker.read_text(), "keep")

    def test_rejects_other_runner_and_source_directories(self):
        for target in (self.other, self.root):
            with self.subTest(target=target), self.assertRaisesRegex(ValueError, "isolated cache"):
                build_space.ensure_build_space(target, 64)
        (self.target / "Cargo.toml").write_text("[workspace]\n")
        with self.assertRaisesRegex(ValueError, "isolated cache"):
            build_space.ensure_build_space(self.target, 64)
        self.clean.assert_not_called()

    def test_rejects_invalid_or_linked_cache_tags(self):
        tag = self.target / "CACHEDIR.TAG"
        tag.write_text("unrelated file")
        with self.assertRaisesRegex(ValueError, "invalid signature"):
            self.run_with_free_space(10)
        tag.unlink()
        tag.symlink_to(self.marker)
        with self.assertRaisesRegex(ValueError, "regular file"):
            self.run_with_free_space(10)
        self.clean.assert_not_called()

    def test_cleanup_failure_is_not_ignored(self):
        self.clean.side_effect = subprocess.CalledProcessError(1, "cargo")
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_with_free_space(10)

    def test_rejects_symlink_and_non_cache_roots_before_cleanup(self):
        alias = self.root / "alias"
        alias.symlink_to(self.other, target_is_directory=True)
        for target in (alias, Path("/"), Path.cwd(), Path.home(), Path("relative"),
                       self.marker, self.root / "missing"):
            with self.subTest(target=target), self.assertRaises(ValueError):
                build_space.ensure_build_space(target, 64)
        self.clean.assert_not_called()
        self.assertEqual(self.marker.read_text(), "keep")

    def test_rejects_non_positive_reserve(self):
        for minimum in (0, -1):
            with self.subTest(minimum=minimum), self.assertRaises(ValueError):
                build_space.ensure_build_space(self.target, minimum)
        self.clean.assert_not_called()

    def test_rejects_non_positive_cap(self):
        for maximum in (0, -1):
            with self.subTest(maximum=maximum), self.assertRaises(ValueError):
                build_space.ensure_build_space(self.target, 64, maximum)
        self.clean.assert_not_called()


def loosely_equal(left, right):
    """GitHub expression equality: strings compare case-insensitively, and
    values of different types compare as numbers (null and '' are 0)."""
    if isinstance(left, str) and isinstance(right, str):
        return left.lower() == right.lower()
    if type(left) is type(right):
        return left == right

    def number(value):
        if value is None:
            return 0.0
        if isinstance(value, bool):
            return float(value)
        if isinstance(value, str):
            try:
                return float(value.strip() or 0)
            except ValueError:
                return float("nan")
        return float(value) if isinstance(value, (int, float)) else float("nan")

    return number(left) == number(right)


def evaluate(expression, context):
    """Evaluate the subset of GitHub expression syntax the trust condition and
    the gate's job conditions use: comparisons, &&, ||, contains, startsWith and fromJSON."""
    tokens = re.findall(r"\(|\)|,|&&|\|\||==|!=|'[^']*'|\.?[A-Za-z_][A-Za-z0-9_.*-]*", expression)
    if "".join(tokens) != re.sub(r"\s+", "", expression):
        raise ValueError(f"unsupported expression syntax: {expression}")
    position = 0

    def take():
        nonlocal position
        position += 1
        return tokens[position - 1]

    def peek():
        return tokens[position] if position < len(tokens) else None

    def operand():
        token = take()
        if token == "(":
            value = disjunction()
            if take() != ")":
                raise ValueError("unbalanced parentheses")
            return value
        if token.startswith("'"):
            return token[1:-1]
        if token in ("true", "false", "null"):
            return {"true": True, "false": False, "null": None}[token]
        if token in FUNCTIONS and peek() == "(":
            take()
            arguments = [] if peek() == ")" else [disjunction()]
            while peek() == ",":
                take()
                arguments.append(disjunction())
            if take() != ")":
                raise ValueError(f"unbalanced call to {token}")
            value = FUNCTIONS[token](*arguments)
            while peek() and peek().startswith("."):
                for part in take()[1:].split("."):
                    value = value.get(part) if isinstance(value, dict) else None
            return value
        value = context
        parts = token.split(".")
        for index, part in enumerate(parts):
            if part == "*":
                if not isinstance(value, list):
                    return []
                rest = parts[index + 1:]
                projected = []
                for item in value:
                    cur = item
                    for sub in rest:
                        cur = cur.get(sub) if isinstance(cur, dict) else None
                    projected.append(cur)
                return projected
            value = value.get(part) if isinstance(value, dict) else None
        return value

    def comparison():
        left = operand()
        while peek() in ("==", "!="):
            operator, right = take(), operand()
            left = loosely_equal(left, right) == (operator == "==")
        return left

    def conjunction():
        left = comparison()
        while peek() == "&&":
            take()
            right = comparison()
            left = right if left else left
        return left

    def disjunction():
        left = conjunction()
        while peek() == "||":
            take()
            right = conjunction()
            left = left if left else right
        return left

    value = disjunction()
    if position != len(tokens):
        raise ValueError(f"trailing tokens in {expression}")
    return value


def contains(haystack, needle):
    if isinstance(haystack, list):
        return any(loosely_equal(item, needle) for item in haystack)
    return str(needle).lower() in str(haystack or "").lower()


def starts_with(text, prefix):
    return str(text or "").lower().startswith(str(prefix or "").lower())


FUNCTIONS = {"contains": contains, "startsWith": starts_with, "fromJSON": json.loads, "always": lambda: True}


def keep_condition(workflow=WORKFLOW):
    text = workflow.read_text()
    match = re.search(r"^  GENTS_CI_KEEP_TARGET_TREE: >-\n((?:    .*\n)+)", text, re.MULTILINE)
    if match is None:
        raise AssertionError(f"{workflow.name} must set GENTS_CI_KEEP_TARGET_TREE at workflow level")
    folded = " ".join(line.strip() for line in match.group(1).splitlines())
    inner = re.fullmatch(r"\$\{\{(.*)\}\}", folded)
    if inner is None:
        raise AssertionError("GENTS_CI_KEEP_TARGET_TREE must be one expression")
    return inner.group(1)


def top_level_block(text, key):
    match = re.search(rf"^{key}:[^\n]*\n((?:(?:[ #].*)?\n)*)", text, re.MULTILINE)
    return match.group(1) if match else ""


def job_field(block, field):
    match = re.search(rf"^    {field}:(.*\n(?:(?:      .*| *)\n)*)", block, re.MULTILINE)
    if match is None:
        return None
    lines = [line.strip() for line in match.group(1).splitlines()]
    value = " ".join(line for line in lines if line and not line.startswith("#"))
    return value[2:].strip() if value.startswith(">-") else value


def pull_request_jobs():
    """(workflow, job, runs-on, if) for every job of every workflow a pull
    request can start."""
    for workflow in sorted(WORKFLOW.parent.glob("*.yml")):
        text = workflow.read_text()
        if not re.search(r"^  pull_request(_target)?:", top_level_block(text, "on"),
                         re.MULTILINE):
            continue
        jobs = top_level_block(text, "jobs")
        starts = list(re.finditer(r"^  ([A-Za-z0-9_-]+):\s*$", jobs, re.MULTILINE))
        for index, start in enumerate(starts):
            end = starts[index + 1].start() if index + 1 < len(starts) else len(jobs)
            block = jobs[start.end():end]
            yield workflow.name, start.group(1), runner_labels(block), job_field(block, "if")


HOSTED_LABEL = re.compile(r"(ubuntu|macos|windows)-[A-Za-z0-9.-]+")


def runner_labels(block):
    """The job's runs-on as written."""
    return job_field(block, "runs-on") or ""


def resolve_runners(runs_on, block, context):
    """Every runner a job's runs-on can select in this context: a label list or
    a hosted label per alternative (matrix entries, evaluated expressions)."""
    runs_on = runs_on.strip()
    if runs_on.startswith(">-"):
        runs_on = runs_on[2:].strip()
    whole = re.fullmatch(r"\$\{\{(.*)\}\}", runs_on)
    if whole and whole.group(1).strip() == "matrix.runs_on":
        return [entry["runs_on"] for entry in context["matrix_entries"]]
    if whole and re.fullmatch(r"\s*matrix\.([A-Za-z0-9_-]+)\s*", whole.group(1)):
        key = whole.group(1).strip().split(".")[1]
        values = re.search(rf"^\s+{key}:\s*\[(.*)\]\s*$", block, re.MULTILINE)
        return [label.strip(" '\"") for label in values.group(1).split(",")] if values else [runs_on]
    if whole:
        return [evaluate(whole.group(1), context)]
    items = [item.strip().strip('"') for item in runs_on.strip("[]").split(",")]
    return [[str(evaluate(re.fullmatch(r"\$\{\{(.*)\}\}", item).group(1), context))
             if item.startswith("${{") else item for item in items]]


def self_hosted(runners):
    return any(runner == "self-hosted" or (isinstance(runner, list) and "self-hosted" in runner)
               for runner in runners)


def pull_request_job_blocks():
    for workflow in sorted(WORKFLOW.parent.glob("*.yml")):
        text = workflow.read_text()
        if not re.search(r"^  pull_request(_target)?:", top_level_block(text, "on"), re.MULTILINE):
            continue
        jobs = top_level_block(text, "jobs")
        starts = list(re.finditer(r"^  ([A-Za-z0-9_-]+):\s*$", jobs, re.MULTILINE))
        for index, start in enumerate(starts):
            end = starts[index + 1].start() if index + 1 < len(starts) else len(jobs)
            yield workflow.name, start.group(1), jobs[start.end():end]


def job_runs(condition, context):
    if condition is None:
        return True
    inner = re.fullmatch(r"\$\{\{(.*)\}\}", condition.strip())
    return bool(evaluate(inner.group(1) if inner else condition, context))


def gate_module():
    path = Path(__file__).resolve().parents[2] / "scripts" / "ci" / "gate.py"
    spec = importlib.util.spec_from_file_location("gate", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GATE = gate_module()


def pull_request_context(head_repository, repository="gents-ai/gents", labels=("ci:run",)):
    head = {"repo": {"full_name": head_repository} if head_repository else None}
    fork = (head_repository or "").lower() != repository.lower()
    # A gate plan that selects every job, so only the trust rule can skip one,
    # routed as the plan job routes this pull request.
    plan = {"runs_on": {role: GATE.runner(role, fork=fork, fallback=False, event="pull_request")
                        for role in GATE.WORKFLOW_ROLES},
            "jobs": ["contract", "fmt", "lint", "lint-variants", "notary-control",
                     "ts-browser", "ts-fuzz", "ts-packages", "ts-unit",
                     "test:runtime", "build:portable", "shard:portable"]}
    matrix = [{"group": group.name, "runs_on": GATE.runner(group.runner, fork=fork, fallback=False, event="pull_request")}
              for group in GATE.GROUPS]
    outputs = {"plan": json.dumps(plan), "jobs": json.dumps(plan["jobs"]), "tests": "true"}
    return {"github": {"event_name": "pull_request", "ref": "refs/pull/1/merge",
                       "repository": repository,
                       "event": {"label": {"name": "adapter-interop"},
                                 "pull_request": {"draft": False, "stack": None,
                                                  "labels": [{"name": name} for name in labels],
                                                  "head": head}}},
            "inputs": {}, "vars": {}, "needs": {"plan": {"outputs": outputs}},
            "matrix_entries": matrix}


class SelfHostedRoutingTests(unittest.TestCase):
    """Self-hosted runner accounts share the Cargo home, the compiler cache and
    kept target trees across runners, so no fork's code may run on one."""

    HEADS = ("someone/gents", "gents-ai/gents-fork", None)
    # Jobs that build or test the change on our runners: a fork skips them.
    BUILDS = {("ci.yml", "lint"), ("ci.yml", "lint-variants"), ("ci.yml", "contract"), ("ci.yml", "test"),
              ("ci.yml", "desktop-ui-bombadil"), ("core-packs.yml", "build"),
              ("adapter-interop.yml", "adapter-interop")}

    def routed(self, context):
        for workflow, job, block in pull_request_job_blocks():
            runners = resolve_runners(runner_labels(block), block, context)
            yield workflow, job, job_field(block, "if"), runners

    def test_fork_pull_requests_never_reach_a_self_hosted_runner(self):
        for head in self.HEADS:
            context = pull_request_context(head)
            for workflow, job, condition, runners in self.routed(context):
                with self.subTest(workflow=workflow, job=job, head=head):
                    if self_hosted(runners):
                        self.assertFalse(job_runs(condition, context), runners)

    def test_forks_skip_the_build_jobs(self):
        for head in self.HEADS:
            context = pull_request_context(head)
            ran = {(workflow, job) for workflow, job, condition, _ in self.routed(context)
                   if job_runs(condition, context)}
            self.assertFalse(ran & self.BUILDS, head)

    def test_unlabeled_pull_requests_skip_plan(self):
        context = pull_request_context("gents-ai/gents", labels=())
        routed = {(workflow, job): condition for workflow, job, condition, _ in self.routed(context)}
        self.assertFalse(job_runs(routed[("ci.yml", "plan")], context))

    def test_pull_requests_from_this_repository_run_on_our_runners(self):
        context = pull_request_context("gents-ai/gents")
        routed = {(workflow, job): (condition, runners) for workflow, job, condition, runners in self.routed(context)}
        self.assertLessEqual(self.BUILDS, set(routed))
        hosted_pool = {role for role, entry in GATE.inventory()["roles"].items()
                       if entry.get("pool") == "hosted"}
        # desktop-ui runs on the browser role, which may still be hosted.
        browser_hosted = "browser" in hosted_pool
        for key in sorted(self.BUILDS | {("ci.yml", name) for name in (
                "plan", "repository-guard", "fmt", "publish-control-tests", "desktop-packages",
                "desktop-ui", "triage-hygiene", "notary-control-tests", "gate")}):
            condition, runners = routed[key]
            with self.subTest(job=key):
                self.assertTrue(job_runs(condition, context))
                expected = not (key == ("ci.yml", "desktop-ui") and browser_hosted)
                self.assertEqual(self_hosted(runners), expected, runners)

    def test_pull_request_jobs_stay_off_the_merge_queues_reserved_runners(self):
        label = GATE.reserve_label()
        context = pull_request_context("gents-ai/gents")
        for workflow, job, condition, runners in self.routed(context):
            with self.subTest(workflow=workflow, job=job):
                for labels in runners:
                    if isinstance(labels, list) and "self-hosted" in labels:
                        self.assertIn(label, labels)

    def test_the_guard_reports_skipped_jobs_to_a_fork(self):
        guard = [job_field(block, "if") for workflow, job, block in pull_request_job_blocks()
                 if (workflow, job) == ("ci.yml", "repository-guard")]
        self.assertEqual(guard, [None])
        text = WORKFLOW.read_text()
        step = re.search(r"- name: Report jobs a fork pull request skips\n"
                         r"        if: >-\n((?:          .*\n)+)", text)
        self.assertIsNotNone(step)
        condition = " ".join(line.strip() for line in step.group(1).splitlines())
        self.assertTrue(job_runs(condition, pull_request_context("someone/gents")))
        self.assertFalse(job_runs(condition, pull_request_context("gents-ai/gents")))
        for _, job in self.BUILDS:
            self.assertIn(job, text[step.end():step.end() + 2000])


class KeptTreeTrustTests(unittest.TestCase):
    REPOSITORY = "gents-ai/gents"

    def keeps(self, event_name, ref="refs/heads/main", head_repository=None, **event):
        if head_repository is not None or event_name == "pull_request":
            head = {"repo": {"full_name": head_repository} if head_repository else None}
            event = {**event, "pull_request": {"head": head}}
        context = {"github": {"event_name": event_name, "ref": ref,
                              "repository": self.REPOSITORY, "event": event}}
        return evaluate(keep_condition(), context) is True

    def test_push_to_main_keeps_the_tree(self):
        self.assertTrue(self.keeps("push"))

    def test_main_merge_queue_keeps_the_tree(self):
        self.assertTrue(self.keeps("merge_group", "refs/heads/gh-readonly-queue/main/pr-1-abc"))

    def test_scheduled_run_of_main_keeps_the_tree(self):
        self.assertTrue(self.keeps("schedule"))

    def test_pull_request_from_this_repository_keeps_the_tree(self):
        self.assertTrue(self.keeps("pull_request", "refs/pull/1/merge", self.REPOSITORY))
        self.assertTrue(self.keeps("pull_request", "refs/pull/1/merge", "Gents-AI/Gents"))

    def test_fork_pull_request_builds_in_a_job_tree(self):
        self.assertFalse(self.keeps("pull_request", "refs/pull/1/merge", "someone/gents"))
        self.assertFalse(self.keeps("pull_request", "refs/pull/1/merge", "gents-ai/gents-fork"))

    def test_pull_request_whose_head_repository_is_gone_builds_in_a_job_tree(self):
        self.assertFalse(self.keeps("pull_request", "refs/pull/1/merge"))

    def test_other_events_and_refs_build_in_a_job_tree(self):
        self.assertFalse(self.keeps("push", "refs/heads/feature"))
        self.assertFalse(self.keeps("push", "refs/tags/v1.0.0"))
        self.assertFalse(self.keeps("merge_group", "refs/heads/gh-readonly-queue/release/pr-1"))
        self.assertFalse(self.keeps("schedule", "refs/heads/feature"))
        self.assertFalse(self.keeps("workflow_dispatch"))
        self.assertFalse(self.keeps("pull_request_target", "refs/heads/main", self.REPOSITORY))

    def test_only_the_workflow_condition_selects_a_kept_tree(self):
        script = (Path(__file__).with_name("setup-rust-build-cache.sh")).read_text()
        self.assertIn('"${GENTS_CI_KEEP_TARGET_TREE:-}" == "true"', script)
        for path in sorted(WORKFLOW.parent.glob("*.yml")):
            text = path.read_text()
            if "GENTS_CI_KEEP_TARGET_TREE" not in text:
                continue
            with self.subTest(workflow=path.name):
                self.assertEqual(text.count("GENTS_CI_KEEP_TARGET_TREE"), 1,
                                 "no job or step may override the workflow-level condition")
                self.assertEqual(keep_condition(path), keep_condition())


if __name__ == "__main__":
    unittest.main()
