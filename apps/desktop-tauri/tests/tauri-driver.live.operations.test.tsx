import { screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

const OPERATIONS_PROMPT =
  "Reply with exactly this sentence and no tools: operations drawer smoke ready.";

describeLive("Tauri app live operations drawer", () => {
  it("opens every operations tab after a real chat turn", async () => {
    const runner = await LiveBridgeRunner.start({
      inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
      modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
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
      logTurn(`operations drawer driver ready agentDid=${runner.agentDid}`);

      await driver.typeComposer(OPERATIONS_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = runner.sendResults.at(-1);
      expect(submitted).toBeDefined();
      const session = await runner.waitForRequestCompletion(submitted!);
      expectCompletedSession("operations drawer seed turn", session);

      await driver.user.click(
        screen.getByRole("button", { name: /open operations drawer/i }),
      );
      expect(screen.getByRole("heading", { name: "Operations" })).toBeInTheDocument();
      expect(screen.getByRole("tablist", { name: "Operations" })).toBeInTheDocument();

      const backgroundPanel = await expectPanel("background-tools");
      expect(backgroundPanel).toHaveTextContent(/parent/i);
      expect(backgroundPanel).toHaveTextContent(/live/i);
      expectNoBridgeError(backgroundPanel);

      await driver.user.click(screen.getByRole("tab", { name: "Lineage" }));
      const lineagePanel = await expectPanel("lineage");
      expect(
        within(lineagePanel).getByRole("heading", { name: "Lineage" }),
      ).toBeInTheDocument();
      expect(lineagePanel).toHaveTextContent(
        /no active subagent dispatches|subagent lineage|loading lineage/i,
      );
      expectNoBridgeError(lineagePanel);

      await driver.user.click(screen.getByRole("tab", { name: "Backends" }));
      const backendPanel = await expectPanel("backend-health");
      expect(
        within(backendPanel).getByRole("heading", { name: "Backend health" }),
      ).toBeInTheDocument();
      expect(backendPanel).toHaveTextContent(/registered|backends/i);
      expectNoBridgeError(backendPanel);

      await driver.user.click(screen.getByRole("tab", { name: "MCP health" }));
      const mcpPanel = await expectPanel("mcp-health");
      expect(
        within(mcpPanel).getByRole("heading", { name: "MCP services / health" }),
      ).toBeInTheDocument();
      expect(mcpPanel).toHaveTextContent(/no mcp services registered|healthy|all/i);
      expectNoBridgeError(mcpPanel);
    } finally {
      await driver.dispose();
    }
  }, 600_000);
});

async function expectPanel(tabId: string) {
  const panelId = `operations-rail-panel-${tabId}`;
  await waitFor(() => {
    expect(document.getElementById(panelId)).toBeInTheDocument();
  });
  return document.getElementById(panelId)!;
}

function expectNoBridgeError(panel: HTMLElement) {
  expect(within(panel).queryByRole("alert")).not.toBeInTheDocument();
  expect(panel).not.toHaveTextContent(
    /bridge unavailable|desktop bridge|failed to load|not initialized|not running|fetch failed/i,
  );
}
