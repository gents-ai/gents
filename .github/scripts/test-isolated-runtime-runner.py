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
            module.check(self.account, self.env, self.uid, self.processes)

    def test_dedicated_account_and_live_fleet_home_pass(self):
        self.check()

    def test_requires_the_running_fleet_home(self):
        self.processes = f"{os.getuid()} /usr/local/bin/gents server --home /other\n"
        with self.assertRaisesRegex(ValueError, "running gents server"):
            self.check()

    def test_rejects_same_account_and_readable_fleet_home(self):
        with self.assertRaisesRegex(ValueError, "owned by the runner"):
            module.check(self.account, self.env, os.getuid(), self.processes)
        with patch.object(module.os, "access", return_value=True):
            with self.assertRaisesRegex(ValueError, "can access"):
                module.check(self.account, self.env, self.uid, self.processes)

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

    def test_rejects_unrelated_process_user(self):
        self.processes = f"{os.getuid() + 9} /usr/local/bin/gents server --home {self.fleet}\n"
        with self.assertRaisesRegex(ValueError, "running gents server"):
            self.check()

    def test_rejects_a_dangling_runtime_home(self):
        (self.account / ".gents").symlink_to(self.account / "missing")
        with self.assertRaisesRegex(ValueError, "persistent .gents"):
            self.check()


if __name__ == "__main__":
    unittest.main()
