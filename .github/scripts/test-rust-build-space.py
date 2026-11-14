#!/usr/bin/env python3
import importlib.util
import os
from pathlib import Path
from collections import namedtuple
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

    def limits(self, runner, maximum="", floor=""):
        return build_space.target_limits({"RUNNER_NAME": runner,
                                          "GENTS_CI_TARGET_MAX_GIB": maximum,
                                          "GENTS_CI_TARGET_FLOOR_GIB": floor})

    def test_unset_limits_use_safe_defaults(self):
        self.assertEqual(self.limits("studio-1"), (60, 150))

    def test_limits_are_chosen_per_runner(self):
        caps = '{"studio-1": 70, "studio-2": 250, "default": 40}'
        floors = '{"minisforum-1": 50}'
        self.assertEqual(self.limits("studio-1", caps, floors), (70, 150))
        self.assertEqual(self.limits("studio-2", caps, floors), (250, 150))
        self.assertEqual(self.limits("minisforum-1", caps, floors), (40, 50))
        self.assertEqual(self.limits("studio-1", "90", "120"), (90, 120))
        self.assertEqual(self.limits("studio-1", '{"studio-2": 250}'), (60, 150))

    def test_rejects_malformed_or_non_positive_limits(self):
        for setting in ("0", "-5", "1.5", "true", "big", '{"studio-1": 0}',
                        '{"studio-1": "70"}', '{"default": null}', "[70]"):
            with self.subTest(setting=setting), self.assertRaises(ValueError):
                self.limits("studio-1", setting)
        with self.assertRaisesRegex(ValueError, "RUNNER_NAME"):
            self.limits("")

    def test_limits_command_prints_cap_and_floor(self):
        environ = {**os.environ, "RUNNER_NAME": "studio-2",
                   "GENTS_CI_TARGET_MAX_GIB": '{"studio-2": 250}',
                   "GENTS_CI_TARGET_FLOOR_GIB": ""}
        output = run_command([sys.executable, str(Path(build_space.__file__)), "--limits"],
                             env=environ, check=True, capture_output=True, text=True)
        self.assertEqual(output.stdout.split(), ["250", "150"])

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


def evaluate(expression, context):
    """Evaluate the subset of GitHub expression syntax the trust condition uses."""
    tokens = re.findall(r"\(|\)|&&|\|\||==|!=|'[^']*'|[A-Za-z_][A-Za-z0-9_.-]*", expression)
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
        value = context
        for part in token.split("."):
            value = value.get(part) if isinstance(value, dict) else None
        return value

    def comparison():
        left = operand()
        while peek() in ("==", "!="):
            operator, right = take(), operand()
            # GitHub compares strings case-insensitively.
            fold = [v.lower() if isinstance(v, str) else v for v in (left, right)]
            left = (fold[0] == fold[1]) == (operator == "==")
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


def keep_condition():
    text = WORKFLOW.read_text()
    match = re.search(r"^  GENTS_CI_KEEP_TARGET_TREE: >-\n((?:    .*\n)+)", text, re.MULTILINE)
    if match is None:
        raise AssertionError("ci.yml must set GENTS_CI_KEEP_TARGET_TREE at workflow level")
    folded = " ".join(line.strip() for line in match.group(1).splitlines())
    inner = re.fullmatch(r"\$\{\{(.*)\}\}", folded)
    if inner is None:
        raise AssertionError("GENTS_CI_KEEP_TARGET_TREE must be one expression")
    return inner.group(1)


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
        self.assertFalse(self.keeps("merge_group", "refs/heads/gh-readonly-queue/main/pr-1"))
        self.assertFalse(self.keeps("workflow_dispatch"))
        self.assertFalse(self.keeps("pull_request_target", "refs/heads/main", self.REPOSITORY))

    def test_only_the_workflow_condition_selects_a_kept_tree(self):
        script = (Path(__file__).with_name("setup-rust-build-cache.sh")).read_text()
        self.assertIn('"${GENTS_CI_KEEP_TARGET_TREE:-}" == "true"', script)
        workflow = WORKFLOW.read_text()
        self.assertEqual(workflow.count("GENTS_CI_KEEP_TARGET_TREE"), 1,
                         "no job or step may override the workflow-level condition")


if __name__ == "__main__":
    unittest.main()
