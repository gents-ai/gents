import { LiveBridgeRunner, type LiveBridgeRunnerOptions } from "../live-bridge-runner";
import { expect, gotoLiveHarness, test } from "../playwright/desktopTest";

test.describe.configure({ mode: "serial" });

test.describe("desktop live browser smoke", () => {
  let runner: LiveBridgeRunner | null = null;

  test.beforeAll(async () => {
    runner = await LiveBridgeRunner.start(liveOptionsFromEnv());
  });

  test.afterAll(async () => {
    await runner?.dispose();
  });

  test("drives Chromium through the live bridge and runtime", async ({ page }) => {
    expect(runner).toBeTruthy();
    const liveRunner = runner!;

    await gotoLiveHarness(page, liveRunner.baseUrl);
    await expect(page.getByTestId("fleet-dashboard")).toBeVisible();

    await page.locator('[data-testid^="fleet-chat-name-"]').first().click();
    await expect(page.getByTestId("composer-input")).toBeVisible();

    await page
      .getByTestId("composer-input")
      .fill("Reply with a short desktop live browser smoke confirmation.");
    await page.getByTestId("composer-send").click();

    await expect
      .poll(
        async () => {
          const deployment = (await liveRunner.fetchSnapshot()).client?.deployments[0];
          return deployment?.conversations[0]?.turnState ?? "missing";
        },
        { timeout: 600_000 },
      )
      .toBe("completed");

    await expect(
      page.locator('[data-testid="transcript-panel"] .message-card'),
    ).toHaveCount(2, { timeout: 30_000 });

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await expect(page.getByRole("tab", { name: /Backends/ })).toBeVisible();

    await page.getByRole("button", { name: "Configure" }).click();
    await expect(page.locator(".config-workspace")).toBeVisible();
    await expect(page.getByTestId("backend-save")).toBeVisible();
  });
});

function liveOptionsFromEnv(): LiveBridgeRunnerOptions {
  return {
    inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
    modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
    provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
    apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
    apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
    subagentInferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_INFERENCE_URL,
    subagentModelName: process.env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_MODEL_NAME,
    subagentProvider: process.env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_PROVIDER,
    subagentApiKey: process.env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_API_KEY,
    subagentApiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_SUBAGENT_API_KEY_ENV_VAR,
  };
}
