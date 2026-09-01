#!/usr/bin/env python3
"""Live, dependency-free probes for the Gents-owned Grok leader socket."""

from __future__ import annotations

import argparse
import json
import os
import socket
import struct
import sys
import time
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Callable


DEFAULT_MODEL = "GLM-5.3-NVFP4"
DEFAULT_CONTEXT_WINDOW = 524_288
MAX_FRAME = 64 * 1024 * 1024


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def graphql_escape(value: str) -> str:
    return (
        value.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
    )


class LeaderClient:
    def __init__(self, path: str, timeout: float, model: str) -> None:
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(path)
        self.next_id = 1
        self.model = model

    def close(self) -> None:
        try:
            self.send_frame({"type": "disconnect"})
        finally:
            self.sock.close()

    def send_frame(self, value: dict[str, Any]) -> None:
        body = json.dumps(value, separators=(",", ":")).encode()
        require(len(body) <= MAX_FRAME, "outgoing frame exceeds 64 MiB")
        self.sock.sendall(struct.pack(">I", len(body)) + body)

    def recv_exact(self, length: int) -> bytes:
        chunks: list[bytes] = []
        remaining = length
        while remaining:
            chunk = self.sock.recv(remaining)
            if not chunk:
                raise EOFError("leader socket closed")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def recv_frame(self) -> dict[str, Any]:
        length = struct.unpack(">I", self.recv_exact(4))[0]
        require(length <= MAX_FRAME, f"incoming frame is too large: {length}")
        value = json.loads(self.recv_exact(length))
        require(isinstance(value, dict), "leader frame is not a JSON object")
        return value

    def register(self) -> dict[str, Any]:
        self.send_frame(
            {
                "type": "register",
                "client_type": "gents-edge-probe",
                "mode": "headless",
                "capabilities": {
                    "yolo_mode": True,
                    "auto_mode": False,
                    "default_model": self.model,
                    "client_version": "gents-edge-probe/1",
                    "code_nav_enabled": False,
                    "terminal": False,
                    "fs_read": False,
                    "fs_write": False,
                    "status_line": False,
                },
            }
        )
        registered = self.recv_frame()
        require(registered.get("type") == "registered", "register was not acknowledged")
        require(registered.get("ready") is True, "leader is not ready")
        require(registered.get("leader_protocol_version") == 1, "wrong leader protocol")
        return registered

    def ping(self) -> None:
        self.send_frame({"type": "ping"})
        require(self.recv_frame() == {"type": "pong"}, "ping did not return pong")

    def send_acp(self, payload: dict[str, Any]) -> None:
        self.send_frame(
            {
                "type": "acp",
                "payload": json.dumps(payload, separators=(",", ":")),
            }
        )

    def recv_acp(self) -> dict[str, Any]:
        frame = self.recv_frame()
        require(frame.get("type") == "acp", f"unexpected leader frame: {frame}")
        payload = json.loads(frame["payload"])
        require(isinstance(payload, dict), "ACP payload is not a JSON object")
        return payload

    def request(
        self,
        method: str,
        params: dict[str, Any],
        on_message: Callable[[dict[str, Any]], None] | None = None,
    ) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        request_id = self.next_id
        self.next_id += 1
        self.send_acp(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        notifications: list[dict[str, Any]] = []
        while True:
            message = self.recv_acp()
            if message.get("id") == request_id:
                return message, notifications
            notifications.append(message)
            if on_message is not None:
                on_message(message)

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self.send_acp({"jsonrpc": "2.0", "method": method, "params": params})


def initialize(
    client: LeaderClient, cwd: str, context_window: int
) -> tuple[str, dict[str, Any]]:
    registered = client.register()
    client.ping()
    initialized, _ = client.request(
        "initialize",
        {"protocolVersion": 1, "clientInfo": {"name": "gents-edge-probe"}},
    )
    require("error" not in initialized, f"initialize failed: {initialized}")
    result = initialized["result"]
    require(result["agentCapabilities"]["loadSession"] is False, "loadSession must be false")
    require(result["authMethods"][0]["id"] == "gents.runtime", "auth method drift")

    authenticated, _ = client.request("authenticate", {"methodId": "gents.runtime"})
    require(authenticated.get("result", {}).get("_meta", {}).get("provider") == "gents", "auth failed")

    preferred = f"grok-edge-{uuid.uuid4().hex[:16]}"
    created, _ = client.request(
        "session/new",
        {
            "cwd": cwd,
            "mcpServers": [],
            "_meta": {
                "sessionId": preferred,
                "modelId": client.model,
                "yoloMode": True,
                "autoMode": False,
            },
        },
    )
    require("error" not in created, f"session/new failed: {created}")
    session = created["result"]
    require(session["sessionId"] == preferred, "preferred session id was not honored")
    require(session["models"]["currentModelId"] == client.model, "current model mismatch")
    model = session["models"]["availableModels"][0]
    require(model["modelId"] == client.model, "catalog model mismatch")
    require(model["meta"]["totalContextTokens"] == context_window, "context window mismatch")
    return preferred, registered


def expect_error(client: LeaderClient, method: str, session_id: str) -> int:
    response, _ = client.request(method, {"sessionId": session_id})
    require("error" in response, f"{method} unexpectedly succeeded")
    return int(response["error"]["code"])


def probe_handshake(
    client: LeaderClient, session_id: str, context_window: int
) -> dict[str, Any]:
    switched, notifications = client.request(
        "session/set_model",
        {
            "sessionId": session_id,
            "modelId": client.model,
            "_meta": {"reasoningEffort": "high"},
        },
    )
    require("error" not in switched, f"session/set_model failed: {switched}")
    require(
        any(message.get("method") == "x.ai/models/update" for message in notifications),
        "session/set_model did not emit x.ai/models/update",
    )

    mode, notifications = client.request(
        "session/set_mode", {"sessionId": session_id, "modeId": "yolo"}
    )
    require("error" not in mode, f"session/set_mode failed: {mode}")
    require(
        any(
            message.get("params", {}).get("update", {}).get("sessionUpdate")
            == "current_mode_update"
            for message in notifications
        ),
        "session/set_mode did not emit current_mode_update",
    )

    load, _ = client.request(
        "session/load", {"sessionId": session_id, "cwd": str(Path.cwd()), "mcpServers": []}
    )
    require(load.get("error", {}).get("code") == -32601, "session/load must fail explicitly")
    unsupported = {
        method: expect_error(client, method, session_id)
        for method in (
            "x.ai/interject",
            "x.ai/compact_conversation",
            "terminal/create",
        )
    }
    subagent, _ = client.request(
        "x.ai/subagent/get", {"sessionId": session_id, "subagentId": "missing"}
    )
    require(subagent.get("result", {}).get("outcome", {}).get("kind") == "not_found", "subagent stub drift")
    running, _ = client.request("x.ai/subagent/list_running", {"sessionId": session_id})
    require(running.get("result", {}).get("running") == [], "subagent running stub drift")
    return {
        "model": client.model,
        "context_window": context_window,
        "unsupported": unsupported,
        "subagent_stub": "not_found",
    }


def event_sequence(update: dict[str, Any]) -> int:
    event_id = update.get("params", {}).get("_meta", {}).get("eventId", "")
    try:
        return int(event_id.rsplit("-", 1)[1])
    except (IndexError, ValueError) as error:
        raise AssertionError(f"invalid eventId: {event_id!r}") from error


def validate_turn(
    notifications: list[dict[str, Any]], prompt_id: str, stop_reason: str
) -> dict[str, Any]:
    updates = [message for message in notifications if message.get("method") == "session/update"]
    require(updates, "turn emitted no session/update notifications")
    require(
        all(message.get("params", {}).get("_meta", {}).get("promptId") == prompt_id for message in updates),
        "turn update promptId drift",
    )
    sequences = [event_sequence(message) for message in updates]
    require(sequences == sorted(set(sequences)), "eventId counters are not strictly monotonic")
    kinds = [message["params"]["update"]["sessionUpdate"] for message in updates]
    require(kinds[0] == "user_message_chunk", "user echo was not the first turn update")
    for message in updates:
        update = message["params"]["update"]
        if update["sessionUpdate"] in (
            "user_message_chunk",
            "agent_message_chunk",
            "agent_thought_chunk",
        ):
            require("content" in update, "chunk uses contentBlock instead of content")
            require("contentBlock" not in update, "chunk leaked obsolete contentBlock field")
    if stop_reason == "end_turn":
        require("agent_message_chunk" in kinds, "completed turn emitted no assistant text")
    totals = [message["params"]["_meta"].get("totalTokens", 0) for message in updates]
    require(totals == sorted(totals), "totalTokens moved backwards within the turn")
    text = "".join(
        message["params"]["update"].get("content", {}).get("text", "")
        for message in updates
        if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
    )
    assistant_chunks = [
        {
            "index": index,
            "text": message["params"]["update"].get("content", {}).get("text", ""),
        }
        for index, message in enumerate(updates)
        if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
    ]
    tool_calls_by_id: dict[str, dict[str, Any]] = {}
    for message in updates:
        update = message["params"]["update"]
        tool_call_id = update.get("toolCallId")
        if update["sessionUpdate"] == "tool_call" and tool_call_id:
            tool_calls_by_id[tool_call_id] = dict(update)
        elif update["sessionUpdate"] == "tool_call_update" and tool_call_id:
            require(tool_call_id in tool_calls_by_id, "tool_call_update preceded tool_call")
            fields = update.get("fields") or {}
            require(isinstance(fields, dict), "tool_call_update fields are not an object")
            tool_calls_by_id[tool_call_id].update(fields)
    return {
        "updates": len(updates),
        "kinds": kinds,
        "assistant_text": text,
        "assistant_chunks": assistant_chunks,
        "total_tokens": totals[-1],
        "tool_calls": list(tool_calls_by_id.values()),
    }


def probe_prompt(client: LeaderClient, session_id: str, text: str) -> dict[str, Any]:
    prompt_id = str(uuid.uuid4())
    response, notifications = client.request(
        "session/prompt",
        {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": text}],
            "_meta": {"promptId": prompt_id, "screenMode": "inline", "sendNow": True},
        },
    )
    require(response.get("result", {}).get("stopReason") == "end_turn", f"prompt failed: {response}")
    result = validate_turn(notifications, prompt_id, "end_turn")
    result["prompt_id"] = prompt_id
    return result


def probe_tool(client: LeaderClient, session_id: str) -> dict[str, Any]:
    marker = "GENTS_POST_TOOL_FINAL:"
    result = probe_prompt(
        client,
        session_id,
        "Use the list_files tool on the current directory. Only after the tool result arrives, "
        f"reply on one line beginning exactly {marker} followed by the first entry name. "
        "Do not emit that marker before calling the tool.",
    )
    require("tool_call" in result["kinds"], "tool turn emitted no tool_call update")
    tool_call = result["tool_calls"][0]
    require(tool_call.get("toolCallId"), "tool_call lacks toolCallId")
    require(tool_call.get("kind") in ("read", "search"), "read tool has the wrong Grok kind")
    require(tool_call.get("status") in ("in_progress", "completed"), "tool status is invalid")
    require(isinstance(tool_call.get("rawInput"), dict), "tool_call lacks structured rawInput")
    last_tool = max(i for i, kind in enumerate(result["kinds"]) if kind == "tool_call")
    post_tool_text = "".join(
        chunk["text"]
        for chunk in result["assistant_chunks"]
        if chunk["index"] > last_tool
    )
    require(post_tool_text, "tool turn emitted no assistant chunk after the tool update")
    require(marker in post_tool_text, "post-tool final marker was lost")
    result["tool_observed"] = True
    result["post_tool_final_observed"] = True
    return result


def probe_subprocess(client: LeaderClient, session_id: str) -> dict[str, Any]:
    marker = "gents-subprocess-probe"
    result = probe_prompt(
        client,
        session_id,
        f"Run the shell command `echo {marker}` exactly once, then briefly confirm completion.",
    )
    execute_calls = [call for call in result["tool_calls"] if call.get("kind") == "execute"]
    require(execute_calls, "subprocess turn emitted no execute tool_call")
    matching = [
        call
        for call in execute_calls
        if marker in str(call.get("rawInput", {}).get("command", ""))
        and marker in json.dumps(call.get("rawOutput", {}), sort_keys=True)
    ]
    require(matching, "execute tool_call did not preserve the probe command and output")
    result["subprocess_observed"] = True
    return result


def probe_cancel(client: LeaderClient, session_id: str) -> dict[str, Any]:
    request_id = client.next_id
    client.next_id += 1
    prompt_id = str(uuid.uuid4())
    client.send_acp(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [
                    {
                        "type": "text",
                        "text": "Write fifty thousand numbered one-word lines without abbreviating or stopping early.",
                    }
                ],
                "_meta": {"promptId": prompt_id, "screenMode": "inline"},
            },
        }
    )
    notifications: list[dict[str, Any]] = []
    cancelled = False
    while True:
        message = client.recv_acp()
        if message.get("id") == request_id:
            response = message
            break
        notifications.append(message)
        update = message.get("params", {}).get("update", {})
        if not cancelled and update.get("sessionUpdate") == "user_message_chunk":
            client.notify(
                "session/cancel",
                {
                    "sessionId": session_id,
                    "_meta": {
                        "promptId": prompt_id,
                        "cancelSubagents": True,
                        "cancelTrigger": "edge_probe",
                        "rewindIfNoOutput": False,
                        "rewindIfPristine": False,
                    },
                },
            )
            cancelled = True
    require(cancelled, "cancel notification was never sent")
    require(response.get("result", {}).get("stopReason") == "cancelled", f"cancel failed: {response}")
    result = validate_turn(notifications, prompt_id, "cancelled")
    result["prompt_id"] = prompt_id
    return result


def query_documents(endpoint: str, session_id: str) -> dict[str, Any]:
    escaped = graphql_escape(session_id)
    query = f"""{{
      AgentSession(filter: {{session_id: {{_eq: \"{escaped}\"}}}}) {{
        session_id behavior_id status started ended
      }}
      AgentRequest(filter: {{session_id: {{_eq: \"{escaped}\"}}}}, order: {{created_at: ASC}}) {{
        request_id content metadata status lifecycle_state terminalized_at interrupt_requested_at
      }}
      AgentResponse(filter: {{session_id: {{_eq: \"{escaped}\"}}}}, order: {{created_at: ASC}}) {{
        request_id status token_count completed_at interrupted_at error_message
      }}
      AgentMessage(filter: {{session_id: {{_eq: \"{escaped}\"}}}}, order: {{sequence: ASC}}) {{
        request_id sequence role content
      }}
      AgentToolCall(filter: {{session_id: {{_eq: \"{escaped}\"}}}}, order: {{started_at: ASC}}) {{
        request_id tool_call_id tool_name args result status lifecycle_state completed_at
      }}
    }}"""
    request = urllib.request.Request(
        endpoint,
        data=json.dumps({"query": query}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        payload = json.load(response)
    require(not payload.get("errors"), f"GraphQL errors: {payload.get('errors')}")
    data = payload.get("data", {})
    require(data.get("AgentSession"), "no AgentSession document for probe session")
    return data


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, help="Grok leader Unix socket")
    parser.add_argument("--graphql", help="Optional Gents GraphQL endpoint for document assertions")
    parser.add_argument("--cwd", default=str(Path.cwd()), help="session/new cwd")
    parser.add_argument("--timeout", type=float, default=600.0, help="socket timeout seconds")
    parser.add_argument(
        "--model",
        default=os.environ.get("GENTS_GROK_PORT_MODEL", DEFAULT_MODEL),
        help="expected model id (default: GENTS_GROK_PORT_MODEL or pack default)",
    )
    parser.add_argument(
        "--context-window",
        type=int,
        default=int(os.environ.get("GENTS_GROK_PORT_CONTEXT_WINDOW", DEFAULT_CONTEXT_WINDOW)),
        help="expected advertised context window",
    )
    parser.add_argument(
        "--edge",
        choices=("handshake", "prompt", "tool", "subprocess", "cancel", "all"),
        default="all",
    )
    parser.add_argument("--prompt", default="Reply with exactly: gents glm edge probe ok")
    args = parser.parse_args()

    started = time.monotonic()
    client = LeaderClient(args.socket, args.timeout, args.model)
    output: dict[str, Any] = {
        "edge": args.edge,
        "socket": args.socket,
        "model": args.model,
        "context_window": args.context_window,
    }
    try:
        session_id, registered = initialize(client, args.cwd, args.context_window)
        output["session_id"] = session_id
        output["leader"] = {
            "client_id": registered["client_id"],
            "version": registered["leader_binary_version"],
        }
        if args.edge in ("handshake", "all"):
            output["handshake"] = probe_handshake(client, session_id, args.context_window)
        if args.edge in ("prompt", "all"):
            output["prompt"] = probe_prompt(client, session_id, args.prompt)
        if args.edge in ("tool", "all"):
            output["tool"] = probe_tool(client, session_id)
        if args.edge in ("subprocess", "all"):
            output["subprocess"] = probe_subprocess(client, session_id)
        if args.edge in ("cancel", "all"):
            output["cancel"] = probe_cancel(client, session_id)
        if args.graphql:
            documents = query_documents(args.graphql, session_id)
            requests = documents.get("AgentRequest", [])
            responses = documents.get("AgentResponse", [])
            expected_turns = (
                int(args.edge in ("prompt", "all"))
                + int(args.edge in ("tool", "all"))
                + int(args.edge in ("subprocess", "all"))
                + int(args.edge in ("cancel", "all"))
            )
            require(len(requests) == expected_turns, "unexpected AgentRequest count")
            require(len(responses) == expected_turns, "unexpected AgentResponse count")
            if args.edge in ("cancel", "all"):
                require(requests[-1].get("interrupt_requested_at"), "cancel request lacks interrupt marker")
                require(responses[-1].get("interrupted_at"), "cancel response lacks interrupted marker")
            if args.edge in ("tool", "all"):
                require(documents.get("AgentToolCall"), "tool edge lacks AgentToolCall document")
            if args.edge in ("subprocess", "all"):
                subprocess_calls = [
                    call
                    for call in documents.get("AgentToolCall", [])
                    if "gents-subprocess-probe" in call.get("args", "")
                    and "gents-subprocess-probe" in call.get("result", "")
                ]
                require(
                    subprocess_calls,
                    "subprocess edge lacks persisted command and output",
                )
            output["documents"] = {
                "sessions": len(documents.get("AgentSession", [])),
                "requests": len(requests),
                "responses": len(responses),
                "messages": len(documents.get("AgentMessage", [])),
                "tool_calls": len(documents.get("AgentToolCall", [])),
            }
    finally:
        client.close()
    output["elapsed_seconds"] = round(time.monotonic() - started, 3)
    print(json.dumps(output, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, EOFError, OSError, ValueError) as error:
        print(json.dumps({"status": "failed", "error": str(error)}), file=sys.stderr)
        raise SystemExit(1) from error
