import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import { exerciseOperationsDrawerTabs } from "./tauri-driver-live/operations-assertions";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

const OPERATIONS_PROMPT =
  "Reply with exactly this sentence and no tools: operations drawer smoke ready.";

describeLive("Tauri app live operations drawer", () => {
  it("opens every operations tab after a real chat turn", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      await driver.ready();
      await driver.openChat();
      logTurn(`operations drawer driver ready agentDid=${runner.agentDid}`);

      await driver.typeComposer(OPERATIONS_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "operations seed turn");
      const session = await runner.waitForRequestCompletion(submitted);
      expectCompletedSession("operations drawer seed turn", session);

      await exerciseOperationsDrawerTabs(driver);
    });
  }, 600_000);
});
