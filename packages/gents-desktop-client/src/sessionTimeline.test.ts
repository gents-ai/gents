import { expect, it } from "vitest";
import { mergeSessionTipSnapshot } from "./sessionTimeline.js";
import type { DesktopSessionSnapshot, RenderedTimelineItem } from "./types.js";

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

it("updates physical ownership and terminal facts even when assistant text is unchanged", () => {
  const item: RenderedTimelineItem = {
    kind: "assistantMessage",
    itemKey: "answer",
    requestId: "logical",
    content: "Done",
    requestTurn: {
      requestDocId: "physical",
      requestId: "logical",
      settled: false,
    },
    sequence: 1,
    reconstruction: { state: "ready" },
  };
  const current = { ...session(), timelineItems: [item] };
  const nextItem = {
    ...item,
    requestTurn: { ...item.requestTurn!, settled: true },
  };
  const tip: DesktopSessionSnapshot = {
    ...current,
    timelineItems: [nextItem],
    timelinePage: {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "answer",
      newestItemKey: "answer",
    },
  };
  const merged = mergeSessionTipSnapshot(current, tip);
  expect(merged.timelineItems[0]).toBe(nextItem);
  expect(current.timelineItems[0]).toBe(item);
  expect(mergeSessionTipSnapshot(merged, tip).timelineItems[0]).toBe(nextItem);
});
