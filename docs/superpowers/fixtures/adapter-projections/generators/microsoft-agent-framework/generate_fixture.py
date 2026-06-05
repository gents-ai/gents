#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import dataclasses
import importlib.metadata
import json
import os
from pathlib import Path
from typing import Any, Awaitable, Mapping, Sequence

from agent_framework import (
    Agent,
    BaseChatClient,
    ChatResponse,
    ChatResponseUpdate,
    Content,
    Message,
    ResponseStream,
)
from agent_framework.orchestrations import GroupChatBuilder, GroupChatState


CONTEXT_ID = "context-ms-agent-framework-docker-1"
REQUEST_ID = "req-ms-agent-framework-docker-1"
RESEARCH_REQUEST_ID = "req-ms-agent-framework-research-docker-1"
WRITER_REQUEST_ID = "req-ms-agent-framework-writer-docker-1"
TASK_TEXT = "Map Microsoft Agent Framework group chat to Defra Agent projection fields."


AGENTS = [
    {
        "name": "Researcher",
        "role": "researcher",
        "agent_did": "did:defra-agent:microsoft-agent-framework-researcher",
        "behavior_id": "microsoft_agent_framework.researcher",
        "description": "Collects evidence from the shared group-chat conversation.",
        "instructions": "Gather concise facts that help map this workflow.",
        "request_id": RESEARCH_REQUEST_ID,
        "response": (
            "RESEARCH: Microsoft Agent Framework group chat preserves "
            "author names, shared conversation, and orchestrator-selected turns."
        ),
    },
    {
        "name": "Writer",
        "role": "writer",
        "agent_did": "did:defra-agent:microsoft-agent-framework-writer",
        "behavior_id": "microsoft_agent_framework.writer",
        "description": "Synthesizes the adapter projection mapping.",
        "instructions": "Write the final projection mapping in one concise answer.",
        "request_id": WRITER_REQUEST_ID,
        "response": (
            "WRITE: projection fields map to participants, messages, "
            "group-chat request events, response events, and orchestrated turns."
        ),
    },
]

ORCHESTRATOR = {
    "name": "group_chat_orchestrator",
    "role": "orchestrator",
    "agent_did": "did:defra-agent:microsoft-agent-framework-orchestrator",
    "behavior_id": "microsoft_agent_framework.group_chat_orchestrator",
}


class ScriptedChatClient(BaseChatClient):
    def __init__(self, definition: dict[str, str]) -> None:
        super().__init__()
        self.definition = definition
        self.calls: list[dict[str, Any]] = []
        self.outputs: list[dict[str, Any]] = []

    def _inner_get_response(
        self,
        *,
        messages: Sequence[Message],
        stream: bool,
        options: Mapping[str, Any],
        **kwargs: Any,
    ) -> ResponseStream[ChatResponseUpdate, ChatResponse[Any]] | Awaitable[ChatResponse[Any]]:
        response_index = len(self.outputs) + 1
        message_id = f"msaf:message:{self.definition['role']}:{response_index}"
        response_id = f"msaf:response:{self.definition['role']}:{response_index}"
        text = self.definition["response"]
        self.calls.append(
            {
                "request_id": self.definition["request_id"],
                "stream": stream,
                "options": to_jsonable(options),
                "messages": [to_jsonable(message) for message in messages],
                "client_kwargs": to_jsonable(kwargs),
            }
        )
        self.outputs.append(
            {
                "request_id": self.definition["request_id"],
                "message_id": message_id,
                "response_id": response_id,
                "author_name": self.definition["name"],
                "role": self.definition["role"],
                "text": text,
            }
        )

        if stream:
            content = Content("text", text=text)

            async def updates() -> Any:
                yield ChatResponseUpdate(
                    role="assistant",
                    contents=[content],
                    author_name=self.definition["name"],
                    message_id=message_id,
                    response_id=response_id,
                )

            return ResponseStream(updates())

        async def response() -> ChatResponse[Any]:
            return ChatResponse(
                messages=[
                    Message(
                        role="assistant",
                        contents=[text],
                        author_name=self.definition["name"],
                        message_id=message_id,
                    )
                ],
                response_id=response_id,
                model="defra-scripted-agent-framework",
            )

        return response()

    def service_url(self) -> str:
        return "local://defra-scripted-agent-framework"


def round_robin_selector(state: GroupChatState) -> str:
    participant_names = list(state.participants.keys())
    return participant_names[state.current_round % len(participant_names)]


def build_workflow() -> tuple[Any, dict[str, ScriptedChatClient]]:
    clients = {
        definition["name"]: ScriptedChatClient(definition)
        for definition in AGENTS
    }
    agents = [
        Agent(
            client=clients[definition["name"]],
            name=definition["name"],
            description=definition["description"],
            instructions=definition["instructions"],
        )
        for definition in AGENTS
    ]
    workflow = GroupChatBuilder(
        participants=agents,
        selection_func=round_robin_selector,
        termination_condition=lambda messages: len(messages) >= 3,
        max_rounds=3,
    ).build()
    return workflow, clients


async def run_capture() -> tuple[list[dict[str, Any]], dict[str, ScriptedChatClient]]:
    workflow, clients = build_workflow()
    events: list[dict[str, Any]] = []
    async for event in workflow.run(TASK_TEXT, stream=True):
        events.append(event_to_json(event))
    return events, clients


def build_projection(
    events: list[dict[str, Any]],
    clients: dict[str, ScriptedChatClient],
) -> dict[str, Any]:
    outputs = agent_outputs(clients)
    final_output = final_orchestrator_output(events)
    return {
        "projection_id": "multi_agent_task",
        "projection_version": "v1",
        "source_request_id": REQUEST_ID,
        "source_session_id": CONTEXT_ID,
        "source_agent_did": ORCHESTRATOR["agent_did"],
        "source_behavior_id": ORCHESTRATOR["behavior_id"],
        "redaction_mode": "full",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:microsoft-agent-framework-fixture-reader",
        },
        "output": {
            "adapter": "multi_agent_task",
            "projection": {
                "task_id": REQUEST_ID,
                "context_id": CONTEXT_ID,
                "status": "completed" if outputs else "stopped",
                "participants": [
                    {
                        "agent_did": ORCHESTRATOR["agent_did"],
                        "behavior_id": ORCHESTRATOR["behavior_id"],
                        "role": ORCHESTRATOR["role"],
                    },
                    *[
                        {
                            "agent_did": definition["agent_did"],
                            "behavior_id": definition["behavior_id"],
                            "role": definition["role"],
                        }
                        for definition in AGENTS
                    ],
                ],
                "messages": [
                    {
                        "id": "msaf:message:user-task",
                        "request_id": REQUEST_ID,
                        "role": "user",
                        "content": TASK_TEXT,
                    },
                    *[
                        {
                            "id": output["message_id"],
                            "request_id": output["request_id"],
                            "role": output["role"],
                            "content": output["text"],
                        }
                        for output in outputs
                    ],
                    {
                        "id": "msaf:message:orchestrator:final",
                        "request_id": REQUEST_ID,
                        "role": ORCHESTRATOR["role"],
                        "content": final_output,
                    },
                ],
                "delegations": [
                    {
                        "parent_request_id": REQUEST_ID,
                        "child_request_id": RESEARCH_REQUEST_ID,
                        "parent_tool_call_id": "msaf:group-chat:round:0:Researcher",
                        "agent_did": AGENTS[0]["agent_did"],
                        "behavior_id": AGENTS[0]["behavior_id"],
                        "status": "completed",
                    },
                    {
                        "parent_request_id": REQUEST_ID,
                        "child_request_id": WRITER_REQUEST_ID,
                        "parent_tool_call_id": "msaf:group-chat:round:1:Writer",
                        "agent_did": AGENTS[1]["agent_did"],
                        "behavior_id": AGENTS[1]["behavior_id"],
                        "status": "completed",
                    },
                ],
                "tool_events": [
                    {
                        "id": "msaf:event:round:0:request:Researcher",
                        "request_id": REQUEST_ID,
                        "tool_name": "group_chat_request",
                        "status": "completed",
                        "child_request_id": RESEARCH_REQUEST_ID,
                    },
                    {
                        "id": "msaf:event:round:1:response:Researcher",
                        "request_id": RESEARCH_REQUEST_ID,
                        "tool_name": "group_chat_response",
                        "status": "completed",
                    },
                    {
                        "id": "msaf:event:round:1:request:Writer",
                        "request_id": REQUEST_ID,
                        "tool_name": "group_chat_request",
                        "status": "completed",
                        "child_request_id": WRITER_REQUEST_ID,
                    },
                    {
                        "id": "msaf:event:round:2:response:Writer",
                        "request_id": WRITER_REQUEST_ID,
                        "tool_name": "group_chat_response",
                        "status": "completed",
                    },
                    {
                        "id": "msaf:event:termination",
                        "request_id": REQUEST_ID,
                        "tool_name": "group_chat_termination",
                        "status": "completed",
                    },
                ],
            },
        },
    }


def agent_outputs(clients: dict[str, ScriptedChatClient]) -> list[dict[str, Any]]:
    outputs: list[dict[str, Any]] = []
    for definition in AGENTS:
        outputs.extend(clients[definition["name"]].outputs)
    return outputs


def final_orchestrator_output(events: list[dict[str, Any]]) -> str:
    for event in reversed(events):
        data = event.get("data")
        if event.get("type") == "output" and isinstance(data, dict):
            text = data.get("text")
            if isinstance(text, str) and text:
                return text
    return "The group chat has reached its termination condition."


def event_to_json(event: Any) -> dict[str, Any]:
    data = getattr(event, "data", None)
    return {
        "type": getattr(event, "type", None),
        "executor_id": getattr(event, "executor_id", None),
        "data_type": type(data).__name__ if data is not None else None,
        "data": to_jsonable(data),
    }


def to_jsonable(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        return {str(key): to_jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [to_jsonable(item) for item in value]
    if hasattr(value, "to_dict"):
        return to_jsonable(value.to_dict())
    if dataclasses.is_dataclass(value):
        return to_jsonable(dataclasses.asdict(value))
    if hasattr(value, "__dict__"):
        return {
            key: to_jsonable(item)
            for key, item in vars(value).items()
            if not key.startswith("_")
        }
    return str(value)


def package_version(package: str) -> str:
    return importlib.metadata.version(package)


def write_fixture(
    out_dir: Path,
    events: list[dict[str, Any]],
    clients: dict[str, ScriptedChatClient],
) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "source": {
            "system": "microsoft-agent-framework",
            "package": "agent-framework-core",
            "package_version": package_version("agent-framework-core"),
            "orchestrations_package": "agent-framework-orchestrations",
            "orchestrations_package_version": package_version(
                "agent-framework-orchestrations"
            ),
            "generator": "adapter-projections/generators/microsoft-agent-framework",
            "capture": os.environ.get("DEFRA_FIXTURE_CAPTURE", "local"),
            "api": [
                "Agent",
                "BaseChatClient",
                "GroupChatBuilder",
                "GroupChatState",
                "Workflow.run(stream=True)",
            ],
        },
        "native": {
            "workflow": {
                "type": "GroupChatBuilder",
                "selection_func": "round_robin_selector",
                "termination_condition": "len(messages) >= 3",
                "max_rounds": 3,
                "participants": [
                    {
                        "name": definition["name"],
                        "role": definition["role"],
                        "description": definition["description"],
                    }
                    for definition in AGENTS
                ],
            },
            "task": TASK_TEXT,
            "events": events,
            "agent_calls": {name: client.calls for name, client in clients.items()},
            "agent_outputs": {name: client.outputs for name, client in clients.items()},
        },
        "envelope": build_projection(events, clients),
    }
    path = out_dir / "multi_agent_task.microsoft_agent_framework_group_chat.capture.json"
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=Path("/out"))
    args = parser.parse_args()

    events, clients = asyncio.run(run_capture())
    print(write_fixture(args.out_dir, events, clients))


if __name__ == "__main__":
    main()
