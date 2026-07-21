import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  logTurn,
  waitForBehaviorConfig,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner behavior config", () => {
  it("edits behavior config through the real UI and observes replication", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const behavior =
        deployment.behaviors.find((candidate) => candidate.isDefault) ??
        deployment.behaviors[0];
      expect(behavior).toBeDefined();
      const suffix = Date.now().toString();
      const behaviorId = behavior!.behaviorId;
      const systemPrompt = `You are Amy, a repository analysis agent. Config sentinel ${suffix}.`;

      await driver.ready();
      await driver.openConfig();
      await waitFor(() => {
        expect(driver.behaviorSystemPrompt()).toBeInTheDocument();
      });

      await driver.replaceBehaviorSystemPrompt(systemPrompt);
      await driver.saveBehaviorConfig();

      await waitFor(() => {
        expect(driver.behaviorSaveStatus()).toHaveTextContent("Saved");
      });
      await waitForBehaviorConfig(runner, behaviorId, behaviorId, systemPrompt);
      logTurn(`behavior config saved behaviorId=${behaviorId}`);
    });
  }, 240_000);
});
