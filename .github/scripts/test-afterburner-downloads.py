#!/usr/bin/env python3
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest


class ArtifactHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_CONNECT(self):
        self.server.requests += 1
        self.send_response(500)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        self.server.requests += 1
        payload = self.server.payload
        if self.server.requests <= self.server.failures:
            self.send_response(503)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if self.server.partial and self.server.requests == 1:
            self.wfile.write(payload[:5])
        else:
            self.wfile.write(payload)


class ArtifactDownloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="gents downloads ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / ".curlrc").write_text("")
        self.server = HTTPServer(("127.0.0.1", 0), ArtifactHandler)
        self.server.payload = b"verified compiler artifact\n"
        self.server.requests = 0
        self.server.failures = 0
        self.server.partial = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.close_server)
        self.address = f"http://127.0.0.1:{self.server.server_port}"
        self.marker = self.root / "installed"
        self.output = self.root / "compiler archive"

    def close_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        self.assertFalse(self.thread.is_alive())

    def download(self, *, proxy=False, checksum=None):
        env = {key: value for key, value in os.environ.items()
               if not key.lower().endswith("_proxy")}
        env["NO_PROXY"] = "" if proxy else "*"
        env["CURL_HOME"] = str(self.root)
        if proxy:
            env["https_proxy"] = self.address
        url = "https://download.invalid/compiler" if proxy else self.address
        checksum = checksum or hashlib.sha256(self.server.payload).hexdigest()
        script = Path(__file__).with_name("setup-afterburner-toolchains.sh")
        return subprocess.run([
            "bash", "-euo", "pipefail", "-c",
            'source "$1"; download_verified "$2" "$3" "$4"; printf ready > "$5"',
            "download-test", str(script), url, checksum, str(self.output), str(self.marker),
        ], env=env, capture_output=True, text=True, timeout=30)

    def test_retries_proxy_connect_errors_with_a_finite_attempt_limit(self):
        result = self.download(proxy=True)
        self.assertEqual(self.server.requests, 4)
        self.assertIn(result.returncode, (22, 56), result.stderr)
        self.assertFalse(self.marker.exists())

    def test_recovers_after_transient_http_errors(self):
        self.server.failures = 2
        result = self.download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.server.requests, 3)
        self.assertEqual(self.output.read_bytes(), self.server.payload)
        self.assertTrue(self.marker.exists())

    def test_replaces_partial_output_before_verifying_the_retry(self):
        self.server.partial = True
        result = self.download()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.server.requests, 2)
        self.assertEqual(self.output.read_bytes(), self.server.payload)
        self.assertTrue(self.marker.exists())

    def test_checksum_mismatch_stops_before_installation(self):
        result = self.download(checksum="0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.server.requests, 1)
        self.assertFalse(self.marker.exists())


if __name__ == "__main__":
    unittest.main()
