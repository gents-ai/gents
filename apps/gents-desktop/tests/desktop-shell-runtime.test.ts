import { describe, expect, it, vi } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import {
  applySessionLiveDelta,
  createTrailingRefreshQueue,
  desktopUpdateRefreshScope,
  dismissMailboxItemAndClearMatchingRoute,
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
  sessionLiveDeltaRequest,
} from "../src/hooks/desktopShellRuntime";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("createTrailingRefreshQueue", () => {
  it("collapses a same-turn burst and retains one update received during refresh", async () => {
    const passes = [deferred(), deferred()];
    let active = 0;
    let maxActive = 0;
    const refresh = vi.fn(async () => {
      const pass = passes[refresh.mock.calls.length - 1];
      active += 1;
      maxActive = Math.max(maxActive, active);
      await pass.promise;
      active -= 1;
    });
    const queue = createTrailingRefreshQueue(refresh);

    const first = queue.request();
    const second = queue.request();
    const third = queue.request();
    expect(refresh).toHaveBeenCalledTimes(0);
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    const duringRefresh = queue.request();

    passes[0].resolve();
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(2));
    expect(maxActive).toBe(1);

    passes[1].resolve();
    await Promise.all([first, second, third, duringRefresh]);
    expect(refresh).toHaveBeenCalledTimes(2);
    expect(maxActive).toBe(1);
  });

  it("drops a queued trailing refresh when disposed", async () => {
    const pass = deferred();
    const refresh = vi.fn(() => pass.promise);
    const queue = createTrailingRefreshQueue(refresh);

    const active = queue.request();
    void queue.request();
    queue.dispose();
    pass.resolve();
    await active;

    expect(refresh).toHaveBeenCalledTimes(1);
    await queue.request();
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});

describe("desktopUpdateRefreshScope", () => {
  it("keeps health, active-turn, and general store reads separate", () => {
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1")).toBe(
      "snapshot",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1")).toBe(
      "session",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1", true)).toBe(
      "sessionDelta",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", null)).toBe("full");
    expect(desktopUpdateRefreshScope("config", null, null)).toBe("full");
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
});
