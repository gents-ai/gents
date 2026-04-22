import { describe, expect, test } from "bun:test";

import type { ConversationSummary, DesktopSessionSnapshot } from "./types";
import { projectChatShell } from "./chat-shell";

function conversation(overrides: Partial<ConversationSummary> = {}): ConversationSummary {
  return {
    sessionId: "session-1",
    title: "conversation",
    previewText: "preview",
    status: "active",
    behaviorId: "default",
    latestRequestId: "req-1",
    createdAt: "2026-04-21T00:00:00Z",
    updatedAt: "2026-04-21T00:00:00Z",
    turnState: "completed",
    messageCount: 1,
    toolCallCount: 0,
    ...overrides,
  };
}

function session(overrides: Partial<DesktopSessionSnapshot> = {}): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:defra:amy",
    behaviorId: "default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "completed",
    latestRequestId: "req-1",
    latestResponse: null,
    activeResponseOverlay: null,
    pendingTurn: null,
    messages: [],
    toolCalls: [],
    toolResults: [],
    ...overrides,
  };
}

describe("projectChatShell", () => {
  test("blocks follow up while turn is streaming", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "streaming" }),
      session: session({ turnState: "streaming", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint: "Turn still streaming",
    });
  });

  test("uses tracked request before observed latest request catches up", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ latestRequestId: "req-old", turnState: "completed" }),
      session: session({
        latestRequestId: "req-new",
        turnState: "streaming",
        pendingTurn: {
          requestId: "req-new",
          content: "follow up",
          lifecycleState: "processing",
          createdAt: "2026-04-21T00:01:00Z",
        },
      }),
      localWorkflow: {
        kind: "awaitingObservation",
        sessionId: "session-1",
        requestId: "req-new",
      },
    });

    expect(projection.activeRequestId).toBe("req-new");
    expect(projection.workflow.kind).toBe("turnInProgress");
    expect(projection.sendStatus.kind).toBe("disabled");
  });

  test("blocks inconsistent observation when latest request is missing", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ latestRequestId: "req-missing", turnState: undefined }),
      session: session({ latestRequestId: undefined, turnState: undefined }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({
      kind: "blocked",
      reason: "inconsistentTurnObservation",
      turnState: undefined,
    });
    expect(projection.sendStatus).toEqual({
      kind: "disabled",
      reason: "inconsistentTurnObservation",
      hint: "Waiting for consistent turn observation",
    });
  });

  test("allows follow up after terminal turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "completed" }),
      session: session({ turnState: "completed", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up after interrupted turn", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: conversation({ turnState: "interrupted" }),
      session: session({ turnState: "interrupted", latestRequestId: "req-1" }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });

  test("allows follow up when conversation summary is missing but session snapshot is terminal", () => {
    const projection = projectChatShell({
      clientAvailable: true,
      selectedAgentDid: "did:defra:amy",
      selectedSessionId: "session-1",
      draft: "follow up",
      sending: false,
      selectedConversation: null,
      session: session({
        title: null,
        previewText: null,
        turnState: "completed",
        latestRequestId: "req-1",
      }),
      localWorkflow: { kind: "ready" },
    });

    expect(projection.workflow).toEqual({ kind: "ready" });
    expect(projection.sendStatus).toEqual({ kind: "ready" });
  });
});
