import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const DEFAULT_PEERS = [
  ["studio-1", "http://100.69.4.79:9491"],
  ["studio-2", "http://100.76.203.120:9491"],
  ["mini-1", "http://100.102.157.108:9191"],
  ["mini-2", "http://100.107.77.21:9191"],
];

const READY_TIMEOUT_MS = 120_000;
const FETCH_TIMEOUT_MS = 75_000;
const SYNC_TIMEOUT_MS = 45_000;
const CHAT_TIMEOUT_MS = 90_000;

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "../../..");

const argv = [...process.argv.slice(2)];
const shouldSendChat = takeBooleanFlag(argv, "--send");
const keepRunner = takeBooleanFlag(argv, "--keep-runner");
const jsonOutput = takeBooleanFlag(argv, "--json");
const peerOverrides = takeRepeatedFlag(argv, "--peer");

if (argv.length > 0) {
  throw new Error(`unknown argument(s): ${argv.join(" ")}`);
}

const peers = peerOverrides.length > 0 ? peerOverrides.map(parsePeerArg) : DEFAULT_PEERS;

const runnerLogs = [];

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exit(1);
});

async function main() {
  const startedAt = new Date().toISOString();
  log(`remote fleet smoke starting (${shouldSendChat ? "sync + chat" : "sync only"})`);

  const resolvedPeers = [];
  for (const [label, serverAddress] of peers) {
    const peer = await resolvePeer(label, serverAddress);
    resolvedPeers.push(peer);
    log(
      `${label}: status ok, did=${peer.agentDid}, graphql=${peer.graphql}, addr=${shorten(peer.addr, 90)}`,
    );
  }

  const runner = await startBridgeRunner();
  const results = [];
  try {
    log(`bridge runner ready at ${runner.baseUrl}`);
    for (const peer of resolvedPeers) {
      const result = await smokePeer(runner.baseUrl, peer);
      results.push(result);
      const counts = result.syncedCounts;
      log(
        `${peer.label}: synced behaviors=${counts.behaviors} tasks=${counts.tasks} conversations=${counts.conversations} backends=${counts.inferenceBackends}`,
      );
      if (result.chat) {
        log(
          `${peer.label}: chat request=${result.chat.requestId} local=${result.chat.localAccepted} remote=${result.chat.remoteVisible}`,
        );
      }
    }
  } finally {
    if (keepRunner) {
      log(`leaving bridge runner alive at ${runner.baseUrl}`);
    } else {
      await stopBridgeRunner(runner.process);
    }
  }

  const runnerErrors = runnerLogs.filter((line) =>
    /\b(ERROR|panic|panicked)\b|Block merge failed/i.test(line),
  );
  const failed = results.flatMap((result) => result.failures);
  if (runnerErrors.length > 0) {
    failed.push(`bridge runner emitted ${runnerErrors.length} error log line(s)`);
  }

  const summary = {
    startedAt,
    completedAt: new Date().toISOString(),
    mode: shouldSendChat ? "sync+chat" : "sync",
    peers: results,
    runnerErrors: runnerErrors.slice(-20),
    ok: failed.length === 0,
    failures: failed,
  };

  if (jsonOutput) {
    console.log(JSON.stringify(summary, null, 2));
  } else {
    printSummary(summary);
  }

  if (!summary.ok) {
    process.exit(1);
  }
}

async function smokePeer(baseUrl, peer) {
  const failures = [];
  const addSnapshot = await postJson(`${baseUrl}/desktop/peer/add`, {
    label: peer.label,
    agentDid: peer.agentDid,
    addr: peer.addr,
    graphql: peer.graphql,
  });

  let deployment = findDeployment(addSnapshot, peer.agentDid);
  if (!deployment) {
    deployment = await waitForDeployment(baseUrl, peer.agentDid, SYNC_TIMEOUT_MS);
  }

  if (!deployment) {
    return {
      label: peer.label,
      agentDid: peer.agentDid,
      graphql: peer.graphql,
      syncedCounts: emptyCounts(),
      chat: null,
      failures: [`${peer.label}: deployment did not appear after peer add`],
    };
  }

  const counts = deploymentCounts(deployment);
  if (counts.behaviors < 1) {
    failures.push(`${peer.label}: no behaviors synced`);
  }
  if (counts.tasks < 1) {
    failures.push(`${peer.label}: no tasks synced`);
  }
  if (counts.inferenceBackends < 1) {
    failures.push(`${peer.label}: no inference backends synced`);
  }

  let chat = null;
  if (shouldSendChat) {
    chat = await smokeChat(baseUrl, peer, deployment);
    if (!chat.localAccepted) {
      failures.push(`${peer.label}: desktop did not accept chat send`);
    }
    if (!chat.remoteVisible) {
      failures.push(
        `${peer.label}: remote GraphQL did not show new AgentRequest ${chat.requestId}`,
      );
    }
  }

  return {
    label: peer.label,
    agentDid: peer.agentDid,
    graphql: peer.graphql,
    displayName: deployment.agentPrincipal?.displayName ?? deployment.label,
    syncedCounts: counts,
    chat,
    failures,
  };
}

async function smokeChat(baseUrl, peer, deployment) {
  const behaviorId =
    deployment.defaultBehaviorId ||
    deployment.behaviors?.find((behavior) => behavior.isDefault)?.behaviorId ||
    deployment.behaviors?.[0]?.behaviorId ||
    null;
  const content = [
    "Desktop remote smoke test.",
    `Peer: ${peer.label}.`,
    `Timestamp: ${new Date().toISOString()}.`,
    "Please reply with exactly one short sentence.",
  ].join(" ");

  try {
    const sent = await postJson(`${baseUrl}/desktop/chat/send`, {
      agentDid: peer.agentDid,
      behaviorId,
      sessionId: null,
      content,
    });
    const localSession = await waitForLocalSession(
      baseUrl,
      peer.agentDid,
      sent.sessionId,
      sent.requestId,
      CHAT_TIMEOUT_MS,
    );
    const remote = await waitForRemoteRequest(
      peer.graphql,
      sent.sessionId,
      sent.requestId,
      CHAT_TIMEOUT_MS,
    );
    return {
      localAccepted: Boolean(sent.sessionId && sent.requestId),
      sessionId: sent.sessionId,
      requestId: sent.requestId,
      behaviorId: sent.behaviorId ?? behaviorId,
      localTurnState: localSession?.turnState ?? null,
      localTimelineItems: localSession?.timelineItems?.length ?? 0,
      remoteVisible: Boolean(remote?.request),
      remoteLifecycleState: remote?.request?.lifecycle_state ?? null,
      remoteStatus: remote?.request?.status ?? null,
      remoteResponseStatus: remote?.response?.status ?? null,
      remoteResponseError: remote?.response?.error_message ?? null,
    };
  } catch (error) {
    return {
      localAccepted: false,
      sessionId: null,
      requestId: null,
      behaviorId,
      localTurnState: null,
      localTimelineItems: 0,
      remoteVisible: false,
      remoteLifecycleState: null,
      remoteStatus: null,
      remoteResponseStatus: null,
      remoteResponseError: String(error.message || error),
    };
  }
}

async function resolvePeer(label, serverAddress) {
  const statusUrl = normalizeStatusUrl(serverAddress);
  const status = await getJson(statusUrl);
  const agentDid = stringValue(status.agent_did || status.agentDid);
  if (!agentDid) {
    throw new Error(`${label}: status response is missing agent_did`);
  }
  const graphql = normalizeGraphqlUrl(status, statusUrl);
  const addr = selectPeerAddr(status, statusUrl);
  if (!graphql) {
    throw new Error(`${label}: status response is missing GraphQL endpoint`);
  }
  if (!addr) {
    throw new Error(`${label}: status response is missing P2P address`);
  }
  return {
    label,
    serverAddress,
    statusUrl,
    agentDid,
    agentName: stringValue(status.agent_name || status.agentName) || label,
    graphql,
    addr,
  };
}

function normalizeStatusUrl(serverAddress) {
  const trimmed = serverAddress.trim();
  const url = new URL(
    trimmed.startsWith("http://") || trimmed.startsWith("https://")
      ? trimmed
      : `http://${trimmed}`,
  );
  const path = url.pathname.replace(/\/+$/, "");
  if (
    path === "" ||
    path === "/" ||
    path === "/api/v0" ||
    path === "/api/v0/graphql"
  ) {
    url.pathname = "/status";
  } else if (!path.endsWith("/status")) {
    url.pathname = `${path}/status`;
  }
  url.search = "";
  url.hash = "";
  return url.toString();
}

function normalizeGraphqlUrl(status, statusUrl) {
  const raw = stringValue(
    status.desktop_graphql ||
      status.desktopGraphql ||
      status.graphql ||
      status.graphql_url ||
      status.graphqlUrl,
  );
  const statusEndpoint = new URL(statusUrl);
  if (!raw) {
    const fallback = new URL(statusEndpoint.toString());
    fallback.pathname = "/api/v0/graphql";
    fallback.search = "";
    fallback.hash = "";
    return fallback.toString();
  }

  const graphql = new URL(raw, statusEndpoint);
  if (
    graphql.hostname === "127.0.0.1" ||
    graphql.hostname === "localhost" ||
    graphql.hostname === "0.0.0.0" ||
    graphql.hostname === "::1"
  ) {
    graphql.protocol = statusEndpoint.protocol;
    graphql.hostname = statusEndpoint.hostname;
    graphql.port = statusEndpoint.port;
  }
  if (graphql.pathname === "/" || graphql.pathname === "") {
    graphql.pathname = "/api/v0/graphql";
  }
  graphql.search = "";
  graphql.hash = "";
  return graphql.toString();
}

function selectPeerAddr(status, statusUrl) {
  const statusHost = new URL(statusUrl).hostname;
  const p2p = status.p2p ?? {};
  const addrs = [
    ...arrayValue(p2p.p2p_listen_addresses),
    ...arrayValue(p2p.listen_addresses),
    ...arrayValue(status.p2p_listen_addresses),
    ...arrayValue(status.listen_addresses),
  ].filter((value) => typeof value === "string" && value.trim() !== "");
  return (
    addrs.find((addr) => addr.includes(statusHost)) ||
    stringValue(p2p.p2p_shareable_address) ||
    stringValue(status.p2p_shareable_address) ||
    addrs[0] ||
    ""
  );
}

async function startBridgeRunner() {
  const env = {
    ...process.env,
    CARGO_NET_GIT_FETCH_WITH_CLI:
      process.env.CARGO_NET_GIT_FETCH_WITH_CLI ?? "true",
  };
  const child = spawn(
    "cargo",
    [
      "run",
      "-p",
      "defra-agent-desktop-tauri",
      "--bin",
      "bridge_runner",
      "--quiet",
      "--",
      "--desktop-only",
    ],
    {
      cwd: repoRoot,
      env,
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => collectRunnerLog(chunk));

  let ready;
  try {
    ready = await waitForReadyMessage(child, READY_TIMEOUT_MS);
  } catch (error) {
    await stopBridgeRunner(child);
    throw error;
  }
  return {
    process: child,
    baseUrl: ready.baseUrl,
    ready,
  };
}

async function waitForReadyMessage(child, timeoutMs) {
  let stdoutBuffer = "";
  let stdout = "";
  let stderr = "";

  return await new Promise((resolvePromise, rejectPromise) => {
    const timeout = setTimeout(() => {
      cleanup();
      rejectPromise(
        new Error(
          `bridge runner did not become ready within ${timeoutMs}ms\nstdout:\n${stdout}\nstderr:\n${stderr}`,
        ),
      );
    }, timeoutMs);

    const onStdout = (chunk) => {
      stdoutBuffer += chunk.toString();
      let newlineIndex = stdoutBuffer.indexOf("\n");
      while (newlineIndex !== -1) {
        const line = stdoutBuffer.slice(0, newlineIndex).trim();
        stdoutBuffer = stdoutBuffer.slice(newlineIndex + 1);
        stdout += `${line}\n`;
        collectRunnerLog(line);
        try {
          const message = JSON.parse(line);
          if (message.kind === "ready" && message.baseUrl) {
            cleanup();
            resolvePromise(message);
            return;
          }
        } catch {
          // Logs can appear before the ready JSON.
        }
        newlineIndex = stdoutBuffer.indexOf("\n");
      }
    };

    const onStderr = (chunk) => {
      stderr += chunk.toString();
    };

    const onExit = (code) => {
      cleanup();
      rejectPromise(
        new Error(
          `bridge runner exited before ready (code=${code ?? "null"})\nstdout:\n${stdout}${stdoutBuffer}\nstderr:\n${stderr}`,
        ),
      );
    };

    const cleanup = () => {
      clearTimeout(timeout);
      child.stdout.off("data", onStdout);
      child.stderr.off("data", onStderr);
      child.off("exit", onExit);
    };

    child.stdout.on("data", onStdout);
    child.stderr.on("data", onStderr);
    child.on("exit", onExit);
  });
}

function collectRunnerLog(chunk) {
  const lines = String(chunk)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  for (const line of lines) {
    runnerLogs.push(line);
    if (runnerLogs.length > 500) {
      runnerLogs.shift();
    }
  }
}

async function stopBridgeRunner(child) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  child.stdin.end();
  await new Promise((resolvePromise) => {
    const timeout = setTimeout(() => {
      child.kill("SIGTERM");
      resolvePromise();
    }, 10_000);
    child.once("exit", () => {
      clearTimeout(timeout);
      resolvePromise();
    });
  });
}

async function waitForDeployment(baseUrl, agentDid, timeoutMs) {
  return await waitFor(
    async () => {
      const snapshot = await getJson(`${baseUrl}/desktop/client/snapshot`);
      const deployment = findDeployment(snapshot, agentDid);
      if (deployment) {
        return deployment;
      }
      return null;
    },
    timeoutMs,
    1_500,
  );
}

async function waitForLocalSession(baseUrl, agentDid, sessionId, requestId, timeoutMs) {
  return await waitFor(
    async () => {
      const snapshot = await postJson(`${baseUrl}/desktop/session/snapshot`, {
        agentDid,
        sessionId,
        requestId,
      });
      if (snapshot?.sessionId === sessionId) {
        return snapshot;
      }
      return null;
    },
    timeoutMs,
    1_500,
  );
}

async function waitForRemoteRequest(graphql, sessionId, requestId, timeoutMs) {
  return await waitFor(
    async () => {
      const data = await queryRemoteChat(graphql, sessionId, requestId);
      if (data.request) {
        return data;
      }
      return null;
    },
    timeoutMs,
    2_000,
  );
}

async function queryRemoteChat(graphql, sessionId, requestId) {
  const query = `query RemoteChatSmoke {
    AgentRequest(filter: { request_id: { _eq: "${escapeGraphql(requestId)}" } }, limit: 1) {
      request_id
      agent_did
      behavior_id
      session_id
      status
      lifecycle_state
      failure_reason
      created_at
      claimed_at
      execution_origin
    }
    AgentConversation(filter: { session_id: { _eq: "${escapeGraphql(sessionId)}" } }, limit: 1) {
      session_id
      agent_did
      behavior_id
      title
      status
      latest_request_id
      updated_at
    }
    AgentResponse(filter: { request_id: { _eq: "${escapeGraphql(requestId)}" } }, limit: 1) {
      request_id
      status
      error_message
      completed_at
      materialized_at
    }
  }`;
  const response = await postJson(graphql, { query });
  const errors = response.errors;
  if (Array.isArray(errors) && errors.length > 0) {
    throw new Error(`remote GraphQL returned errors: ${JSON.stringify(errors)}`);
  }
  return {
    request: response.data?.AgentRequest?.[0] ?? null,
    conversation: response.data?.AgentConversation?.[0] ?? null,
    response: response.data?.AgentResponse?.[0] ?? null,
  };
}

async function waitFor(probe, timeoutMs, intervalMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value) {
        return value;
      }
    } catch (error) {
      lastError = error;
    }
    await sleep(intervalMs);
  }
  if (lastError) {
    throw lastError;
  }
  return null;
}

async function getJson(url) {
  return await fetchJson(url, { method: "GET" });
}

async function postJson(url, body) {
  return await fetchJson(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function fetchJson(url, init) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  try {
    let response;
    try {
      response = await fetch(url, { ...init, signal: controller.signal });
    } catch (error) {
      if (error?.name === "AbortError") {
        throw new Error(`${init.method ?? "GET"} ${url} timed out after ${FETCH_TIMEOUT_MS}ms`);
      }
      throw error;
    }
    const text = await response.text();
    let json = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch (error) {
      throw new Error(`failed to parse JSON from ${url}: ${text.slice(0, 300)}`);
    }
    if (!response.ok) {
      throw new Error(
        `${init.method ?? "GET"} ${url} failed with ${response.status}: ${JSON.stringify(json)}`,
      );
    }
    return json;
  } finally {
    clearTimeout(timeout);
  }
}

function findDeployment(snapshot, agentDid) {
  return (
    snapshot?.client?.deployments?.find((deployment) => deployment.agentDid === agentDid) ??
    null
  );
}

function deploymentCounts(deployment) {
  return {
    behaviors: deployment.behaviors?.length ?? 0,
    tasks: deployment.tasks?.length ?? 0,
    conversations: deployment.conversations?.length ?? 0,
    inferenceBackends: deployment.inferenceBackends?.length ?? 0,
    inferenceProfiles: deployment.inferenceProfiles?.length ?? 0,
    toolSelections: deployment.toolSelections?.length ?? 0,
    toolServiceRegistries: deployment.toolServiceRegistries?.length ?? 0,
    schedules: deployment.schedules?.length ?? 0,
    eventTriggers: deployment.eventTriggers?.length ?? 0,
  };
}

function emptyCounts() {
  return {
    behaviors: 0,
    tasks: 0,
    conversations: 0,
    inferenceBackends: 0,
    inferenceProfiles: 0,
    toolSelections: 0,
    toolServiceRegistries: 0,
    schedules: 0,
    eventTriggers: 0,
  };
}

function printSummary(summary) {
  console.log("");
  console.log("Remote fleet smoke summary");
  console.log(`Mode: ${summary.mode}`);
  for (const peer of summary.peers) {
    const counts = peer.syncedCounts;
    console.log(
      [
        `${summary.okForPeer?.[peer.label] ?? (peer.failures.length === 0 ? "PASS" : "FAIL")} ${peer.label}`,
        `behaviors=${counts.behaviors}`,
        `tasks=${counts.tasks}`,
        `conversations=${counts.conversations}`,
        `backends=${counts.inferenceBackends}`,
        `profiles=${counts.inferenceProfiles}`,
      ].join(" "),
    );
    if (peer.chat) {
      console.log(
        `  chat request=${peer.chat.requestId ?? "n/a"} local=${peer.chat.localAccepted} remote=${peer.chat.remoteVisible} state=${peer.chat.remoteLifecycleState ?? peer.chat.localTurnState ?? "n/a"}`,
      );
    }
    for (const failure of peer.failures) {
      console.log(`  - ${failure}`);
    }
  }
  if (summary.runnerErrors.length > 0) {
    console.log("");
    console.log("Bridge runner error logs:");
    for (const line of summary.runnerErrors) {
      console.log(`  ${line}`);
    }
  }
  if (summary.failures.length === 0) {
    console.log("");
    console.log("All checks passed.");
  } else {
    console.log("");
    console.log("Failures:");
    for (const failure of summary.failures) {
      console.log(`  - ${failure}`);
    }
  }
}

function takeBooleanFlag(args, name) {
  const index = args.indexOf(name);
  if (index === -1) {
    return false;
  }
  args.splice(index, 1);
  return true;
}

function takeRepeatedFlag(args, name) {
  const values = [];
  for (let index = 0; index < args.length; ) {
    const value = args[index];
    if (value === name) {
      const next = args[index + 1];
      if (!next || next.startsWith("--")) {
        throw new Error(`missing value for ${name}`);
      }
      values.push(next);
      args.splice(index, 2);
      continue;
    }
    if (value.startsWith(`${name}=`)) {
      values.push(value.slice(name.length + 1));
      args.splice(index, 1);
      continue;
    }
    index += 1;
  }
  return values;
}

function parsePeerArg(value) {
  const [label, ...addressParts] = value.split("=");
  const address = addressParts.join("=");
  if (!label || !address) {
    throw new Error(`invalid --peer value "${value}", expected label=http://host:port`);
  }
  return [label, address];
}

function stringValue(value) {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : "";
}

function arrayValue(value) {
  return Array.isArray(value) ? value : [];
}

function escapeGraphql(value) {
  return String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function shorten(value, maxLength) {
  if (!value || value.length <= maxLength) {
    return value;
  }
  return `${value.slice(0, maxLength - 1)}...`;
}

function sleep(ms) {
  return new Promise((resolvePromise) => setTimeout(resolvePromise, ms));
}

function log(message) {
  if (!jsonOutput) {
    console.log(`[remote-smoke] ${message}`);
  }
}
