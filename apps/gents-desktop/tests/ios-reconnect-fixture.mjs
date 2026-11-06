import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { createConnection, createServer } from "node:net";
import { networkInterfaces } from "node:os";
import { join } from "node:path";

async function listen(server, host = "127.0.0.1") {
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, host, resolve);
  });
  return server.address().port;
}

function simulatorHost() {
  const interfaces = networkInterfaces();
  const candidates = Object.entries(interfaces)
    .sort(([left], [right]) => {
      const rank = (name) => (name === "en0" ? 0 : name.startsWith("en") ? 1 : 2);
      return rank(left) - rank(right);
    })
    .flatMap(([, addresses]) => addresses ?? [])
    .filter((address) => address.family === "IPv4" && !address.internal)
    .map((address) => address.address);
  const requested = process.env.GENTS_IOS_RECONNECT_HOST?.trim();
  if (requested && !candidates.includes(requested)) {
    throw new Error(
      "GENTS_IOS_RECONNECT_HOST must be an assigned non-loopback IPv4 address",
    );
  }
  const host = requested ?? candidates[0];
  if (!host) {
    throw new Error(
      "The reconnect fixture requires an assigned non-loopback IPv4 interface reachable by the simulator",
    );
  }
  return host;
}

async function allocatePort() {
  const server = createServer();
  const port = await listen(server);
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  return port;
}

function checked(binary, args, env) {
  const result = spawnSync(binary, args, { env, encoding: "utf8", timeout: 180_000 });
  if (result.status !== 0) {
    throw new Error(
      `Fixture command ${args.join(" ")} failed: ${result.error?.message ?? result.stderr ?? result.status}`,
    );
  }
  return result.stdout;
}

export async function createIosReconnectFixture(
  binary,
  artifactRoot,
  signal,
  inferenceArgs,
) {
  const root = await mkdtemp(join(artifactRoot, "reconnect-runtime-"));
  const home = join(root, "node");
  const owner = join(root, "owner");
  const env = { ...process.env, GENTS_DESKTOP_HOME: owner, RUST_LOG: "info" };
  let server;
  let proxy;
  let log;
  const sockets = new Set();
  let online = true;
  let agentEvidence = null;
  const cleanup = async () => {
    online = false;
    for (const socket of sockets) socket.destroy();
    if (proxy?.listening) await new Promise((resolve) => proxy.close(resolve));
    if (server?.pid && server.exitCode === null && server.signalCode === null) {
      const exited = new Promise((resolve) => server.once("exit", resolve));
      server.kill("SIGTERM");
      await Promise.race([
        exited,
        new Promise((resolve) => setTimeout(resolve, 5_000)),
      ]);
      if (server.exitCode === null && server.signalCode === null) {
        server.kill("SIGKILL");
        await exited;
      }
    }
    log?.end();
    await rm(root, { recursive: true, force: true });
  };
  try {
    await mkdir(home, { recursive: true });
    await mkdir(owner, { recursive: true });
    signal?.throwIfAborted();
    const host = simulatorHost();
    const init = JSON.parse(
      checked(
        binary,
        [
          "init",
          "--home",
          home,
          "--store-key-custody",
          "file",
          "--node-name",
          "iOS Reconnect Fixture",
          ...inferenceArgs,
          "--tool-root",
          root,
        ],
        env,
      ),
    );
    const nodeDid = init.node_did;
    const agentDid = init.default_agent_did;
    const profile = init.inference_profile_id;
    if (
      ![nodeDid, agentDid, profile].every(
        (value) => typeof value === "string" && value.length,
      )
    ) {
      throw new Error(
        "Fixture init did not return its node, agent, and profile identities",
      );
    }
    const runtimePort = await allocatePort();
    proxy = createServer((client) => {
      if (!online) return client.destroy();
      const upstream = createConnection({ host: "127.0.0.1", port: runtimePort });
      let request = "";
      let agentStream = false;
      client.on("data", (chunk) => {
        if (agentStream || request.length >= 16_384) return;
        request += chunk.toString("utf8");
        agentStream = request.includes("subscription { Agent(");
      });
      upstream.on("data", (chunk) => {
        if (agentStream && agentEvidence !== null) {
          agentEvidence = (agentEvidence + chunk.toString("utf8")).slice(-1_000_000);
        }
      });
      sockets.add(client);
      sockets.add(upstream);
      client.on("close", () => sockets.delete(client));
      upstream.on("close", () => sockets.delete(upstream));
      client.on("error", () => upstream.destroy());
      upstream.on("error", () => client.destroy());
      client.pipe(upstream).pipe(client);
    });
    const proxyPort = await listen(proxy, host);
    const graphql = `http://127.0.0.1:${runtimePort}/api/v0/graphql`;
    const advertised = `http://${host}:${proxyPort}/api/v0/graphql`;
    const statusUrl = `http://${host}:${proxyPort}`;
    const runtimeStatusUrl = `http://127.0.0.1:${runtimePort}`;
    log = createWriteStream(join(artifactRoot, "reconnect-runtime.log"));
    server = spawn(
      binary,
      [
        "server",
        "--home",
        home,
        "--http-addr",
        "127.0.0.1",
        "--http-port",
        String(runtimePort),
        "--advertise-graphql-url",
        advertised,
        "--no-codex-shim",
        "--p2p-port",
        "0",
      ],
      { env, stdio: ["ignore", "pipe", "pipe"] },
    );
    server.stdout.pipe(log, { end: false });
    server.stderr.pipe(log, { end: false });
    let spawnError;
    server.once("error", (error) => {
      spawnError = error;
    });
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      signal?.throwIfAborted();
      if (spawnError) throw spawnError;
      if (server.exitCode !== null || server.signalCode !== null)
        throw new Error("Isolated runtime exited; see reconnect-runtime.log");
      try {
        const response = await fetch(`${runtimeStatusUrl}/status`);
        if (response.ok) break;
      } catch {
        /* Await the runtime's status endpoint. */
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    if (Date.now() >= deadline)
      throw new Error(
        "Isolated runtime status did not become ready; see reconnect-runtime.log",
      );
    const correlationId = `ios-reconnect-${randomBytes(8).toString("hex")}`;
    return {
      correlationId,
      home,
      owner,
      statusUrl,
      setOnline(value) {
        online = value;
        if (!online) for (const socket of sockets) socket.destroy();
      },
      writeName(name) {
        const result = JSON.parse(
          checked(
            binary,
            [
              "config",
              "agent",
              "set",
              "--graphql",
              graphql,
              "--node-did",
              nodeDid,
              "--agent-did",
              agentDid,
              "--display-name",
              name,
              "--context-id",
              `${agentDid}-context`,
              "--inference-profile-id",
              profile,
            ],
            env,
          ),
        );
        if (typeof result.doc_id !== "string" || !result.doc_id)
          throw new Error("Agent owner write returned no document ID");
        return result.doc_id;
      },
      beginAgentEvidence() {
        agentEvidence = "";
      },
      observedAgentDocument(id) {
        return agentEvidence?.includes(id) ?? false;
      },
      cleanup,
    };
  } catch (error) {
    await cleanup();
    throw error;
  }
}
