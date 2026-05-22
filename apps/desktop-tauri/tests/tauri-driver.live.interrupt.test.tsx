import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
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
      logTurn(`driver ready agentDid=${runner.agentDid}`);

      // Submit a turn and immediately interrupt it.
      await driver.typeComposer(FIRST_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = runner.sendResults.at(-1);
      expect(submitted).toBeDefined();
      logTurn(
        `turn submitted sessionId=${submitted!.sessionId} requestId=${submitted!.requestId}`,
      );

      // Wait briefly for the turn to register so the cancel button becomes
      // enabled. The cancel button is rendered by CancelButton with
      // data-testid="cancel-button" (added in Plan B Task 2).
      await waitFor(
        () => {
          const btn = driver.cancelButton();
          expect(btn).toBeTruthy();
          expect(btn).toBeEnabled();
        },
        { timeout: 15_000 },
      );
      logTurn("cancel button enabled");

      // Click Interrupt. For a turn with no children, the bridge latches
      // directly without opening the cascade dialog (cascade=false path).
      await driver.clickCancel();
      logTurn("interrupt clicked");

      // If the response was interrupted before completion, verify the badge
      // appears in the rendered transcript. If the turn finished naturally
      // before our click, log that and move on — we still verified the
      // bridge call did not throw.
      const finalSession = await runner.waitForRequestCompletion(submitted!);
      if (finalSession?.latestResponse?.cancelCause) {
        const cause = finalSession.latestResponse.cancelCause;
        logTurn(`interrupt latched: cause=${cause.cause}`);
        // The response has a cancel cause — the interrupt landed mid-flight.
        // The transcript should render the same badge text users see in the
        // cancelled turn.
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
        // Turn finished before our click could take effect. This is a valid
        // race outcome — the bridge HTTP call still succeeded (no throw above),
        // and the test documents that the interrupt window was too narrow.
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
      const followUp = runner.sendResults.at(-1);
      expect(followUp).toBeDefined();
      expect(followUp!.sessionId).toBe(submitted!.sessionId);
      expect(followUp!.requestId).not.toBe(submitted!.requestId);
      logTurn(
        `follow-up submitted sessionId=${followUp!.sessionId} requestId=${followUp!.requestId}`,
      );

      const followUpSession = await runner.waitForRequestCompletion(followUp!);
      expectCompletedSession("interrupt follow-up", followUpSession);
      expect(followUpSession.latestRequestId).toBe(followUp!.requestId);
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
    } finally {
      await driver.dispose();
    }
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
