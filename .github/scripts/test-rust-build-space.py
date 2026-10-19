#!/usr/bin/env python3
import importlib.util
from pathlib import Path
from collections import namedtuple
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "build_space", Path(__file__).with_name("ensure-rust-build-space.py"))
build_space = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build_space)
Usage = namedtuple("Usage", "total used free")


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

    def test_insufficient_host_space_fails_before_compilation(self):
        with self.assertRaisesRegex(RuntimeError, "free additional space"):
            self.run_with_free_space(10, 30, 40)

    def test_recreates_target_removed_by_cargo_clean(self):
        self.clean.side_effect = lambda *args, **kwargs: self.target.rmdir()
        self.run_with_free_space(10, 30, 70)
        self.assertTrue(self.target.is_dir())

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


if __name__ == "__main__":
    unittest.main()
