import { describe, expect, it } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import {
  applySessionLiveDelta,
  sessionLiveDeltaRequest,
} from "@source-inc/gents-desktop-client";
import { mergeSessionTipSnapshot } from "@source-inc/gents-desktop-client";

describe("live session deltas", () => {
  it("applies a verified suffix while preserving historical row identity", () => {
    const current = session(["k1"], null);
    const historical = current.timelineItems[0];
    current.projectionRevision = { storeVersion: 7, provenanceVersion: 1 };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "hello",
      reasoning: null,
    });
    const request = sessionLiveDeltaRequest(current, "request-1");
    expect(request).toMatchObject({
      baseLiveCursor: "cursor",
      baseContentByteLen: 5,
      baseContentHash: "4f9f2cab",
    });

    const next = applySessionLiveDelta(current, {
      outcome: "delta",
      liveCursor: "cursor",
      revision: { storeVersion: 8, provenanceVersion: 1 },
      requestId: "request-1",
      turnState: "running",
      status: null,
      content: {
        mode: "append",
        value: " world",
        byteLen: 11,
        hash: "d58b3fa7",
      },
      reasoning: {
        mode: "unchanged",
        value: "",
        byteLen: 0,
        hash: "811c9dc5",
      },
    });
    expect(next?.timelineItems[0]).toBe(historical);
    expect(next?.timelineItems.at(-1)).toMatchObject({ content: "hello world" });
  });

  it("keeps timeline and live item identity for an unchanged delta", () => {
    const current = session(["k1"], null);
    current.projectionRevision = { storeVersion: 7, provenanceVersion: 1 };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "hello",
      reasoning: null,
    });
    const live = current.timelineItems.at(-1);
    const unchanged = { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" };

    const next = applySessionLiveDelta(current, {
      outcome: "unchanged",
      liveCursor: "cursor",
      revision: { storeVersion: 8, provenanceVersion: 1 },
      requestId: "request-1",
      turnState: "running",
      status: null,
      content: { ...unchanged, byteLen: 5, hash: "4f9f2cab" },
      reasoning: unchanged,
    });

    expect(next?.timelineItems).toBe(current.timelineItems);
    expect(next?.timelineItems.at(-1)).toBe(live);
    expect(next?.projectionRevision).toEqual({ storeVersion: 8, provenanceVersion: 1 });
  });

  it("removes a reset live tail between tool-loop assistant turns", () => {
    const current = session(["k1"], null);
    current.projectionRevision = { storeVersion: 7, provenanceVersion: 1 };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "stale opening prefix",
      reasoning: null,
    });

    const next = applySessionLiveDelta(current, {
      outcome: "delta",
      liveCursor: "cursor",
      revision: { storeVersion: 8, provenanceVersion: 1 },
      requestId: "request-1",
      turnState: "running",
      status: null,
      content: {
        mode: "replace",
        value: "",
        byteLen: 0,
        hash: "811c9dc5",
      },
      reasoning: {
        mode: "unchanged",
        value: "",
        byteLen: 0,
        hash: "811c9dc5",
      },
    });

    expect(next?.timelineItems).toHaveLength(1);
    expect(next?.timelineItems[0].itemKey).toBe("k1");
  });

  it("rejects a corrupt suffix", () => {
    const current = session([], null);
    current.projectionRevision = { storeVersion: 4, provenanceVersion: 1 };
    current.timelineItems = [
      {
        kind: "liveAssistant",
        itemKey: "live-assistant",
        content: "a",
        reasoning: null,
      },
    ];
    const base = {
      outcome: "delta",
      liveCursor: "cursor",
      revision: { storeVersion: 5, provenanceVersion: 1 },
      requestId: "request-1",
      turnState: "running",
      status: null,
      content: {
        mode: "append",
        value: "b",
        byteLen: 2,
        hash: "deadbeef",
      },
      reasoning: {
        mode: "unchanged",
        value: "",
        byteLen: 0,
        hash: "811c9dc5",
      },
    };
    expect(applySessionLiveDelta(current, base)).toBeNull();
  });
});

function session(
  keys: string[],
  page: DesktopSessionSnapshot["timelinePage"],
): DesktopSessionSnapshot {
  return {
    liveCursor: "cursor",
    sessionId: "session-1",
    nodeDid: "did:key:test",
    agentDid: "agent-1",
    title: "Test",
    previewText: null,
    status: "active",
    goal: null,
    turnState: "running",
    latestRequestId: "request-1",
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestRequestOutcome: null,
    pendingTurn: null,
    queuedTurns: [],
    foldedInputs: [],
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 1,
      compactionThreshold: 0.8,
      compactionThresholdTokens: 1,
      compactionStrategy: "summary",
      durableMessageCount: keys.length,
      providerMessageCount: keys.length,
      totalCompactedMessages: 0,
      sessionCompactionCount: null,
      requestReductionHistoryLoaded: false,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: keys.map((key) => ({
      kind: "userMessage" as const,
      itemKey: key,
      requestId: key,
      ownsTurn: true,
      inputRequestId: key,
      sequence: Number(key.slice(1)),
      content: key,
      timestamp: null,
      reconstruction: { state: "ready" },
    })),
    timelinePage: page,
  };
}

describe("session timeline page merging", () => {
  it(
    "keeps a queued input when only the request's context row is saved on the tip page",
    () => {
      const page = {
        totalItems: 3,
        pageItems: 2,
        hasOlder: true,
        hasNewer: false,
        oldestItemKey: "k1",
        newestItemKey: "k2",
      };
      const current = session(["k1", "k2"], page);
      current.timelineItems.unshift({
        kind: "pendingUserTurn",
        itemKey: "pending-r",
        requestId: "r",
        content: "same text",
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin: null,
        createdAt: null,
      });
      const incoming = session(["k1", "k2"], page);
      incoming.timelineItems.push({
        kind: "userMessage",
        itemKey: "authored-r-context",
        requestId: "r",
        ownsTurn: false,
        sequence: 3,
        content: "<context>\nworkspace instructions\n</context>",
        timestamp: null,
        reconstruction: { state: "ready" },
      });
      const merged = mergeSessionTipSnapshot(current, incoming);
      expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
        "pending-r",
        "k1",
        "k2",
        "authored-r-context",
      ]);
    },
  );

  it(
    "replaces a queued input with its durable owner when the tip page arrives",
    () => {
      const page = {
        totalItems: 3,
        pageItems: 2,
        hasOlder: true,
        hasNewer: false,
        oldestItemKey: "k1",
        newestItemKey: "k2",
      };
      const current = session(["k1", "k2"], page);
      current.timelineItems.unshift({
        kind: "pendingUserTurn",
        itemKey: "pending-r",
        requestId: "r",
        content: "same text",
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin: null,
        createdAt: null,
      });
      const incoming = session(["k1", "k2"], page);
      incoming.timelineItems.push({
        kind: "userMessage",
        itemKey: "authored-r",
        requestId: "r",
        ownsTurn: true,
        inputRequestId: "r",
        sequence: 3,
        content: "same text",
        timestamp: null,
        reconstruction: { state: "ready" },
      });
      const merged = mergeSessionTipSnapshot(current, incoming);
      expect(
        merged.timelineItems
          .filter(
            (item) =>
              (item.kind === "userMessage" || item.kind === "pendingUserTurn") &&
              item.requestId === "r",
          )
          .map((item) => item.kind),
      ).toEqual(["userMessage"]);
      expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
        "k1",
        "k2",
        "authored-r",
      ]);
    },
  );

  it(
    "replaces a queued trigger input with its published automated item when the tip page arrives",
    () => {
      const page = {
        totalItems: 3,
        pageItems: 2,
        hasOlder: true,
        hasNewer: false,
        oldestItemKey: "k1",
        newestItemKey: "k2",
      };
      const origin = {
        kind: "trigger" as const,
        triggerId: "nightly",
        triggerKind: "schedule",
      };
      const current = session(["k1", "k2"], page);
      current.timelineItems.unshift({
        kind: "pendingUserTurn",
        itemKey: "pending-t",
        requestId: "t",
        content: "write the report",
        selectedSkillIds: [],
        lifecycleState: "pending",
        foldedIntoRequestId: null,
        origin,
        createdAt: null,
      });
      const incoming = session(["k1", "k2"], page);
      incoming.timelineItems.push({
        kind: "automatedInput",
        itemKey: "authored:doc-t:prompt",
        requestId: "doc-t",
        inputRequestId: "t",
        sequence: 3,
        origin,
        content: "write the report",
        timestamp: null,
        reconstruction: { state: "ready" },
      });
      const merged = mergeSessionTipSnapshot(current, incoming);
      expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
        "k1",
        "k2",
        "authored:doc-t:prompt",
      ]);
    },
  );

  it.each(["userMessage", "assistantMessage"] as const)(
    "updates %s reconstruction when blank content is unchanged",
    (kind) => {
      const page = {
        totalItems: 1,
        pageItems: 1,
        hasOlder: false,
        hasNewer: false,
        oldestItemKey: "k1",
        newestItemKey: "k1",
      };
      const current = session(["k1"], page);
      current.timelineItems = [
        {
          kind,
          itemKey: "k1",
          ownsTurn: true,
          sequence: 1,
          content: null,
          reasoning: null,
          timestamp: null,
          reconstruction: { state: "loading" },
        },
      ];
      for (const reconstruction of [
        { state: "denied" as const, deniedDependencyDocId: "output-1" },
        { state: "invalid" as const, error: "conflicting closure" },
        { state: "ready" as const },
      ]) {
        const next = {
          ...current,
          timelineItems: [{ ...current.timelineItems[0], reconstruction }],
        } as DesktopSessionSnapshot;
        const merged = mergeSessionTipSnapshot(current, next);
        expect(merged.timelineItems[0]).toEqual(next.timelineItems[0]);
        expect(merged.timelineItems[0]).not.toBe(current.timelineItems[0]);
      }
    },
  );

  it("updates the authoritative tip while retaining loaded older rows and identities", () => {
    const current = session(["k1", "k2", "k3", "k4"], {
      totalItems: 5,
      pageItems: 4,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k4",
    });
    const retained = current.timelineItems[1];
    const next = session(["k3", "k4", "k5"], {
      totalItems: 5,
      pageItems: 3,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k3",
      newestItemKey: "k5",
    });

    const merged = mergeSessionTipSnapshot(current, next);
    expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k2",
      "k3",
      "k4",
      "k5",
    ]);
    expect(merged.timelineItems[1]).toBe(retained);
  });

  it("never adds rows above the first one shown when the tip starts earlier", () => {
    const current = session(["k2", "k3", "live-assistant"], {
      totalItems: 4,
      pageItems: 3,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k2",
      newestItemKey: "live-assistant",
    });
    const tip = session(["k1", "k2", "k3"], {
      totalItems: 3,
      pageItems: 3,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k3",
    });

    const merged = mergeSessionTipSnapshot(current, tip);

    expect(merged.timelineItems.map((item) => item.itemKey)).toEqual(["k2", "k3"]);
    expect(merged.timelinePage?.hasOlder).toBe(true);
    expect(merged.timelinePage?.oldestItemKey).toBe("k2");
  });

  it("keeps an exhausted older-page boundary across a tip refresh", () => {
    const current = session(["k1", "k2", "k8", "k9"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 4,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k9",
    });
    const refreshedTip = session(["k8", "k9"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 2,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k9",
    });

    const merged = mergeSessionTipSnapshot(current, refreshedTip);

    expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k2",
      "k8",
      "k9",
    ]);
    expect(merged.timelinePage?.oldestItemKey).toBe("k1");
    expect(merged.timelinePage?.hasOlder).toBe(false);
  });
});

describe("transcript cursor ownership", () => {
  const page = {
    totalItems: -1,
    totalItemsExact: false,
    pageItems: 1,
    hasOlder: true,
    hasNewer: false,
    oldestItemKey: "tools-7",
    newestItemKey: "live-assistant",
  };
  const overlay = {
    kind: "liveAssistant" as const,
    itemKey: "live-assistant",
    content: "working",
    reasoning: null,
  };

  it("keeps the backend cursor when a new tip contains only an overlay", () => {
    const current = session(["k8"], { ...page, oldestItemKey: "k8" });
    const tip = session([], page);
    tip.timelineItems = [overlay];
    expect(mergeSessionTipSnapshot(current, tip).timelinePage?.oldestItemKey).toBe(
      "tools-7",
    );
  });

  it("never uses a pending input as the anchor when trimming an expanded tip", () => {
    const pending = {
      kind: "pendingUserTurn" as const,
      itemKey: "pending-r",
      requestId: "r",
      content: "queued",
      lifecycleState: "pending" as const,
      selectedSkillIds: [],
      foldedIntoRequestId: null,
      origin: null,
      createdAt: null,
    };
    const current = session(["k8"], { ...page, oldestItemKey: "k8" });
    current.timelineItems.unshift(pending);
    const tip = session(["k7", "k8"], { ...page, oldestItemKey: "k7" });
    tip.timelineItems.splice(1, 0, pending);
    const merged = mergeSessionTipSnapshot(current, tip);
    expect(merged.timelinePage?.oldestItemKey).toBe("k8");
    expect(merged.timelineItems[0]).toEqual(pending);
  });

});
