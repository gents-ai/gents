"""Tests for the Harbor adapter's post-run metadata projection.

Runs with the standard library only::

    python3 scripts/harbor/test_gents_agent.py

Harbor and certifi are stubbed before import so the adapter's metadata
contract can be exercised without a Harbor installation.
"""

from __future__ import annotations

import json
import logging
import sys
import tempfile
import types
import unittest
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[2]


def _stub_module(name: str, **attrs: object) -> None:
    module = types.ModuleType(name)
    for attr, value in attrs.items():
        setattr(module, attr, value)
    sys.modules.setdefault(name, module)


class _AgentContext:
    def __init__(self) -> None:
        self.metadata = None


_stub_module("certifi", where=lambda: "/nonexistent/ca-bundle.pem")
_stub_module("harbor")
_stub_module("harbor.agents")
_stub_module("harbor.agents.base", BaseAgent=object)
_stub_module("harbor.environments")
_stub_module("harbor.environments.base", BaseEnvironment=object)
_stub_module("harbor.models")
_stub_module("harbor.models.agent")
_stub_module("harbor.models.agent.context", AgentContext=_AgentContext)

sys.path.insert(0, str(_REPO_ROOT))

from scripts.harbor.gents_agent import GentsAgent  # noqa: E402

_TRAJECTORY = {
    "session_id": "session-1",
    "trajectory_id": "trajectory-1",
    "final_metrics": {"total_steps": 7},
}
_MAX_TURN_ERROR = (
    "agent stream failed: PromptError: MaxTurnError: (reached max turn limit: 250)"
)


class PopulateContextPostRunTest(unittest.TestCase):
    def _run(self, files: dict[str, object]) -> dict[str, object]:
        agent = GentsAgent.__new__(GentsAgent)
        agent.logger = logging.getLogger("test_gents_agent")
        with tempfile.TemporaryDirectory() as temp_dir:
            agent.logs_dir = Path(temp_dir)
            for name, payload in files.items():
                text = payload if isinstance(payload, str) else json.dumps(payload)
                (agent.logs_dir / name).write_text(text)
            context = _AgentContext()
            agent.populate_context_post_run(context)
            return ((context.metadata or {}).get("gents")) or {}

    def test_max_turn_exhaustion_is_identified(self) -> None:
        gents = self._run(
            {
                "trajectory.json": _TRAJECTORY,
                "request.json": {"request_id": "req-1"},
                "gents-outcome.json": {
                    "outcome": "max_turns_exhausted",
                    "response_status": "error",
                    "max_turns": 250,
                    "request_id": "req-1",
                },
                "response.json": {"status": "error", "error_message": _MAX_TURN_ERROR},
            }
        )
        self.assertEqual(gents.get("outcome"), "max_turns_exhausted")
        self.assertIs(gents.get("budget_exhausted"), True)
        self.assertEqual(gents.get("terminal_error"), _MAX_TURN_ERROR)
        self.assertEqual(gents.get("request_id"), "req-1")
        self.assertEqual(gents.get("total_steps"), 7)

    def test_completed_run_is_not_budget_exhausted(self) -> None:
        gents = self._run(
            {
                "trajectory.json": _TRAJECTORY,
                "request.json": {"request_id": "req-2"},
                "gents-outcome.json": {
                    "outcome": "completed",
                    "response_status": "complete",
                },
                "response.json": {"status": "complete", "error_message": None},
            }
        )
        self.assertEqual(gents.get("outcome"), "completed")
        self.assertIs(gents.get("budget_exhausted"), False)
        self.assertIsNone(gents.get("terminal_error"))

    def test_missing_outcome_artifacts_degrade_to_null(self) -> None:
        gents = self._run(
            {
                "trajectory.json": _TRAJECTORY,
                "request.json": {"request_id": "req-3"},
            }
        )
        self.assertIsNone(gents.get("outcome"))
        self.assertIs(gents.get("budget_exhausted"), False)
        self.assertIsNone(gents.get("terminal_error"))

    def test_corrupt_outcome_file_degrades_to_null(self) -> None:
        gents = self._run(
            {
                "trajectory.json": _TRAJECTORY,
                "request.json": {"request_id": "req-4"},
                "gents-outcome.json": '{"outcome": "max_turns_exhausted", oops',
                "response.json": {"status": "error", "error_message": _MAX_TURN_ERROR},
            }
        )
        self.assertIsNone(gents.get("outcome"))
        self.assertIs(gents.get("budget_exhausted"), False)
        self.assertEqual(gents.get("terminal_error"), _MAX_TURN_ERROR)


if __name__ == "__main__":
    unittest.main()
