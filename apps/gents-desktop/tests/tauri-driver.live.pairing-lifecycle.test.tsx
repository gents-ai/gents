import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeRealInferenceLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

type WireReplicator = {
  id?: string | null;
  address?: string | null;
  collections?: string[] | null;
};

describeRealInferenceLive("managed mobile pairing route lifecycle", () => {
  it("survives drift, authoritative removal, re-pairing, and session continuation", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const behaviorId = `${runner.agentDid}:default`;

      // Replace the fixture's compatibility replicator with the same managed
      // add/status path used by a paired mobile client.
      await runner.postJson("/desktop/test-fixture/remove-peer", {
        peerId: deployment.peerId,
      });
      await waitForNoDeployments(runner);

      const addRequest = {
        label: deployment.label,
        agentDid: deployment.agentDid,
        addr: deployment.addr,
        graphql: `${runner.baseUrl}/graphql`,
        defaultBehaviorId: behaviorId,
      };
      await runner.postJson("/desktop/peer/add", addRequest);
      await runner.postJson("/desktop/p2p/repair", {});
      const managed = await waitForManagedRouteReady(runner);
      const returnRoute = managed.routes.find(
        (route) => route.direction === "runtime-to-client",
      );
      expect(returnRoute?.address).toBeTruthy();
      const behavior = await waitForSeededBehavior(runner, managed.peerId, behaviorId);
      logTurn(`managed routes ready peerId=${managed.peerId}`);

      const first = await runner.adapter.sendChatMessage({
        agentDid: managed.agentDid,
        behaviorId: behavior.behaviorId,
        sessionId: null,
        content:
          "Reply with one concise sentence confirming this is the first live pairing lifecycle turn.",
      });
      const firstSession = await runner.waitForRequestCompletion(first);
      expectCompletedSession("managed route turn 1", firstSession);
      expect(firstSession.latestResponse?.content?.trim().length ?? 0).toBeGreaterThan(
        0,
      );
      logTurn(`managed turn 1 completed requestId=${first.requestId}`);

      const drifted = await runner.postJson<WireReplicator[]>(
        "/desktop/test-fixture/drift-remote-return-route",
        {},
      );
      // The fixture remote is isolated to this desktop. Assert its complete
      // route set so an endpoint encoding difference cannot hide residue.
      expect(drifted, JSON.stringify(drifted)).toHaveLength(1);
      expect(drifted[0]?.collections).toHaveLength(1);
      logTurn(`remote return route drifted endpoint=${returnRoute!.address}`);

      const removed = await runner.postJson<{ warning: string | null }>(
        "/desktop/test-fixture/remove-peer",
        { peerId: managed.peerId },
      );
      expect(removed.warning).toBeNull();
      await waitForNoDeployments(runner);
      await waitFor(
        async () => {
          const remote = await runner.getJson<WireReplicator[]>(
            "/desktop/test-fixture/remote-replicators",
          );
          expect(remote, JSON.stringify(remote)).toHaveLength(0);
        },
        { timeout: 30_000, interval: 200 },
      );
      logTurn("drifted remote route authoritatively removed");

      await runner.postJson("/desktop/peer/add", addRequest);
      await runner.postJson("/desktop/p2p/repair", {});
      const repaired = await waitForManagedRouteReady(runner);
      expect(repaired.peerId).not.toBe(managed.peerId);
      expect(repaired.agentDid).toBe(managed.agentDid);
      expect(repaired.addr).toBe(managed.addr);

      const second = await runner.adapter.sendChatMessage({
        agentDid: repaired.agentDid,
        behaviorId: behavior.behaviorId,
        sessionId: first.sessionId,
        content:
          "Reply with one concise sentence confirming this is the second turn after durable re-pairing.",
      });
      expect(second.sessionId).toBe(first.sessionId);
      const secondSession = await runner.waitForRequestCompletion(second);
      expectCompletedSession("managed route turn 2", secondSession);
      expect(secondSession.latestResponse?.content?.trim().length ?? 0).toBeGreaterThan(
        0,
      );
      expect(
        secondSession.timelineItems.filter((item) => item.kind === "userMessage"),
      ).toHaveLength(2);
      logTurn(`managed turn 2 completed requestId=${second.requestId}`);
    });
  }, 900_000);
});

async function waitForSeededBehavior(
  runner: {
    fetchSnapshot: () => Promise<{
      client?: { deployments: DeploymentView[] } | null;
    }>;
  },
  peerId: string,
  behaviorId: string,
) {
  let behavior: DeploymentView["behaviors"][number] | undefined;
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      const deployment = snapshot.client?.deployments.find(
        (candidate) => candidate.peerId === peerId,
      );
      behavior = deployment?.behaviors.find(
        (candidate) => candidate.behaviorId === behaviorId,
      );
      expect(
        behavior,
        `live fixture must replicate its seeded behavior: ${JSON.stringify({
          agentDid: deployment?.agentDid,
          defaultBehaviorId: deployment?.defaultBehaviorId,
          behaviorIds: deployment?.behaviors.map((candidate) => candidate.behaviorId),
        })}`,
      ).toBeDefined();
    },
    { timeout: 30_000, interval: 200 },
  );
  return behavior!;
}

async function waitForManagedRouteReady(runner: {
  fetchSnapshot: () => Promise<{
    client?: { deployments: DeploymentView[] } | null;
  }>;
}) {
  let ready: DeploymentView | undefined;
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      const deployment = snapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      expect(deployment?.source).toBe("server-status");
      expect(deployment?.chatSafe).toBe(true);
      expect(deployment?.routes).toHaveLength(2);
      expect(
        deployment?.routes.every(
          (route) => route.desired && route.applied && route.liveMatch,
        ),
      ).toBe(true);
      ready = deployment;
    },
    { timeout: 120_000, interval: 250 },
  );
  return ready!;
}

async function waitForNoDeployments(runner: {
  fetchSnapshot: () => Promise<{
    client?: { deployments: DeploymentView[] } | null;
  }>;
}) {
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      expect(snapshot.client?.deployments ?? []).toHaveLength(0);
    },
    { timeout: 30_000, interval: 200 },
  );
}
