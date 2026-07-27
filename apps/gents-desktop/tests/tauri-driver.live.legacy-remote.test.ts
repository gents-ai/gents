import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { parsePeerConnectionJson } from "../src/components/fleet/peerConnectionImport";
import type { ChatSendResult } from "../src/lib/types";
import { LiveBridgeRunner } from "./live-bridge-runner";
import { liveRunnerOptionsFromEnv } from "./tauri-driver-live/harness";

const statusUrl = process.env.GENTS_TAURI_LEGACY_E2E_STATUS?.trim();
const describeLegacy = statusUrl ? describe.sequential : describe.skip;

describeLegacy("legacy remote GraphQL recovery and interrupt", () => {
  let runner: LiveBridgeRunner;

  beforeAll(async () => {
    runner = await LiveBridgeRunner.start(liveRunnerOptionsFromEnv());
  }, 360_000);

  afterAll(async () => {
    await runner?.dispose();
  });

  it("rehydrates only this requester and latches interrupt on the remote node", async () => {
    const payload = await runner.adapter.fetchPeerStatus(statusUrl!);
    const peer = parsePeerConnectionJson(JSON.stringify(payload));
    await runner.adapter.addPeer({
      ...peer,
      label: `Legacy E2E ${Date.now()}`,
    });
    await runner.adapter.setSelectedAgent(peer.agentDid);

    const before = await runner.fetchSnapshot();
    const deployment = before.client?.deployments.find(
      (candidate) => candidate.agentDid === peer.agentDid,
    );
    expect(deployment).toBeDefined();
    expect(deployment?.conversations).toHaveLength(0);

    const submitted = await runner.adapter.sendChatMessage({
      agentDid: peer.agentDid,
      behaviorId: peer.defaultBehaviorId ?? "default",
      sessionId: null,
      content:
        "This is an interrupt-path test. Think carefully for several seconds before answering, and do not modify files or configuration.",
    });

    const preview = await retry(
      () =>
        runner.adapter.previewInterruptCascade({
          requestId: submitted.requestId,
          agentDid: peer.agentDid,
          includeTerminal: false,
        }),
      20_000,
    );
    expect(preview.rootRequestId).toBe(submitted.requestId);

    const interrupted = await runner.adapter.interruptRequest({
      requestId: submitted.requestId,
      agentDid: peer.agentDid,
      cause: "userCancelled",
      cascade: false,
    });
    expect(interrupted.accepted).toBe(true);
    expect(interrupted.interruptRequestedAt).toBeTruthy();

    await runner.postJson("/desktop/test-fixture/clear-client-store", {});
    expect(
      await runner.adapter.fetchSessionSnapshot(
        submitted.sessionId,
        peer.agentDid,
        submitted.requestId,
      ),
    ).toBeNull();

    await runner.adapter.setSelectedAgent(peer.agentDid);
    const recovered = await waitForRecoveredSession(runner, peer.agentDid, submitted);
    expect(recovered.latestRequestId).toBe(submitted.requestId);
  }, 120_000);
});

async function waitForRecoveredSession(
  runner: LiveBridgeRunner,
  agentDid: string,
  submitted: ChatSendResult,
) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const session = await runner.adapter.fetchSessionSnapshot(
      submitted.sessionId,
      agentDid,
      submitted.requestId,
    );
    if (session?.latestRequestId === submitted.requestId) {
      return session;
    }
    await delay(250);
  }
  throw new Error(`session ${submitted.sessionId} did not rehydrate`);
}

async function retry<T>(operation: () => Promise<T>, timeoutMs: number): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let lastError: unknown;
  while (Date.now() < deadline) {
    try {
      return await operation();
    } catch (error) {
      lastError = error;
      await delay(250);
    }
  }
  throw lastError;
}

function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
