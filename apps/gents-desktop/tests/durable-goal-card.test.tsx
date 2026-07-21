import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ChatTranscriptPanel } from "../src/components/chat/ChatTranscriptPanel";
import type { DesktopSessionSnapshot } from "../src/lib/types";

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
});
