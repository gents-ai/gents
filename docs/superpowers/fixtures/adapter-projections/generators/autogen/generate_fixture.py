#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import importlib.metadata
import json
import os
from pathlib import Path
from typing import Any, Sequence

from autogen_agentchat.agents import BaseChatAgent
from autogen_agentchat.base import Response, TaskResult
from autogen_agentchat.conditions import MaxMessageTermination, TextMentionTermination
from autogen_agentchat.messages import BaseChatMessage, TextMessage
from autogen_agentchat.teams import RoundRobinGroupChat
from autogen_core import CancellationToken


CONTEXT_ID = "context-autogen-docker-1"
REQUEST_ID = "req-autogen-docker-1"
CHILD_REQUEST_ID = "req-autogen-child-docker-1"
TASK_TEXT = "Map a multi-agent task to Defra Agent projection fields."


AGENTS = [
    {
        "name": "planner",
        "role": "planner",
        "agent_did": "did:defra-agent:autogen-planner",
        "behavior_id": "autogen.planner",
        "description": "Plans the task and delegates research.",
        "content": (
            "PLAN: delegate adapter comparison to researcher; "
            f"child_request={CHILD_REQUEST_ID}"
        ),
    },
    {
        "name": "researcher",
        "role": "researcher",
        "agent_did": "did:defra-agent:autogen-researcher",
        "behavior_id": "autogen.researcher",
        "description": "Completes delegated research.",
        "content": (
            f"RESEARCH: completed child request {CHILD_REQUEST_ID} "
            "with interoperability findings"
        ),
    },
    {
        "name": "reviewer",
        "role": "reviewer",
        "agent_did": "did:defra-agent:autogen-reviewer",
        "behavior_id": "autogen.reviewer",
        "description": "Reviews and approves outputs.",
        "content": "APPROVE: final multi-agent projection output is ready",
    },
]


class ScriptedAgent(BaseChatAgent):
    def __init__(self, definition: dict[str, str]) -> None:
        super().__init__(definition["name"], description=definition["description"])
        self._content = definition["content"]
        self._history: list[BaseChatMessage] = []

    @property
    def produced_message_types(self) -> Sequence[type[BaseChatMessage]]:
        return (TextMessage,)

    async def on_messages(
        self,
        messages: Sequence[BaseChatMessage],
        cancellation_token: CancellationToken,
    ) -> Response:
        self._history.extend(messages)
        message = TextMessage(content=self._content, source=self.name)
        self._history.append(message)
        return Response(chat_message=message)

    async def on_reset(self, cancellation_token: CancellationToken) -> None:
        self._history.clear()


async def run_autogen_capture() -> TaskResult:
    agents = [ScriptedAgent(definition) for definition in AGENTS]
    termination = TextMentionTermination("APPROVE") | MaxMessageTermination(4)
    team = RoundRobinGroupChat(agents, termination_condition=termination)
    return await team.run(task=TASK_TEXT)


def message_to_json(index: int, message: BaseChatMessage) -> dict[str, Any]:
    return {
        "index": index,
        "type": getattr(message, "type", type(message).__name__),
        "source": message.source,
        "content": message.content,
        "models_usage": to_jsonable(getattr(message, "models_usage", None)),
    }


def to_jsonable(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        return {str(key): to_jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [to_jsonable(item) for item in value]
    if hasattr(value, "model_dump"):
        return to_jsonable(value.model_dump())
    if hasattr(value, "_asdict"):
        return to_jsonable(value._asdict())
    return str(value)


def autogen_version() -> str:
    return importlib.metadata.version("autogen-agentchat")


def build_projection(result: TaskResult) -> dict[str, Any]:
    native_messages = [
        message_to_json(index, message) for index, message in enumerate(result.messages)
    ]
    return {
        "projection_id": "multi_agent_task",
        "projection_version": "v1",
        "source_request_id": REQUEST_ID,
        "source_session_id": CONTEXT_ID,
        "source_agent_did": "did:defra-agent:autogen-team",
        "source_behavior_id": "autogen.round_robin_team",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:autogen-fixture-reader",
        },
        "output": {
            "adapter": "multi_agent_task",
            "projection": {
                "task_id": REQUEST_ID,
                "context_id": CONTEXT_ID,
                "status": task_status(result.stop_reason),
                "participants": [
                    {
                        "agent_did": definition["agent_did"],
                        "behavior_id": definition["behavior_id"],
                        "role": definition["role"],
                    }
                    for definition in AGENTS
                ],
                "messages": project_messages(native_messages),
                "delegations": [
                    {
                        "parent_request_id": REQUEST_ID,
                        "child_request_id": CHILD_REQUEST_ID,
                        "parent_tool_call_id": "autogen:handoff:planner-to-researcher",
                        "agent_did": "did:defra-agent:autogen-researcher",
                        "behavior_id": "autogen.researcher",
                        "status": "completed",
                    }
                ],
                "tool_events": [
                    {
                        "id": "autogen:event:handoff:planner-to-researcher",
                        "request_id": REQUEST_ID,
                        "tool_name": "handoff",
                        "status": "completed",
                        "child_request_id": CHILD_REQUEST_ID,
                    },
                    {
                        "id": "autogen:event:review:reviewer-approval",
                        "request_id": CHILD_REQUEST_ID,
                        "tool_name": "review",
                        "status": "completed",
                    },
                ],
            },
        },
    }


def task_status(stop_reason: str | None) -> str:
    if stop_reason and "APPROVE" in stop_reason:
        return "completed"
    return "stopped"


def project_messages(native_messages: list[dict[str, Any]]) -> list[dict[str, str]]:
    projected = []
    for message in native_messages:
        source = message["source"]
        request_id = CHILD_REQUEST_ID if source == "researcher" else REQUEST_ID
        projected.append(
            {
                "id": f"autogen:message:{message['index']}:{source}",
                "request_id": request_id,
                "role": source,
                "content": str(message["content"]),
            }
        )
    return projected


def write_fixture(out_dir: Path, result: TaskResult) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    native_messages = [
        message_to_json(index, message) for index, message in enumerate(result.messages)
    ]
    payload = {
        "source": {
            "system": "autogen-agentchat",
            "package": "autogen-agentchat",
            "package_version": autogen_version(),
            "generator": "adapter-projections/generators/autogen",
            "capture": os.environ.get("DEFRA_FIXTURE_CAPTURE", "local"),
            "api": ["BaseChatAgent", "RoundRobinGroupChat", "TaskResult"],
        },
        "native": {
            "team": {
                "type": "RoundRobinGroupChat",
                "participants": [
                    {
                        "name": definition["name"],
                        "role": definition["role"],
                        "description": definition["description"],
                    }
                    for definition in AGENTS
                ],
                "termination": ["TextMentionTermination(APPROVE)", "MaxMessageTermination(4)"],
            },
            "task": TASK_TEXT,
            "stop_reason": result.stop_reason,
            "messages": native_messages,
        },
        "envelope": build_projection(result),
    }
    path = out_dir / "multi_agent_task.autogen.capture.json"
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


async def async_main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=Path("/out"))
    args = parser.parse_args()

    result = await run_autogen_capture()
    path = write_fixture(args.out_dir, result)
    print(path)


def main() -> None:
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
