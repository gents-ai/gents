import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createNativeProviderGate } from "./native-local-provider-gate.mjs";

async function waitHeld(gate) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if ((await (await fetch(`${gate.endpoint}/__e2e_gate`)).json()).held) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  throw new Error("upstream response did not reach held state");
}

const listen = (server) =>
  new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

test("gate preserves actual upstream bytes, holds only exact main prompt, and releases", async () => {
  const directory = await mkdtemp(join(tmpdir(), "native-provider-gate-"));
  const requests = [];
  let mainReached;
  const mainAtUpstream = new Promise((resolve) => {
    mainReached = resolve;
  });
  const responseBytes = Buffer.from(
    'data: {"choices":[{"delta":{"content":"REAL"}}]}\n\ndata: [DONE]\n\n',
  );
  const upstream = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = Buffer.concat(chunks);
    requests.push({
      url: request.url,
      body,
      authorization: request.headers.authorization,
    });
    if (body.toString().includes('"content":"MAIN"')) mainReached();
    response.writeHead(200, {
      "Content-Type": "text/event-stream",
      "X-Upstream": "real",
    });
    response.end(responseBytes);
  });
  await listen(upstream);
  const gate = await createNativeProviderGate(
    `http://127.0.0.1:${upstream.address().port}/v1`,
    join(directory, "requests.jsonl"),
  );
  try {
    gate.arm("MAIN");
    const options = await fetch(`${gate.endpoint}/__e2e_gate`, { method: "OPTIONS" });
    assert.equal(options.status, 204);
    assert.equal(options.headers.get("access-control-allow-origin"), "*");
    const title = await fetch(`${gate.endpoint}/chat/completions`, {
      method: "POST",
      body: '{"messages":[{"role":"user","content":"Title for MAIN"}]}',
    });
    assert.deepEqual(Buffer.from(await title.arrayBuffer()), responseBytes);
    assert.deepEqual(await (await fetch(`${gate.endpoint}/__e2e_gate`)).json(), {
      held: false,
      released: false,
    });
    const mainBytes =
      '{ "messages": [{"role":"user","content":"MAIN"}], "stream":true }';
    let delivered = false;
    const main = fetch(`${gate.endpoint}/chat/completions`, {
      method: "POST",
      headers: { Authorization: "Bearer test-only" },
      body: mainBytes,
    }).then((response) => {
      delivered = true;
      return response;
    });
    await mainAtUpstream;
    // Status is set when upstream response headers arrive, not merely on local receipt.
    await waitHeld(gate);
    assert.equal(delivered, false);
    assert.equal(requests[1].url, "/v1/chat/completions");
    assert.deepEqual(requests[1].body, Buffer.from(mainBytes));
    assert.equal(requests[1].authorization, "Bearer test-only");
    const released = await fetch(`${gate.endpoint}/__e2e_gate`, { method: "POST" });
    assert.deepEqual(await released.json(), { held: true, released: true });
    const actual = await main;
    assert.equal(actual.headers.get("x-upstream"), "real");
    assert.deepEqual(Buffer.from(await actual.arrayBuffer()), responseBytes);
    const evidence = await readFile(join(directory, "requests.jsonl"), "utf8");
    assert.equal(evidence.trim().split("\n").length, 2);
    assert.ok(!evidence.includes("Bearer"));
  } finally {
    await gate.close();
    upstream.closeAllConnections();
    await new Promise((resolve) => upstream.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test("cleanup aborts a held upstream response and closes owned sockets", async () => {
  const directory = await mkdtemp(join(tmpdir(), "native-provider-cleanup-"));
  const upstream = createServer((_request, response) =>
    response.end("actual upstream"),
  );
  await listen(upstream);
  const gate = await createNativeProviderGate(
    `http://127.0.0.1:${upstream.address().port}/v1`,
    join(directory, "requests.jsonl"),
  );
  gate.arm("MAIN");
  const pending = fetch(`${gate.endpoint}/chat/completions`, {
    method: "POST",
    body: '{"messages":[{"role":"user","content":"MAIN"}]}',
  });
  const aborted = assert.rejects(pending, TypeError);
  try {
    await waitHeld(gate);
    await gate.close();
    await aborted;
    await assert.rejects(fetch(`${gate.endpoint}/__e2e_gate`));
  } finally {
    upstream.closeAllConnections();
    await new Promise((resolve) => upstream.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

for (const action of ["cancel downstream", "close gate"]) {
  test(`open streaming upstream closes on ${action}`, async () => {
    const directory = await mkdtemp(join(tmpdir(), "native-provider-open-stream-"));
    let upstreamClosed;
    const closed = new Promise((resolve) => {
      upstreamClosed = resolve;
    });
    const upstream = createServer((_request, response) => {
      response.once("close", upstreamClosed);
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      response.write("data: still streaming\n\n");
    });
    await listen(upstream);
    const gate = await createNativeProviderGate(
      `http://127.0.0.1:${upstream.address().port}/v1`,
      join(directory, "requests.jsonl"),
    );
    let timer;
    let gateClosed = false;
    try {
      const response = await fetch(`${gate.endpoint}/chat/completions`, {
        method: "POST",
        body: '{"messages":[{"role":"user","content":"ungated"}]}',
      });
      const reader = response.body.getReader();
      const first = await reader.read();
      assert.equal(new TextDecoder().decode(first.value), "data: still streaming\n\n");
      if (action === "cancel downstream") await reader.cancel();
      else {
        await gate.close();
        gateClosed = true;
      }
      await Promise.race([
        closed,
        new Promise((_, reject) => {
          timer = setTimeout(
            () => reject(new Error("upstream stream remained open")),
            3000,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
      if (!gateClosed) await gate.close();
      upstream.closeAllConnections();
      await new Promise((resolve) => upstream.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  });
}
