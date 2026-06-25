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
    const deployment = await firstDeployment(liveRunner);
    const previousRequestIds = new Set(
      deployment.conversations
        .map((conversation) => conversation.latestRequestId)
        .filter((requestId): requestId is string => Boolean(requestId)),
    );

    await page
      .getByTestId("composer-input")
      .fill("Reply with a short desktop live browser smoke confirmation.");
    await page.getByTestId("composer-send").click();

    const submitted = await waitForSubmittedRequest(liveRunner, {
      agentDid: deployment.agentDid,
      previousRequestIds,
    });
    const completedSession = await liveRunner.waitForRequestCompletion(submitted);
    if (completedSession.turnState !== "completed") {
      const diagnostics = await liveRunner.fetchRequestDiagnostics(
        submitted.sessionId,
        submitted.requestId,
      );
      throw new Error(
        `live browser smoke request ended ${completedSession.turnState}; diagnostics=${JSON.stringify(diagnostics)}`,
      );
    }

    await expect
      .poll(
        async () =>
          page.locator('[data-testid="transcript-panel"] .message-card').count(),
        { timeout: 30_000 },
      )
      .toBeGreaterThanOrEqual(2);

    await page.getByRole("button", { name: /open operations drawer/i }).click();
    await expect(page.getByRole("complementary", { name: "Operations" })).toBeVisible();
    await expect(page.getByRole("tab", { name: /Backends/ })).toBeVisible();

    await page.getByRole("button", { name: "Configure" }).click();
    await expect(page.locator(".config-workspace")).toBeVisible();
    await page.getByTestId("config-tab-backends").click();
    await expect(page.getByTestId("backend-save")).toBeVisible();
  });
});

async function firstDeployment(runner: LiveBridgeRunner) {
  const deployment = (await runner.fetchSnapshot()).client?.deployments[0];
  if (!deployment) {
    throw new Error("live browser smoke expected one deployment");
  }
  return deployment;
}

async function waitForSubmittedRequest(
  runner: LiveBridgeRunner,
  expected: {
    agentDid: string;
    previousRequestIds: Set<string>;
  },
) {
  let request: { agentDid: string; requestId: string; sessionId: string } | null = null;
  await expect
    .poll(
      async () => {
        const deployment = await findDeployment(runner, expected.agentDid);
        const conversation = deployment?.conversations.find(
          (candidate) =>
            candidate.latestRequestId &&
            !expected.previousRequestIds.has(candidate.latestRequestId),
        );
        if (conversation?.latestRequestId) {
          request = {
            agentDid: deployment.agentDid,
            requestId: conversation.latestRequestId,
            sessionId: conversation.sessionId,
          };
        }
        return Boolean(request);
      },
      { timeout: 30_000 },
    )
    .toBe(true);
  return request!;
}

async function findDeployment(runner: LiveBridgeRunner, agentDid: string) {
  const snapshot = await runner.fetchSnapshot();
  return (
    snapshot.client?.deployments.find(
      (deployment) => deployment.agentDid === agentDid,
    ) ?? snapshot.client?.deployments[0]
  );
}

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
