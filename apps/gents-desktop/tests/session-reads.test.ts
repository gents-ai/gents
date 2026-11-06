import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  SessionLiveDeltaView,
  SessionRowsView,
} from "@source-inc/gents-desktop-client";
import { projectSessionLoadingStatus } from "../src/lib/loadingStatus";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import { setTimingForTests } from "../src/hooks/timing";
import { createSessionReads } from "../src/hooks/sessionReads";
import { createSelectionStore } from "../src/hooks/selectionStore";
import { createSessionStore, readSession } from "../src/hooks/sessionStore";

function session(
  keys: string[],
  page: NonNullable<DesktopSessionSnapshot["timelinePage"]>,
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
      sequence: Number(key.slice(1)),
      content: key,
      timestamp: null,
      reconstruction: { state: "ready" as const },
    })),
    timelinePage: page,
  };
}

function rowsRead(sessionId: string): SessionRowsView {
  return {
    sessionId,
    nodeDid: "did:key:test",
    rows: [],
    coveredFrom: 0,
    coveredThrough: null,
    hasOlder: false,
    hasNewer: false,
    complete: true,
    unreadable: null,
    queryCount: 1,
  };
}

function readsFor(
  api: DesktopApiAdapter,
  store = createSelectionStore({ nodeDid: "did:key:test", sessionId: "session-1" }),
  setError = vi.fn(),
) {
  const sessionStore = createSessionStore();
  const reads = createSessionReads({
    api,
    store,
    sessionStore,
    trackedRequestId: () => "request-1",
    setError,
  });
  return { ...reads, sessionStore };
}

describe("createSessionReads", () => {
  it("clears an already displayed session when a snapshot read is denied", async () => {
    const tip = session(["k1"], {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    const denied = new BridgeInvokeError({
      code: "permissionDenied",
      message: "Access denied",
      retryable: false,
      endpoint: null,
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(tip)
      .mockRejectedValueOnce(denied);
    const reads = readsFor({ fetchSessionSnapshot } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)).not.toBeNull();
    await reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)).toBeNull();
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "failed",
      error: "BridgeInvokeError: Access denied",
    });
  });

  it("clears the selected session on an explicit history permission denial", async () => {
    const tip = session(["k1"], {
      totalItems: 1,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    const denied = new BridgeInvokeError({
      code: "permissionDenied",
      message: "Access denied",
      retryable: false,
      endpoint: null,
    });
    const reads = readsFor({
      fetchSessionSnapshot: vi.fn().mockResolvedValue(tip),
      fetchSessionRows: vi.fn().mockRejectedValue(denied),
    } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    await expect(reads.loadSessionRows({ kind: "before", sequence: 1 })).rejects.toBe(
      denied,
    );
    expect(readSession(reads.sessionStore)).toBeNull();
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "failed",
      sessionId: "session-1",
      error: "Access denied",
    });
  });

  it("keeps the transcript while a selected request is pending and recovers on arrival", async () => {
    const tip = session(["k1"], {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    let arrive!: (snapshot: DesktopSessionSnapshot) => void;
    const arrival = new Promise<DesktopSessionSnapshot>((resolve) => {
      arrive = resolve;
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(tip)
      .mockRejectedValueOnce(
        new BridgeInvokeError({
          code: "projectionPending",
          message: "selected request has not reached the session owner yet",
          retryable: true,
          endpoint: null,
        }),
      )
      .mockReturnValueOnce(arrival);
    const reads = readsFor({ fetchSessionSnapshot } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    await reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)?.timelineItems).toEqual(tip.timelineItems);
    expect(reads.sessionStore.getState().load.phase).toBe("pending");
    expect(
      projectSessionLoadingStatus({
        selectedSessionId: "session-1",
        selectedNodeDid: "did:key:test",
        session: tip,
        sessionLoad: reads.sessionStore.getState().load,
        operationalState: null,
      }),
    ).toBeNull();
    const refresh = reads.refreshSession("session-1");
    expect(reads.sessionStore.getState().load.phase).toBe("pending");
    arrive(tip);
    await refresh;
    expect(reads.sessionStore.getState().load.phase).toBe("loaded");
    expect(reads.sessionStore.getState().load.error).toBeNull();
  });

  it("shows a session its node has not returned as how far the message got, never as a failed read", async () => {
    const delivery = (
      code: "sessionDelivering" | "sessionNotConnected" | "sessionAccepted",
    ) =>
      new BridgeInvokeError({
        code,
        message: `${code} on Studio`,
        retryable: true,
        endpoint: "did:key:test",
      });
    const tip = session(["k1"], {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockRejectedValueOnce(delivery("sessionNotConnected"))
      .mockRejectedValueOnce(delivery("sessionDelivering"))
      .mockRejectedValueOnce(delivery("sessionAccepted"))
      .mockResolvedValueOnce(tip);
    const reads = readsFor({ fetchSessionSnapshot } as unknown as DesktopApiAdapter);
    const status = () =>
      projectSessionLoadingStatus({
        selectedSessionId: "session-1",
        selectedNodeDid: "did:key:test",
        session: readSession(reads.sessionStore),
        sessionLoad: reads.sessionStore.getState().load,
        operationalState: null,
      });

    await reads.refreshSession("session-1");
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "notConnected",
      error: "sessionNotConnected on Studio",
    });
    expect(status()).toMatchObject({
      phase: "blocked",
      title: "Not connected to the node",
      detail: "sessionNotConnected on Studio",
      action: null,
    });

    const refresh = reads.refreshSession("session-1");
    expect(reads.sessionStore.getState().load.phase).toBe("notConnected");
    await refresh;
    expect(status()).toMatchObject({ phase: "loading", title: "Sending message" });

    await reads.refreshSession("session-1");
    expect(status()).toMatchObject({ phase: "loading", title: "Message picked up" });

    await reads.refreshSession("session-1");
    expect(reads.sessionStore.getState().load.phase).toBe("loaded");
    expect(status()).toBeNull();
  });

  it("owes an update it cannot read yet a full read instead of dropping it", async () => {
    const clock = vi.spyOn(performance, "now").mockReturnValue(0);
    try {
      let readMs = 400;
      const fetchSessionSnapshot = vi.fn(async () => {
        clock.mockReturnValue(clock.mock.results.length + readMs);
        return null;
      });
      const reads = readsFor({ fetchSessionSnapshot } as unknown as DesktopApiAdapter);
      clock.mockReturnValue(0);
      await reads.refreshSession("session-1");
      readMs = 0;
      const readAt = performance.now();
      clock.mockReturnValue(readAt + 100);
      const owed = await reads.readSessionUpdate();
      expect(owed).toBeGreaterThan(0);
      expect(owed).toBeLessThanOrEqual(400);
      clock.mockReturnValue(readAt + 100 + (owed ?? 0));
      expect(await reads.readSessionUpdate()).toBe(0);
    } finally {
      clock.mockRestore();
    }
  });

  it("keeps a hydration failure across routine tip refreshes", async () => {
    const tip = session(["k1"], {
      totalItems: 2,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    let failHydration!: (error: Error) => void;
    const setError = vi.fn();
    const reads = readsFor(
      {
        fetchSessionSnapshot: vi.fn().mockResolvedValue(tip),
        reloadSession: () =>
          new Promise<void>((_, reject) => {
            failHydration = reject;
          }),
      } as unknown as DesktopApiAdapter,
      undefined,
      setError,
    );
    await reads.refreshSession("session-1");
    const hydration = reads.reloadSession("session-1");
    await reads.refreshSession("session-1");
    failHydration(new Error("hydration failed"));
    expect(await hydration).toBeNull();
    expect(setError).toHaveBeenLastCalledWith("Error: hydration failed");
  });

  it("revokes pending rows and hydration reads when observation stops", async () => {
    const tip = session(["k1"], {
      totalItems: 2,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    });
    let finishRows!: (value: SessionRowsView) => void;
    let finishHydration!: () => void;
    const fetchSessionSnapshot = vi.fn().mockResolvedValueOnce(tip);
    const reads = readsFor({
      fetchSessionSnapshot,
      fetchSessionRows: () =>
        new Promise<SessionRowsView>((resolve) => {
          finishRows = resolve;
        }),
      reloadSession: () =>
        new Promise<void>((resolve) => {
          finishHydration = resolve;
        }),
    } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    const rows = reads.loadSessionRows({ kind: "before", sequence: 1 });
    reads.invalidateSessionReads();
    finishRows(rowsRead("session-1"));
    expect(await rows).toBeNull();
    const hydration = reads.reloadSession("session-1");
    reads.invalidateSessionReads();
    finishHydration();
    expect(await hydration).toBeNull();
    expect(fetchSessionSnapshot).toHaveBeenCalledTimes(1);
  });

  it("recovers a failed live read through a snapshot without a sticky global error", async () => {
    const tip = session([], {
      totalItems: 0,
      pageItems: 0,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: null,
      newestItemKey: null,
    });
    tip.projectionRevision = { storeVersion: 1, provenanceVersion: 1 };
    const fetchSessionSnapshot = vi.fn(async () => tip);
    const fetchSessionLiveDelta = vi.fn(async () => {
      throw new Error("operator restarted");
    });
    const setError = vi.fn();
    const reads = readsFor(
      { fetchSessionSnapshot, fetchSessionLiveDelta } as unknown as DesktopApiAdapter,
      createSelectionStore({ nodeDid: "did:key:test", sessionId: "session-1" }),
      setError,
    );
    await reads.refreshSession("session-1");
    expect(await reads.readSessionUpdate()).toBe(0);
    await reads.refreshSession("session-1");
    expect(fetchSessionLiveDelta).toHaveBeenCalledTimes(1);
    expect(fetchSessionSnapshot).toHaveBeenCalledTimes(2);
    expect(reads.sessionStore.getState().load.phase).toBe("loaded");
    expect(setError).not.toHaveBeenCalled();
  });

  it("keeps rendered replies while a database refresh stalls and then fails", async () => {
    const page = {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    };
    const saved = session(["k1"], page);
    saved.timelineItems = [
      {
        kind: "assistantMessage",
        itemKey: "k1",
        sequence: 1,
        content: "Saved agent reply",
        reasoning: null,
        timestamp: null,
        reconstruction: { state: "ready" },
      },
    ];
    let reject!: (reason: Error) => void;
    const stalled = new Promise<DesktopSessionSnapshot>((_, fail) => {
      reject = fail;
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(saved)
      .mockReturnValueOnce(stalled);
    const reads = readsFor({
      fetchSessionSnapshot,
    } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    const refresh = reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)?.timelineItems).toEqual(saved.timelineItems);
    expect(reads.sessionStore.getState().load.phase).toBe("loading");
    reject(new Error("database read timed out"));
    await refresh;
    expect(readSession(reads.sessionStore)?.timelineItems).toEqual(saved.timelineItems);
    expect(reads.sessionStore.getState().load.phase).toBe("failed");
    expect(reads.sessionStore.getState().load.error).toContain(
      "database read timed out",
    );
  });

  it("retains a read failure during retry and clears it only when the read succeeds", async () => {
    let resolve!: (value: null) => void;
    const retry = new Promise<null>((done) => {
      resolve = done;
    });
    const reads = readsFor({
      fetchSessionSnapshot: vi
        .fn()
        .mockRejectedValueOnce(new Error("read timed out"))
        .mockReturnValueOnce(retry),
    } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    const refresh = reads.refreshSession("session-1");
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "loading",
      error: "Error: read timed out",
    });
    resolve(null);
    await refresh;
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "loaded",
      error: null,
    });
  });

  it("rejects an old snapshot after navigating away and back to the same session", async () => {
    const page = {
      totalItems: 1,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    };
    const old = session(["k1"], page);
    const terminal = { ...session(["k2"], page), turnState: "interrupted" };
    const other = { ...session(["k3"], page), sessionId: "session-2" };
    let release!: (snapshot: DesktopSessionSnapshot) => void;
    const pending = new Promise<DesktopSessionSnapshot>((resolve) => {
      release = resolve;
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockReturnValueOnce(pending)
      .mockResolvedValueOnce(other)
      .mockResolvedValueOnce(terminal);
    const store = createSelectionStore({
      nodeDid: "did:key:test",
      sessionId: "session-1",
    });
    const reads = readsFor(
      { fetchSessionSnapshot } as unknown as DesktopApiAdapter,
      store,
    );
    const first = reads.refreshSession("session-1");
    store.setState({ sessionId: "session-2" });
    await reads.refreshSession("session-2");
    store.setState({ sessionId: "session-1" });
    await reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)?.turnState).toBe("interrupted");
    release(old);
    await first;
    expect(readSession(reads.sessionStore)?.turnState).toBe("interrupted");
    expect(readSession(reads.sessionStore)?.timelineItems[0]?.itemKey).toBe("k2");
    expect(reads.sessionStore.getState().load.phase).toBe("loaded");
  });

  it("rejects old live reads after a full refresh and after observation stops", async () => {
    const tip = session([], {
      totalItems: 0,
      pageItems: 0,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: null,
      newestItemKey: null,
    });
    tip.projectionRevision = { storeVersion: 7, provenanceVersion: 1 };
    tip.timelineItems = [
      { kind: "liveAssistant", itemKey: "live", content: "hello", reasoning: null },
    ];
    let finish!: (value: SessionLiveDeltaView) => void;
    const api = {
      fetchSessionSnapshot: vi.fn(async () => tip),
      fetchSessionLiveDelta: vi.fn(
        () =>
          new Promise<SessionLiveDeltaView>((resolve) => {
            finish = resolve;
          }),
      ),
    } as unknown as DesktopApiAdapter;
    const reads = readsFor(api);
    await reads.refreshSession("session-1");
    const stale: SessionLiveDeltaView = {
      outcome: "delta",
      liveCursor: "cursor",
      requestId: "request-1",
      revision: { storeVersion: 8, provenanceVersion: 1 },
      turnState: "running",
      status: null,
      content: { mode: "replace", value: "old", byteLen: 3, hash: "bd2b9bd6" },
      reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
    };
    let pending!: Promise<number | null>;
    pending = reads.readSessionUpdate();
    await reads.refreshSession("session-1");
    finish(stale);
    expect(await pending).toBeNull();
    expect(readSession(reads.sessionStore)?.timelineItems[0]).toMatchObject({
      content: "hello",
    });
    pending = reads.readSessionUpdate();
    reads.invalidateSessionReads();
    finish(stale);
    expect(await pending).toBeNull();
  });

  it("requires history reconciliation even when every live read succeeds", async () => {
    const clock = vi.spyOn(performance, "now").mockReturnValue(0);
    try {
      const tip = session([], {
        totalItems: 0,
        pageItems: 0,
        hasOlder: false,
        hasNewer: false,
        oldestItemKey: null,
        newestItemKey: null,
      });
      tip.projectionRevision = { storeVersion: 1, provenanceVersion: 1 };
      tip.timelineItems = [
        { kind: "liveAssistant", itemKey: "live", content: "hello", reasoning: null },
      ];
      const fetchSessionLiveDelta = vi.fn(async () => ({
        outcome: "unchanged",
        liveCursor: "cursor",
        requestId: "request-1",
        revision: { storeVersion: 2, provenanceVersion: 1 },
        turnState: "running",
        status: null,
        content: { mode: "unchanged", value: "", byteLen: 5, hash: "4f9f2cab" },
        reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
      }));
      const reads = readsFor({
        fetchSessionSnapshot: async () => tip,
        fetchSessionLiveDelta,
      } as unknown as DesktopApiAdapter);
      await reads.refreshSession("session-1");
      clock.mockReturnValue(250);
      expect(
        await reads.readSessionUpdate(),
        "a live read covers the live text, not the update; the full read is owed",
      ).toBe(1_250);
      clock.mockReturnValue(1_500);
      expect(await reads.readSessionUpdate()).toBe(0);
      expect(fetchSessionLiveDelta).toHaveBeenCalledTimes(1);
      await reads.refreshSession("session-1");
      expect(await reads.readSessionUpdate()).toBe(1_500);
    } finally {
      clock.mockRestore();
    }
  });

  it("keeps applying live reads when periodic reconciliation is disabled", async () => {
    const clock = vi.spyOn(performance, "now").mockReturnValue(0);
    setTimingForTests({ sessionReadGapMs: null });
    try {
      const tip = session([], {
        totalItems: 0,
        pageItems: 0,
        hasOlder: false,
        hasNewer: false,
        oldestItemKey: null,
        newestItemKey: null,
      });
      tip.projectionRevision = { storeVersion: 1, provenanceVersion: 1 };
      tip.timelineItems = [
        { kind: "liveAssistant", itemKey: "live", content: "hello", reasoning: null },
      ];
      const fetchSessionLiveDelta = vi.fn(async () => ({
        outcome: "unchanged",
        liveCursor: "cursor",
        requestId: "request-1",
        revision: { storeVersion: 2, provenanceVersion: 1 },
        turnState: "running",
        status: null,
        content: { mode: "unchanged", value: "", byteLen: 5, hash: "4f9f2cab" },
        reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
      }));
      const reads = readsFor({
        fetchSessionSnapshot: async () => tip,
        fetchSessionLiveDelta,
      } as unknown as DesktopApiAdapter);
      await reads.refreshSession("session-1");
      clock.mockReturnValue(60_000);
      expect(await reads.readSessionUpdate()).toBeNull();
      expect(fetchSessionLiveDelta).toHaveBeenCalledTimes(1);
    } finally {
      setTimingForTests(null);
      clock.mockRestore();
    }
  });

  it("drops a rows read that lands after the reader opened another session", async () => {
    let finishRows!: (value: SessionRowsView) => void;
    const store = createSelectionStore({
      nodeDid: "did:key:test",
      sessionId: "session-1",
    });
    const reads = readsFor(
      {
        fetchSessionSnapshot: vi.fn().mockResolvedValue(
          session(["k1"], {
            totalItems: 1,
            pageItems: 1,
            hasOlder: false,
            hasNewer: false,
            oldestItemKey: "k1",
            newestItemKey: "k1",
          }),
        ),
        fetchSessionRows: () =>
          new Promise<SessionRowsView>((resolve) => {
            finishRows = resolve;
          }),
      } as unknown as DesktopApiAdapter,
      store,
    );
    await reads.refreshSession("session-1");
    const rows = reads.loadSessionRows({ kind: "around", sequence: 40 });
    store.setState({ sessionId: "session-2" });
    finishRows(rowsRead("session-1"));
    expect(await rows).toBeNull();
  });

  it("drops a delayed snapshot after the selected session changes", async () => {
    let release!: (value: DesktopSessionSnapshot) => void;
    const delayed = new Promise<DesktopSessionSnapshot>((resolve) => {
      release = resolve;
    });
    const other = session(["other"], {
      totalItems: 1,
      totalItemsExact: true,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "other",
      newestItemKey: "other",
    });
    other.sessionId = "session-2";
    const fetchSessionSnapshot = vi.fn().mockImplementation((sessionId: string) => {
      if (sessionId === "session-1") return delayed;
      return Promise.resolve(other);
    });
    const store = createSelectionStore({
      nodeDid: "did:key:test",
      sessionId: "session-1",
    });
    const reads = readsFor(
      { fetchSessionSnapshot } as unknown as DesktopApiAdapter,
      store,
    );

    const first = reads.refreshSession("session-1");
    store.setState({ sessionId: "session-2" });
    await reads.refreshSession("session-2");
    expect(readSession(reads.sessionStore)?.sessionId).toBe("session-2");
    expect(readSession(reads.sessionStore)?.timelineItems[0]?.itemKey).toBe("other");

    const stale = session(["stale"], {
      totalItems: 1,
      totalItemsExact: true,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "stale",
      newestItemKey: "stale",
    });
    release(stale);
    await first!;
    expect(readSession(reads.sessionStore)?.sessionId).toBe("session-2");
    expect(
      readSession(reads.sessionStore)?.timelineItems.map((item) => item.itemKey),
    ).toEqual(["other"]);
  });
});
