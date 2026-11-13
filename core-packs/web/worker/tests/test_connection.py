import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import browser_connection
import browser_worker


class ConnectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.environment = patch.dict(
            os.environ, {"GENTS_BROWSER_ROOTS": json.dumps({"fixture": str(self.root)})}
        )
        self.environment.start()
        self.worker = browser_worker.Worker(self.root / "worker")

    def tearDown(self):
        self.worker.shutdown()
        self.environment.stop()
        self.temp.cleanup()

    def test_discovery_is_passive_and_missing_connection_never_launches(self):
        with patch.object(browser_worker, "Session") as launch:
            found = self.worker.call({"op": "discover"})["response"]
            self.assertEqual(
                found["browsers"], [{"browser": "fixture", "status": "setup_required"}]
            )
            result = self.worker.call({"op": "connect", "browser": "fixture"})[
                "response"
            ]
            self.assertEqual(result["status"], "connection_required")
            self.assertFalse(result["code_executed"])
            self.assertIn("Allow", result["next"])
            launch.assert_not_called()
            self.assertEqual(self.worker.sessions, {})
            retry = self.worker.call({"code": "print('must not run')"})["response"]
            self.assertEqual(retry["status"], "connection_required")
            launch.assert_not_called()

    def test_endpoint_validation_and_permission_diagnosis(self):
        active = self.root / "DevToolsActivePort"
        active.write_text("9321\n/devtools/browser/example-id\n")
        self.assertEqual(
            browser_connection.resolve("fixture"),
            "ws://127.0.0.1:9321/devtools/browser/example-id",
        )
        result = browser_connection.discover()
        self.assertEqual(result["browsers"][0]["status"], "available")
        self.assertNotIn("example-id", json.dumps(result))
        for value in (
            "0\n/devtools/browser/id",
            "65536\n/devtools/browser/id",
            "9222\n//remote/host",
            "9222\n/devtools/browser/x?token=secret",
        ):
            active.write_text(value)
            with self.assertRaises(ValueError):
                browser_connection.resolve("fixture")
        with patch.object(
            browser_connection, "endpoint", side_effect=PermissionError()
        ):
            self.assertEqual(
                browser_connection.discover()["browsers"][0]["status"], "access_denied"
            )

    def test_live_session_requires_connection_and_explicit_tab(self):
        session = browser_worker.Session(
            self.root,
            "live",
            "ws://127.0.0.1:9/devtools/browser/id",
            live_browser="fixture",
        )
        self.worker.sessions["live"] = session
        with self.assertRaisesRegex(ValueError, "has not completed"):
            self.worker.call({"session": "live", "code": "print(1)"})
        session.connected = True
        for op in ("exec", "read"):
            with self.assertRaisesRegex(ValueError, "Choose target_id"):
                self.worker.call({"op": op, "session": "live", "code": "print(1)"})
        self.assertEqual(self.worker.jobs, {})
        with patch.object(
            browser_connection,
            "resolve",
            return_value="ws://127.0.0.1:9/devtools/browser/other",
        ):
            with self.assertRaisesRegex(ValueError, "different browser"):
                self.worker.call(
                    {"op": "connect", "session": "live", "browser": "other"}
                )
        self.assertEqual(session.cdp, "ws://127.0.0.1:9/devtools/browser/id")
        self.worker.live_bindings["live"] = "fixture"
        self.worker.close_session("live")
        with patch.object(browser_worker, "Session") as launch:
            retry = self.worker.call(
                {"op": "exec", "session": "live", "code": "print(1)"}
            )
            self.assertEqual(retry["response"]["status"], "connection_required")
            launch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
