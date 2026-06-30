#!/usr/bin/env python3
"""Minimal OpenAI-compatible mock inference server for the local demos.

This is a *real* HTTP backend that speaks the subset of the OpenAI API the
defra-agent runtime uses, so the desktop/two-node demos can show chat working
end-to-end without an external model (llama-server, ollama, OpenAI, ...).

It returns canned, deterministic completions, so it proves the request ->
daemon -> owned loop -> response -> replication pipeline, not model quality.
Swap in a hosted preset or a local llama-server for real responses.

The response shape mirrors the project's own live test fixture
(apps/desktop-tauri/src-tauri/src/runner/live_fixture.rs): a content delta
chunk, a finish_reason=stop chunk, then `data: [DONE]`. The runtime must be
told to use the chat-completions route via DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS=1.

Routes:
  GET  /v1/models, /models               -> {"data":[{"id":"<model>"}]}
  POST /v1/chat/completions,
       /chat/completions                 -> text/event-stream SSE completion
  GET  /healthz                          -> 200 ok

Usage:
  demo-mock-inference.py [--host 127.0.0.1] [--port 0] [--model mock-model]
Environment overrides: MOCK_HOST, MOCK_PORT, MOCK_MODEL.
When --port 0 (the default) the chosen port is printed as `listening port=<n>`.
"""

import argparse
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def build_completion_sse(text: str) -> bytes:
    """Two SSE data chunks plus the [DONE] sentinel, OpenAI streaming shape."""
    chunk_1 = {
        "choices": [{"delta": {"content": text}, "finish_reason": None}],
        "usage": None,
    }
    chunk_2 = {
        "choices": [
            {
                "delta": {"content": None, "tool_calls": []},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 24, "completion_tokens": 6, "total_tokens": 30},
    }
    body = "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n".format(
        json.dumps(chunk_1), json.dumps(chunk_2)
    )
    return body.encode("utf-8")


def build_tool_call_sse(tool_name: str, arguments: dict) -> bytes:
    """OpenAI streaming shape for a single function/tool call."""
    chunk_1 = {
        "choices": [
            {
                "delta": {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_mock_0",
                            "type": "function",
                            "function": {
                                "name": tool_name,
                                "arguments": json.dumps(arguments),
                            },
                        }
                    ],
                },
                "finish_reason": None,
            }
        ],
        "usage": None,
    }
    chunk_2 = {
        "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 24, "completion_tokens": 6, "total_tokens": 30},
    }
    body = "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n".format(
        json.dumps(chunk_1), json.dumps(chunk_2)
    )
    return body.encode("utf-8")


def last_user_text(request_json: dict) -> str:
    last_user = ""
    for message in request_json.get("messages", []):
        if message.get("role") == "user":
            content = message.get("content", "")
            if isinstance(content, list):
                # Some clients send content as an array of parts.
                content = "".join(
                    part.get("text", "")
                    for part in content
                    if isinstance(part, dict)
                )
            last_user = content or last_user
    return (last_user or "").strip()


def reply_for(request_json: dict) -> str:
    """Echo the last user message so the demo chat feels responsive."""
    last_user = last_user_text(request_json)
    if not last_user:
        return "Mock inference backend online. Send a message to see it reply."
    return f'Mock backend reply. You said: "{last_user}"'


def offered_tool(request_json: dict, name: str) -> dict:
    """Return the offered tool's function schema by name, or {} if absent."""
    for tool in request_json.get("tools", []) or []:
        fn = tool.get("function", {}) if isinstance(tool, dict) else {}
        if fn.get("name") == name:
            return fn
    return {}


def first_subagent_target(spawn_fn: dict) -> str:
    """Pull the first allowed subagent name from the spawn_subagent schema enum."""
    enum = (
        spawn_fn.get("parameters", {})
        .get("properties", {})
        .get("name", {})
        .get("enum", [])
    )
    return enum[0] if enum else "worker"


def wants_subagent(text: str) -> bool:
    return "subagent" in text.lower() or "delegate" in text.lower()


def planned_tool_call(request_json: dict):
    """Decide whether this turn should call spawn_subagent.

    Fires only when the tool is actually offered, the user asked for delegation,
    and no tool result has come back yet (so the post-result turn answers in
    text). The child prompt deliberately omits the trigger word so a same-DID
    target cannot recurse.
    """
    spawn_fn = offered_tool(request_json, "spawn_subagent")
    if not spawn_fn:
        return None
    messages = request_json.get("messages", []) or []
    if any(m.get("role") == "tool" for m in messages):
        return None
    if not wants_subagent(last_user_text(request_json)):
        return None
    target = first_subagent_target(spawn_fn)
    call = {
        "name": target,
        "prompt": "Reply with one short sentence describing the worker node.",
    }
    # Cross-deployment (remote) targets require background await; foreground is
    # rejected. Prefer background whenever the schema offers it.
    await_enum = (
        spawn_fn.get("parameters", {})
        .get("properties", {})
        .get("await_mode", {})
        .get("enum", [])
    )
    if "background" in await_enum:
        call["await_mode"] = "background"
    return call


class Handler(BaseHTTPRequestHandler):
    # Quiet by default; the launcher captures stderr to a log file.
    def log_message(self, *_args):
        return

    def _send_json(self, status: int, payload: dict):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path in ("/v1/models", "/models"):
            self._send_json(200, {"data": [{"id": self.server.model_name}]})
        elif path == "/healthz":
            self._send_json(200, {"status": "ok"})
        else:
            self._send_json(404, {"error": "not found"})

    def do_POST(self):
        path = self.path.split("?", 1)[0]
        if path not in ("/v1/chat/completions", "/chat/completions"):
            self._send_json(404, {"error": "not found"})
            return
        length = int(self.headers.get("content-length", 0) or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            request_json = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            self._send_json(400, {"error": "invalid json"})
            return
        call = planned_tool_call(request_json)
        if call is not None:
            sse = build_tool_call_sse("spawn_subagent", call)
        else:
            sse = build_completion_sse(reply_for(request_json))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("content-length", str(len(sse)))
        self.end_headers()
        self.wfile.write(sse)


def main() -> int:
    parser = argparse.ArgumentParser(description="OpenAI-compatible mock backend")
    parser.add_argument("--host", default=os.environ.get("MOCK_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port", type=int, default=int(os.environ.get("MOCK_PORT", "0"))
    )
    parser.add_argument(
        "--model", default=os.environ.get("MOCK_MODEL", "mock-model")
    )
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    server.model_name = args.model
    host, port = server.server_address[0], server.server_address[1]
    # Stable, parseable line so a launcher can capture the chosen ephemeral port.
    print(f"listening host={host} port={port} model={args.model}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
