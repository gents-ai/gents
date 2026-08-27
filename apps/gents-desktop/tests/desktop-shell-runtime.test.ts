import { describe, expect, it, vi } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import {
  applySessionLiveDelta,
  desktopUpdateRefreshScope,
  dismissMailboxItemAndClearMatchingRoute,
  sessionLiveDeltaRequest,
} from "../src/hooks/desktopShellRuntime";
import {
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
} from "../src/hooks/desktopTimelinePaging";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("desktopUpdateRefreshScope", () => {
  it("keeps health, active-turn, and general store reads separate", () => {
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1")).toBe(
      "snapshot",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1")).toBe(
      "sessionEvent",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1", true)).toBe(
      "sessionDelta",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", null)).toBe("full");
    expect(desktopUpdateRefreshScope("config", null, null)).toBe("full");
    expect(desktopUpdateRefreshScope("hydration", "session-1", "request-1")).toBe(
      "full",
    );
    expect(desktopUpdateRefreshScope("hydration", null, null)).toBe("snapshot");
  });
});

describe("dismissMailboxItemAndClearMatchingRoute", () => {
  it("preserves a newer compose route while an older dismissal is in flight", async () => {
    const pass = deferred();
    let currentRouteItemId: string | null = "item-a";
    const clearMatchingRoute = vi.fn();
    const dismissal = dismissMailboxItemAndClearMatchingRoute(
      "item-a",
      () => pass.promise,
      () => currentRouteItemId,
      clearMatchingRoute,
    );

    currentRouteItemId = "item-b";
    pass.resolve();
    await dismissal;

    expect(clearMatchingRoute).not.toHaveBeenCalled();
  });

  it("clears the compose route when the dismissed item is still current", async () => {
    const clearMatchingRoute = vi.fn();
    await dismissMailboxItemAndClearMatchingRoute(
      "item-a",
      async () => {},
      () => "item-a",
      clearMatchingRoute,
    );

    expect(clearMatchingRoute).toHaveBeenCalledOnce();
  });
});

describe("live session deltas", () => {
  it("applies a verified suffix while preserving historical row identity", () => {
    const current = session(["k1"], null);
    const historical = current.timelineItems[0];
    current.projectionRevision = { storeVersion: 7, reconcileVersion: 3 };
    current.latestResponse = {
      status: "streaming",
      content: "hello",
      reasoning: null,
      errorMessage: null,
      tokenCount: null,
      materializedMessageSequence: null,
      materializedAt: null,
      interruptedAt: null,
      completedAt: null,
    };
    current.activeResponseOverlay = { ...current.latestResponse };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "hello",
      reasoning: null,
    });
    const request = sessionLiveDeltaRequest(current, "request-1");
    expect(request).toMatchObject({
      baseReconcileVersion: 3,
      baseContentByteLen: 5,
      baseContentHash: "4f9f2cab",
    });

    const next = applySessionLiveDelta(current, {
      outcome: "delta",
      revision: { storeVersion: 8, reconcileVersion: 3 },
      requestId: "request-1",
      progressSeq: 2,
      turnState: "streaming",
      status: "streaming",
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
    expect(next?.activeResponseOverlay?.content).toBe("hello world");
    expect(next?.timelineItems[0]).toBe(historical);
    expect(next?.timelineItems.at(-1)).toMatchObject({ content: "hello world" });
  });

  it("removes a reset live tail between tool-loop assistant turns", () => {
    const current = session(["k1"], null);
    current.projectionRevision = { storeVersion: 7, reconcileVersion: 3 };
    current.latestResponse = {
      status: "streaming",
      content: "stale opening prefix",
      reasoning: null,
      errorMessage: null,
      tokenCount: null,
      materializedMessageSequence: null,
      materializedAt: null,
      interruptedAt: null,
      completedAt: null,
    };
    current.activeResponseOverlay = { ...current.latestResponse };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "stale opening prefix",
      reasoning: null,
    });

    const next = applySessionLiveDelta(current, {
      outcome: "delta",
      revision: { storeVersion: 8, reconcileVersion: 3 },
      requestId: "request-1",
      progressSeq: 3,
      turnState: "streaming",
      status: "streaming",
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

    expect(next?.activeResponseOverlay).toBeNull();
    expect(next?.timelineItems).toHaveLength(1);
    expect(next?.timelineItems[0].itemKey).toBe("k1");
  });

  it("keeps a loaded older page when a live delta arrives", () => {
    const current = session(["k8"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k8",
    });
    current.projectionRevision = { storeVersion: 7, reconcileVersion: 3 };
    current.latestResponse = {
      status: "streaming",
      content: "hello",
      reasoning: null,
      errorMessage: null,
      tokenCount: null,
      materializedMessageSequence: null,
      materializedAt: null,
      interruptedAt: null,
      completedAt: null,
    };
    current.activeResponseOverlay = { ...current.latestResponse };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "hello",
      reasoning: null,
    });
    const older = session(["k1"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 1,
      hasOlder: false,
      hasNewer: true,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    const withOlder = mergeOlderSessionTimelinePage(current, older);

    const next = applySessionLiveDelta(withOlder, {
      outcome: "delta",
      revision: { storeVersion: 8, reconcileVersion: 3 },
      requestId: "request-1",
      progressSeq: 2,
      turnState: "streaming",
      status: "streaming",
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

    expect(next?.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k8",
      "live-assistant",
    ]);
    expect(next?.timelineItems.at(-1)).toMatchObject({ content: "hello world" });
  });

  it("rejects a reconcile gap and a corrupt suffix", () => {
    const current = session([], null);
    current.projectionRevision = { storeVersion: 4, reconcileVersion: 2 };
    current.latestResponse = {
      status: "streaming",
      content: "a",
      reasoning: null,
      errorMessage: null,
      tokenCount: null,
      materializedMessageSequence: null,
      materializedAt: null,
      interruptedAt: null,
      completedAt: null,
    };
    current.activeResponseOverlay = { ...current.latestResponse };
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
      revision: { storeVersion: 5, reconcileVersion: 2 },
      requestId: "request-1",
      progressSeq: 2,
      turnState: "streaming",
      status: "streaming",
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
    expect(
      applySessionLiveDelta(current, {
        ...base,
        revision: { storeVersion: 5, reconcileVersion: 3 },
      }),
    ).toBeNull();
  });
});

function session(
  keys: string[],
  page: DesktopSessionSnapshot["timelinePage"],
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:key:test",
    behaviorId: "behavior-1",
    title: "Test",
    previewText: null,
    status: "processing",
    goal: null,
    turnState: "streaming",
    latestRequestId: "request-1",
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestResponse: null,
    activeResponseOverlay: null,
    pendingTurn: null,
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
      compactions: [],
      lastRequest: null,
    },
    timelineItems: keys.map((key) => ({
      kind: "userMessage" as const,
      itemKey: key,
      requestId: key,
      sequence: Number(key.slice(1)),
      content: key,
      timestamp: null,
    })),
    timelinePage: page,
  };
}

describe("session timeline page merging", () => {
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

  it("prepends an older page without regressing live session metadata", () => {
    const current = session(["k3", "k4", "k5"], {
      totalItems: 5,
      pageItems: 3,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k3",
      newestItemKey: "k5",
    });
    const older = session(["k1", "k2"], {
      totalItems: 5,
      pageItems: 2,
      hasOlder: false,
      hasNewer: true,
      oldestItemKey: "k1",
      newestItemKey: "k2",
    });
    older.status = "stale-page-metadata";

    const merged = mergeOlderSessionTimelinePage(current, older);
    expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k2",
      "k3",
      "k4",
      "k5",
    ]);
    expect(merged.status).toBe("processing");
    expect(merged.timelinePage?.hasOlder).toBe(false);
  });

  it("advances through an older page containing only hidden durable rows", () => {
    const current = session(["k8", "k9"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 2,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k9",
    });
    const older = session([], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 0,
      hasOlder: true,
      hasNewer: true,
      oldestItemKey: "tools-7",
      newestItemKey: "tools-7",
    });

    const merged = mergeOlderSessionTimelinePage(current, older);

    expect(merged.timelineItems.map((item) => item.itemKey)).toEqual(["k8", "k9"]);
    expect(merged.timelinePage?.oldestItemKey).toBe("tools-7");
    expect(merged.timelinePage?.hasOlder).toBe(true);

    const refreshedTip = session(["k8", "k9"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 2,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k9",
    });
    const afterTipRefresh = mergeSessionTipSnapshot(merged, refreshedTip);
    expect(afterTipRefresh.timelinePage?.oldestItemKey).toBe("tools-7");
    expect(afterTipRefresh.timelinePage?.hasOlder).toBe(true);
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
