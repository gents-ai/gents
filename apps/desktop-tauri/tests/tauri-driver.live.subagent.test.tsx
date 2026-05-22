import { screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  closeOperationsDrawer,
  expectOperationsPanel,
  openOperationsDrawer,
} from "./tauri-driver-live/operations-assertions";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

const SUBAGENT_PROMPT =
  'Use the configured local subagent target. Call spawn_subagent with await_mode "background" and ask the child to read workspace/PROMPT.md and return the phrase live-subagent-smoke with one short finding. Then call wait_subagent for that child request and reply with one sentence containing live-subagent-smoke.';
const FOLLOW_UP_PROMPT =
  "Without calling tools, reply with one short sentence containing live-subagent-followup.";

describeLive("Tauri app live subagent backgrounding", () => {
  it("spawns a configured local subagent and renders its lineage", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const defaultBehavior = deployment.behaviors.find(
        (behavior) => behavior.isDefault,
      );
      const defaultTools = deployment.toolSelections.find(
        (selection) => selection.selectionId === defaultBehavior?.toolSelectionId,
      );
      const subagentTarget = defaultTools?.subagentTargets[0];
      expect(
        subagentTarget,
        "live fixture did not expose a subagent target",
      ).toBeDefined();
      expect(defaultTools?.subagentSpawnEnabled).toBe(true);
      expect(defaultTools?.subagentBackgroundEnabled).toBe(true);

      await driver.ready();
      await driver.openChat();
      logTurn(`subagent driver ready target=${subagentTarget}`);

      await driver.typeComposer(SUBAGENT_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "subagent turn");
      const session = await runner.waitForRequestCompletion(submitted);
      if (session.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          submitted.sessionId,
          submitted.requestId,
        );
        throw new Error(
          `subagent turn failed diagnostics=${JSON.stringify(diagnostics)}`,
        );
      }
      expectCompletedSession("subagent turn", session);
      expect(
        hasAssistantResponse(session.timelineItems),
        `subagent turn rendered no assistant response: ${JSON.stringify(
          session.timelineItems.slice(-8),
        )}`,
      ).toBe(true);

      const toolNames = session.timelineItems.flatMap((item) =>
        item.kind === "toolGroup" ? item.tools.map((tool) => tool.toolName) : [],
      );
      expect(toolNames.some((name) => /spawn_subagent/i.test(name))).toBe(true);

      await waitFor(
        async () => {
          const tree = await runner.adapter.listSubagentTree({
            rootRequestId: submitted.requestId,
            agentDid: runner.agentDid,
            includeTerminal: true,
          });
          expect(tree.edges.length).toBeGreaterThan(0);
          expect(tree.edges.some((edge) => edge.awaitMode === "background")).toBe(true);
          expect(tree.nodes.some((node) => node.behaviorId === subagentTarget)).toBe(
            true,
          );
        },
        { timeout: 60_000 },
      );

      await openOperationsDrawer(driver);
      await driver.user.click(screen.getByRole("tab", { name: "Lineage" }));
      const lineagePanel = await expectOperationsPanel("lineage");
      await waitFor(() => {
        expect(
          within(lineagePanel).getByRole("tree", { name: "Subagent lineage" }),
        ).toBeInTheDocument();
        expect(lineagePanel).toHaveTextContent(/spawn_subagent/i);
        expect(lineagePanel).toHaveTextContent(subagentTarget!);
      });

      await closeOperationsDrawer(driver);
      await waitFor(() => {
        expect(driver.composer()).toBeInTheDocument();
      });
      await driver.typeComposer(FOLLOW_UP_PROMPT);
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(2);
      });
      const followUp = expectLatestSendResult(runner, "subagent follow-up");
      expect(followUp.sessionId).toBe(submitted.sessionId);
      expect(followUp.requestId).not.toBe(submitted.requestId);
      logTurn(
        `follow-up submitted sessionId=${followUp.sessionId} requestId=${followUp.requestId}`,
      );

      const followUpSession = await runner.waitForRequestCompletion(followUp);
      expectCompletedSession("subagent follow-up", followUpSession);
      expect(followUpSession.latestRequestId).toBe(followUp.requestId);
      expect(
        hasAssistantResponse(followUpSession.timelineItems),
        `subagent follow-up rendered no assistant response: ${JSON.stringify(
          followUpSession.timelineItems.slice(-8),
        )}`,
      ).toBe(true);
      expect(followUpSession.pendingTurn).toBeNull();
      expect(followUpSession.activeResponseOverlay).toBeNull();
    });
  }, 600_000);
});

function hasAssistantResponse(
  timelineItems: Array<{ kind: string; content?: unknown; reasoning?: unknown }>,
) {
  return timelineItems.some((item) => {
    if (item.kind !== "assistantMessage" && item.kind !== "liveAssistant") {
      return false;
    }
    const content = normalizeTimelineText(item.content);
    const reasoning = normalizeTimelineText(item.reasoning);
    return content.length > 0 || reasoning.length > 0;
  });
}

function normalizeTimelineText(content: unknown) {
  if (typeof content === "string") {
    return content.trim();
  }
  if (content == null) {
    return "";
  }
  return JSON.stringify(content).trim();
}
