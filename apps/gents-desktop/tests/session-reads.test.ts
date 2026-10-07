import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  SessionLiveDeltaView,
} from "@source-inc/gents-desktop-client";
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
    agentDid: "did:key:test",
    behaviorId: "behavior-1",
    title: "Test",
    previewText: null,
    status: "active",
    goal: null,
    turnState: "running",
    latestRequestId: "request-1",
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestRequestOutcome: null,
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
      reconstruction: { state: "ready" as const },
    })),
    timelinePage: page,
  };
}

function readsFor(
  api: DesktopApiAdapter,
  store = createSelectionStore({ agentDid: "did:key:test", sessionId: "session-1" }),
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
  it("recovers a failed live read through a snapshot without a sticky global error", async () => {
    const tip = session([], {
      totalItems: 0,
      pageItems: 0,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: null,
      newestItemKey: null,
    });
    tip.projectionRevision = { storeVersion: 1 };
    const fetchSessionSnapshot = vi.fn(async () => tip);
    const fetchSessionLiveDelta = vi.fn(async () => {
      throw new Error("operator restarted");
    });
    const setError = vi.fn();
    const reads = readsFor(
      { fetchSessionSnapshot, fetchSessionLiveDelta } as unknown as DesktopApiAdapter,
      createSelectionStore({ agentDid: "did:key:test", sessionId: "session-1" }),
      setError,
    );
    await reads.refreshSession("session-1");
    expect(await reads.refreshSessionLiveDelta()).toBe(false);
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
      agentDid: "did:key:test",
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

  it("crosses a hidden-only durable page to the next visible rows", async () => {
    const tip = session(["k8", "k9"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 2,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k9",
    });
    const hidden = session([], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 0,
      hasOlder: true,
      hasNewer: true,
      oldestItemKey: "tools-7",
      newestItemKey: "tools-7",
    });
    const visible = session(["k1", "k2"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 2,
      hasOlder: false,
      hasNewer: true,
      oldestItemKey: "k1",
      newestItemKey: "k2",
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(tip)
      .mockResolvedValueOnce(hidden)
      .mockResolvedValueOnce(visible);
    const reads = readsFor({
      fetchSessionSnapshot,
    } as unknown as DesktopApiAdapter);

    await reads.refreshSession("session-1");
    const loaded = await reads.loadOlderSessionTimeline();

    expect(loaded).toBe(true);
    expect(fetchSessionSnapshot).toHaveBeenCalledTimes(3);
    expect(fetchSessionSnapshot.mock.calls[1]?.[3]).toMatchObject({
      beforeItemKey: "k8",
    });
    expect(fetchSessionSnapshot.mock.calls[2]?.[3]).toMatchObject({
      beforeItemKey: "tools-7",
    });
    expect(
      readSession(reads.sessionStore)?.timelineItems.map((item) => item.itemKey),
    ).toEqual(["k1", "k2", "k8", "k9"]);
  });

  it("applies a delayed live delta to the page loaded while it was in flight", async () => {
    const tip = session(["k8"], {
      totalItems: -1,
      totalItemsExact: false,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k8",
      newestItemKey: "k8",
    });
    tip.projectionRevision = { storeVersion: 7 };
    tip.timelineItems.push({
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
    let resolveDelta!: (delta: SessionLiveDeltaView) => void;
    const deltaResponse = new Promise<SessionLiveDeltaView>((resolve) => {
      resolveDelta = resolve;
    });
    const fetchSessionSnapshot = vi
      .fn()
      .mockResolvedValueOnce(tip)
      .mockResolvedValueOnce(older);
    const reads = readsFor({
      fetchSessionSnapshot,
      fetchSessionLiveDelta: vi.fn(() => deltaResponse),
    } as unknown as DesktopApiAdapter);

    await reads.refreshSession("session-1");
    const pendingDelta = reads.refreshSessionLiveDelta();
    await reads.loadOlderSessionTimeline();
    resolveDelta({
      outcome: "delta",
      liveCursor: "cursor",
      revision: { storeVersion: 8 },
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
    await pendingDelta;

    expect(
      readSession(reads.sessionStore)?.timelineItems.map((item) => item.itemKey),
    ).toEqual(["k1", "k8", "live-assistant"]);
    expect(readSession(reads.sessionStore)?.timelineItems.at(-1)).toMatchObject({
      content: "hello world",
    });
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
    tip.projectionRevision = { storeVersion: 7 };
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
    {
      await reads.refreshSession("session-1");
    }
    const stale: SessionLiveDeltaView = {
      outcome: "delta",
      liveCursor: "cursor",
      requestId: "request-1",
      revision: { storeVersion: 8 },
      turnState: "running",
      status: null,
      content: { mode: "replace", value: "old", byteLen: 3, hash: "bd2b9bd6" },
      reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
    };
    let pending!: Promise<boolean>;
    {
      pending = reads.refreshSessionLiveDelta();
    }
    {
      await reads.refreshSession("session-1");
    }
    {
      finish(stale);
      expect(await pending).toBe(true);
    }
    expect(readSession(reads.sessionStore)?.timelineItems[0]).toMatchObject({
      content: "hello",
    });
    {
      pending = reads.refreshSessionLiveDelta();
    }
    reads.invalidateSessionReads();
    finish(stale);
    expect(await pending).toBe(true);
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
      tip.projectionRevision = { storeVersion: 1 };
      tip.timelineItems = [
        { kind: "liveAssistant", itemKey: "live", content: "hello", reasoning: null },
      ];
      const fetchSessionLiveDelta = vi.fn(async () => ({
        outcome: "unchanged",
        liveCursor: "cursor",
        requestId: "request-1",
        revision: { storeVersion: 2 },
        turnState: "running",
        status: null,
        content: { mode: "unchanged", value: "", byteLen: 5, hash: "4f9f2cab" },
        reasoning: { mode: "unchanged", value: "", byteLen: 0, hash: "811c9dc5" },
      }));
      const reads = readsFor({
        fetchSessionSnapshot: async () => tip,
        fetchSessionLiveDelta,
      } as unknown as DesktopApiAdapter);
      {
        await reads.refreshSession("session-1");
    }
      clock.mockReturnValue(250);
      {
        expect(await reads.refreshSessionLiveDelta()).toBe(true);
    }
      clock.mockReturnValue(1_500);
      {
        expect(await reads.refreshSessionLiveDelta()).toBe(false);
    }
      expect(fetchSessionLiveDelta).toHaveBeenCalledTimes(1);
      {
        await reads.refreshSession("session-1");
    }
      {
        expect(await reads.refreshSessionLiveDelta()).toBe(true);
    }
    } finally {
      clock.mockRestore();
    }
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
      agentDid: "did:key:test",
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
