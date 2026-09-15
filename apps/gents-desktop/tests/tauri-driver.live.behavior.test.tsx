import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
  waitForBehaviorConfig,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner behavior config", () => {
  it("saves and reloads a context prompt, then uses it on the next turn", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const behavior =
        deployment.behaviors.find((candidate) => candidate.isDefault) ??
        deployment.behaviors[0];
      expect(behavior).toBeDefined();
      const suffix = Date.now().toString();
      const behaviorId = behavior!.behaviorId;
      const contextId = behavior!.contextId;
      const systemPrompt = `You are Amy, a repository analysis agent. When asked for the live config marker, include exactly CONFIG-${suffix}.`;

      await driver.ready();
      let previousGeneration = 0;
      await waitFor(
        async () => {
          const current = (await runner.fetchSnapshot()).client?.deployments[0];
          expect(current?.runtime?.reconcilePhase).toBe("idle");
          expect(current?.behaviorReadiness.activeGeneration).toBeGreaterThan(0);
          previousGeneration = current!.behaviorReadiness.activeGeneration!;
        },
        { timeout: 30_000 },
      );
      await driver.openConfig();
      await driver.openConfigSection("contexts");
      await driver.openConfigItem(contextId);
      await waitFor(() => {
        expect(driver.contextSystemPrompt()).toBeInTheDocument();
      });

      await driver.replaceContextSystemPrompt(systemPrompt);
      await driver.user.click(screen.getByRole("button", { name: "Save changes" }));

      await waitForBehaviorConfig(
        runner,
        behaviorId,
        behavior!.displayName,
        systemPrompt,
        previousGeneration,
      );
      logTurn(`context config saved behaviorId=${behaviorId} contextId=${contextId}`);

      await driver.openConfigSection("agent");
      await driver.openConfigSection("contexts");
      await driver.openConfigItem(contextId);
      await waitFor(() => {
        expect(driver.contextSystemPrompt()).toHaveValue(systemPrompt);
      });

      await driver.openChat();
      await driver.typeComposer("What is the live config marker?");
      await driver.pressEnter();
      await waitFor(() => expect(runner.sendResults).toHaveLength(1));
      const submitted = expectLatestSendResult(runner, "config marker turn");
      expect(submitted.behaviorId).toBe(behaviorId);
      const session = await runner.waitForRequestCompletion(submitted);
      expectCompletedSession("config marker turn", session);
      const assistantText = session.timelineItems
        .filter((item) => item.kind === "assistantMessage")
        .map((item) => `${item.content ?? ""}\n${item.reasoning ?? ""}`)
        .join("\n");
      expect(assistantText).toContain(`CONFIG-${suffix}`);
    });
  }, 600_000);
});
