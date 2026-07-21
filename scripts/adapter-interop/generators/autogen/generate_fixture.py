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
from autogen_agentchat.messages import BaseChatMessage, HandoffMessage, TextMessage
from autogen_agentchat.teams import RoundRobinGroupChat, Swarm
from autogen_core import CancellationToken


CONTEXT_ID = "context-autogen-docker-1"
REQUEST_ID = "req-autogen-docker-1"
CHILD_REQUEST_ID = "req-autogen-child-docker-1"
TASK_TEXT = "Map a multi-agent task to Gents projection fields."

SWARM_CONTEXT_ID = "context-autogen-swarm-docker-1"
SWARM_REQUEST_ID = "req-autogen-swarm-docker-1"
SWARM_RESEARCH_REQUEST_ID = "req-autogen-swarm-research-docker-1"
SWARM_REVIEW_REQUEST_ID = "req-autogen-swarm-review-docker-1"
SWARM_TASK_TEXT = "Route a multi-agent task through explicit AutoGen Swarm handoffs."


AGENTS = [
    {
        "name": "planner",
        "role": "planner",
        "agent_did": "did:test:autogen-planner",
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
        "agent_did": "did:test:autogen-researcher",
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
        "agent_did": "did:test:autogen-reviewer",
        "behavior_id": "autogen.reviewer",
        "description": "Reviews and approves outputs.",
        "content": "APPROVE: final multi-agent projection output is ready",
    },
]

SWARM_AGENTS = [
    {
        "name": "planner",
        "role": "planner",
        "agent_did": "did:test:autogen-swarm-planner",
        "behavior_id": "autogen.swarm.planner",
        "description": "Plans the task and hands off research.",
        "message_type": "handoff",
        "target": "researcher",
        "content": (
            "HANDOFF: planner delegates interop research to researcher; "
            f"child_request={SWARM_RESEARCH_REQUEST_ID}"
        ),
    },
    {
        "name": "researcher",
        "role": "researcher",
        "agent_did": "did:test:autogen-swarm-researcher",
        "behavior_id": "autogen.swarm.researcher",
        "description": "Completes research and hands off review.",
        "message_type": "handoff",
        "target": "reviewer",
        "content": (
            "HANDOFF: researcher completed findings and requests review; "
            f"child_request={SWARM_REVIEW_REQUEST_ID}"
        ),
    },
    {
        "name": "reviewer",
        "role": "reviewer",
        "agent_did": "did:test:autogen-swarm-reviewer",
        "behavior_id": "autogen.swarm.reviewer",
        "description": "Reviews the delegated task and approves it.",
        "message_type": "text",
        "content": "APPROVE: Swarm handoff projection output is ready",
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


class SwarmScriptedAgent(BaseChatAgent):
    def __init__(self, definition: dict[str, str]) -> None:
        super().__init__(definition["name"], description=definition["description"])
        self._definition = definition
        self._history: list[BaseChatMessage] = []

    @property
    def produced_message_types(self) -> Sequence[type[BaseChatMessage]]:
        return (TextMessage, HandoffMessage)

    async def on_messages(
        self,
        messages: Sequence[BaseChatMessage],
        cancellation_token: CancellationToken,
    ) -> Response:
        self._history.extend(messages)
        if self._definition["message_type"] == "handoff":
            message = HandoffMessage(
                content=self._definition["content"],
                source=self.name,
                target=self._definition["target"],
            )
        else:
            message = TextMessage(content=self._definition["content"], source=self.name)
        self._history.append(message)
        return Response(chat_message=message)

    async def on_reset(self, cancellation_token: CancellationToken) -> None:
        self._history.clear()


async def run_autogen_capture() -> TaskResult:
    agents = [ScriptedAgent(definition) for definition in AGENTS]
    termination = TextMentionTermination("APPROVE") | MaxMessageTermination(4)
    team = RoundRobinGroupChat(agents, termination_condition=termination)
    return await team.run(task=TASK_TEXT)


async def run_autogen_swarm_capture() -> TaskResult:
    agents = [SwarmScriptedAgent(definition) for definition in SWARM_AGENTS]
    termination = TextMentionTermination("APPROVE") | MaxMessageTermination(6)
    team = Swarm(agents, termination_condition=termination)
    return await team.run(task=SWARM_TASK_TEXT)


def message_to_json(index: int, message: BaseChatMessage) -> dict[str, Any]:
    return {
        "index": index,
        "type": getattr(message, "type", type(message).__name__),
        "source": message.source,
        "target": getattr(message, "target", None),
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
        "source_agent_did": "did:test:autogen-team",
        "source_behavior_id": "autogen.round_robin_team",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "gents",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:test:autogen-fixture-reader",
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
                        "agent_did": "did:test:autogen-researcher",
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


def build_swarm_projection(result: TaskResult) -> dict[str, Any]:
    native_messages = [
        message_to_json(index, message) for index, message in enumerate(result.messages)
    ]
    return {
        "projection_id": "multi_agent_task",
        "projection_version": "v1",
        "source_request_id": SWARM_REQUEST_ID,
        "source_session_id": SWARM_CONTEXT_ID,
        "source_agent_did": "did:test:autogen-swarm-team",
        "source_behavior_id": "autogen.swarm_team",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "gents",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:test:autogen-fixture-reader",
        },
        "output": {
            "adapter": "multi_agent_task",
            "projection": {
                "task_id": SWARM_REQUEST_ID,
                "context_id": SWARM_CONTEXT_ID,
                "status": task_status(result.stop_reason),
                "participants": [
                    {
                        "agent_did": definition["agent_did"],
                        "behavior_id": definition["behavior_id"],
                        "role": definition["role"],
                    }
                    for definition in SWARM_AGENTS
                ],
                "messages": project_swarm_messages(native_messages),
                "delegations": [
                    {
                        "parent_request_id": SWARM_REQUEST_ID,
                        "child_request_id": SWARM_RESEARCH_REQUEST_ID,
                        "parent_tool_call_id": "autogen:swarm:handoff:planner-to-researcher",
                        "agent_did": "did:test:autogen-swarm-researcher",
                        "behavior_id": "autogen.swarm.researcher",
                        "status": "completed",
                    },
                    {
                        "parent_request_id": SWARM_RESEARCH_REQUEST_ID,
                        "child_request_id": SWARM_REVIEW_REQUEST_ID,
                        "parent_tool_call_id": "autogen:swarm:handoff:researcher-to-reviewer",
                        "agent_did": "did:test:autogen-swarm-reviewer",
                        "behavior_id": "autogen.swarm.reviewer",
                        "status": "completed",
                    },
                ],
                "tool_events": [
                    {
                        "id": "autogen:swarm:event:handoff:planner-to-researcher",
                        "request_id": SWARM_REQUEST_ID,
                        "tool_name": "handoff",
                        "status": "completed",
                        "child_request_id": SWARM_RESEARCH_REQUEST_ID,
                    },
                    {
                        "id": "autogen:swarm:event:handoff:researcher-to-reviewer",
                        "request_id": SWARM_RESEARCH_REQUEST_ID,
                        "tool_name": "handoff",
                        "status": "completed",
                        "child_request_id": SWARM_REVIEW_REQUEST_ID,
                    },
                    {
                        "id": "autogen:swarm:event:review:reviewer-approval",
                        "request_id": SWARM_REVIEW_REQUEST_ID,
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


def project_swarm_messages(native_messages: list[dict[str, Any]]) -> list[dict[str, str]]:
    projected = []
    for message in native_messages:
        source = message["source"]
        if source == "researcher":
            request_id = SWARM_RESEARCH_REQUEST_ID
        elif source == "reviewer":
            request_id = SWARM_REVIEW_REQUEST_ID
        else:
            request_id = SWARM_REQUEST_ID
        projected.append(
            {
                "id": f"autogen:swarm:message:{message['index']}:{source}",
                "request_id": request_id,
                "role": source,
                "content": str(message["content"]),
            }
        )
    return projected


def write_fixture_file(
    out_dir: Path,
    filename: str,
    source_api: list[str],
    native: dict[str, Any],
    mapping: dict[str, Any],
    envelope: dict[str, Any],
) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "source": {
            "system": "autogen-agentchat",
            "package": "autogen-agentchat",
            "package_version": autogen_version(),
            "generator": "adapter-projections/generators/autogen",
            "capture": os.environ.get("GENTS_FIXTURE_CAPTURE", "local"),
            "api": source_api,
        },
        "native": native,
        "mapping": mapping,
        "envelope": envelope,
    }
    path = out_dir / filename
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def write_fixture(out_dir: Path, result: TaskResult) -> Path:
    native_messages = [
        message_to_json(index, message) for index, message in enumerate(result.messages)
    ]
    return write_fixture_file(
        out_dir,
        "multi_agent_task.autogen.capture.json",
        ["BaseChatAgent", "RoundRobinGroupChat", "TaskResult"],
        {
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
        {
            "projection": "multi_agent_task",
            "scenario_id": "autogen.round_robin_group_chat",
            "request_id": REQUEST_ID,
            "session_id": CONTEXT_ID,
            "agent_did": "did:test:autogen-team",
            "behavior_id": "autogen.round_robin_team",
            "actor_did": "did:test:autogen-fixture-reader",
            "status": task_status(result.stop_reason),
            "participants": [
                {
                    "native_name": definition["name"],
                    "role": definition["role"],
                    "agent_did": definition["agent_did"],
                    "behavior_id": definition["behavior_id"],
                    "request_id": (
                        CHILD_REQUEST_ID
                        if definition["name"] == "researcher"
                        else REQUEST_ID
                    ),
                }
                for definition in AGENTS
            ],
            "delegations": [
                {
                    "parent_request_id": REQUEST_ID,
                    "child_request_id": CHILD_REQUEST_ID,
                    "parent_tool_call_id": "autogen:handoff:planner-to-researcher",
                    "tool_name": "handoff",
                    "agent_did": "did:test:autogen-researcher",
                    "behavior_id": "autogen.researcher",
                    "status": "completed",
                }
            ],
            "tool_events": [
                {
                    "id": "autogen:event:review:reviewer-approval",
                    "request_id": CHILD_REQUEST_ID,
                    "tool_name": "review",
                    "status": "completed",
                }
            ],
        },
        build_projection(result),
    )


def write_swarm_fixture(out_dir: Path, result: TaskResult) -> Path:
    native_messages = [
        message_to_json(index, message) for index, message in enumerate(result.messages)
    ]
    return write_fixture_file(
        out_dir,
        "multi_agent_task.autogen_swarm.capture.json",
        ["BaseChatAgent", "Swarm", "HandoffMessage", "TaskResult"],
        {
            "team": {
                "type": "Swarm",
                "participants": [
                    {
                        "name": definition["name"],
                        "role": definition["role"],
                        "description": definition["description"],
                    }
                    for definition in SWARM_AGENTS
                ],
                "termination": ["TextMentionTermination(APPROVE)", "MaxMessageTermination(6)"],
            },
            "task": SWARM_TASK_TEXT,
            "stop_reason": result.stop_reason,
            "messages": native_messages,
        },
        {
            "projection": "multi_agent_task",
            "scenario_id": "autogen.swarm",
            "request_id": SWARM_REQUEST_ID,
            "session_id": SWARM_CONTEXT_ID,
            "agent_did": "did:test:autogen-swarm-team",
            "behavior_id": "autogen.swarm_team",
            "actor_did": "did:test:autogen-fixture-reader",
            "status": task_status(result.stop_reason),
            "participants": [
                {
                    "native_name": definition["name"],
                    "role": definition["role"],
                    "agent_did": definition["agent_did"],
                    "behavior_id": definition["behavior_id"],
                    "request_id": (
                        SWARM_RESEARCH_REQUEST_ID
                        if definition["name"] == "researcher"
                        else SWARM_REVIEW_REQUEST_ID
                        if definition["name"] == "reviewer"
                        else SWARM_REQUEST_ID
                    ),
                }
                for definition in SWARM_AGENTS
            ],
            "delegations": [
                {
                    "parent_request_id": SWARM_REQUEST_ID,
                    "child_request_id": SWARM_RESEARCH_REQUEST_ID,
                    "parent_tool_call_id": "autogen:swarm:handoff:planner-to-researcher",
                    "tool_name": "handoff",
                    "agent_did": "did:test:autogen-swarm-researcher",
                    "behavior_id": "autogen.swarm.researcher",
                    "status": "completed",
                },
                {
                    "parent_request_id": SWARM_RESEARCH_REQUEST_ID,
                    "child_request_id": SWARM_REVIEW_REQUEST_ID,
                    "parent_tool_call_id": "autogen:swarm:handoff:researcher-to-reviewer",
                    "tool_name": "handoff",
                    "agent_did": "did:test:autogen-swarm-reviewer",
                    "behavior_id": "autogen.swarm.reviewer",
                    "status": "completed",
                },
            ],
            "tool_events": [
                {
                    "id": "autogen:swarm:event:review:reviewer-approval",
                    "request_id": SWARM_REVIEW_REQUEST_ID,
                    "tool_name": "review",
                    "status": "completed",
                }
            ],
        },
        build_swarm_projection(result),
    )


async def async_main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=Path("/out"))
    args = parser.parse_args()

    round_robin_result = await run_autogen_capture()
    swarm_result = await run_autogen_swarm_capture()
    for path in [
        write_fixture(args.out_dir, round_robin_result),
        write_swarm_fixture(args.out_dir, swarm_result),
    ]:
        print(path)


def main() -> None:
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
