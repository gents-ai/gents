import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

describeLive("Tauri app live fleet add flow", () => {
  it("previews a live runner /status payload before adding the connection", async () => {
    const runner = await LiveBridgeRunner.start({
      inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
      modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
      provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
      apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
      apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
    });
    const initialSnapshot = await runner.fetchSnapshot();
    const firstPeerId = initialSnapshot.client?.deployments[0]?.peerId ?? null;
    const firstDeployment = initialSnapshot.client?.deployments[0];
    const driver = renderTauriAppDriverWithBridge(runner, firstPeerId);

    try {
      expect(firstDeployment).toBeDefined();
      await driver.ready();
      logTurn(`fleet driver ready statusUrl=${runner.baseUrl}/status`);

      await driver.user.click(screen.getByRole("button", { name: "Add Agent" }));
      await driver.replaceInput("fleet-add-server-address", runner.baseUrl);
      await driver.user.click(screen.getByTestId("fleet-fetch-status"));

      await waitFor(
        () => {
          expect(driver.input("fleet-add-label")).toHaveValue(firstDeployment!.label);
          expect(driver.input("fleet-add-agent-did")).toHaveValue(runner.agentDid);
          expect(driver.input("fleet-add-addr")).toHaveValue(firstDeployment!.addr);
          expect(screen.getByText("Fetched /status")).toBeInTheDocument();
        },
        { timeout: 30_000 },
      );

      await driver.user.click(screen.getByTestId("fleet-add-submit"));

      await waitFor(
        async () => {
          const latest = await runner.fetchSnapshot();
          const deployment = latest.client?.deployments.find(
            (candidate) => candidate.agentDid === runner.agentDid,
          );
          expect(deployment).toBeDefined();
          expect(deployment?.addr).toBe(firstDeployment!.addr);
          expect(deployment?.label).toBe(firstDeployment!.label);
          expect(
            screen.queryByTestId("fleet-add-server-address"),
          ).not.toBeInTheDocument();
          expect(
            screen.getByTestId(`fleet-row-${deployment!.peerId}`),
          ).toBeInTheDocument();
        },
        { timeout: 60_000 },
      );

      expect(screen.queryByText(/failed|error|not found/i)).not.toBeInTheDocument();
    } finally {
      await driver.dispose();
    }
  }, 300_000);
});
