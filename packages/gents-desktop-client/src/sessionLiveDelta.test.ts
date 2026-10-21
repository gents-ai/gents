import { expect, it } from "vitest";
import { applySessionLiveDelta } from "./sessionLiveDelta.js";
import type { DesktopSessionSnapshot, SessionLiveDeltaView } from "./types.js";

function session(): DesktopSessionSnapshot {
  return {
    sessionId: "session",
    nodeDid: "node",
    agentId: null,
    title: null,
    previewText: null,
    status: null,
    goal: null,
    turnState: "running",
    latestRequestId: "request",
    retryEligibility: { eligible: false, denialReason: null },
    latestRequestOutcome: null,
    pendingTurn: null,
    queuedTurns: [],
    foldedInputs: [],
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 1000,
      compactionThreshold: 0.8,
      compactionThresholdTokens: 800,
      compactionStrategy: "none",
      durableMessageCount: 0,
      providerMessageCount: 0,
      totalCompactedMessages: 0,
      sessionCompactionCount: null,
      requestReductionHistoryLoaded: null,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: [
      {
        kind: "liveAssistant",
        itemKey: "live",
        content: "Streaming response",
        reasoning: null,
      },
    ],
    liveCursor: "current-output",
    projectionRevision: { storeVersion: 1, provenanceVersion: 1 },
  };
}
function unchanged(provenanceVersion: number): SessionLiveDeltaView {
  return {
    outcome: "unchanged",
    requestId: "request",
    turnState: "running",
    status: null,
    content: null,
    reasoning: null,
    liveCursor: "current-output",
    revision: { storeVersion: 2, provenanceVersion },
  };
}
it("requires a bounded snapshot when tool or request provenance changed despite an unchanged live cursor", () => {
  const current = session();
  expect(applySessionLiveDelta(current, unchanged(2))).toBeNull();
  expect(current.projectionRevision).toEqual({
    storeVersion: 1,
    provenanceVersion: 1,
  });
});
it("keeps text-only live updates on the delta path when provenance is unchanged", () => {
  const current = session();
  const next = applySessionLiveDelta(current, unchanged(1));
  expect(next?.projectionRevision).toEqual({
    storeVersion: 2,
    provenanceVersion: 1,
  });
  expect(next?.timelineItems).toBe(current.timelineItems);
});
