import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";
import { createFixtureHelpers } from "./live-bridge-runner/adapter";

describeLive("Tauri app live runtime observation", () => {
  it("a remote configuration write reaches the desktop snapshot through its runtime subscription", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const agent = deployment.agents.find((a) => a.isDefault) ?? deployment.agents[0];
      expect(agent).toBeDefined();

      const sentinel = `repl-d1-${Date.now()}`;

      const fixture = createFixtureHelpers(runner);
      await fixture.saveAgentConfigOnRemote({
        document: {
          node_did: agent!.nodeDid,
          agent_did: agent!.agentDid,
          creation_nonce: agent!.creationNonce,
          display_name: `${agent!.displayName} ${sentinel}`,
          description: agent!.description,
          context_id: agent!.contextId,
          inference_profile_id: agent!.inferenceProfileId!,
          enabled: agent!.enabled,
          tags: agent!.tags,
          created_at: agent!.createdAt,
        },
      });
      logTurn(`remote write issued agentDid=${agent!.agentDid} sentinel=${sentinel}`);

      await waitFor(
        async () => {
          const snapshot = await runner.fetchSnapshot();
          const observed = snapshot.client?.deployments[0]?.agents.find(
            (a) => a.agentDid === agent!.agentDid,
          );
          expect(observed?.displayName).toContain(sentinel);
          const observedContext = snapshot.client?.deployments[0]?.contexts.find(
            (candidate) => candidate.context_id === observed?.contextId,
          );
          expect(
            observedContext?.tools_id,
            "observed agent must keep its canonical context linkage",
          ).toBeDefined();
        },
        { timeout: 5_000, interval: 200 },
      );
      logTurn(
        `runtime subscription observed: remote write → desktop snapshot agentDid=${agent!.agentDid} sentinel=${sentinel}`,
      );
    });
  }, 180_000);
});
