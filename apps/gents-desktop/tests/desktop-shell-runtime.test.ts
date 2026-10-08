import { describe, expect, it } from "vitest";

import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import {
  applySessionLiveDelta,
  desktopUpdateRefreshScope,
  sessionLiveDeltaRequest,
} from "../src/hooks/desktopShellRuntime";
import {
  mergeOlderSessionTimelinePage,
  mergeSessionTipSnapshot,
} from "../src/hooks/desktopTimelinePaging";

describe("desktopUpdateRefreshScope", () => {
  it("uses ordinary store wakes to probe the canonical live cursor", () => {
    expect(desktopUpdateRefreshScope("health", "session-1", "request-1")).toBe(
      "snapshot",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", "request-1")).toBe(
      "sessionDelta",
    );
    expect(desktopUpdateRefreshScope("store", "session-1", null)).toBe("full");
    expect(desktopUpdateRefreshScope("config", null, null)).toBe("full");
  });
});

describe("live session deltas", () => {
  it("applies a verified suffix while preserving historical row identity", () => {
    const current = session(["k1"], null);
    const historical = current.timelineItems[0];
    current.projectionRevision = { storeVersion: 7 };
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
    expect(next?.timelineItems[0]).toBe(historical);
    expect(next?.timelineItems.at(-1)).toMatchObject({ content: "hello world" });
  });

  it("keeps timeline and live item identity for an unchanged delta", () => {
    const current = session(["k1"], null);
    current.projectionRevision = { storeVersion: 7 };
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
      revision: { storeVersion: 8 },
      requestId: "request-1",
      turnState: "running",
      status: null,
      content: { ...unchanged, byteLen: 5, hash: "4f9f2cab" },
      reasoning: unchanged,
    });

    expect(next?.timelineItems).toBe(current.timelineItems);
    expect(next?.timelineItems.at(-1)).toBe(live);
    expect(next?.projectionRevision).toEqual({ storeVersion: 8 });
  });

  it("removes a reset live tail between tool-loop assistant turns", () => {
    const current = session(["k1"], null);
    current.projectionRevision = { storeVersion: 7 };
    current.timelineItems.push({
      kind: "liveAssistant",
      itemKey: "live-assistant",
      content: "stale opening prefix",
      reasoning: null,
    });

    const next = applySessionLiveDelta(current, {
      outcome: "delta",
      liveCursor: "cursor",
      revision: { storeVersion: 8 },
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
    current.projectionRevision = { storeVersion: 7 };
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

    expect(next?.timelineItems.map((item) => item.itemKey)).toEqual([
      "k1",
      "k8",
      "live-assistant",
    ]);
    expect(next?.timelineItems.at(-1)).toMatchObject({ content: "hello world" });
  });

  it("rejects a corrupt suffix", () => {
    const current = session([], null);
    current.projectionRevision = { storeVersion: 4 };
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
      revision: { storeVersion: 5 },
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
      ownsTurn: true,
      sequence: Number(key.slice(1)),
      content: key,
      timestamp: null,
      reconstruction: { state: "ready" },
    })),
    timelinePage: page,
  };
}

describe("session timeline page merging", () => {
  it("keeps distinct requests with identical content across older pages", () => {
    const page = {
      totalItems: 2,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k2",
      newestItemKey: "k2",
    };
    const current = session(["k2"], page);
    current.timelineItems = [
      {
        kind: "pendingUserTurn",
        itemKey: "pending-r2",
        requestId: "r2",
        content: "repeat",
        selectedSkillIds: [],
        lifecycleState: "pending",
        createdAt: null,
      },
    ];
    const older = session(["k1"], { ...page, hasOlder: false });
    older.timelineItems = [
      {
        kind: "userMessage",
        itemKey: "authored-r1",
        requestId: "r1",
        ownsTurn: true,
        sequence: 1,
        content: "repeat",
        timestamp: null,
        reconstruction: { state: "ready" },
      },
    ];
    expect(
      mergeOlderSessionTimelinePage(current, older).timelineItems.map(
        (item) => item.itemKey,
      ),
    ).toEqual(["authored-r1", "pending-r2"]);
  });

  it("removes the current pending row when its durable owner arrives in older history", () => {
    const page = {
      totalItems: 2,
      pageItems: 1,
      hasOlder: true,
      hasNewer: false,
      oldestItemKey: "k2",
      newestItemKey: "k2",
    };
    const current = session(["k2"], page);
    current.timelineItems.push({
      kind: "pendingUserTurn",
      itemKey: "pending-r",
      requestId: "r",
      content: "repeat",
      selectedSkillIds: [],
      lifecycleState: "processing",
      createdAt: null,
    });
    const older = session(["k1"], { ...page, hasOlder: false });
    older.timelineItems = [
      {
        kind: "userMessage",
        itemKey: "authored-r",
        requestId: "r",
        ownsTurn: true,
        sequence: 1,
        content: "repeat",
        timestamp: null,
        reconstruction: { state: "ready" },
      },
    ];
    expect(
      mergeOlderSessionTimelinePage(current, older).timelineItems.map(
        (item) => item.itemKey,
      ),
    ).toEqual(["authored-r", "k2"]);
  });

  /* A request authors rows besides the person's message: the workspace
     instructions it publishes beside the prompt arrive as their own row and
     must not retire the pending turn standing in for the message. */
  it.each(["older", "tip"] as const)(
    "keeps a queued input when only the request's context row is saved on the %s page",
    (direction) => {
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
      const merged =
        direction === "tip"
          ? mergeSessionTipSnapshot(current, incoming)
          : mergeOlderSessionTimelinePage(incoming, current);
      expect(merged.timelineItems.map((item) => item.itemKey)).toEqual([
        "pending-r",
        "k1",
        "k2",
        "authored-r-context",
      ]);
    },
  );

  it.each(["older", "tip"] as const)(
    "replaces a queued input with its durable owner when the %s page arrives",
    (direction) => {
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
        createdAt: null,
      });
      const incoming = session(["k1", "k2"], page);
      incoming.timelineItems.push({
        kind: "userMessage",
        itemKey: "authored-r",
        requestId: "r",
        ownsTurn: true,
        sequence: 3,
        content: "same text",
        timestamp: null,
        reconstruction: { state: "ready" },
      });
      const merged =
        direction === "tip"
          ? mergeSessionTipSnapshot(current, incoming)
          : mergeOlderSessionTimelinePage(incoming, current);
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
    expect(merged.status).toBe("active");
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

  it("never adds rows above the first one shown when the tip starts earlier", () => {
    /* the live tail left the end of the newest page, so the page reaches
       one row further back */
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
