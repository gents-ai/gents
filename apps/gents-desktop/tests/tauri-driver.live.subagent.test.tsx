import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

const SUBAGENT_PROMPT =
  "Use the configured local subagent target. Call create_session with that agent and ask it to read workspace/AGENTS.md and return the phrase live-subagent-smoke with one short finding. Then reply with one sentence saying it has started.";
const FOLLOW_UP_PROMPT =
  "Without calling tools, reply with one short sentence containing live-subagent-followup.";

describeLive("Tauri app live subagent sessions", () => {
  it("starts a session with a configured target and projects its provenance", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const defaultBehavior = deployment.behaviors.find(
        (behavior) =>
          behavior.behaviorId === deployment.agentPrincipal.defaultBehaviorId,
      );
      const context = deployment.contexts.find(
        (candidate) => candidate.context_id === defaultBehavior?.contextId,
      );
      const defaultTools = deployment.tools.find(
        (tools) => tools.tools_id === context?.tools_id,
      );
      const targetId = defaultTools?.subagents?.target_ids?.[0];
      const subagentTarget = deployment.subagentTargets.find(
        (target) => target.target_id === targetId,
      );
      expect(
        subagentTarget,
        "live fixture did not expose a subagent target",
      ).toBeDefined();
      const subagentBehaviorId = subagentTarget?.behavior_id;
      expect(defaultTools?.subagents?.enabled).toBe(true);

      await driver.ready();
      await driver.openChat();
      logTurn(`subagent driver ready target=${subagentBehaviorId}`);

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
        toolNames.some((name) => /create_session/i.test(name)),
        `expected create_session in tool names: ${JSON.stringify(toolNames)}`,
      ).toBe(true);

      await waitFor(
        async () => {
          const provenance = await runner.adapter.sessionProvenance({
            sessionId: submitted.sessionId,
            agentDid: runner.agentDid,
          });
          const caused = provenance.sent.filter(
            (request) => request.causedByRequestId === submitted.requestId,
          );
          expect(caused.length).toBeGreaterThan(0);
          expect(caused.every((request) => request.hop === 1)).toBe(true);
          const started = caused.filter(
            (request) => request.behaviorId === subagentBehaviorId,
          );
          expect(
            started.some((request) => request.lifecycleState === "completed"),
            `expected the started session's request to complete; caused=${JSON.stringify(caused)}`,
          ).toBe(true);
          const child = started[0]!;
          expect(child.sessionId).not.toBe(submitted.sessionId);

          const received = await runner.adapter.sessionProvenance({
            sessionId: child.sessionId!,
            agentDid: runner.agentDid,
          });
          expect(
            received.received.some(
              (request) => request.causedBySessionId === submitted.sessionId,
            ),
          ).toBe(true);

          const childProfile = deployment.inferenceProfiles.find(
            (profile) =>
              profile.profile_id ===
              deployment.behaviors.find(
                (behavior) => behavior.behaviorId === child.behaviorId,
              )?.inferenceProfileId,
          );
          expect(
            childProfile,
            "the target behavior should reference a resolvable inference profile",
          ).toBeDefined();
        },
        { timeout: 120_000 },
      );

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
