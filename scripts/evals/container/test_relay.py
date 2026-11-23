import asyncio
import json
import pathlib
import socket
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import relay  # noqa: E402

LIMITS = relay.Limits(connections=2, head_secs=0.5, body_secs=0.5, body_bytes=4096, log_bytes=4096)


def chat(body):
    if isinstance(body, dict):
        body = {"model": "m", **body}
    data = json.dumps(body).encode()
    return (b"POST /v1/chat/completions HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\n"
            b"content-length: %d\r\n\r\n%s" % (len(data), data))


class Upstream:
    """A model server that records every request it is sent."""

    def __init__(self, stream=False):
        self.requests = []
        self.stream = stream

    async def handle(self, reader, writer):
        try:
            head = await reader.readuntil(b"\r\n\r\n")
        except (asyncio.IncompleteReadError, ConnectionError):
            writer.close()
            return
        length = 0
        for line in head.split(b"\r\n"):
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":", 1)[1])
        body = await reader.readexactly(length)
        self.requests.append(head + body)
        if self.stream:
            writer.write(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
            for index in range(3):
                chunk = b"data: %d\n\n" % index
                writer.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
                await writer.drain()
            writer.write(b"0\r\n\r\n")
        else:
            writer.write(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
        await writer.drain()
        writer.close()


class RelayTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.directory.name)
        self.upstream = Upstream()
        await self.start(LIMITS)

    async def start(self, limits, stream=False):
        self.upstream = Upstream(stream)
        self.model = await asyncio.start_server(self.upstream.handle, "127.0.0.1", 0)
        port = self.model.sockets[0].getsockname()[1]
        self.socket = str(self.root / "r.sock")
        self.log_path = self.root / "relay.jsonl"
        self.server, self.log = await relay.listen(self.socket, port, str(self.log_path), limits=limits, model="m")

    async def asyncTearDown(self):
        self.server.close()
        self.model.close()
        self.log.close()
        self.directory.cleanup()

    async def send(self, raw, wait=None):
        reader, writer = await asyncio.open_unix_connection(self.socket)
        writer.write(raw)
        await writer.drain()
        if wait:
            await asyncio.sleep(wait)
        response = await asyncio.wait_for(reader.read(), 5)
        writer.close()
        await writer.wait_closed()
        return response

    def status(self, response):
        return int(response.split(b" ", 2)[1]) if response else None

    def decisions(self):
        return [json.loads(line)["decision"] for line in self.log_path.read_text().splitlines()]

    async def test_only_the_allowed_routes_reach_the_model(self):
        self.assertEqual(self.status(await self.send(b"GET /v1/models HTTP/1.1\r\nhost: x\r\n\r\n")), 200)
        self.assertEqual(self.status(await self.send(chat({"messages": [{"role": "user", "content": "hi"}]}))), 200)
        for raw in [b"GET /metrics HTTP/1.1\r\n\r\n", b"POST /v1/load_lora_adapter HTTP/1.1\r\ncontent-length: 0\r\n\r\n",
                    b"GET /v1/models/../metrics HTTP/1.1\r\n\r\n", b"DELETE /v1/models HTTP/1.1\r\n\r\n"]:
            self.assertEqual(self.status(await self.send(raw)), 403, raw)
        self.assertEqual(len(self.upstream.requests), 2)
        forwarded = self.upstream.requests[0]
        self.assertIn(b"connection: close\r\n", forwarded)
        self.assertNotIn(b"host: x", forwarded)

    async def test_bodies_must_have_one_plain_length_within_the_limit(self):
        cases = {
            411: b"POST /v1/chat/completions HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n0\r\n\r\n",
            400: b"POST /v1/chat/completions HTTP/1.1\r\ncontent-length: 2\r\ncontent-length: 2\r\n\r\n{}",
            413: b"POST /v1/chat/completions HTTP/1.1\r\ncontent-length: 5000\r\n\r\n",
        }
        for expected, raw in cases.items():
            self.assertEqual(self.status(await self.send(raw)), expected, raw)
        self.assertEqual(self.status(await self.send(
            b"POST /v1/chat/completions HTTP/1.1\r\ncontent-length: +2\r\n\r\n{}")), 400)
        self.assertEqual(self.upstream.requests, [])

    async def test_control_characters_and_folded_headers_are_refused(self):
        smuggled = b"POST /v1/chat/completions HTTP/1.1\r\nx-a: a\nTransfer-Encoding: chunked\r\ncontent-length: 2\r\n\r\n{}"
        for raw in [
            smuggled,
            b"GET /v1/models HTTP/1.1\r\nx-a: a\rb\r\n\r\n",
            b"GET /v1/models HTTP/1.1\r\nx-a: a\x00b\r\n\r\n",
            b"GET /v1/models HTTP/1.1\r\nx-a: a\r\n folded\r\n\r\n",
            b"GET /v1/models\nGET /metrics HTTP/1.1\r\n\r\n",
            b"GET /v1/models\tx HTTP/1.1\r\n\r\n",
            b"GET /v1/models HTTP/1.0\r\n\r\n",
            b"GET v1/models HTTP/1.1\r\n\r\n",
            b"GET /v1/models HTTP/1.1\r\nbad header: x\r\n\r\n",
        ]:
            self.assertEqual(self.status(await self.send(raw)), 400, raw)
        self.assertEqual(self.upstream.requests, [])

    async def test_chat_requests_name_the_target_model_and_carry_only_text_parts(self):
        text = {"messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]}
        self.assertEqual(self.status(await self.send(chat(text))), 200)
        for kind in ["image_url", "video_url", "audio_url", "input_audio", "file"]:
            body = {"messages": [{"role": "user", "content": [{"type": kind, kind: {"url": "http://x/y"}}]}]}
            self.assertEqual(self.status(await self.send(chat(body))), 403, kind)
        for body in [[], {"messages": "x"}, {"messages": [{"content": 7}]}]:
            self.assertEqual(self.status(await self.send(chat(body))), 400, body)
        for model in ["inkling-2-mini-automation-v1", None]:
            body = {"messages": [{"role": "user", "content": "hi"}], "model": model}
            self.assertEqual(self.status(await self.send(chat(body))), 403, model)
        self.assertEqual(self.status(await self.send(
            b"POST /v1/chat/completions HTTP/1.1\r\ncontent-length: 3\r\n\r\n{x}")), 400)
        self.assertEqual(len(self.upstream.requests), 1)

    async def test_slow_requests_time_out(self):
        self.assertEqual(self.status(await self.send(b"GET /v1/models HTTP/1.1\r\n", wait=1)), 408)
        self.assertEqual(self.status(await self.send(
            b"POST /v1/chat/completions HTTP/1.1\r\ncontent-length: 10\r\n\r\n{}", wait=1)), 408)
        self.assertIn("head-timeout", self.decisions())
        self.assertIn("body-timeout", self.decisions())

    async def test_open_connections_are_capped(self):
        held = [await asyncio.open_unix_connection(self.socket) for _ in range(2)]
        for _, writer in held:
            writer.write(b"GET /v1/models HTTP/1.1\r\n")
        await asyncio.sleep(0.1)
        self.assertEqual(self.status(await self.send(b"GET /v1/models HTTP/1.1\r\n\r\n")), 503)
        for _, writer in held:
            writer.close()
            await writer.wait_closed()
        self.assertIn("denied-connections", self.decisions())

    async def test_the_log_is_clipped_and_budgeted(self):
        await self.send(b"GET /" + b"a" * 8000 + b" HTTP/1.1\r\n\r\n")
        entry = json.loads(self.log_path.read_text().splitlines()[-1])
        self.assertLessEqual(len(entry["path"]), relay.LOGGED_CHARS + 3)
        for _ in range(60):
            await self.send(b"GET /metrics HTTP/1.1\r\n\r\n")
        self.assertIn("log-budget-exhausted", self.decisions())
        self.assertLess(self.log_path.stat().st_size, LIMITS.log_bytes + 200)

    async def test_a_streamed_response_passes_through_and_forward_reaches_the_socket(self):
        self.server.close()
        self.model.close()
        self.log.close()
        await self.start(LIMITS, stream=True)
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        forwarder = asyncio.ensure_future(relay.forward(port, self.socket))
        self.addCleanup(forwarder.cancel)
        for _ in range(50):
            try:
                reader, writer = await asyncio.open_connection("127.0.0.1", port)
                break
            except OSError:
                await asyncio.sleep(0.05)
        writer.write(chat({"messages": [{"role": "user", "content": "hi"}], "stream": True}))
        await writer.drain()
        response = await asyncio.wait_for(reader.read(), 5)
        writer.close()
        self.assertEqual(self.status(response), 200)
        for index in range(3):
            self.assertIn(b"data: %d" % index, response)


if __name__ == "__main__":
    unittest.main()
