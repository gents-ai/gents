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
      requestReductionHistoryLoaded: false,
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

it("updates activity on an unchanged live tail and clears it when unavailable", () => {
  const current = { ...session(), inferenceActivity: "working" };
  const retry = applySessionLiveDelta(current, {
    ...unchanged(1),
    inferenceActivity: "retrying",
  });
  expect(retry?.inferenceActivity).toBe("retrying");
  const unavailable = applySessionLiveDelta(retry!, {
    ...unchanged(1),
    inferenceActivity: "unavailable",
  });
  expect(unavailable?.inferenceActivity).toBe("unavailable");
});

it("rejects activity from a different request, terminal turn or stale cursor", () => {
  const current = { ...session(), inferenceActivity: "working" };
  for (const stale of [
    { ...unchanged(1), requestId: "other", inferenceActivity: "retrying" },
    { ...unchanged(1), turnState: "completed", inferenceActivity: "retrying" },
    { ...unchanged(1), liveCursor: "older", inferenceActivity: "retrying" },
    {
      ...unchanged(1),
      revision: { storeVersion: 0, provenanceVersion: 1 },
      inferenceActivity: "retrying",
    },
  ]) {
    expect(applySessionLiveDelta(current, stale)).toBeNull();
  }
  expect(current.inferenceActivity).toBe("working");
});

it("updates activity while appending a streamed segment", () => {
  const current = { ...session(), inferenceActivity: "retrying" };
  const next = applySessionLiveDelta(current, {
    ...unchanged(1),
    outcome: "delta",
    inferenceActivity: "working",
    content: { mode: "append", value: "!", byteLen: 19, hash: "c2d12fb1" },
    reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
  });
  expect(next?.inferenceActivity).toBe("working");
  expect(next?.timelineItems[0]).toMatchObject({
    content: "Streaming response!",
  });
});
