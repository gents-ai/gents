#!/usr/bin/env python3
from __future__ import annotations

import argparse
import importlib.metadata
import json
import os
from pathlib import Path
from typing import Any, TypedDict

from langchain_core.language_models.fake_chat_models import FakeListChatModel
from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph


THREAD_ID = "thread-langgraph-docker-1"
REQUEST_ID = "req-langgraph-docker-1"
CHILD_REQUEST_ID = "req-langgraph-child-docker-1"

SUBGRAPH_THREAD_ID = "thread-langgraph-subgraph-docker-1"
SUBGRAPH_REQUEST_ID = "req-langgraph-subgraph-docker-1"
SUBGRAPH_CHILD_REQUEST_ID = "req-langgraph-subgraph-child-docker-1"

PROVIDER_THREAD_ID = "thread-langgraph-provider-docker-1"
PROVIDER_REQUEST_ID = "req-langgraph-provider-docker-1"
PROVIDER_TOOL_REQUEST_ID = "req-langgraph-provider-tool-docker-1"
PROVIDER_TOOL_CALL_ID = "langgraph-provider-tool-call-1"


class FixtureState(TypedDict, total=False):
    request_id: str
    session_id: str
    topic: str
    messages: list[str]
    attempts: int
    retries: int
    child_request_id: str
    final_output: str
    status: str


class ProviderState(TypedDict, total=False):
    request_id: str
    session_id: str
    topic: str
    messages: list[Any]
    model_response: dict[str, Any]
    provider_mode: str
    provider_model: str
    provider_base_url: str
    tool_call_id: str
    tool_result: str
    final_output: str
    status: str


def collect(state: FixtureState) -> FixtureState:
    return {
        "attempts": 1,
        "messages": state.get("messages", [])
        + ["collect: searched current adapter projection requirements"],
        "retries": 0,
        "status": "collect_needs_retry",
    }


def collect_retry(state: FixtureState) -> FixtureState:
    return {
        "attempts": 2,
        "messages": state.get("messages", [])
        + ["collect_retry: captured durable checkpoint history"],
        "retries": 1,
        "status": "collected",
    }


def delegate(state: FixtureState) -> FixtureState:
    return {
        "child_request_id": CHILD_REQUEST_ID,
        "messages": state.get("messages", [])
        + ["delegate: assigned fixture review to child request"],
        "status": "delegated",
    }


def finalize(state: FixtureState) -> FixtureState:
    return {
        "final_output": "LangGraph fixture produced checkpoint, retry, and child-run evidence.",
        "messages": state.get("messages", []) + ["finalize: wrote adapter projection fixture"],
        "status": "completed",
    }


def parent_prepare(state: FixtureState) -> FixtureState:
    return {
        "attempts": 1,
        "messages": state.get("messages", [])
        + ["parent_prepare: prepared parent graph state for subgraph review"],
        "status": "parent_prepared",
    }


def draft_review(state: FixtureState) -> FixtureState:
    return {
        "child_request_id": SUBGRAPH_CHILD_REQUEST_ID,
        "messages": state.get("messages", [])
        + ["draft_review: reviewed adapter projection in compiled subgraph"],
        "status": "subgraph_review_drafted",
    }


def approve_review(state: FixtureState) -> FixtureState:
    return {
        "messages": state.get("messages", [])
        + ["approve_review: approved child subgraph result"],
        "status": "subgraph_approved",
    }


def parent_finalize(state: FixtureState) -> FixtureState:
    return {
        "final_output": (
            "LangGraph subgraph fixture produced parent checkpoints, nested review "
            "tasks, and child-run evidence."
        ),
        "messages": state.get("messages", [])
        + ["parent_finalize: merged subgraph review result into parent output"],
        "status": "completed",
    }


def provider_model_node(state: ProviderState) -> ProviderState:
    model, provider_metadata = build_provider_chat_model()
    human = HumanMessage(
        content=(
            "Map a provider-backed LangGraph run to Defra Agent projection "
            f"fields for topic={state.get('topic', 'adapter projection')}."
        )
    )
    response = model.invoke([human])
    content = message_text(response) or "Provider model returned an empty response."
    tool_call = {
        "name": "lookup_projection_requirements",
        "args": {"topic": state.get("topic", "adapter projection")},
        "id": PROVIDER_TOOL_CALL_ID,
        "type": "tool_call",
    }
    tool_request = AIMessage(
        content=content,
        tool_calls=[tool_call],
        response_metadata=getattr(response, "response_metadata", {}),
    )
    return {
        "provider_mode": provider_metadata["mode"],
        "provider_model": provider_metadata["model"],
        "provider_base_url": provider_metadata["base_url"],
        "tool_call_id": PROVIDER_TOOL_CALL_ID,
        "model_response": to_jsonable(response),
        "messages": [human, response, tool_request],
        "status": "model_requested_tool",
    }


def provider_tool_node(state: ProviderState) -> ProviderState:
    tool_result = (
        "LangGraph provider fixture observed checkpointed chat messages, "
        "a tool-shaped model request, and a tool response boundary."
    )
    return {
        "tool_result": tool_result,
        "messages": state.get("messages", [])
        + [
            ToolMessage(
                content=tool_result,
                tool_call_id=state.get("tool_call_id", PROVIDER_TOOL_CALL_ID),
            )
        ],
        "status": "tool_completed",
    }


def provider_finalize_node(state: ProviderState) -> ProviderState:
    final_output = (
        "Provider-backed LangGraph fixture produced model, tool, checkpoint, "
        "and adapter projection evidence."
    )
    return {
        "final_output": final_output,
        "messages": state.get("messages", []) + [AIMessage(content=final_output)],
        "status": "completed",
    }


GRAPH_NODES = ["collect", "collect_retry", "delegate", "finalize"]
GRAPH_EDGES = [
    ("langgraph:start", "langgraph:node:collect", "start"),
    ("langgraph:node:collect", "langgraph:node:collect_retry", "retry"),
    ("langgraph:node:collect_retry", "langgraph:node:delegate", "transition"),
    ("langgraph:node:delegate", "langgraph:node:finalize", "child_request"),
    ("langgraph:node:finalize", "langgraph:end", "end"),
]

SUBGRAPH_PARENT_NODES = ["parent_prepare", "review_subgraph", "parent_finalize"]
SUBGRAPH_INTERNAL_NODES = ["draft_review", "approve_review"]
SUBGRAPH_EDGES = [
    ("langgraph:start", "langgraph:node:parent_prepare", "start"),
    ("langgraph:node:parent_prepare", "langgraph:subgraph:review", "subgraph"),
    ("langgraph:subgraph:review", "langgraph:node:parent_finalize", "transition"),
    ("langgraph:node:parent_finalize", "langgraph:end", "end"),
    ("langgraph:subgraph:review:start", "langgraph:subgraph:review:draft_review", "start"),
    (
        "langgraph:subgraph:review:draft_review",
        "langgraph:subgraph:review:approve_review",
        "transition",
    ),
    ("langgraph:subgraph:review:approve_review", "langgraph:subgraph:review:end", "end"),
]

PROVIDER_GRAPH_NODES = ["provider_model", "provider_tool", "provider_finalize"]
PROVIDER_GRAPH_EDGES = [
    ("langgraph:start", "langgraph:node:provider_model", "start"),
    ("langgraph:node:provider_model", "langgraph:node:provider_tool", "tool_call"),
    ("langgraph:node:provider_tool", "langgraph:node:provider_finalize", "transition"),
    ("langgraph:node:provider_finalize", "langgraph:end", "end"),
]


def build_graph():
    builder = StateGraph(FixtureState)
    builder.add_node("collect", collect)
    builder.add_node("collect_retry", collect_retry)
    builder.add_node("delegate", delegate)
    builder.add_node("finalize", finalize)
    builder.add_edge(START, "collect")
    builder.add_edge("collect", "collect_retry")
    builder.add_edge("collect_retry", "delegate")
    builder.add_edge("delegate", "finalize")
    builder.add_edge("finalize", END)
    return builder.compile(checkpointer=InMemorySaver())


def build_review_subgraph():
    builder = StateGraph(FixtureState)
    builder.add_node("draft_review", draft_review)
    builder.add_node("approve_review", approve_review)
    builder.add_edge(START, "draft_review")
    builder.add_edge("draft_review", "approve_review")
    builder.add_edge("approve_review", END)
    return builder.compile()


def build_subgraph_graph():
    builder = StateGraph(FixtureState)
    builder.add_node("parent_prepare", parent_prepare)
    builder.add_node("review_subgraph", build_review_subgraph())
    builder.add_node("parent_finalize", parent_finalize)
    builder.add_edge(START, "parent_prepare")
    builder.add_edge("parent_prepare", "review_subgraph")
    builder.add_edge("review_subgraph", "parent_finalize")
    builder.add_edge("parent_finalize", END)
    return builder.compile(checkpointer=InMemorySaver())


def build_provider_graph():
    builder = StateGraph(ProviderState)
    builder.add_node("provider_model", provider_model_node)
    builder.add_node("provider_tool", provider_tool_node)
    builder.add_node("provider_finalize", provider_finalize_node)
    builder.add_edge(START, "provider_model")
    builder.add_edge("provider_model", "provider_tool")
    builder.add_edge("provider_tool", "provider_finalize")
    builder.add_edge("provider_finalize", END)
    return builder.compile(checkpointer=InMemorySaver())


def build_provider_chat_model() -> tuple[Any, dict[str, str]]:
    mode = os.environ.get("DEFRA_LANGGRAPH_PROVIDER_MODE", "fake").strip().lower()
    if mode not in {"fake", "auto", "live"}:
        raise RuntimeError(
            "DEFRA_LANGGRAPH_PROVIDER_MODE must be one of fake, auto, or live"
        )
    model_name = (
        os.environ.get("DEFRA_LANGGRAPH_OPENAI_MODEL")
        or os.environ.get("OPENAI_MODEL")
        or "gpt-4.1-mini"
    )
    base_url = os.environ.get("OPENAI_BASE_URL", "")
    if mode == "live" and not os.environ.get("OPENAI_API_KEY"):
        raise RuntimeError(
            "DEFRA_LANGGRAPH_PROVIDER_MODE=live requires OPENAI_API_KEY"
        )
    if mode in {"live", "auto"} and os.environ.get("OPENAI_API_KEY"):
        from langchain_openai import ChatOpenAI

        kwargs: dict[str, Any] = {"model": model_name, "temperature": 0}
        if base_url:
            kwargs["base_url"] = base_url
        return ChatOpenAI(**kwargs), {
            "mode": "live",
            "model": model_name,
            "base_url": base_url,
        }
    response = (
        "FAKE_PROVIDER: map LangGraph provider messages, tool calls, and "
        "checkpoint history into Defra Agent projection fields."
    )
    return FakeListChatModel(responses=[response]), {
        "mode": "fake",
        "model": "FakeListChatModel",
        "base_url": "",
    }


def run_langgraph_capture() -> dict[str, Any]:
    graph = build_graph()
    config = {"configurable": {"thread_id": THREAD_ID}}
    result = graph.invoke(
        {
            "request_id": REQUEST_ID,
            "session_id": THREAD_ID,
            "topic": "adapter projection interop",
            "messages": [],
        },
        config,
    )
    history = list(graph.get_state_history(config))
    if not history:
        raise RuntimeError("LangGraph did not produce state history")
    return {
        "result": result,
        "history": [snapshot_to_json(snapshot) for snapshot in history],
    }


def run_langgraph_subgraph_capture() -> dict[str, Any]:
    graph = build_subgraph_graph()
    config = {"configurable": {"thread_id": SUBGRAPH_THREAD_ID}}
    result = graph.invoke(
        {
            "request_id": SUBGRAPH_REQUEST_ID,
            "session_id": SUBGRAPH_THREAD_ID,
            "topic": "adapter projection subgraph interop",
            "messages": [],
        },
        config,
    )
    history = state_history_to_json(graph, config)
    if not history:
        raise RuntimeError("LangGraph subgraph did not produce state history")
    return {
        "result": result,
        "history": history,
    }


def run_langgraph_provider_capture() -> dict[str, Any]:
    graph = build_provider_graph()
    config = {"configurable": {"thread_id": PROVIDER_THREAD_ID}}
    result = graph.invoke(
        {
            "request_id": PROVIDER_REQUEST_ID,
            "session_id": PROVIDER_THREAD_ID,
            "topic": "provider-backed adapter projection interop",
            "messages": [],
        },
        config,
    )
    history = list(graph.get_state_history(config))
    if not history:
        raise RuntimeError("LangGraph provider-backed graph did not produce state history")
    return {
        "result": result,
        "history": [snapshot_to_json(snapshot) for snapshot in history],
    }


def state_history_to_json(graph: Any, config: dict[str, Any]) -> list[dict[str, Any]]:
    try:
        history = graph.get_state_history(config, subgraphs=True)
    except TypeError:
        history = graph.get_state_history(config)
    return [snapshot_to_json(snapshot) for snapshot in history]


def snapshot_to_json(snapshot: Any) -> dict[str, Any]:
    namespace = None
    if isinstance(snapshot, tuple) and len(snapshot) == 2 and not hasattr(snapshot, "values"):
        namespace = to_jsonable(snapshot[0])
        snapshot = snapshot[1]
    payload = {
        "values": to_jsonable(snapshot.values),
        "next": to_jsonable(snapshot.next),
        "config": to_jsonable(snapshot.config),
        "metadata": to_jsonable(snapshot.metadata),
        "created_at": to_jsonable(snapshot.created_at),
        "parent_config": to_jsonable(snapshot.parent_config),
        "tasks": [task_to_json(task) for task in snapshot.tasks],
        "interrupts": to_jsonable(snapshot.interrupts),
    }
    if namespace is not None:
        payload["namespace"] = namespace
    return payload


def task_to_json(task: Any) -> dict[str, Any]:
    return {
        "id": task.id,
        "name": task.name,
        "path": to_jsonable(task.path),
        "error": to_jsonable(task.error),
        "interrupts": to_jsonable(task.interrupts),
        "state": to_jsonable(task.state),
        "result": to_jsonable(task.result),
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


def message_text(message: Any) -> str:
    content = getattr(message, "content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for item in content:
            if isinstance(item, str):
                parts.append(item)
            elif isinstance(item, dict):
                text = item.get("text") or item.get("content")
                if isinstance(text, str):
                    parts.append(text)
        return "\n".join(parts)
    return str(content)


def build_projection(capture: dict[str, Any]) -> dict[str, Any]:
    history = capture["history"]
    latest_snapshot = history[0]
    latest_values = dict(latest_snapshot["values"])
    latest_values["history_checkpoint_count"] = len(history)
    latest_values["langgraph_package_version"] = langgraph_version()

    checkpoint_id = (
        latest_snapshot["config"].get("configurable", {}).get("checkpoint_id")
        or "langgraph:checkpoint:missing"
    )

    return {
        "projection_id": "langgraph_state_history",
        "projection_version": "v1",
        "source_request_id": REQUEST_ID,
        "source_session_id": THREAD_ID,
        "source_agent_did": "did:defra-agent:langgraph-fixture",
        "source_behavior_id": "langgraph-research-flow",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:langgraph-fixture-reader",
        },
        "output": {
            "adapter": "langgraph_state_history",
            "projection": {
                "thread_id": THREAD_ID,
                "checkpoint_id": checkpoint_id,
                "root_request_id": REQUEST_ID,
                "values": latest_values,
                "nodes": build_projection_nodes(latest_values),
                "edges": [
                    {"from": source, "to": target, "kind": kind}
                    for source, target, kind in GRAPH_EDGES
                ],
                "tasks": build_projection_tasks(history, latest_values),
            },
        },
    }


def build_subgraph_projection(capture: dict[str, Any]) -> dict[str, Any]:
    history = capture["history"]
    latest_snapshot = history[0]
    latest_values = dict(latest_snapshot["values"])
    latest_values["history_checkpoint_count"] = len(history)
    latest_values["langgraph_package_version"] = langgraph_version()
    latest_values["subgraph_node"] = "review_subgraph"
    latest_values["subgraph_child_request_id"] = latest_values.get(
        "child_request_id", SUBGRAPH_CHILD_REQUEST_ID
    )

    checkpoint_id = (
        latest_snapshot["config"].get("configurable", {}).get("checkpoint_id")
        or "langgraph:subgraph-checkpoint:missing"
    )

    return {
        "projection_id": "langgraph_state_history",
        "projection_version": "v1",
        "source_request_id": SUBGRAPH_REQUEST_ID,
        "source_session_id": SUBGRAPH_THREAD_ID,
        "source_agent_did": "did:defra-agent:langgraph-subgraph-fixture",
        "source_behavior_id": "langgraph-review-subgraph-flow",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:langgraph-fixture-reader",
        },
        "output": {
            "adapter": "langgraph_state_history",
            "projection": {
                "thread_id": SUBGRAPH_THREAD_ID,
                "checkpoint_id": checkpoint_id,
                "root_request_id": SUBGRAPH_REQUEST_ID,
                "values": latest_values,
                "nodes": build_subgraph_projection_nodes(latest_values),
                "edges": [
                    {"from": source, "to": target, "kind": kind}
                    for source, target, kind in SUBGRAPH_EDGES
                ],
                "tasks": build_subgraph_projection_tasks(history, latest_values),
            },
        },
    }


def build_provider_projection(capture: dict[str, Any]) -> dict[str, Any]:
    history = capture["history"]
    latest_snapshot = history[0]
    latest_values = dict(latest_snapshot["values"])
    latest_values["history_checkpoint_count"] = len(history)
    latest_values["langgraph_package_version"] = langgraph_version()
    latest_values["langchain_core_package_version"] = package_version("langchain-core")
    latest_values["langchain_openai_package_version"] = package_version("langchain-openai")

    checkpoint_id = (
        latest_snapshot["config"].get("configurable", {}).get("checkpoint_id")
        or "langgraph:provider-checkpoint:missing"
    )

    return {
        "projection_id": "langgraph_state_history",
        "projection_version": "v1",
        "source_request_id": PROVIDER_REQUEST_ID,
        "source_session_id": PROVIDER_THREAD_ID,
        "source_agent_did": "did:defra-agent:langgraph-provider-fixture",
        "source_behavior_id": "langgraph-provider-backed-flow",
        "redaction_mode": "training_safe",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:langgraph-fixture-reader",
        },
        "output": {
            "adapter": "langgraph_state_history",
            "projection": {
                "thread_id": PROVIDER_THREAD_ID,
                "checkpoint_id": checkpoint_id,
                "root_request_id": PROVIDER_REQUEST_ID,
                "values": latest_values,
                "nodes": build_provider_projection_nodes(latest_values),
                "edges": [
                    {"from": source, "to": target, "kind": kind}
                    for source, target, kind in PROVIDER_GRAPH_EDGES
                ],
                "tasks": build_provider_projection_tasks(history, latest_values),
            },
        },
    }


def build_projection_nodes(latest_values: dict[str, Any]) -> list[dict[str, Any]]:
    status = latest_values.get("status", "completed")
    nodes = [{"id": "langgraph:start", "kind": "start", "status": "completed"}]
    nodes.extend(
        {
            "id": f"langgraph:node:{name}",
            "kind": "node",
            "request_id": REQUEST_ID,
            "status": status if name == "finalize" else "completed",
        }
        for name in GRAPH_NODES
    )
    nodes.append({"id": "langgraph:end", "kind": "end", "status": status})
    return nodes


def build_subgraph_projection_nodes(latest_values: dict[str, Any]) -> list[dict[str, Any]]:
    status = latest_values.get("status", "completed")
    child_request_id = latest_values.get("child_request_id", SUBGRAPH_CHILD_REQUEST_ID)
    return [
        {"id": "langgraph:start", "kind": "start", "status": "completed"},
        {
            "id": "langgraph:node:parent_prepare",
            "kind": "node",
            "request_id": SUBGRAPH_REQUEST_ID,
            "status": "completed",
        },
        {
            "id": "langgraph:subgraph:review",
            "kind": "subgraph",
            "request_id": child_request_id,
            "status": "completed",
        },
        {
            "id": "langgraph:subgraph:review:draft_review",
            "kind": "subgraph_node",
            "request_id": child_request_id,
            "status": "completed",
        },
        {
            "id": "langgraph:subgraph:review:approve_review",
            "kind": "subgraph_node",
            "request_id": child_request_id,
            "status": "completed",
        },
        {
            "id": "langgraph:node:parent_finalize",
            "kind": "node",
            "request_id": SUBGRAPH_REQUEST_ID,
            "status": status,
        },
        {"id": "langgraph:end", "kind": "end", "status": status},
    ]


def build_provider_projection_nodes(latest_values: dict[str, Any]) -> list[dict[str, Any]]:
    status = latest_values.get("status", "completed")
    nodes = [{"id": "langgraph:start", "kind": "start", "status": "completed"}]
    nodes.extend(
        {
            "id": f"langgraph:node:{name}",
            "kind": "provider_node" if name == "provider_model" else "node",
            "request_id": (
                PROVIDER_TOOL_REQUEST_ID if name == "provider_tool" else PROVIDER_REQUEST_ID
            ),
            "status": status if name == "provider_finalize" else "completed",
        }
        for name in PROVIDER_GRAPH_NODES
    )
    nodes.append({"id": "langgraph:end", "kind": "end", "status": status})
    return nodes


def build_projection_tasks(
    history: list[dict[str, Any]], latest_values: dict[str, Any]
) -> list[dict[str, Any]]:
    native_tasks = collect_native_tasks(history)

    projected = []
    for name in GRAPH_NODES:
        task = native_tasks.get(name)
        status = "completed"
        if task and task.get("error"):
            status = "failed"
        elif task and task.get("result") is None:
            status = "pending"

        projected_task = {
            "id": task["id"] if task else f"langgraph:task:{name}",
            "request_id": REQUEST_ID,
            "name": name,
            "status": status,
        }
        if name == "delegate":
            projected_task["child_request_id"] = latest_values.get(
                "child_request_id", CHILD_REQUEST_ID
            )
        projected.append(projected_task)
    return projected


def build_subgraph_projection_tasks(
    history: list[dict[str, Any]], latest_values: dict[str, Any]
) -> list[dict[str, Any]]:
    native_tasks = collect_native_tasks(history)
    child_request_id = latest_values.get("child_request_id", SUBGRAPH_CHILD_REQUEST_ID)
    task_specs = [
        ("parent_prepare", SUBGRAPH_REQUEST_ID, None),
        ("review_subgraph", SUBGRAPH_REQUEST_ID, child_request_id),
        ("draft_review", child_request_id, None),
        ("approve_review", child_request_id, None),
        ("parent_finalize", SUBGRAPH_REQUEST_ID, None),
    ]

    projected = []
    for name, request_id, child in task_specs:
        task = native_tasks.get(name)
        status = "completed"
        if task and task.get("error"):
            status = "failed"
        elif task and task.get("result") is None:
            status = "pending"

        projected_task = {
            "id": task["id"] if task else f"langgraph:subgraph-task:{name}",
            "request_id": request_id,
            "name": name,
            "status": status,
        }
        if child:
            projected_task["child_request_id"] = child
        projected.append(projected_task)
    return projected


def build_provider_projection_tasks(
    history: list[dict[str, Any]], latest_values: dict[str, Any]
) -> list[dict[str, Any]]:
    native_tasks = collect_native_tasks(history)
    task_specs = [
        ("provider_model", PROVIDER_REQUEST_ID, PROVIDER_TOOL_REQUEST_ID),
        ("provider_tool", PROVIDER_TOOL_REQUEST_ID, None),
        ("provider_finalize", PROVIDER_REQUEST_ID, None),
    ]
    projected = []
    for name, request_id, child_request_id in task_specs:
        task = native_tasks.get(name)
        status = "completed"
        if task and task.get("error"):
            status = "failed"
        elif task and task.get("result") is None:
            status = "pending"

        projected_task = {
            "id": task["id"] if task else f"langgraph:provider-task:{name}",
            "request_id": request_id,
            "name": name,
            "status": status,
        }
        if child_request_id:
            projected_task["child_request_id"] = child_request_id
        projected.append(projected_task)
    return projected


def collect_native_tasks(history: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    native_tasks: dict[str, dict[str, Any]] = {}
    for snapshot in reversed(history):
        for task in snapshot["tasks"]:
            native_tasks[task["name"]] = task
    return native_tasks


def langgraph_version() -> str:
    return importlib.metadata.version("langgraph")


def package_version(package: str) -> str:
    return importlib.metadata.version(package)


def write_fixture_file(
    out_dir: Path,
    filename: str,
    source_api: list[str],
    native: dict[str, Any],
    envelope: dict[str, Any],
) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / filename
    payload = {
        "source": {
            "system": "langgraph",
            "package": "langgraph",
            "package_version": langgraph_version(),
            "generator": "adapter-projections/generators/langgraph",
            "capture": os.environ.get("DEFRA_FIXTURE_CAPTURE", "local"),
            "api": source_api,
        },
        "native": native,
        "envelope": envelope,
    }
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def write_linear_fixture(out_dir: Path, capture: dict[str, Any]) -> Path:
    native = {
        "thread_id": THREAD_ID,
        "graph": {
            "nodes": GRAPH_NODES,
            "edges": [
                {"from": source, "to": target, "kind": kind}
                for source, target, kind in GRAPH_EDGES
            ],
        },
        "history_order": "most_recent_first",
        "history": capture["history"],
    }
    return write_fixture_file(
        out_dir,
        "langgraph_state_history.capture.json",
        ["StateGraph", "InMemorySaver", "get_state_history"],
        native,
        build_projection(capture),
    )


def write_subgraph_fixture(out_dir: Path, capture: dict[str, Any]) -> Path:
    native = {
        "thread_id": SUBGRAPH_THREAD_ID,
        "graph": {
            "nodes": SUBGRAPH_PARENT_NODES,
            "subgraphs": {
                "review_subgraph": {
                    "nodes": SUBGRAPH_INTERNAL_NODES,
                }
            },
            "edges": [
                {"from": source, "to": target, "kind": kind}
                for source, target, kind in SUBGRAPH_EDGES
            ],
        },
        "history_order": "most_recent_first",
        "history": capture["history"],
    }
    return write_fixture_file(
        out_dir,
        "langgraph_state_history.subgraph.capture.json",
        ["StateGraph", "CompiledStateGraph node", "InMemorySaver", "get_state_history"],
        native,
        build_subgraph_projection(capture),
    )


def write_provider_fixture(out_dir: Path, capture: dict[str, Any]) -> Path:
    native = {
        "thread_id": PROVIDER_THREAD_ID,
        "graph": {
            "nodes": PROVIDER_GRAPH_NODES,
            "edges": [
                {"from": source, "to": target, "kind": kind}
                for source, target, kind in PROVIDER_GRAPH_EDGES
            ],
        },
        "provider": {
            "mode": capture["result"].get("provider_mode"),
            "model": capture["result"].get("provider_model"),
            "base_url": capture["result"].get("provider_base_url"),
            "live_env": {
                "DEFRA_LANGGRAPH_PROVIDER_MODE": os.environ.get(
                    "DEFRA_LANGGRAPH_PROVIDER_MODE", "fake"
                ),
                "DEFRA_LANGGRAPH_OPENAI_MODEL_SET": bool(
                    os.environ.get("DEFRA_LANGGRAPH_OPENAI_MODEL")
                ),
                "OPENAI_BASE_URL_SET": bool(os.environ.get("OPENAI_BASE_URL")),
                "OPENAI_API_KEY_SET": bool(os.environ.get("OPENAI_API_KEY")),
            },
        },
        "history_order": "most_recent_first",
        "history": capture["history"],
    }
    return write_fixture_file(
        out_dir,
        "langgraph_state_history.provider.capture.json",
        [
            "StateGraph",
            "InMemorySaver",
            "get_state_history",
            "langchain_core.messages",
            "langchain_core.language_models.fake_chat_models.FakeListChatModel",
            "langchain_openai.ChatOpenAI",
        ],
        native,
        build_provider_projection(capture),
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=Path("/out"))
    args = parser.parse_args()

    linear_capture = run_langgraph_capture()
    subgraph_capture = run_langgraph_subgraph_capture()
    provider_capture = run_langgraph_provider_capture()
    for path in [
        write_linear_fixture(args.out_dir, linear_capture),
        write_subgraph_fixture(args.out_dir, subgraph_capture),
        write_provider_fixture(args.out_dir, provider_capture),
    ]:
        print(path)


if __name__ == "__main__":
    main()
