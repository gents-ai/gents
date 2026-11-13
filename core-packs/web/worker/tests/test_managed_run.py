import sys
import unittest
from pathlib import Path
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from managed_run import pin_requests


class PinnedRequestsTests(unittest.TestCase):
    def test_stale_session_fails_without_replay(self):
        requests = []

        def send(req, **kwargs):
            requests.append(req)
            if req.get("meta") == "session":
                return {"session_id": "closed-session"}
            if req.get("session_id") == "closed-session":
                raise RuntimeError("Session with given id not found")
            self.fail("An unpinned command could be replayed on another tab")

        with self.assertRaisesRegex(RuntimeError, "Session with given id not found"):
            pin_requests(send)(
                {"method": "Runtime.evaluate", "params": {"expression": "submit()"}}
            )
        self.assertEqual(len(requests), 2)

    def test_explicit_switches_are_respected(self):
        send = Mock(
            side_effect=[{"session_id": "first"}, {}, {"session_id": "second"}, {}]
        )
        pinned = pin_requests(send)
        pinned({"method": "Page.navigate"})
        pinned({"method": "Page.navigate"})
        self.assertEqual(send.call_args_list[1].args[0]["session_id"], "first")
        self.assertEqual(send.call_args_list[3].args[0]["session_id"], "second")

    def test_explicit_sessions_and_browser_commands_pass_through(self):
        send = Mock(return_value={})
        pinned = pin_requests(send)
        for req in (
            {"method": "Target.getTargets"},
            {"meta": "session"},
            {"method": "Runtime.evaluate", "session_id": "iframe"},
        ):
            pinned(req, response_timeout=12)
            send.assert_called_with(req, response_timeout=12)
        self.assertEqual(send.call_count, 3)

    def test_missing_session_does_not_send_action(self):
        send = Mock(return_value={})
        with self.assertRaisesRegex(RuntimeError, "select a target_id"):
            pin_requests(send)({"method": "Page.navigate"})
        send.assert_called_once_with({"meta": "session"})
