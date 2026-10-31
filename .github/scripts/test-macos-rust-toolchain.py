#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
SCRIPT = SCRIPTS / "setup-macos-rust-toolchain.sh"


class ToolchainSetupTests(unittest.TestCase):
    """Fences the CI toolchain selection that fixes the `ld: tapi` link failure.

    The script must resolve the developer directory the linker comes from,
    pin SDKROOT to that directory's macOS SDK, prove the C compiler the tests
    spawn can link, and export both variables to later steps. Each test drives
    a fake toolchain so the assertions hold on any host.
    """

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.developer = self.root / "Xcode-beta.app" / "Contents" / "Developer"
        self.developer.mkdir(parents=True)
        self.sdk = (
            self.developer
            / "Platforms"
            / "MacOSX.platform"
            / "Developer"
            / "SDKs"
            / "MacOSX26.4.sdk"
        )
        self.sdk.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.github_env = self.root / "github_env"
        self.github_env.write_text("")
        self.cc_log = self.root / "cc.log"
        self.cargo_log = self.root / "cargo.log"

    def write_tool(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)
        return path

    def fake_xcode_select(self, developer_dir):
        return self.write_tool(
            "xcode-select",
            "#!/bin/sh\n" f'printf "%s\\n" "{developer_dir}"\n',
        )

    def fake_xcrun(self, sdk_path):
        return self.write_tool(
            "xcrun",
            "#!/bin/sh\n" f'printf "%s\\n" "{sdk_path}"\n',
        )

    def fake_cc(self, exit_code=0, stderr=""):
        return self.write_tool(
            "cc",
            "#!/bin/sh\n"
            f'printf "%s\\n" "$*" >> "{self.cc_log}"\n'
            f'printf "%s" "{stderr}" >&2\n'
            f"exit {exit_code}\n",
        )

    def fake_cargo(self, exit_code=0, stderr=""):
        return self.write_tool(
            "cargo",
            "#!/bin/sh\n"
            f'printf "%s\\n" "$*" >> "{self.cargo_log}"\n'
            f'printf "%s" "{stderr}" >&2\n'
            f"exit {exit_code}\n",
        )

    def run_script(self, **env_overrides):
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env["GITHUB_ENV"] = str(self.github_env)
        env["GENTS_PROBE_CC"] = str(self.bin / "cc")
        env.pop("GENTS_DEVELOPER_DIR", None)
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

    def test_pins_sdkroot_to_the_selected_developer_directory(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        exported = self.exported()
        self.assertEqual(exported["DEVELOPER_DIR"], str(self.developer))
        self.assertEqual(exported["SDKROOT"], str(self.sdk))
        self.assertIn("Selected toolchain", result.stdout)

    def test_uses_the_developer_directory_the_linker_comes_from(self):
        # The regression this fences: pinning the CLT developer dir made
        # rust-lld fail on the CLT 27.2 SDK. The SDK must come from the
        # directory xcode-select reports, not from the newest SDK on disk.
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.exported()["SDKROOT"], str(self.sdk))
        self.assertNotIn("CommandLineTools", self.exported()["SDKROOT"])

    def test_honours_an_explicit_developer_directory(self):
        other = self.root / "CommandLineTools"
        other.mkdir()
        other_sdk = other / "SDKs" / "MacOSX27.2.sdk"
        other_sdk.mkdir(parents=True)
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(other_sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script(GENTS_DEVELOPER_DIR=str(other))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.exported()["DEVELOPER_DIR"], str(other))
        self.assertEqual(self.exported()["SDKROOT"], str(other_sdk))

    def test_probes_the_compiler_the_tests_spawn(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The probe must be a real link of a C source, not a version check:
        # the failing tests build a fixture with /usr/bin/cc.
        self.assertTrue(self.cc_log.exists(), "the C compiler was never invoked")
        invocation = self.cc_log.read_text()
        self.assertIn("probe.c", invocation)
        self.assertIn("-o", invocation)

    def test_fails_fast_when_the_compiler_cannot_link(self):
        self.fake_xcode_select(self.developer)
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

    def test_probes_the_rust_link_path_through_cargo(self):
        # The C probe alone is not enough: .cargo/config.toml routes Rust links
        # through scripts/macos-clang-lld.sh, so a rust-lld/SDK mismatch passes
        # the C probe and only fails deep inside a suite.
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.cargo_log.exists(), "cargo was never invoked")
        invocation = self.cargo_log.read_text()
        self.assertIn("run", invocation)
        self.assertIn("--manifest-path", invocation)

    def test_fails_fast_when_cargo_cannot_link(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo(
            exit_code=101,
            stderr="rust-lld: error: could not load TAPI file "
            "libSystem.B.tbd: unknown target arm64e.x1-macos\n",
        )
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot link a Rust binary", result.stdout + result.stderr)
        self.assertIn("unknown target", result.stdout + result.stderr)
        self.assertEqual(self.exported(), {})

    def test_cargo_probe_clears_the_rustc_wrapper(self):
        # The job sets RUSTC_WRAPPER=sccache, which is not on PATH this early.
        # A probe that inherits it fails with "could not execute process
        # `sccache ... rustc -vV`" — a cache problem that looks like a linker
        # problem. The probe must clear the wrapper, in BOTH forms: the
        # environment variable and a `build.rustc-wrapper` in a cargo config
        # file, which `env -u` does not remove.
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script(RUSTC_WRAPPER="/nonexistent/sccache")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.cargo_log.exists(), "cargo was never invoked")
        invocation = self.cargo_log.read_text()
        self.assertIn("--config", invocation)
        self.assertIn("build.rustc-wrapper", invocation)

    def test_cargo_probe_can_be_skipped(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        result = self.run_script(GENTS_SKIP_CARGO_PROBE="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.cargo_log.exists(), "cargo ran despite the skip")

    def test_fails_when_xcrun_reports_no_sdk(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun("")
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no macOS SDK", result.stdout + result.stderr)
        self.assertEqual(self.exported(), {})

    def test_fails_when_the_developer_directory_is_absent(self):
        self.fake_xcode_select(self.root / "missing-Developer")
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Developer directory not found", result.stdout + result.stderr)

    def test_fails_when_xcode_select_is_absent(self):
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("xcode-select is missing", result.stdout + result.stderr)

    def test_succeeds_without_github_env(self):
        self.fake_xcode_select(self.developer)
        self.fake_xcrun(self.sdk)
        self.fake_cc()
        self.fake_cargo()
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env.pop("GITHUB_ENV", None)
        env.pop("GENTS_DEVELOPER_DIR", None)
        env["GENTS_PROBE_CC"] = str(self.bin / "cc")
        result = subprocess.run(
            ["bash", str(SCRIPT)], env=env, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
