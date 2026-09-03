import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

const NATIVE_BACKGROUND_PROMPT =
  'Launch a native background process now. You MUST call spawn_process exactly once with tool_name "bash_unrestricted". Its args must set command to "sleep 20; printf live-native-background-smoke", args to an empty array, and timeout_secs to 25. Do not call wait_process, read_process, list_processes, cancel_process, or any other tool. Reply immediately with one short sentence after spawn_process returns.';

describeLive("Tauri app live operations snapshot", () => {
  it("projects a live native background process through the bridge", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const defaultBehavior = deployment.behaviors.find(
        (behavior) =>
          behavior.behaviorId === deployment.agentPrincipal.defaultBehaviorId,
      );
      const defaultTools = deployment.toolSelections.find(
        (selection) => selection.selectionId === defaultBehavior?.toolSelectionId,
      );
      expect(defaultTools?.enableBash).toBe(true);
      expect(defaultTools?.bashMode).toBe("Unrestricted");
      expect(defaultTools?.backgroundableToolNames).toContain("bash_unrestricted");

      await driver.ready();
      await driver.openChat();
      await driver.typeComposer(NATIVE_BACKGROUND_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "native background turn");

      const nativeTool = await waitFor(
        async () => {
          const snapshot = await runner.adapter.fetchOperationsSnapshot({
            agentDid: runner.agentDid,
            rootRequestId: submitted.requestId,
          });
          const row = snapshot.backgroundedTools.find(
            (candidate) =>
              candidate.requestId === submitted.requestId &&
              candidate.toolName === "bash_unrestricted",
          );
          expect(
            row,
            `expected running native background tool: ${JSON.stringify(snapshot.backgroundedTools)}`,
          ).toBeDefined();
          expect(row?.awaitMode).toBe("background");
          expect(row?.childRequestId).toBeNull();
          expect(row?.lifecycleState).toMatch(/pending|running/i);
          expect(row?.nativeExecutor).not.toBeNull();
          return row!;
        },
        { timeout: 60_000, interval: 250 },
      );
      logTurn(
        `native background process projected toolCallId=${nativeTool.toolCallId} requestId=${submitted.requestId}`,
      );

      const session = await runner.waitForRequestCompletion(submitted);
      expectCompletedSession("native background parent turn", session);
    });
  }, 600_000);
});
