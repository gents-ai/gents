import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner sad paths", () => {
  it("surfaces a missing-model inference failure and returns the composer to ready", async () => {
    const baseModel =
      process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ?? "missing-live-model";
    const missingModel = `${baseModel}__defra_missing_sad_path__`;
    const runner = await LiveBridgeRunner.start({
      inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
      modelName: missingModel,
      provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
      apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
      apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
    });
    const initialSnapshot = await runner.fetchSnapshot();
    const firstPeerId = initialSnapshot.client?.deployments[0]?.peerId ?? null;
    const driver = renderTauriAppDriverWithBridge(runner, firstPeerId);

    try {
      await driver.ready();
      await driver.openChat();
      logTurn(
        `sad path ready deployment=${runner.deploymentLabel} model=${missingModel}`,
      );

      await driver.typeComposer("Reply with one short sentence.");
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = runner.sendResults.at(-1);
      expect(submitted).toBeDefined();
      logTurn(
        `sad path submitted sessionId=${submitted!.sessionId} requestId=${submitted!.requestId}`,
      );

      const failedSession = await runner.waitForRequestCompletion(submitted!);
      expect(failedSession.turnState).toBe("failed");
      expect(failedSession.latestResponse?.errorMessage).toBeTruthy();

      await waitFor(
        () => {
          expect(screen.getByTestId("response-error-card")).toHaveTextContent(
            /agent stream failed|model|404|not found/i,
          );
        },
        { timeout: 30_000 },
      );

      await driver.typeComposer("Can I type after the failure?");
      await waitFor(() => {
        expect(driver.sendButton()).toBeEnabled();
      });
    } finally {
      await driver.dispose();
    }
  }, 240_000);
});
