#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("setup-lean-build-cache.sh")


class LeanBuildCacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name).resolve()
        self.workspace = root / "workspace"
        self.proofs = self.workspace / "crates/gents/proofs"
        self.proofs.mkdir(parents=True)
        self.cache_root = root / "lean-cache"
        self.cache = self.cache_root / "runner_1"
        self.lake = self.proofs / ".lake"

    def setup(self):
        env = dict(os.environ, GITHUB_WORKSPACE=str(self.workspace), LEAN_CACHE_ROOT=str(self.cache_root),
                   RUNNER_NAME="runner 1")
        return subprocess.run(["bash", str(SCRIPT)], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True)

    def assert_linked(self, result):
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertTrue(self.lake.is_symlink())
        self.assertEqual(os.readlink(self.lake), str(self.cache))

    def test_a_fresh_checkout_links_the_runner_cache(self):
        self.assert_linked(self.setup())

    def test_the_expected_link_is_reused(self):
        self.assert_linked(self.setup())
        (self.cache / "kept").write_text("olean")
        result = self.setup()
        self.assert_linked(result)
        self.assertIn("Reusing", result.stdout)
        self.assertTrue((self.cache / "kept").exists())

    def test_locked_package_checkout_is_preserved_and_next_cache_is_reused(self):
        self.assert_linked(self.setup())
        lock = self.cache / "packages/mathlib/.git/index.lock"
        lock.parent.mkdir(parents=True)
        lock.write_bytes(b"pending index")
        result = self.setup()
        self.assertEqual(result.returncode, 0, result.stdout)
        alternate = self.lake.resolve()
        self.assertNotEqual(alternate, self.cache)
        (alternate / "kept").write_text("olean")
        self.assertEqual(self.setup().returncode, 0)
        self.assertEqual(self.lake.resolve(), alternate)
        self.assertTrue((self.lake / "kept").is_file())
        self.assertEqual(lock.read_bytes(), b"pending index")

    def test_a_stray_build_directory_is_replaced(self):
        (self.lake / "build").mkdir(parents=True)
        (self.lake / "build/stale.olean").write_text("stale")
        self.assert_linked(self.setup())

    def test_a_stale_link_is_replaced_without_touching_its_target(self):
        other = Path(self.temporary.name) / "other-cache"
        other.mkdir()
        (other / "kept").write_text("olean")
        self.lake.symlink_to(other)
        self.assert_linked(self.setup())
        self.assertTrue((other / "kept").exists())

    def test_a_dangling_link_is_replaced(self):
        self.lake.symlink_to(Path(self.temporary.name) / "gone")
        self.assert_linked(self.setup())

    def test_a_regular_file_is_refused(self):
        self.lake.write_text("not build output")
        result = self.setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("neither a directory nor a link", result.stdout)
        self.assertEqual(self.lake.read_text(), "not build output")

    def test_proofs_outside_the_checkout_are_refused(self):
        outside = Path(self.temporary.name) / "outside-proofs"
        (outside / ".lake").mkdir(parents=True)
        (self.workspace / "crates/gents/proofs").rmdir()
        (self.workspace / "crates/gents/proofs").symlink_to(outside)
        result = self.setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside", result.stdout)
        self.assertTrue((outside / ".lake").is_dir())


if __name__ == "__main__":
    unittest.main()
