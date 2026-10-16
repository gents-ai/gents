"""Real browser smoke test. Uses only a temporary profile and fixture website."""

import base64
import importlib.util
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

spec = importlib.util.spec_from_file_location(
    "worker", Path(__file__).parents[1] / "browser_worker.py"
)
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        body = b"""<!doctype html><title>Browser fixture</title>
        <label>Name <input id="name"></label><button onclick="document.querySelector('#result').textContent=document.querySelector('#name').value;localStorage.setItem('saved',document.querySelector('#name').value)">Save</button><p id="result"></p>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    with tempfile.TemporaryDirectory(prefix="gents-browser-test-") as root:
        session = worker.Session(Path(root), "smoke")
        try:
            url = f"http://127.0.0.1:{server.server_port}"
            first = session.execute(
                f"goto_url({url!r})\nwait_for_element('#name')\nfill_input('#name', 'Gents')\njs(\"document.querySelector('button').click()\")\nprint(js(\"document.querySelector('#result').textContent\"))\ncapture_screenshot()",
                30,
                "first",
            )
            assert first["response"]["status"] == "completed", first
            assert "Gents" in first["response"]["stdout"], first
            assert first["parts"] and base64.b64decode(
                first["parts"][0]["data"]
            ).startswith(b"\xff\xd8"), first
            second = session.execute(
                "print(js(\"localStorage.getItem('saved')\"))\nfrom pathlib import Path\nPath('agent_helpers.py').write_text('def saved_marker():\\n    return \\\"workspace survived\\\"\\n')",
                15,
                "second",
            )
            assert (
                second["response"]["status"] == "completed"
                and "Gents" in second["response"]["stdout"]
            ), second
            timeout = session.execute("import time; time.sleep(10)", 1, "timeout")
            assert timeout["response"]["status"] == "timeout", timeout
            recovery = session.execute(
                "print(js('document.title'))\nprint(saved_marker())", 15, "recovery"
            )
            assert (
                recovery["response"]["status"] == "completed"
                and "Browser fixture" in recovery["response"]["stdout"]
            ), recovery
            assert "workspace survived" in recovery["response"]["stdout"], recovery
            attached = worker.Session(
                Path(root), "borrowed", cdp=session.env["BU_CDP_URL"]
            )
            try:
                borrowed = attached.execute(
                    f"goto_url({url!r})\nwait_for_element('#name')\nprint(js('document.title'))",
                    15,
                    "borrowed",
                )
                assert borrowed["response"]["status"] == "completed", borrowed

                subprocess.run(
                    [sys.executable, "-m", "browser_harness.run", "--reload"],
                    env=attached.env,
                    check=True,
                    capture_output=True,
                    timeout=15,
                )
                restarted = attached.execute(
                    "print(js('location.href'))", 20, "restarted"
                )
                assert restarted["response"]["status"] == "completed", restarted
                assert "about:blank" in restarted["response"]["stdout"], restarted
            finally:
                attached.close()
            assert session.browser.poll() is None, (
                "Closing an attachment killed the borrowed browser"
            )
            original = session.execute(
                "print(js(\"document.querySelector('#result').textContent\"))",
                15,
                "original",
            )
            assert "Gents" in original["response"]["stdout"], original
            print(
                "ok: real Chromium form, persistent state/helpers, image result, timeout recovery, attachment and driver-restart tab ownership"
            )
        finally:
            session.close()
            server.shutdown()
            server.server_close()
            thread.join(2)


if __name__ == "__main__":
    main()
