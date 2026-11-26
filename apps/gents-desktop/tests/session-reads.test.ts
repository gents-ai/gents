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

  it("keeps the held session and its load when a re-read finds nothing new", async () => {
    const tip = () => {
      const read = session(["m1", "m2"], {
        totalItems: 2,
        pageItems: 2,
        hasOlder: false,
        hasNewer: false,
        oldestItemKey: "m1",
        newestItemKey: "m2",
      });
      read.projectionRevision = { storeVersion: ++version, provenanceVersion: 1 };
      return read;
    };
    let version = 0;
    let finish!: (value: DesktopSessionSnapshot) => void;
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(tip())
      .mockImplementationOnce(
        () =>
          new Promise<DesktopSessionSnapshot>((resolve) => {
            finish = resolve;
          }),
      )
      .mockImplementation(async () => ({ ...tip(), title: "Renamed" }));
    const reads = readsFor({ fetchSessionSnapshot } as unknown as DesktopApiAdapter);
    await reads.refreshSession("session-1");
    const shown = reads.sessionStore.getState();
    expect(shown.load).toMatchObject({ phase: "loaded", found: true, reread: false });
    const commits = vi.fn();
    const unsubscribe = reads.sessionStore.subscribe(commits);
    try {
      const refresh = reads.refreshSession("session-1");
      expect(reads.sessionStore.getState().load).toBe(shown.load);
      expect(
        projectSessionLoadingStatus({
          selectedSessionId: "session-1",
          selectedNodeDid: "did:key:test",
          session: shown.session,
          sessionLoad: reads.sessionStore.getState().load,
          operationalState: null,
        }),
      ).toBeNull();
      /* the store moved on elsewhere; this session did not */
      finish(tip());
      await refresh;
      expect(commits).not.toHaveBeenCalled();
      expect(reads.sessionStore.getState()).toBe(shown);

      await reads.refreshSession("session-1");
      expect(commits).toHaveBeenCalledTimes(1);
      expect(readSession(reads.sessionStore)?.title).toBe("Renamed");
      expect(reads.sessionStore.getState().load).toBe(shown.load);
    } finally {
      unsubscribe();
    }
  });

  it("drops a full read older than a live read that landed while it ran", async () => {
    const clock = vi.spyOn(performance, "now").mockReturnValue(0);
    try {
      const tip = (storeVersion: number) => {
        const read = session([], {
          totalItems: 0,
          pageItems: 0,
          hasOlder: false,
          hasNewer: false,
          oldestItemKey: null,
          newestItemKey: null,
        });
        read.projectionRevision = { storeVersion, provenanceVersion: 1 };
        read.timelineItems = [
          { kind: "liveAssistant", itemKey: "live", content: "hello", reasoning: null },
        ];
        return read;
      };
      let finish!: (value: DesktopSessionSnapshot) => void;
      const api = {
        fetchSessionSnapshot: vi
          .fn()
          .mockResolvedValueOnce(tip(7))
          .mockImplementationOnce(
            () =>
              new Promise<DesktopSessionSnapshot>((resolve) => {
                finish = resolve;
              }),
          ),
        fetchSessionLiveDelta: vi.fn(async (): Promise<SessionLiveDeltaView> => ({
          outcome: "delta",
          liveCursor: "cursor",
          requestId: "request-1",
          revision: { storeVersion: 9, provenanceVersion: 1 },
          turnState: "running",
          status: null,
          content: { mode: "append", value: " world", byteLen: 11, hash: "d58b3fa7" },
          reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
        })),
      } as unknown as DesktopApiAdapter;
      const reads = readsFor(api);
      await reads.refreshSession("session-1");
      /* the retry action reads beside the drain loop, which reads a delta */
      const full = reads.refreshSession("session-1");
      await reads.readSessionUpdate();
      expect(readSession(reads.sessionStore)?.timelineItems[0]).toMatchObject({
        content: "hello world",
      });
      finish(tip(8));
      await full;
      expect(readSession(reads.sessionStore)?.timelineItems[0]).toMatchObject({
        content: "hello world",
      });
      expect(readSession(reads.sessionStore)?.projectionRevision?.storeVersion).toBe(9);
      expect(reads.sessionStore.getState().load).toMatchObject({
        phase: "loaded",
        found: true,
      });
    } finally {
      clock.mockRestore();
    }
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

  it("reloads a shown session for a selection that names no node, and shows its answer", async () => {
    const page = {
      totalItems: 1,
      totalItemsExact: true,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "k1",
      newestItemKey: "k1",
    };
    const shown = session(["k1"], page);
    const fetchSessionSnapshot = vi.fn().mockResolvedValue(shown);
    const reload = vi.fn(async () => {});
    const store = createSelectionStore({ nodeDid: null, sessionId: "session-1" });
    const reads = readsFor(
      { fetchSessionSnapshot, reloadSession: reload } as unknown as DesktopApiAdapter,
      store,
    );
    await reads.refreshSession("session-1");
    expect(readSession(reads.sessionStore)?.nodeDid).toBe("did:key:test");
    /* the reload narrows the node to the shown session's; that is the same
       selection, not a mismatched answer */
    await reads.reloadSession("session-1");
    expect(reload).toHaveBeenCalledWith("session-1", "did:key:test");
    expect(fetchSessionSnapshot).toHaveBeenLastCalledWith(
      "session-1",
      "did:key:test",
      "request-1",
      expect.anything(),
    );
    expect(reads.sessionStore.getState().load).toMatchObject({
      phase: "loaded",
      nodeDid: "did:key:test",
      error: null,
    });
  });
});
