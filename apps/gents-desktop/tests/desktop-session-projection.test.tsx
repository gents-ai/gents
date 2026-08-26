import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  SessionLiveDeltaView,
} from "@source-inc/gents-desktop-client";
import { useDesktopSessionProjection } from "../src/hooks/useDesktopSessionProjection";

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

function renderProjection(api: DesktopApiAdapter) {
  return renderHook(() =>
    useDesktopSessionProjection({
      api,
      selectedAgentDidRef: { current: "did:key:test" },
      selectedSessionIdRef: { current: "session-1" },
      selectedTrackedRequestIdRef: { current: "request-1" },
      setError: vi.fn(),
    }),
  );
}

describe("useDesktopSessionProjection", () => {
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
    expect(result.current.session?.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k2",
      "k8",
      "k9",
    ]);
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
    tip.latestResponse = {
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
    tip.activeResponseOverlay = { ...tip.latestResponse };
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
    await act(async () => {
      await pendingDelta;
    });

    expect(result.current.session?.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k8",
      "live-assistant",
    ]);
    expect(result.current.session?.timelineItems.at(-1)).toMatchObject({
      content: "hello world",
    });
  });
});
