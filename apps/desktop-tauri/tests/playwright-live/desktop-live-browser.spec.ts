import { writeFile } from "node:fs/promises";

import { LiveBridgeRunner, type LiveBridgeRunnerOptions } from "../live-bridge-runner";
import type { RequestDiagnosticsBundle } from "../live-bridge-runner";
import {
  liveSmokeFailureSummary,
  liveSmokeSummary,
  type LiveSmokeEvidence,
  type LiveSmokeFailureEvidence,
  type SubmittedRequest,
} from "./liveSmokeEvidence";
import {
  expect,
  gotoLiveHarness,
  test,
  type Page,
  type TestInfo,
} from "../playwright/desktopTest";

test.describe.configure({ mode: "serial" });

test.describe("desktop live browser smoke", () => {
  let runner: LiveBridgeRunner | null = null;

  test.beforeAll(async () => {
    runner = await LiveBridgeRunner.start(liveOptionsFromEnv());
  });

  test.afterAll(async () => {
    await runner?.dispose();
  });

  test("drives Chromium through the live bridge and runtime", async ({
    page,
  }, testInfo) => {
    const liveRunner = runner;
    let submitted: SubmittedRequest | null = null;
    let diagnostics: RequestDiagnosticsBundle | null = null;

    try {
      expect(liveRunner).toBeTruthy();
      if (!liveRunner) {
        throw new Error("live browser smoke runner was not initialized");
      }

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

      submitted = await waitForSubmittedRequest(liveRunner, {
        agentDid: deployment.agentDid,
        previousRequestIds,
      });
      const completedSession = await liveRunner.waitForRequestCompletion(submitted);
      if (completedSession.turnState !== "completed") {
        diagnostics = await liveRunner.fetchRequestDiagnostics(
          submitted.sessionId,
          submitted.requestId,
        );
        throw new Error(
          `live browser smoke request ended ${completedSession.turnState}`,
        );
      }

      const transcriptRows = page.locator(
        '[data-testid="transcript-panel"] .message-card',
      );
      await expect
        .poll(async () => transcriptRows.count(), { timeout: 30_000 })
        .toBeGreaterThanOrEqual(2);
      const transcriptRowCount = await transcriptRows.count();

      await page.getByRole("button", { name: /open operations drawer/i }).click();
      await expect(
        page.getByRole("complementary", { name: "Operations" }),
      ).toBeVisible();
      await expect(page.getByRole("tab", { name: /Backends/ })).toBeVisible();

      await page.getByRole("button", { name: "Configure" }).click();
      await expect(page.locator(".config-workspace")).toBeVisible();
      await page.getByTestId("config-tab-backends").click();
      await expect(page.getByTestId("backend-save")).toBeVisible();

      diagnostics = await liveRunner.fetchRequestDiagnostics(
        submitted.sessionId,
        submitted.requestId,
      );
      await attachLiveSmokeEvidence(testInfo, {
        baseUrl: liveRunner.baseUrl,
        deploymentLabel: liveRunner.deploymentLabel,
        agentDid: liveRunner.agentDid,
        toolRoot: liveRunner.toolRoot,
        sessionId: submitted.sessionId,
        requestId: submitted.requestId,
        turnState: completedSession.turnState,
        transcriptRows: transcriptRowCount,
        diagnostics,
      });

      await attachPageScreenshot(page, testInfo, "desktop-live-browser-final.png");
    } catch (error) {
      if (liveRunner) {
        await attachLiveSmokeFailureEvidence(testInfo, page, {
          error,
          runner: liveRunner,
          submitted,
          diagnostics:
            diagnostics ??
            (submitted
              ? await tryFetchRequestDiagnostics(liveRunner, submitted)
              : null),
        });
      }
      throw error;
    }
  });
});

async function attachLiveSmokeEvidence(
  testInfo: TestInfo,
  evidence: LiveSmokeEvidence,
) {
  const diagnosticsPath = testInfo.outputPath("desktop-live-browser-diagnostics.json");
  await writeFile(
    diagnosticsPath,
    `${JSON.stringify(evidence.diagnostics, null, 2)}\n`,
  );
  await testInfo.attach("desktop-live-browser-diagnostics.json", {
    path: diagnosticsPath,
    contentType: "application/json",
  });

  const summaryPath = testInfo.outputPath("desktop-live-browser-smoke.md");
  await writeFile(summaryPath, liveSmokeSummary(evidence));
  await testInfo.attach("desktop-live-browser-smoke.md", {
    path: summaryPath,
    contentType: "text/markdown",
  });
}

async function attachLiveSmokeFailureEvidence(
  testInfo: TestInfo,
  page: Page,
  evidence: Omit<LiveSmokeFailureEvidence, "screenshotAttached">,
) {
  const screenshotAttached = await tryAttachPageScreenshot(
    page,
    testInfo,
    "desktop-live-browser-failure.png",
  );

  if (evidence.diagnostics) {
    const diagnosticsPath = testInfo.outputPath(
      "desktop-live-browser-failure-diagnostics.json",
    );
    await writeFile(
      diagnosticsPath,
      `${JSON.stringify(evidence.diagnostics, null, 2)}\n`,
    );
    await testInfo.attach("desktop-live-browser-failure-diagnostics.json", {
      path: diagnosticsPath,
      contentType: "application/json",
    });
  }

  const summaryPath = testInfo.outputPath("desktop-live-browser-failure.md");
  await writeFile(
    summaryPath,
    liveSmokeFailureSummary({ ...evidence, screenshotAttached }),
  );
  await testInfo.attach("desktop-live-browser-failure.md", {
    path: summaryPath,
    contentType: "text/markdown",
  });
}

async function attachPageScreenshot(
  page: Page,
  testInfo: TestInfo,
  attachmentName: string,
) {
  const path = testInfo.outputPath(attachmentName);
  await page.screenshot({ fullPage: true, path });
  await testInfo.attach(attachmentName, {
    path,
    contentType: "image/png",
  });
}

async function tryAttachPageScreenshot(
  page: Page,
  testInfo: TestInfo,
  attachmentName: string,
) {
  try {
    await attachPageScreenshot(page, testInfo, attachmentName);
    return true;
  } catch {
    return false;
  }
}

async function tryFetchRequestDiagnostics(
  runner: LiveBridgeRunner,
  submitted: SubmittedRequest,
) {
  try {
    return await runner.fetchRequestDiagnostics(
      submitted.sessionId,
      submitted.requestId,
    );
  } catch {
    return null;
  }
}

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
        if (!deployment) {
          return false;
        }
        const conversation = deployment.conversations.find(
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
