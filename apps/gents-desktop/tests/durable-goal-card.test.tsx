import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ChatTranscriptPanel } from "@source-inc/gents-desktop-chat";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { goalControl, observedGoal } from "./goal-state-fixture";
import { sessionContext } from "./timeline-fixture";

describe("durable goal transcript card", () => {
  it("renders persisted goal status, objective, token usage, and active time", () => {
    const session: DesktopSessionSnapshot = {
      sessionId: "session-goal",
      nodeDid: "did:test:goal-node",
      agentDid: "default",
      title: "goal session",
      previewText: "",
      status: "active",
      turnState: "completed",
      latestRequestId: "request-1",
      retryEligibility: { eligible: false, denialReason: null },
      latestRequestOutcome: null,
      pendingTurn: null,
      queuedTurns: [],
      foldedInputs: [],
      context: sessionContext(),
      timelineItems: [],
      goal: observedGoal(
        goalControl("active", {
          goalId: "goal-1",
          objective: "Ship the durable controller",
          tokenBudget: "50000",
          tokensUsed: "1200",
          activeTimeSeconds: "42",
          continuationSequence: "2",
        }),
      ),
    };

    render(
      <ChatTranscriptPanel selectedSessionId={session.sessionId} session={session} />,
    );

    const card = screen.getByTestId("durable-goal-card");
    expect(card).toHaveTextContent("durable goal · active");
    expect(card).toHaveTextContent("Ship the durable controller");
    expect(card).toHaveTextContent("1200 / 50000 charged tokens");
    expect(card).toHaveTextContent("42s active");
  });

  it("renders runtime fallbacks for a goal persisted without status, objective, or budget", () => {
    const session: DesktopSessionSnapshot = {
      sessionId: "session-goal-partial",
      nodeDid: "did:test:goal-node",
      agentDid: "default",
      title: "goal session",
      previewText: "",
      status: "active",
      turnState: "completed",
      latestRequestId: "request-1",
      latestRequestOutcome: null,
      pendingTurn: null,
      queuedTurns: [],
      foldedInputs: [],
      timelineItems: [],
      goal: observedGoal(
        goalControl("", { goalId: "goal-partial", objective: "", tokensUsed: "37" }),
      ),
      retryEligibility: { eligible: false, denialReason: null },
      context: {
        estimatedDurableTokens: 0,
        estimatedConversationTokens: 0,
        contextWindow: 1,
        compactionThreshold: 0.8,
        compactionThresholdTokens: 1,
        compactionStrategy: "summary",
        durableMessageCount: 0,
        providerMessageCount: 0,
        totalCompactedMessages: 0,
        sessionCompactionCount: null,
        requestReductionHistoryLoaded: false,
        compactions: [],
        lastRequest: null,
      },
    };

    render(
      <ChatTranscriptPanel selectedSessionId={session.sessionId} session={session} />,
    );

    const card = screen.getByTestId("durable-goal-card");
    expect(card).toHaveTextContent("durable goal · unknown");
    expect(card).toHaveTextContent("No objective");
    expect(card).toHaveTextContent("37 charged tokens");
    expect(card).toHaveTextContent("0s active");
    expect(card.textContent).not.toContain("/");
  });

  it("shows no card for a Goal state that is not a verified Goal", () => {
    for (const goal of [
      observedGoal(null),
      { kind: "no_readable_session", next: "send" },
      {
        kind: "unknown",
        cause: "projection_unverified",
        next: "await_publication",
        reason: "unverified",
      },
      null,
    ] as const) {
      const { unmount } = render(
        <ChatTranscriptPanel
          selectedSessionId="session-goal"
          session={
            {
              sessionId: "session-goal",
              nodeDid: "did:test:node",
              timelineItems: [],
              goal,
            } as unknown as DesktopSessionSnapshot
          }
        />,
      );
      expect(screen.queryByTestId("durable-goal-card")).toBeNull();
      unmount();
    }
  });
});
