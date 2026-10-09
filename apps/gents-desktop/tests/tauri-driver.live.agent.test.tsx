import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
  waitForAgentConfig,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner agent config", () => {
  it("saves and reloads a context prompt, then uses it on the next turn", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const agent =
        deployment.agents.find((candidate) => candidate.isDefault) ??
        deployment.agents[0];
      expect(agent).toBeDefined();
      const suffix = Date.now().toString();
      const agentId = agent!.agentId;
      const contextId = agent!.contextId;
      const systemPrompt = `You are Amy, a repository analysis agent. When asked for the live config marker, include exactly CONFIG-${suffix}.`;

      await driver.ready();
      let previousGeneration = 0;
      await waitFor(
        async () => {
          const current = (await runner.fetchSnapshot()).client?.deployments[0];
          expect(current?.runtime?.reconcilePhase).toBe("idle");
          expect(current?.nodeReadiness.activeGeneration).toBeGreaterThan(0);
          previousGeneration = current!.nodeReadiness.activeGeneration!;
        },
        { timeout: 30_000 },
      );
      /* an agent edits its own instructions (its context) on its page */
      await driver.openConfig();
      await driver.openConfigSection("agents");
      await driver.openConfigItem(agentId);
      await waitFor(() => {
        expect(driver.contextSystemPrompt()).toBeInTheDocument();
      });

      await driver.replaceContextSystemPrompt(systemPrompt);
      await driver.user.click(screen.getByRole("button", { name: "Save" }));

      await waitForAgentConfig(
        runner,
        agentId,
        agent!.displayName,
        systemPrompt,
        previousGeneration,
      );
      logTurn(`context config saved agentId=${agentId} contextId=${contextId}`);

      await driver.openConfigSection("agent");
      await driver.openConfigSection("agents");
      await driver.openConfigItem(agentId);
      await waitFor(() => {
        expect(driver.contextSystemPrompt()).toHaveValue(systemPrompt);
      });

      await driver.openChat();
      await driver.typeComposer("What is the live config marker?");
      await driver.pressEnter();
      await waitFor(() => expect(runner.sendResults).toHaveLength(1));
      const submitted = expectLatestSendResult(runner, "config marker turn");
      expect(submitted.agentId).toBe(agentId);
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
