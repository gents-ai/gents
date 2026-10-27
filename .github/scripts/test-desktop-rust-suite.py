#!/usr/bin/env python3
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class DesktopRustSuiteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in (
            "compile-rust-ci-suite.sh",
            "desktop-rust-suite.sh",
            "desktop-runtime-journey.sh",
        ):
            shutil.copy2(SCRIPTS / name, self.root / name)
        self.log = self.root / "cargo.jsonl"
        cargo = self.root / "cargo"
        cargo.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['TEST_CARGO_LOG'], 'a') as log:\n"
            "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "sys.exit(int(os.environ.get('TEST_CARGO_EXIT', '0')))\n"
        )
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.root}{os.pathsep}{os.environ['PATH']}")
        self.env["TEST_CARGO_LOG"] = str(self.log)

    def run_script(self, name, *args):
        return subprocess.run(
            ["bash", str(self.root / name), *args],
            env=self.env,
            capture_output=True,
            text=True,
        )

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_compile_and_execution_keep_the_same_package_and_feature_graph(self):
        build = self.run_script("compile-rust-ci-suite.sh", "desktop")
        self.assertEqual(build.returncode, 0, build.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        compiled, journey = calls
        self.assertEqual(compiled[0], "test")
        self.assertIn("gents-desktop-tauri", compiled)
        self.assertIn("--all-targets", compiled)
        self.assertIn("--no-run", compiled)
        self.assertEqual(journey[0], "build")
        self.assertIn("gents-client-host/runtime-e2e", journey)
        self.assertNotIn("gents-desktop-tauri", journey)

        result = self.run_script("desktop-rust-suite.sh", "test")
        self.assertEqual(result.returncode, 0, result.stderr)
        executed = self.calls()[-1]
        self.assertIn("--no-fail-fast", executed)
        self.assertEqual(compiled[1:-2], executed[1:-1])
        self.assertIn("notify/macos_kqueue", executed)
        self.assertNotIn("--lib", executed)
        self.assertNotIn("native-e2e", " ".join(executed))

    def test_package_coverage_uses_the_same_roster_without_running_cargo(self):
        direct = self.run_script("desktop-rust-suite.sh", "--list-packages")
        delegated = self.run_script("compile-rust-ci-suite.sh", "desktop", "--list-packages")
        self.assertEqual(direct.returncode, 0, direct.stderr)
        self.assertEqual(delegated.returncode, 0, delegated.stderr)
        self.assertEqual(direct.stdout, delegated.stdout)
        packages = direct.stdout.splitlines()
        self.assertEqual(len(packages), len(set(packages)))
        self.assertIn("gents-desktop-tauri", packages)
        self.assertFalse(self.log.exists())

    def test_compile_failure_stops_before_the_journey_build(self):
        self.env["TEST_CARGO_EXIT"] = "19"
        result = self.run_script("compile-rust-ci-suite.sh", "desktop")
        self.assertEqual(result.returncode, 19)
        self.assertEqual(len(self.calls()), 1)

    def test_invalid_modes_do_not_run_cargo(self):
        for script, args in (
            ("desktop-rust-suite.sh", ["typo"]),
            ("desktop-rust-suite.sh", ["test", "extra"]),
            ("compile-rust-ci-suite.sh", ["desktop", "typo"]),
        ):
            with self.subTest(script=script, args=args):
                result = self.run_script(script, *args)
                self.assertEqual(result.returncode, 2)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
