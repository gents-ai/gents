"""Model access for a `--network none` eval container.

`serve` runs on the docker host: it listens on a unix socket mounted into one
trial's container and forwards to one loopback model port. It admits only the
inference requests a trial makes (`--allow METHOD PATH`, default chat
completions and the model list), one request per connection, and only after
the whole request parsed strictly: a token method, a visible-ASCII target, no
control characters in any header, one Content-Length and no Transfer-Encoding,
a body within the limit and, for chat completions, the target's model and only
text content parts, so the model server never fetches a URL for a trial or
serves it another model or adapter. Connections, read times and log volume are
bounded per relay. `forward` runs inside the container: the
target file's `http://127.0.0.1:<port>` endpoint reaches the socket through it.
"""

import argparse
import asyncio
import dataclasses
import json
import os
import re
import sys
import time

CHUNK = 64 * 1024
DEFAULT_ALLOW = [("POST", "/v1/chat/completions"), ("GET", "/v1/models")]
HOP_HEADERS = {b"connection", b"keep-alive", b"proxy-connection", b"te", b"upgrade", b"expect", b"host"}
TOKEN = re.compile(rb"^[!#$%&'*+.^_`|~0-9A-Za-z-]+$")
TARGET = re.compile(rb"^/[\x21-\x7e]*$")
VALUE = re.compile(rb"^[\t\x20-\x7e\x80-\xff]*$")
TEXT_PARTS = {"text"}
LOGGED_CHARS = 120


@dataclasses.dataclass(frozen=True)
class Limits:
    connections: int = 8
    head_bytes: int = 16 * 1024
    body_bytes: int = 8 * 1024 * 1024
    head_secs: float = 30
    body_secs: float = 120
    connect_secs: float = 10
    upstream_idle_secs: float = 900
    connection_secs: float = 3600
    log_bytes: int = 2 * 1024 * 1024


class Refused(Exception):
    def __init__(self, status, reason, decision):
        super().__init__(reason)
        self.status, self.reason, self.decision = status, reason, decision


async def pipe(reader, writer, idle_secs=None):
    sent = 0
    try:
        while True:
            chunk = await asyncio.wait_for(reader.read(CHUNK), idle_secs)
            if not chunk:
                break
            writer.write(chunk)
            await writer.drain()
            sent += len(chunk)
    except (ConnectionError, asyncio.IncompleteReadError, asyncio.TimeoutError):
        pass
    finally:
        try:
            if writer.can_write_eof():
                writer.write_eof()
        except (OSError, RuntimeError):
            pass
    return sent


async def close(writer):
    writer.close()
    try:
        await writer.wait_closed()
    except (ConnectionError, OSError):
        pass


async def linger(reader, writer, secs=1.0, limit=1 << 20):
    """Half-close after an early answer and drain what the client already sent,
    within `secs` and `limit` bytes: closing over unread input makes Linux
    reset the connection and discard the answer."""
    try:
        await writer.drain()
        if writer.can_write_eof():
            writer.write_eof()
        drained = 0
        while drained < limit:
            chunk = await asyncio.wait_for(reader.read(CHUNK), secs)
            if not chunk:
                break
            drained += len(chunk)
    except (ConnectionError, OSError, asyncio.TimeoutError, asyncio.IncompleteReadError, RuntimeError):
        pass


def clipped(raw):
    text = raw.decode("latin-1") if isinstance(raw, bytes) else str(raw)
    return text if len(text) <= LOGGED_CHARS else text[:LOGGED_CHARS] + "..."


def respond(writer, status, reason):
    body = json.dumps({"error": reason}).encode()
    writer.write(
        b"HTTP/1.1 %d %s\r\ncontent-type: application/json\r\ncontent-length: %d\r\n"
        b"connection: close\r\n\r\n%s" % (status, reason.encode(), len(body), body)
    )


def checked_chat(body, model):
    """Refuse a chat request for another model than `model`, or with any
    non-text content part (an image, audio or file URL the server could fetch)."""
    try:
        request = json.loads(body)
    except (ValueError, UnicodeDecodeError):
        raise Refused(400, "Bad Request", "denied-json") from None
    if not isinstance(request, dict) or not isinstance(request.get("messages"), list):
        raise Refused(400, "Bad Request", "denied-json")
    if model is not None and request.get("model") != model:
        raise Refused(403, "Forbidden", f"denied-model-{clipped(str(request.get('model')))}")
    for message in request["messages"]:
        content = message.get("content") if isinstance(message, dict) else None
        if content is None or isinstance(content, str):
            continue
        if not isinstance(content, list):
            raise Refused(400, "Bad Request", "denied-json")
        for part in content:
            kind = part.get("type") if isinstance(part, dict) else None
            if kind not in TEXT_PARTS:
                raise Refused(403, "Forbidden", f"denied-content-{clipped(str(kind))}")


def parse_head(head, allow):
    lines = head[:-4].split(b"\r\n")
    parts = lines[0].split(b" ")
    if len(parts) != 3 or parts[2] != b"HTTP/1.1" or not TOKEN.match(parts[0]) or not TARGET.match(parts[1]):
        raise Refused(400, "Bad Request", "malformed")
    method, target, _ = parts
    path = target.split(b"?", 1)[0]
    headers = []
    for line in lines[1:]:
        name, colon, value = line.partition(b":")
        if not colon or not TOKEN.match(name) or not VALUE.match(value):
            raise Refused(400, "Bad Request", "malformed-header")
        headers.append((name.lower(), value.strip(b" \t")))
    if (method, path) not in allow:
        raise Refused(403, "Forbidden", "denied")
    if any(name == b"transfer-encoding" for name, _ in headers):
        raise Refused(411, "Length Required", "denied-chunked")
    lengths = [value for name, value in headers if name == b"content-length"]
    if len(lengths) > 1 or (lengths and not lengths[0].isdigit()):
        raise Refused(400, "Bad Request", "denied-length")
    return method, target, path, headers, int(lengths[0]) if lengths else 0


class Filter:
    def __init__(self, port, allow, log, limits, model):
        self.port = port
        self.model = model
        self.allow = {(method.encode(), path.encode()) for method, path in allow}
        self.log = log
        self.limits = limits
        self.active = 0
        self.logged = 0
        self.suppressed = 0

    def record(self, **fields):
        fields["at"] = round(time.time(), 3)
        line = json.dumps(fields) + "\n"
        if self.logged + len(line) > self.limits.log_bytes:
            if self.suppressed == 0:
                self.log.write(json.dumps({"decision": "log-budget-exhausted", "at": fields["at"]}) + "\n")
                self.log.flush()
            self.suppressed += 1
            return
        self.logged += len(line)
        self.log.write(line)
        self.log.flush()

    async def handle(self, reader, writer):
        if self.active >= self.limits.connections:
            respond(writer, 503, "Service Unavailable")
            self.record(decision="denied-connections", open=self.active)
            await linger(reader, writer)
            await close(writer)
            return
        self.active += 1
        try:
            await asyncio.wait_for(self.serve(reader, writer), self.limits.connection_secs)
        except asyncio.TimeoutError:
            self.record(decision="connection-timeout")
        finally:
            self.active -= 1
            await close(writer)

    async def serve(self, reader, writer):
        started = time.monotonic()
        line = {}
        try:
            try:
                head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), self.limits.head_secs)
            except asyncio.TimeoutError:
                raise Refused(408, "Request Timeout", "head-timeout") from None
            except asyncio.LimitOverrunError:
                raise Refused(431, "Request Header Fields Too Large", "denied-head-size") from None
            except (asyncio.IncompleteReadError, ConnectionError):
                return
            seen = head.split(b"\r\n", 1)[0].split(b" ")
            line = {"method": clipped(seen[0]), "path": clipped(seen[1].split(b"?", 1)[0] if len(seen) > 1 else b"")}
            method, target, path, headers, length = parse_head(head, self.allow)
            if length > self.limits.body_bytes:
                raise Refused(413, "Payload Too Large", "denied-size")
            try:
                body = await asyncio.wait_for(reader.readexactly(length), self.limits.body_secs)
            except asyncio.TimeoutError:
                raise Refused(408, "Request Timeout", "body-timeout") from None
            except (asyncio.IncompleteReadError, ConnectionError):
                return
            if path == b"/v1/chat/completions":
                checked_chat(body, self.model)
            try:
                upstream_reader, upstream_writer = await asyncio.wait_for(
                    asyncio.open_connection("127.0.0.1", self.port), self.limits.connect_secs
                )
            except (asyncio.TimeoutError, OSError) as error:
                raise Refused(502, "Bad Gateway", f"upstream-error {clipped(str(error))}") from None
        except Refused as refusal:
            respond(writer, refusal.status, refusal.reason)
            self.record(decision=refusal.decision, status=refusal.status, **line)
            await linger(reader, writer)
            return
        kept = [(name, value) for name, value in headers if name not in HOP_HEADERS]
        request = b"%s %s HTTP/1.1\r\nhost: 127.0.0.1:%d\r\n" % (method, target, self.port)
        request += b"".join(b"%s: %s\r\n" % pair for pair in kept)
        request += b"connection: close\r\n\r\n" + body
        upstream_writer.write(request)
        await upstream_writer.drain()
        received = await pipe(upstream_reader, writer, self.limits.upstream_idle_secs)
        await close(upstream_writer)
        self.record(
            decision="forwarded",
            request_bytes=len(body),
            response_bytes=received,
            secs=round(time.monotonic() - started, 3),
            **line,
        )


async def listen(socket_path, port, log_path, allow=DEFAULT_ALLOW, limits=Limits(), model=None):
    """Open a filtering relay; the caller closes the server and its log."""
    if os.path.lexists(socket_path):
        os.unlink(socket_path)
    log = open(log_path, "a", encoding="utf-8")
    relay = Filter(port, allow, log, limits, model)
    previous = os.umask(0o077)
    try:
        server = await asyncio.start_unix_server(relay.handle, socket_path, limit=limits.head_bytes)
    finally:
        os.umask(previous)
    relay.record(decision="listening", port=port)
    return server, log


async def serve(socket_path, port, allow, log_path):
    server, log = await listen(socket_path, port, log_path, allow)
    with log:
        async with server:
            await server.serve_forever()


async def forward(port, socket_path):
    async def handle(reader, writer):
        try:
            upstream_reader, upstream_writer = await asyncio.open_unix_connection(socket_path)
        except OSError:
            await close(writer)
            return
        await asyncio.gather(pipe(reader, upstream_writer), pipe(upstream_reader, writer))
        await close(upstream_writer)
        await close(writer)

    server = await asyncio.start_server(handle, "127.0.0.1", port)
    async with server:
        await server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    host = commands.add_parser("serve")
    host.add_argument("--socket", required=True)
    host.add_argument("--port", type=int, required=True)
    host.add_argument("--log", required=True)
    host.add_argument("--allow", nargs=2, action="append", metavar=("METHOD", "PATH"))
    guest = commands.add_parser("forward")
    guest.add_argument("--socket", required=True)
    guest.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    if args.command == "serve":
        asyncio.run(serve(args.socket, args.port, args.allow or DEFAULT_ALLOW, args.log))
    else:
        asyncio.run(forward(args.port, args.socket))


if __name__ == "__main__":
    sys.exit(main())
