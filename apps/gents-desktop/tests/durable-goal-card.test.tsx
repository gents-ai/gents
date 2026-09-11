import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ChatTranscriptPanel } from "@source-inc/gents-desktop-chat";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

describe("durable goal transcript card", () => {
  it("renders persisted goal status, objective, token usage, and active time", () => {
    const session: DesktopSessionSnapshot = {
      sessionId: "session-goal",
      agentDid: "did:test:goal-agent",
      behaviorId: "default",
      title: "goal session",
      previewText: "",
      status: "active",
      turnState: "completed",
      latestRequestId: "request-1",
      latestResponse: null,
      activeResponseOverlay: null,
      pendingTurn: null,
      timelineItems: [],
      goal: {
        goalId: "goal-1",
        objective: "Ship the durable controller",
        status: "active",
        tokenBudget: 50_000,
        tokensUsed: 1_200,
        activeTimeSeconds: 42,
        consecutiveBlockedAudits: 0,
        continuationSequence: 2,
        wrapupRequested: false,
        wrapupCompleted: false,
        lastBlockedReason: null,
        lastFailure: null,
        completionEvidence: null,
      },
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
    // GoalView.status/objective/tokenBudget are all nullable in the generated
    // snapshot: a replicated or partially written Goal row reaches this card.
    // The component has distinct fallback branches for each; without this case
    // only the fully populated projection is observed.
    const session: DesktopSessionSnapshot = {
      sessionId: "session-goal-partial",
      agentDid: "did:test:goal-agent",
      behaviorId: "default",
      title: "goal session",
      previewText: "",
      status: "active",
      turnState: "completed",
      latestRequestId: "request-1",
      latestResponse: null,
      activeResponseOverlay: null,
      pendingTurn: null,
      timelineItems: [],
      goal: {
        goalId: "goal-partial",
        objective: null,
        status: null,
        tokenBudget: null,
        tokensUsed: 37,
        activeTimeSeconds: 0,
        consecutiveBlockedAudits: 0,
        continuationSequence: 0,
        wrapupRequested: false,
        wrapupCompleted: false,
        lastBlockedReason: null,
        lastFailure: null,
        completionEvidence: null,
      },
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
    // An unbudgeted goal renders no budget separator.
    expect(card.textContent).not.toContain("/");
  });
});
