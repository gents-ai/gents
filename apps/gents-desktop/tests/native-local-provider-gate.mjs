import { createServer, request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";
import { appendFile } from "node:fs/promises";

/** Test-only transparent transport: hold one real provider response, never synthesize it. */
export async function createNativeProviderGate(upstream, evidencePath) {
  const base = new URL(upstream.endsWith("/") ? upstream : `${upstream}/`);
  let marker = "";
  let held = false;
  let reserved = false;
  let released = false;
  let release;
  let barrier = Promise.resolve();
  const captures = [];
  const sockets = new Set();
  const upstreamStreams = new Set();
  const server = createServer(async (incoming, outgoing) => {
    outgoing.setHeader("Access-Control-Allow-Origin", "*");
    outgoing.setHeader("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    outgoing.setHeader("Access-Control-Allow-Headers", "Content-Type");
    if (incoming.method === "OPTIONS") {
      outgoing.writeHead(204).end();
      return;
    }
    const path = new URL(incoming.url, "http://localhost").pathname;
    if (path.endsWith("/__e2e_gate")) {
      if (incoming.method === "POST") {
        released = true;
        release?.();
      }
      outgoing.setHeader("Content-Type", "application/json");
      outgoing.end(JSON.stringify({ held, released }));
      return;
    }
    try {
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const body = Buffer.concat(chunks);
      let parsed;
      try {
        parsed = JSON.parse(body.toString("utf8"));
      } catch {
        /* catalog GET */
      }
      if (parsed?.messages) {
        captures.push(parsed);
        await appendFile(evidencePath, `${JSON.stringify(parsed)}\n`);
      }
      const gateThis =
        !reserved &&
        marker &&
        parsed?.messages?.some(
          (message) =>
            message.role === "user" &&
            (message.content === marker ||
              (Array.isArray(message.content) &&
                message.content.some((part) => part.text === marker))),
        );
      if (gateThis) reserved = true;
      const target = new URL(base);
      target.pathname = path;
      target.search = new URL(incoming.url, "http://localhost").search;
      const headers = { ...incoming.headers, host: target.host };
      delete headers.connection;
      let upstreamResponse;
      const forward = (target.protocol === "https:" ? httpsRequest : httpRequest)(
        target,
        {
          method: incoming.method,
          headers,
        },
        async (response) => {
          upstreamResponse = response;
          upstreamStreams.add(response);
          response.once("close", () => upstreamStreams.delete(response));
          response.on("error", () => outgoing.destroy());
          response.pause();
          if (gateThis) held = true;
          if (gateThis) await barrier;
          if (outgoing.destroyed) {
            response.destroy();
            return;
          }
          const responseHeaders = { ...response.headers };
          delete responseHeaders["access-control-allow-origin"];
          outgoing.writeHead(response.statusCode ?? 502, responseHeaders);
          response.pipe(outgoing);
        },
      );
      upstreamStreams.add(forward);
      forward.once("close", () => upstreamStreams.delete(forward));
      outgoing.once("close", () => {
        upstreamResponse?.destroy();
        forward.destroy();
      });
      forward.on("error", () => {
        if (outgoing.destroyed) return;
        if (!outgoing.headersSent) outgoing.writeHead(502);
        outgoing.end();
      });
      incoming.on("aborted", () => forward.destroy());
      forward.end(body);
    } catch {
      if (!outgoing.headersSent) outgoing.writeHead(502);
      outgoing.end();
    }
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const endpoint = `http://127.0.0.1:${server.address().port}${base.pathname.replace(/\/$/, "")}`;
  return {
    endpoint,
    arm(value) {
      marker = value;
      reserved = false;
      held = false;
      released = false;
      captures.length = 0;
      barrier = new Promise((resolve) => {
        release = resolve;
      });
    },
    captures,
    async close() {
      release?.();
      for (const stream of upstreamStreams) stream.destroy();
      for (const socket of sockets) socket.destroy();
      await new Promise((resolve) => server.close(resolve));
    },
  };
}
