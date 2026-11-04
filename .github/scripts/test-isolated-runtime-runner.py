#!/usr/bin/env python3
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

path = Path(__file__).with_name("check-isolated-runtime-runner.py")
spec = importlib.util.spec_from_file_location("isolated_runtime_runner", path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class IsolatedRuntimeRunnerTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.account = root / "runner"
        self.fleet = root / "fleet"
        self.account.mkdir()
        self.fleet.mkdir()
        self.env = {"HOME": str(self.account), "GENTS_CI_FLEET_HOME": str(self.fleet)}
        self.uid = os.getuid() + 1
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home {self.fleet}\n"

    def check(self):
        with patch.object(module.os, "access", return_value=False):
            module.check(self.account, self.env, self.uid, os.getuid(), self.processes)

    def test_dedicated_account_and_live_fleet_home_pass(self):
        self.check()

    def test_requires_the_running_fleet_home(self):
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home /other\n"
        with self.assertRaisesRegex(ValueError, "visible running gents server"):
            self.check()

    def test_accepts_equals_form_of_home_argument(self):
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home={self.fleet}\n"
        self.check()

    def test_rejects_same_account_and_readable_fleet_home(self):
        with self.assertRaisesRegex(ValueError, "uses the runner account"):
            module.check(self.account, self.env, os.getuid(), os.getuid(), self.processes)
        for permitted in (os.R_OK, os.X_OK):
            with self.subTest(permitted=permitted):
                with patch.object(
                    module.os, "access", side_effect=lambda path, mode: mode == permitted
                ):
                    with self.assertRaisesRegex(ValueError, "can access"):
                        module.check(self.account, self.env, self.uid, os.getuid(), self.processes)

    def test_rejects_home_override_and_persistent_runtime_data(self):
        self.env["HOME"] = str(self.fleet)
        with self.assertRaisesRegex(ValueError, "HOME must match"):
            self.check()
        self.env["HOME"] = str(self.account)
        self.env["GENTS_HOME"] = str(self.fleet)
        with self.assertRaisesRegex(ValueError, "must not set GENTS_HOME"):
            self.check()
        del self.env["GENTS_HOME"]
        (self.account / ".gents").mkdir()
        with self.assertRaisesRegex(ValueError, "persistent .gents"):
            self.check()

    def test_rejects_home_missing_or_inside_runner_account(self):
        self.env["GENTS_CI_FLEET_HOME"] = ""
        with self.assertRaisesRegex(ValueError, "live fleet runtime home"):
            self.check()
        child = self.account / "fleet"
        child.mkdir()
        self.env["GENTS_CI_FLEET_HOME"] = str(child)
        with self.assertRaisesRegex(ValueError, "inside the runner account home"):
            self.check()
        self.env["GENTS_CI_FLEET_HOME"] = str(self.fleet / ".." / "fleet")
        with self.assertRaisesRegex(ValueError, "live fleet runtime home"):
            self.check()

    def test_rejects_unrelated_process_user(self):
        self.processes = f"{os.getuid() + 9} /usr/local/bin/gents server --home {self.fleet}\n"
        with self.assertRaisesRegex(ValueError, "visible running gents server"):
            self.check()

    def test_rejects_a_dangling_runtime_home(self):
        (self.account / ".gents").symlink_to(self.account / "missing")
        with self.assertRaisesRegex(ValueError, "persistent .gents"):
            self.check()

    def test_inaccessible_fleet_parent_does_not_prevent_preflight(self):
        private = self.fleet / "private"
        private.mkdir()
        self.fleet = private / ".gents"
        self.fleet.mkdir()
        self.env["GENTS_CI_FLEET_HOME"] = str(self.fleet)
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home {self.fleet}\n"
        private.chmod(0)
        self.addCleanup(private.chmod, 0o700)
        if os.access(self.fleet, os.X_OK):
            self.skipTest("current user can traverse mode-000 directories")
        module.check(self.account, self.env, self.uid, os.getuid(), self.processes)

    def test_hidden_process_arguments_fail_closed(self):
        self.processes = f"{os.getuid()} (gents)\n"
        with self.assertRaisesRegex(ValueError, "process visibility"):
            self.check()

    def test_relative_and_symlink_process_paths_do_not_match(self):
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home .gents\n"
        with self.assertRaisesRegex(ValueError, "visible running gents server"):
            self.check()
        alias = self.account.parent / "fleet-link"
        alias.symlink_to(self.fleet)
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home {alias}\n"
        with self.assertRaisesRegex(ValueError, "visible running gents server"):
            self.check()


if __name__ == "__main__":
    unittest.main()
