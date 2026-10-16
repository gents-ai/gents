"""Installed WASM -> host HTTP rounds -> worker -> Chromium integration test."""

import argparse
import json
import subprocess
import sys
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

from live_browser import Fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gents", default="gents")
    args = parser.parse_args()
    pack = Path(__file__).resolve().parents[2]
    script = pack / "worker/browser_worker.py"
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    thread.start()
    with tempfile.TemporaryDirectory(prefix="gents-browser-integration-") as root:
        state, home = Path(root) / "browser", Path(root) / "home"

        def run(command):
            result = subprocess.run(
                command, check=True, capture_output=True, text=True, timeout=90
            )
            return json.loads(result.stdout)

        worker = [sys.executable, str(script)]
        started = False
        try:
            setup = run(
                worker + ["setup", "--state", str(state), "--allow-host-execution"]
            )
            started = True
            assert setup["status"] == "ready"
            assert run(
                worker + ["setup", "--state", str(state), "--allow-host-execution"]
            )["reused"]
            run(
                [
                    args.gents,
                    "init",
                    "--home",
                    str(home),
                    "--backend-preset",
                    "vllm",
                    "--inference-url",
                    "http://127.0.0.1:8000/v1",
                    "--model-name",
                    "fixture",
                    "--tool-package",
                    "minimal",
                ]
            )
            run(
                [
                    args.gents,
                    "pack",
                    "install",
                    str(pack),
                    "--home",
                    str(home),
                    "--grant-authority",
                ]
            )
            command = [
                args.gents,
                "plugin",
                "run",
                "gents/browser_exec",
                "--home",
                str(home),
                "--bind-dir",
                str(state),
                "--input",
            ]
            assert (
                run(command + [json.dumps({"op": "doctor"})])["response"]["status"]
                == "ready"
            )
            code = f"goto_url('http://127.0.0.1:{fixture.server_port}')\nwait_for_element('#name')\nfill_input('#name', 'WASM round trip')\njs(\"document.querySelector('button').click()\")\nprint(js(\"document.querySelector('#result').textContent\"))\ncapture_screenshot()"
            result = run(
                command
                + [
                    json.dumps(
                        {"code": code, "session": "integration", "timeout_s": 30}
                    )
                ]
            )
            assert result["response"]["status"] == "completed", result["response"]
            assert "WASM round trip" in result["response"]["stdout"], result["response"]
            assert result["parts"][0]["type"] == "image"
            failed = run(
                command
                + [
                    json.dumps(
                        {
                            "code": "import sys; print('x' * 5000, file=sys.stderr); raise RuntimeError('browser-recovery-probe')",
                            "session": "integration",
                        }
                    )
                ]
            )["response"]
            assert failed["status"] == "failed", failed
            assert failed["phase"] == "execution" and failed["code_executed"] is None, (
                failed
            )
            assert (
                failed["stderr_truncated"]
                and "browser-recovery-probe" in failed["stderr"]
            ), failed
            assert "Inspect" in failed["next"], failed
            assert (
                run(command + [json.dumps({"op": "close", "session": "integration"})])[
                    "response"
                ]["status"]
                == "closed"
            )
            run([args.gents, "pack", "remove", "gents/web", "--home", str(home)])
            print(
                "ok: idempotent setup, installed WASM, bound connection, host HTTP, Chromium, image output, close and removal"
            )
        finally:
            if started:
                run(worker + ["stop", "--state", str(state)])

                time.sleep(2)
            fixture.shutdown()
            fixture.server_close()
            thread.join(2)


if __name__ == "__main__":
    main()
