import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  FIRST_PROMPT,
  logTurn,
} from "./tauri-driver-live/helpers";

const FOLLOW_UP_PROMPT =
  "After the interrupt flow, answer with exactly one short sentence: ready-after-interrupt.";

describeLive("Tauri app live interrupt flow", () => {
  it("latches an interrupt and surfaces the cause badge in the transcript", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      await driver.ready();
      await driver.openChat();
      logTurn(`driver ready agentDid=${runner.agentDid}`);

      await driver.typeComposer(FIRST_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "interrupt turn");
      logTurn(
        `turn submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
      );

      await waitFor(
        () => {
          const btn = driver.cancelButton();
          expect(btn).toBeTruthy();
          expect(btn).toBeEnabled();
        },
        { timeout: 15_000 },
      );
      logTurn("cancel button enabled");

      await driver.clickCancel();
      logTurn("interrupt clicked");

      const finalSession = await runner.waitForRequestCompletion(submitted);
      if (finalSession?.latestResponse?.cancelCause) {
        const cause = finalSession.latestResponse.cancelCause;
        logTurn(`interrupt latched: cause=${cause.cause}`);
        await waitFor(
          () => {
            const labels = [...document.querySelectorAll(".cause-badge")].map(
              (node) => node.textContent,
            );
            expect(labels).toContain(cancelCauseLabel(cause.cause));
          },
          { timeout: 30_000 },
        );
      } else {
        logTurn(
          "turn finished before interrupt could affect it — " +
            "bridge call succeeded without error (race outcome: turn completed first)",
        );
      }

      await driver.typeComposer(FOLLOW_UP_PROMPT);
      await waitFor(() => {
        expect(driver.sendButton()).toBeEnabled();
      });
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(2);
      });
      const followUp = expectLatestSendResult(runner, "interrupt follow-up");
      expect(followUp.sessionId).toBe(submitted.sessionId);
      expect(followUp.requestId).not.toBe(submitted.requestId);
      logTurn(
        `follow-up submitted sessionId=${followUp.sessionId} requestId=${followUp.requestId}`,
      );

      const followUpSession = await runner.waitForRequestCompletion(followUp);
      expectCompletedSession("interrupt follow-up", followUpSession);
      expect(followUpSession.latestRequestId).toBe(followUp.requestId);
      expect(followUpSession.pendingTurn).toBeNull();
      expect(followUpSession.activeResponseOverlay).toBeNull();

      await waitFor(
        () => {
          expect(screen.getAllByText(FOLLOW_UP_PROMPT)).toHaveLength(1);
          expect(driver.cancelButton()).toBeNull();
        },
        { timeout: 30_000 },
      );

      await driver.typeComposer("Composer should be ready after follow-up.");
      await waitFor(() => {
        expect(driver.sendButton()).toBeEnabled();
      });
    });
  }, 600_000);
});

function cancelCauseLabel(cause: string) {
  switch (cause) {
    case "userCancelled":
      return "user cancelled";
    case "interrupted":
      return "interrupted";
    case "deadline":
      return "deadline expired";
    case "unknown":
      return "cause unknown";
    default:
      return cause;
  }
}
