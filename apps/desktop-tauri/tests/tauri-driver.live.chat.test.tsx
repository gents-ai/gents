import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import {
  expectLatestSendResult,
  type LiveDesktopDriver,
  withLiveDesktop,
} from "./tauri-driver-live/harness";
import {
  closeOperationsDrawer,
  exerciseOperationsDrawerTabs,
} from "./tauri-driver-live/operations-assertions";
import {
  describeLive,
  expectCompletedSession,
  FIRST_PROMPT,
  logTurn,
  SECOND_PROMPT,
  THIRD_PROMPT,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner chat", () => {
  it("drives three real UI turns through a single live deployment", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      await driver.ready();
      await driver.openChat();
      logTurn(
        `driver ready deployment=${runner.deploymentLabel} agentDid=${runner.agentDid}`,
      );

      await driver.typeComposer(FIRST_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const firstResult = expectLatestSendResult(runner, "turn 1");
      logTurn(
        `turn 1 submitted sessionId=${firstResult.sessionId} requestId=${firstResult.requestId}`,
      );
      await exerciseShellWhileTurnRuns(driver);
      const firstSession = await runner.waitForRequestCompletion(firstResult);
      if (firstSession.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          firstResult.sessionId,
          firstResult.requestId,
        );
        throw new Error(`turn 1 failed diagnostics=${JSON.stringify(diagnostics)}`);
      }
      expectCompletedSession("turn 1", firstSession);
      logTurn(`turn 1 completed requestId=${firstResult.requestId}`);

      await driver.typeComposer(SECOND_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(2);
      });
      const secondResult = expectLatestSendResult(runner, "turn 2");
      expect(secondResult.sessionId).toBe(firstResult.sessionId);
      logTurn(
        `turn 2 submitted sessionId=${secondResult.sessionId} requestId=${secondResult.requestId}`,
      );
      const secondSession = await runner.waitForRequestCompletion(secondResult);
      if (secondSession.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          secondResult.sessionId,
          secondResult.requestId,
        );
        throw new Error(`turn 2 failed diagnostics=${JSON.stringify(diagnostics)}`);
      }
      expectCompletedSession("turn 2", secondSession);
      logTurn(`turn 2 completed requestId=${secondResult.requestId}`);

      await driver.typeComposer(THIRD_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(3);
      });
      const thirdResult = expectLatestSendResult(runner, "turn 3");
      expect(thirdResult.sessionId).toBe(firstResult.sessionId);
      logTurn(
        `turn 3 submitted sessionId=${thirdResult.sessionId} requestId=${thirdResult.requestId}`,
      );
      const finalSession = await runner.waitForRequestCompletion(thirdResult);
      if (finalSession.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          thirdResult.sessionId,
          thirdResult.requestId,
        );
        throw new Error(`turn 3 failed diagnostics=${JSON.stringify(diagnostics)}`);
      }
      logTurn(`turn 3 completed requestId=${thirdResult.requestId}`);

      expect(runner.sentRequests).toHaveLength(3);
      expect(finalSession.sessionId).toBe(firstResult.sessionId);
      expectCompletedSession("turn 3", finalSession);
      expect(finalSession.latestRequestId).toBe(thirdResult.requestId);
      expect(finalSession.timelineItems.length).toBeGreaterThanOrEqual(6);
      expect(finalSession.timelineItems.some((item) => item.kind === "toolGroup")).toBe(
        true,
      );

      const latestSnapshot = await runner.fetchSnapshot();
      const deployment = latestSnapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      expect(deployment?.conversations[0]?.sessionId).toBe(firstResult.sessionId);
      expect(deployment?.conversations[0]?.messageCount).toBeGreaterThanOrEqual(6);
      expect(deployment?.conversations[0]?.toolCallCount).toBeGreaterThan(0);
      logTurn(
        `final snapshot sessionId=${firstResult.sessionId} messageCount=${deployment?.conversations[0]?.messageCount ?? 0} toolCallCount=${deployment?.conversations[0]?.toolCallCount ?? 0}`,
      );
    });
  }, 600_000);
});

async function exerciseShellWhileTurnRuns(driver: LiveDesktopDriver) {
  await exerciseOperationsDrawerTabs(driver);
  await closeOperationsDrawer(driver);

  await driver.openConfig();
  await driver.openConfigSection("backends");
  expect(screen.getByTestId("backend-save")).toBeInTheDocument();
  await driver.openConfigSection("behavior");
  expect(driver.behaviorSystemPrompt()).toBeInTheDocument();

  await driver.openChat();
  expect(driver.composer()).toBeInTheDocument();
}
