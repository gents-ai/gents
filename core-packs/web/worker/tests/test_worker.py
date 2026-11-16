import importlib.util
import json
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

spec = importlib.util.spec_from_file_location(
    "worker", Path(__file__).parents[1] / "browser_worker.py"
)
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class FakeSession:
    def __init__(self, *args, **kwargs):
        self.lock = threading.Lock()
        self.last_used = worker.time.monotonic()
        self.release = threading.Event()
        self.closed = False
        self.live_browser = None
        self.selected_target = None

    def execute(self, code, timeout, job):
        self.release.wait(3)
        return {"response": {"status": "completed", "stdout": code}}

    def close(self):
        self.closed = True
        self.release.set()


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.worker = worker.Worker(Path(self.temp.name))
        self.fake = patch.object(worker, "Session", FakeSession)
        self.fake.start()

    def tearDown(self):
        self.worker.shutdown()
        self.fake.stop()
        self.temp.cleanup()

    def test_same_session_serialized_and_jobs_not_replayed(self):
        first = self.worker.call({"code": "first", "session": "a"})
        with self.assertRaisesRegex(ValueError, "busy"):
            self.worker.call({"code": "second", "session": "a"})
        second = self.worker.call({"code": "other", "session": "b"})
        self.assertNotEqual(first["job"], second["job"])
        self.worker.sessions["a"].release.set()
        record = self.worker.jobs[first["job"]]
        self.assertTrue(record["done"].wait(2))
        self.assertEqual(record["result"]["response"]["stdout"], "first")
        self.assertEqual(
            self.worker.call({"op": "close", "session": "a"})["response"]["status"],
            "closed",
        )
        self.assertIn("b", self.worker.sessions)

    def test_inputs_do_not_create_resources(self):
        for value in [
            {"code": "x", "session": "../escape"},
            {"code": "x", "timeout_s": True},
            {"code": "x", "timeout_s": 301},
            {"code": ""},
            {"op": "arbitrary"},
        ]:
            with self.assertRaises(ValueError):
                self.worker.call(value)
        self.assertEqual(self.worker.sessions, {})

    def test_failed_job_thread_start_releases_session(self):
        with patch.object(
            worker.threading.Thread,
            "start",
            side_effect=RuntimeError("cannot start new thread"),
        ):
            with self.assertRaisesRegex(RuntimeError, "cannot start new thread"):
                self.worker.call({"code": "first", "session": "a"})
        self.assertEqual(self.worker.jobs, {})
        self.assertFalse(self.worker.sessions["a"].lock.locked())
        self.worker.sessions["a"].release.set()
        retry = self.worker.call({"code": "retry", "session": "a"})
        self.assertTrue(self.worker.jobs[retry["job"]]["done"].wait(1))

    def test_closing_one_session_does_not_block_another(self):
        self.worker.sessions["closing"] = FakeSession()
        closing, release, admitted = (
            threading.Event(),
            threading.Event(),
            threading.Event(),
        )

        def slow_close():
            closing.set()
            release.wait(3)

        self.worker.sessions["closing"].close = slow_close
        closer = threading.Thread(
            target=lambda: self.worker.call({"op": "close", "session": "closing"})
        )

        def admit():
            self.worker.call({"code": "other", "session": "other"})
            admitted.set()

        closer.start()
        self.assertTrue(closing.wait(1))
        other = threading.Thread(target=admit)
        other.start()
        try:
            self.assertTrue(admitted.wait(1), "Closing held the global admission lock")
        finally:
            release.set()
            closer.join(2)
            other.join(2)

    def test_http_auth_origin_and_polling(self):
        server = worker.ThreadingHTTPServer(
            ("127.0.0.1", 0), worker.handler(self.worker, "secret")
        )
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        origin = f"http://127.0.0.1:{server.server_port}"
        try:
            for headers in [
                {},
                {"Authorization": "Bearer incorrect"},
                {"Authorization": "Bearer secret", "Origin": "https://hostile.example"},
            ]:
                with self.assertRaises(HTTPError) as error:
                    urlopen(Request(origin + "/health", headers=headers), timeout=2)
                self.assertEqual(error.exception.code, 403)
                denied = json.load(error.exception)["response"]
                self.assertEqual(denied["phase"], "authorization")
                self.assertIsNone(denied["code_executed"])
                error.exception.close()
            conn = {"endpoint": origin, "token": "secret"}
            for html, expected in [
                ("漢" * 180000, "completed"),
                ('"' * 600000, "output_too_large"),
            ]:
                payload = {
                    "response": {
                        "status": "completed",
                        "page": {"html": html},
                        "session": "read-test",
                    }
                }
                with patch.object(self.worker, "call", return_value=payload):
                    response = worker.request(conn, "/health")["response"]
                self.assertEqual(response["status"], expected)
                if expected == "output_too_large":
                    self.assertEqual(response["phase"], "output")
                    self.assertEqual(response["session"], "read-test")
            with self.assertRaises(HTTPError) as error:
                worker.request(conn, "/jobs/missing")
            missing = json.load(error.exception)["response"]
            self.assertEqual(missing["phase"], "polling")
            self.assertIsNone(missing["code_executed"])
            error.exception.close()
            job = worker.request(conn, "/call", {"code": "ok"})
            self.worker.sessions["default"].release.set()
            result = worker.request(conn, "/jobs/" + job["job"])
            self.assertEqual(result["response"]["stdout"], "ok")
            self.assertEqual(worker.request(conn, "/jobs/" + job["job"]), result)
        finally:
            server.shutdown()
            server.server_close()
            thread.join(2)


class StartupTests(unittest.TestCase):
    def test_binding_failure_preserves_healthy_worker_connection(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "connection.json"
            original = '{"endpoint":"http://127.0.0.1:18765","token":"existing"}'
            path.write_text(original)
            argv = [
                "worker",
                "setup",
                "--state",
                root,
                "--allow-host-execution",
                "--gents-home",
                root,
            ]
            with (
                patch.object(worker.sys, "argv", argv),
                patch.object(worker, "request", return_value={}),
                patch.object(
                    worker.subprocess,
                    "run",
                    side_effect=FileNotFoundError("gents unavailable"),
                ),
                patch.object(worker.subprocess, "Popen") as launch,
            ):
                with self.assertRaisesRegex(FileNotFoundError, "gents unavailable"):
                    worker.main()
                launch.assert_not_called()
            self.assertEqual(path.read_text(), original)

    def test_allocation_does_not_launch_and_timeout_never_executes_code(self):
        with (
            tempfile.TemporaryDirectory() as root,
            patch.object(worker.subprocess, "Popen") as launch,
            patch.object(worker, "chrome_binary", return_value="chrome"),
            patch.object(worker, "terminate") as terminate,
            patch.object(worker, "STARTUP_TIMEOUT", 0),
        ):
            session = worker.Session(Path(root), "cold")
            self.assertFalse(
                launch.called, "HTTP session allocation must not launch Chrome"
            )
            proc = Mock()
            launch.return_value = proc
            result = session.execute(
                "raise AssertionError('must not execute')", 1, "probe"
            )
            self.assertEqual(result["response"]["status"], "startup_failed")
            self.assertFalse(result["response"]["code_executed"])
            self.assertEqual(
                launch.call_count,
                1,
                "Only Chrome, never the model interpreter, may start",
            )
            terminate.assert_called_with(proc)
            self.assertIsNone(session.browser)
            session.runtime.cleanup()

    def test_delayed_start_does_not_block_health_or_another_job(self):
        started, release = threading.Event(), threading.Event()

        def execute(session, code, timeout, job):
            if code == "slow":
                started.set()
                release.wait(3)
            return {"response": {"status": "completed"}}

        with (
            tempfile.TemporaryDirectory() as root,
            patch.object(worker.Session, "execute", execute),
            patch.object(worker.Session, "close", lambda s: s.runtime.cleanup()),
            patch.object(worker.importlib.metadata, "version", return_value="test"),
        ):
            instance = worker.Worker(Path(root))
            try:
                first = instance.call({"code": "slow", "session": "cold"})
                self.assertTrue(started.wait(1))
                self.assertEqual(
                    instance.call({"op": "doctor"})["response"]["status"], "ready"
                )
                second = instance.call({"code": "fast", "session": "warm"})
                self.assertTrue(instance.jobs[second["job"]]["done"].wait(1))
            finally:
                release.set()
                instance.jobs[first["job"]]["done"].wait(1)
                instance.shutdown()


if __name__ == "__main__":
    unittest.main()
