#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
SCRIPT = SCRIPTS / "setup-macos-clt-toolchain.sh"


class CltToolchainSetupTests(unittest.TestCase):
    """Fences the CI toolchain selection that fixes the `ld: tapi` link failure.

    The script must select the Command Line Tools developer dir, resolve the
    macOS SDK through it, prove the C compiler the tests spawn can link, and
    export both variables to later steps. Each test drives a fake toolchain so
    the assertions hold on any host.
    """

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.clt = self.root / "CommandLineTools"
        (self.clt / "usr" / "bin").mkdir(parents=True)
        clang = self.clt / "usr" / "bin" / "clang"
        clang.write_text("#!/bin/sh\n")
        clang.chmod(0o755)
        self.sdk = self.clt / "SDKs" / "MacOSX27.2.sdk"
        self.sdk.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.github_env = self.root / "github_env"
        self.github_env.write_text("")
        self.cc_log = self.root / "cc.log"

    def write_tool(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)
        return path

    def fake_xcrun(self, sdk_path):
        return self.write_tool(
            "xcrun",
            "#!/bin/sh\n"
            f'printf "%s\\n" "{sdk_path}"\n',
        )

    def fake_cc(self, exit_code=0, stderr=""):
        return self.write_tool(
            "cc",
            "#!/bin/sh\n"
            f'printf "%s\\n" "$*" >> "{self.cc_log}"\n'
            f'printf "%s" "{stderr}" >&2\n'
            f"exit {exit_code}\n",
        )

    def run_script(self, **env_overrides):
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env["GITHUB_ENV"] = str(self.github_env)
        env["GENTS_CLT_DEVELOPER_DIR"] = str(self.clt)
        env["GENTS_CLT_PROBE_CC"] = str(self.bin / "cc")
        env.update(env_overrides)
        return subprocess.run(
            ["bash", str(SCRIPT)],
            env=env,
            capture_output=True,
            text=True,
        )

    def exported(self):
        values = {}
        for line in self.github_env.read_text().splitlines():
            if "=" in line:
                key, _, value = line.partition("=")
                values[key] = value
        return values

    def test_selects_clt_toolchain_and_exports_it(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        exported = self.exported()
        self.assertEqual(exported["DEVELOPER_DIR"], str(self.clt))
        self.assertEqual(exported["SDKROOT"], str(self.sdk))
        self.assertIn("Selected CLT toolchain", result.stdout)

    def test_probes_the_compiler_the_tests_spawn(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        # The probe must be a real link of a C source, not a version check:
        # the failing tests build a fixture with /usr/bin/cc.
        self.assertTrue(self.cc_log.exists(), "the C compiler was never invoked")
        invocation = self.cc_log.read_text()
        self.assertIn("probe.c", invocation)
        self.assertIn("-o", invocation)

    def test_fails_fast_when_the_compiler_cannot_link(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc(
            exit_code=1,
            stderr="ld: tapi error: malformed file\n"
            "libSystem.B.tbd:4:20: error: unknown architecture\n",
        )
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot link", result.stdout + result.stderr)
        # The compiler's own words must reach the log, not a generic message.
        self.assertIn("unknown architecture", result.stdout + result.stderr)
        self.assertEqual(self.exported(), {})

    def test_fails_when_xcrun_reports_no_sdk(self):
        self.fake_xcrun("")
        self.fake_cc()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no macOS SDK", result.stdout + result.stderr)
        self.assertEqual(self.exported(), {})

    def test_fails_when_clt_is_absent(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        result = self.run_script(
            GENTS_CLT_DEVELOPER_DIR=str(self.root / "missing-CommandLineTools")
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Command Line Tools not found", result.stdout + result.stderr)

    def test_fails_when_clt_clang_is_absent(self):
        (self.clt / "usr" / "bin" / "clang").unlink()
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("clang is missing", result.stdout + result.stderr)

    def test_succeeds_without_github_env(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env.pop("GITHUB_ENV", None)
        env["GENTS_CLT_DEVELOPER_DIR"] = str(self.clt)
        env["GENTS_CLT_PROBE_CC"] = str(self.bin / "cc")
        result = subprocess.run(
            ["bash", str(SCRIPT)], env=env, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
