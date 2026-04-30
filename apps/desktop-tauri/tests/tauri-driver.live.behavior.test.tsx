import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import {
  describeLive,
  logTurn,
  waitForBehaviorConfig,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner behavior config", () => {
  it(
    "edits behavior config through the real UI and observes replication",
    async () => {
      const runner = await LiveBridgeRunner.start({
        inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
        modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
        provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
        apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
        apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
      });
      const initialSnapshot = await runner.fetchSnapshot();
      const deployment = initialSnapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      const behavior =
        deployment!.behaviors.find((candidate) => candidate.isDefault) ??
        deployment!.behaviors[0];
      expect(behavior).toBeDefined();
      const driver = renderTauriAppDriverWithBridge(runner, deployment!.peerId);
      const suffix = Date.now().toString();
      const behaviorId = behavior!.behaviorId;
      const systemPrompt =
        `You are Amy, a repository analysis agent. Config sentinel ${suffix}.`;

      try {
        await driver.ready();
        await driver.openConfig();
        await waitFor(() => {
          expect(driver.behaviorSystemPrompt()).toBeInTheDocument();
        });

        await driver.replaceBehaviorSystemPrompt(systemPrompt);
        await driver.saveBehaviorConfig();

        await waitFor(() => {
          expect(driver.behaviorSaveStatus()).toHaveTextContent("Saved");
        });
        await waitForBehaviorConfig(
          runner,
          behaviorId,
          behaviorId,
          systemPrompt,
        );
        logTurn(
          `behavior config saved behaviorId=${behaviorId}`,
        );
      } finally {
        await driver.dispose();
      }
    },
    240_000,
  );
});
