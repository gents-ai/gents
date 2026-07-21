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
  'Use the configured local subagent target. Call spawn_subagent with await_mode "background" and ask the child to read workspace/CLAUDE.md and return the phrase live-subagent-smoke with one short finding. Then call wait_subagent for that child request and reply with one sentence containing live-subagent-smoke.';
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
      expect(
        toolNames.some((name) => /spawn_subagent/i.test(name)),
        `expected spawn_subagent in tool names: ${JSON.stringify(toolNames)}`,
      ).toBe(true);
      expect(
        toolNames.some((name) => /wait_subagent/i.test(name)),
        `expected wait_subagent in tool names (prompt asked the parent to await the child): ${JSON.stringify(toolNames)}`,
      ).toBe(true);

      const parentReply = collectAssistantText(session.timelineItems);
      expect(
        /live-subagent-smoke/i.test(parentReply),
        `parent reply did not echo the sentinel "live-subagent-smoke"; reply=${parentReply.slice(0, 400)}`,
      ).toBe(true);

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
          const childNodes = tree.nodes.filter(
            (node) => node.behaviorId === subagentTarget,
          );
          expect(
            childNodes.some(
              (node) =>
                node.lifecycleState === "completed" || node.status === "completed",
            ),
            `expected at least one subagent child to reach a completed terminal state; nodes=${JSON.stringify(childNodes)}`,
          ).toBe(true);

          // When subagent inference is configured separately, parent and child
          // nodes must resolve to distinct backend ids. When it falls back to the
          // primary, the assertion still validates that backend_id is populated.
          const parentNode = tree.nodes.find(
            (node) => node.requestId === submitted.requestId,
          );
          const childNode = tree.nodes.find(
            (node) => node.behaviorId === subagentTarget,
          );
          expect(
            parentNode?.backendId,
            "parent backendId should be populated",
          ).toBeTruthy();
          expect(
            childNode?.backendId,
            "child backendId should be populated",
          ).toBeTruthy();
          if (process.env.GENTS_TAURI_LIVE_SUBAGENT_INFERENCE_URL) {
            expect(childNode?.backendId).not.toBe(parentNode?.backendId);
          }
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

      // The follow-up prompt forbids tools and asks for the sentinel phrase.
      // Verify the model honored both constraints.
      const followUpToolGroupsAfterParent = followUpSession.timelineItems.filter(
        (item) => item.kind === "toolGroup",
      ).length;
      const parentToolGroups = session.timelineItems.filter(
        (item) => item.kind === "toolGroup",
      ).length;
      expect(
        followUpToolGroupsAfterParent,
        `follow-up was instructed to use no tools but added ${followUpToolGroupsAfterParent - parentToolGroups} tool group(s)`,
      ).toBe(parentToolGroups);

      const followUpReply = collectAssistantText(followUpSession.timelineItems);
      expect(
        /live-subagent-followup/i.test(followUpReply),
        `follow-up reply did not echo the sentinel "live-subagent-followup"; reply=${followUpReply.slice(0, 400)}`,
      ).toBe(true);
    });
  }, 600_000);
});

function collectAssistantText(
  timelineItems: Array<{
    kind: string;
    content?: unknown;
    reasoning?: unknown;
  }>,
) {
  return timelineItems
    .filter((item) => item.kind === "assistantMessage" || item.kind === "liveAssistant")
    .map((item) =>
      [normalizeTimelineText(item.content), normalizeTimelineText(item.reasoning)]
        .filter((text) => text.length > 0)
        .join(" "),
    )
    .join("\n");
}

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
