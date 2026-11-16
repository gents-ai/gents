#!/usr/bin/env python3
"""host-runtime-service.sh against a fake launchctl: verdicts, and that it only reads."""

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "host-runtime-service.sh"

FAKE_LAUNCHCTL = """#!/bin/sh
echo "$*" >> "$FAKE_DIR/calls"
state="$(cat "$FAKE_DIR/state")"
[ "$state" = absent ] && { echo 'Could not find service' >&2; exit 113; }
printf 'gui/501/ai.gents.runtime = {\\n\\tactive count = 1\\n\\tstate = %s\\n}\\n' "$state"
"""


class HostRuntimeService(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / "bin").mkdir()
        (self.root / "runner").mkdir()
        fake = self.root / "bin" / "launchctl"
        fake.write_text(FAKE_LAUNCHCTL)
        fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
        self.summary = self.root / "summary.md"

    def tearDown(self):
        self.directory.cleanup()

    def run_script(self, command, state):
        (self.root / "state").write_text(state)
        env = dict(os.environ, PATH=f"{self.root / 'bin'}{os.pathsep}{os.environ['PATH']}",
                   FAKE_DIR=str(self.root), RUNNER_TEMP=str(self.root / "runner"),
                   GITHUB_STEP_SUMMARY=str(self.summary))
        return subprocess.run(["bash", str(SCRIPT), command], env=env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT)

    def job(self, before, after):
        recorded = self.run_script("record", before)
        self.assertEqual(recorded.returncode, 0, recorded.stdout)
        return self.run_script("verify", after)

    def test_a_node_left_running_passes(self):
        self.assertEqual(self.job("running", "running").returncode, 0)

    def test_a_node_stopped_during_the_job_fails_and_says_why(self):
        for after in ("waiting", "absent"):
            result = self.job("running", after)
            self.assertEqual(result.returncode, 1, after)
            self.assertIn("::error title=Live Gents node stopped::", result.stdout)
            self.assertIn("Live Gents node stopped", self.summary.read_text())

    def test_a_host_without_the_service_passes(self):
        result = self.job("absent", "absent")
        self.assertEqual(result.returncode, 0)
        self.assertIn("nothing to watch", self.run_script("record", "absent").stdout)

    def test_a_node_that_was_not_running_is_not_blamed_on_the_job(self):
        self.assertEqual(self.job("waiting", "waiting").returncode, 0)
        self.assertEqual(self.job("absent", "running").returncode, 0)

    def test_verify_without_a_record_passes(self):
        self.assertEqual(self.run_script("verify", "absent").returncode, 0)

    def test_it_only_ever_prints_the_service(self):
        self.job("running", "waiting")
        self.job("absent", "absent")
        calls = (self.root / "calls").read_text().splitlines()
        self.assertTrue(calls)
        uid = os.getuid()
        self.assertEqual(set(calls), {f"print gui/{uid}/ai.gents.runtime"})


if __name__ == "__main__":
    unittest.main()
