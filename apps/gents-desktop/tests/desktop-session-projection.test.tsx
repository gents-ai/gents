import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  SessionLiveDeltaView,
} from "@source-inc/gents-desktop-client";
import { useDesktopSessionProjection } from "../src/hooks/useDesktopSessionProjection";
import { createSelectionStore } from "../src/hooks/selectionStore";
import { readSession } from "../src/hooks/sessionStore";

function session(
  keys: string[],
  page: NonNullable<DesktopSessionSnapshot["timelinePage"]>,
): DesktopSessionSnapshot {
  return {
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

function renderProjection(
  api: DesktopApiAdapter,
  store = createSelectionStore({ agentDid: "did:key:test", sessionId: "session-1" }),
) {
  return renderHook(() =>
    useDesktopSessionProjection({
      api,
      store,
      selectedTrackedRequestIdRef: { current: "request-1" },
      setError: vi.fn(),
    }),
  );
}

describe("useDesktopSessionProjection", () => {
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
    const { result } = renderProjection({
      fetchSessionSnapshot,
    } as unknown as DesktopApiAdapter);
    await act(async () => {
      await result.current.refreshSession("session-1");
    });
    let refresh!: Promise<DesktopSessionSnapshot | null>;
    act(() => {
      refresh = result.current.refreshSession("session-1");
    });
    expect(readSession(result.current.sessionStore)?.timelineItems).toEqual(
      saved.timelineItems,
    );
    expect(result.current.sessionLoad.phase).toBe("loading");
    await act(async () => {
      reject(new Error("database read timed out"));
      await refresh;
    });
    expect(readSession(result.current.sessionStore)?.timelineItems).toEqual(
      saved.timelineItems,
    );
    expect(result.current.sessionLoad.phase).toBe("failed");
    expect(result.current.sessionLoad.error).toContain("database read timed out");
  });

  it("retains a read failure during retry and clears it only when the read succeeds", async () => {
    let resolve!: (value: null) => void;
    const retry = new Promise<null>((done) => {
      resolve = done;
    });
    const { result } = renderProjection({
      fetchSessionSnapshot: vi
        .fn()
        .mockRejectedValueOnce(new Error("read timed out"))
        .mockReturnValueOnce(retry),
    } as unknown as DesktopApiAdapter);
    await act(async () => {
      await result.current.refreshSession("session-1");
    });
    let refresh!: Promise<DesktopSessionSnapshot | null>;
    act(() => {
      refresh = result.current.refreshSession("session-1");
    });
    expect(result.current.sessionLoad).toMatchObject({
      phase: "loading",
      error: "Error: read timed out",
    });
    await act(async () => {
      resolve(null);
      await refresh;
    });
    expect(result.current.sessionLoad).toMatchObject({ phase: "loaded", error: null });
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
    const { result } = renderProjection(
      { fetchSessionSnapshot } as unknown as DesktopApiAdapter,
      store,
    );
    let first!: Promise<DesktopSessionSnapshot | null>;
    act(() => {
      first = result.current.refreshSession("session-1");
    });
    store.setState({ sessionId: "session-2" });
    await act(async () => {
      await result.current.refreshSession("session-2");
    });
    store.setState({ sessionId: "session-1" });
    await act(async () => {
      await result.current.refreshSession("session-1");
    });
    expect(readSession(result.current.sessionStore)?.turnState).toBe("interrupted");
    await act(async () => {
      release(old);
      await first;
    });
    expect(readSession(result.current.sessionStore)?.turnState).toBe("interrupted");
    expect(readSession(result.current.sessionStore)?.timelineItems[0]?.itemKey).toBe(
      "k2",
    );
    expect(result.current.sessionLoad.phase).toBe("loaded");
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
    const { result } = renderProjection({
      fetchSessionSnapshot,
    } as unknown as DesktopApiAdapter);

    await act(async () => {
      await result.current.refreshSession("session-1");
    });
    let loaded = false;
    await act(async () => {
      loaded = await result.current.loadOlderSessionTimeline();
    });

    expect(loaded).toBe(true);
    expect(fetchSessionSnapshot).toHaveBeenCalledTimes(3);
    expect(fetchSessionSnapshot.mock.calls[1]?.[3]).toMatchObject({
      beforeItemKey: "k8",
    });
    expect(fetchSessionSnapshot.mock.calls[2]?.[3]).toMatchObject({
      beforeItemKey: "tools-7",
    });
    expect(
      readSession(result.current.sessionStore)?.timelineItems.map(
        (item) => item.itemKey,
      ),
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
    tip.projectionRevision = { storeVersion: 7, reconcileVersion: 3 };
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
    const { result } = renderProjection({
      fetchSessionSnapshot,
      fetchSessionLiveDelta: vi.fn(() => deltaResponse),
    } as unknown as DesktopApiAdapter);

    await act(async () => {
      await result.current.refreshSession("session-1");
    });
    let pendingDelta!: Promise<boolean>;
    act(() => {
      pendingDelta = result.current.refreshSessionLiveDelta();
    });
    await act(async () => {
      await result.current.loadOlderSessionTimeline();
    });
    resolveDelta({
      outcome: "delta",
      revision: { storeVersion: 8, reconcileVersion: 3 },
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
    await act(async () => {
      await pendingDelta;
    });

    expect(
      readSession(result.current.sessionStore)?.timelineItems.map(
        (item) => item.itemKey,
      ),
    ).toEqual(["k1", "k8", "live-assistant"]);
    expect(
      readSession(result.current.sessionStore)?.timelineItems.at(-1),
    ).toMatchObject({
      content: "hello world",
    });
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
    const { result } = renderProjection(
      { fetchSessionSnapshot } as unknown as DesktopApiAdapter,
      store,
    );

    let first: Promise<DesktopSessionSnapshot | null>;
    await act(async () => {
      first = result.current.refreshSession("session-1");
    });
    store.setState({ sessionId: "session-2" });
    await act(async () => {
      await result.current.refreshSession("session-2");
    });
    expect(readSession(result.current.sessionStore)?.sessionId).toBe("session-2");
    expect(readSession(result.current.sessionStore)?.timelineItems[0]?.itemKey).toBe(
      "other",
    );

    const stale = session(["stale"], {
      totalItems: 1,
      totalItemsExact: true,
      pageItems: 1,
      hasOlder: false,
      hasNewer: false,
      oldestItemKey: "stale",
      newestItemKey: "stale",
    });
    await act(async () => {
      release(stale);
      await first!;
    });
    expect(readSession(result.current.sessionStore)?.sessionId).toBe("session-2");
    expect(
      readSession(result.current.sessionStore)?.timelineItems.map(
        (item) => item.itemKey,
      ),
    ).toEqual(["other"]);
  });
});
