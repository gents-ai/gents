import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";
import { createFixtureHelpers } from "./live-bridge-runner/adapter";

/* Live conformance witness for EventDelivery D1/D2 (Lean proofs): update this
   file when a theorem's statement changes. The write must go to the remote node
   and the read to the desktop node; a same-node roundtrip would not exercise
   the P2P subscription path. */
describeLive("Tauri app live replication (EventDelivery witnesses)", () => {
  it("D1/D2: a remote write converges on the desktop node within the subscription window", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const agent = deployment.agents.find((a) => a.isDefault) ?? deployment.agents[0];
      expect(agent).toBeDefined();

      const sentinel = `repl-d1-${Date.now()}`;

      const fixture = createFixtureHelpers(runner);
      await fixture.saveAgentConfigOnRemote({
        document: {
          node_did: agent!.nodeDid,
          agent_id: agent!.agentId,
          display_name: `${agent!.displayName} ${sentinel}`,
          description: agent!.description,
          context_id: agent!.contextId,
          inference_profile_id: agent!.inferenceProfileId!,
          enabled: agent!.enabled,
          tags: agent!.tags,
          created_at: agent!.createdAt,
        },
      });
      logTurn(
        `D1/D2 remote write issued agentId=${agent!.agentId} sentinel=${sentinel}`,
      );

      await waitFor(
        async () => {
          const snapshot = await runner.fetchSnapshot();
          const replicated = snapshot.client?.deployments[0]?.agents.find(
            (a) => a.agentId === agent!.agentId,
          );
          expect(replicated?.displayName).toContain(sentinel);
          const replicatedContext = snapshot.client?.deployments[0]?.contexts.find(
            (candidate) => candidate.context_id === replicated?.contextId,
          );
          expect(
            replicatedContext?.tools_id,
            "replicated agent must keep its canonical context linkage",
          ).toBeDefined();
        },
        { timeout: 5_000, interval: 200 },
      );
      logTurn(
        `D1/D2 witnessed: remote write → desktop snapshot agentId=${agent!.agentId} sentinel=${sentinel}`,
      );
    });
  }, 180_000);
});
