"""Fresh pack install, native service startup, Chromium, and process-group cleanup."""

import argparse
import json
import os
import subprocess
import tempfile
import threading
import time
from http.server import ThreadingHTTPServer
from pathlib import Path

from live_browser import Fixture, worker


class LoggedInFixture(Fixture):
    def do_GET(self):
        if self.path == "/login":
            self.send_response(302)
            self.send_header("Set-Cookie", "session=fixture-login; HttpOnly; Path=/")
            self.send_header("Location", "/")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif self.path == "/private":
            allowed = "session=fixture-login" in self.headers.get("Cookie", "")
            body = (
                b"<title>Private fixture</title><p id='private'>Authenticated fixture</p>"
                if allowed
                else b"Login required"
            )
            self.send_response(200 if allowed else 401)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            super().do_GET()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gents", required=True)
    args = parser.parse_args()
    pack = Path(__file__).resolve().parents[2]
    fixture = ThreadingHTTPServer(("127.0.0.1", 0), LoggedInFixture)
    thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    thread.start()
    with tempfile.TemporaryDirectory(prefix="gents-native-service-") as root:
        home = Path(root) / "home"

        def run(*command, cdp=None):
            environment = {
                k: v for k, v in os.environ.items() if k != "GENTS_BROWSER_CDP"
            }
            if cdp:
                environment["GENTS_BROWSER_CDP"] = cdp
            result = subprocess.run(
                [args.gents, *command],
                capture_output=True,
                text=True,
                timeout=180,
                env=environment,
            )
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        try:
            run(
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
            )
            run("pack", "install", str(pack), "--home", str(home), "--grant-authority")
            command = [
                "plugin",
                "run",
                "gents/browser_exec",
                "--home",
                str(home),
                "--input",
            ]
            run(*command, json.dumps({"op": "help"}))
            assert not (home / "plugin-services").exists(), "Help started a service"
            code = f"goto_url('http://127.0.0.1:{fixture.server_port}')\nassert wait_for_element('#name')\nfill_input('#name', 'Native owner')\njs(\"document.querySelector('button').click()\")\nprint(js(\"document.querySelector('#result').textContent\"))"
            result = run(
                *command,
                json.dumps({"code": code, "session": "native", "timeout_s": 30}),
            )
            assert result["response"]["status"] == "completed", result
            assert "Native owner" in result["response"]["stdout"], result
            states = list((home / "plugin-services").glob("run-*/worker/worker.json"))
            assert len(states) == 1, states
            pgid = json.loads(states[0].read_text())["pgid"]
            assert pgid != os.getpgrp(), "Worker escaped native process ownership"
            for _ in range(100):
                try:
                    os.killpg(pgid, 0)
                except ProcessLookupError:
                    break
                except PermissionError:
                    pass
                time.sleep(0.05)
            else:
                raise AssertionError(
                    f"Service descendants survived native executor exit: {pgid}"
                )
            owner = worker.Session(Path(root), "user-browser")
            try:
                url = f"http://127.0.0.1:{fixture.server_port}"
                login = owner.execute(
                    f"goto_url({url + '/login'!r})\nassert wait_for_element('#name')\nfill_input('#name', 'Original user tab')",
                    30,
                    "login",
                )
                assert login["response"]["status"] == "completed", login
                result = run(
                    *command,
                    json.dumps(
                        {
                            "code": f"goto_url({url + '/private'!r})\nassert wait_for_element('#private')\nprint(js('document.body.innerText'))",
                            "session": "attached",
                            "timeout_s": 30,
                        }
                    ),
                    cdp=owner.cdp,
                )
                assert result["response"]["status"] == "completed", result
                assert "Authenticated fixture" in result["response"]["stdout"], result
                assert owner.browser.poll() is None, (
                    "Gents terminated the borrowed browser"
                )
                original = owner.execute(
                    "print(js(\"document.querySelector('#name').value\"))",
                    15,
                    "original",
                )
                assert original["response"]["status"] == "completed", original
                assert "Original user tab" in original["response"]["stdout"], original
            finally:
                owner.close()
            print(
                "PASS: installed WASM, native startup, Chromium interaction, process-group cleanup, attached login, original tab preserved"
            )
        finally:
            fixture.shutdown()
            fixture.server_close()
            thread.join(2)


if __name__ == "__main__":
    main()
