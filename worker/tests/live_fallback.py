"""Fresh install with no native executables: HTTP works and browser use degrades."""

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gents", required=True)
    args = parser.parse_args()
    gents = str(Path(args.gents).resolve())
    pack = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory(prefix="gents-web-fallback-") as root:
        root = Path(root)
        home, archive = root / "home", root / "web.pack"
        empty_path = root / "no-executables"
        empty_path.mkdir()
        environment = dict(os.environ, PATH=str(empty_path))

        def run(*command, restricted=True):
            result = subprocess.run(
                [gents, *map(str, command)],
                env=environment if restricted else None,
                capture_output=True,
                text=True,
                timeout=180,
            )
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        run("pack", "build", pack, "--out", archive, restricted=False)
        run(
            "init",
            "--home",
            home,
            "--backend-preset",
            "vllm",
            "--inference-url",
            "http://127.0.0.1:8000/v1",
            "--model-name",
            "unused",
            "--tool-package",
            "minimal",
            restricted=False,
        )
        run("pack", "install", archive, "--home", home, "--grant-authority")
        assert not (home / "plugin-services").exists()

        def call(tool, data):
            return run(
                "plugin",
                "run",
                f"gents/{tool}",
                "--home",
                home,
                "--input",
                json.dumps(data),
            )

        call("browser_exec", {"op": "help"})
        assert not (home / "plugin-services").exists()
        doctor = call("browser_exec", {"op": "doctor"})["response"]
        assert doctor["status"] == "limited" and doctor["mode"] == "http_only", doctor
        assert doctor["capabilities"] == {"public_http": True, "browser": False}
        unavailable = call("browser_exec", {"code": "raise Exception('must not run')"})[
            "response"
        ]
        assert (
            unavailable["status"] == "unavailable"
            and unavailable["code_executed"] is False
        )
        service_runs = set((home / "plugin-services").glob("run-*"))
        fetched = call("web_fetch", {"url": "https://example.com", "max_chars": 2000})
        assert fetched["ok"], fetched
        assert "Example Domain" in json.dumps(fetched), fetched
        assert set((home / "plugin-services").glob("run-*")) == service_runs

        # Model a schema-compatible runtime ignoring the unknown service declaration.
        record_path = home / "plugins" / "installed" / "gents" / "browser_exec.json"
        record = json.loads(record_path.read_text())
        record["declaration"].pop("service")
        record_path.write_text(json.dumps(record))
        absent = call("browser_exec", {"op": "doctor"})["response"]
        assert absent["mode"] == "http_only" and "did not supply" in absent["reason"], (
            absent
        )
        assert set((home / "plugin-services").glob("run-*")) == service_runs
        print(
            json.dumps(
                {
                    "passed": True,
                    "missing_dependencies": doctor["mode"],
                    "missing_service_support": absent["mode"],
                    "http_fetch": True,
                }
            )
        )


if __name__ == "__main__":
    main()
