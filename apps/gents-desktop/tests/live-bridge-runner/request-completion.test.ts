import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  waitForRequestCompletion,
  type RequestCompletionTarget,
} from "./request-completion";
import type { RequestDiagnosticsBundle } from "./types";

const target: RequestCompletionTarget = {
  agentDid: "did:key:agent",
  sessionId: "session-1",
  requestId: "follow-up",
};

function diagnostics(state: string, requestPresent = true): RequestDiagnosticsBundle {
  const row = {
    sessionId: target.sessionId,
    requestId: target.requestId,
    turnState: state,
    latestRequestId: requestPresent ? target.requestId : "previous-request",
    request: requestPresent ? { lifecycleState: state } : null,
    toolCalls: { total: 0, completed: 0, pending: 0 },
    toolResultCount: 0,
    messageCount: 0,
    timelineCount: 0,
    activeResponseOverlayContentLen: 0,
    activeResponseOverlayReasoningLen: 0,
  };
  return {
    desktop: { ...row, source: "desktop" },
    remote: { ...row, source: "remote" },
  };
}

function session(state: string, requestId = target.requestId): DesktopSessionSnapshot {
  return {
    sessionId: target.sessionId,
    agentDid: target.agentDid,
    behaviorId: "general",
    title: null,
    previewText: null,
    status: "active",
    goal: null,
    turnState: state,
    latestRequestId: requestId,
    retryEligibility: { eligible: false, denialReason: "notFailed" },
    latestRequestOutcome: state === "running" ? null : { failureReason: null },
    queuedTurns: [],
    foldedInputs: [],
    pendingTurn:
      state === "running"
        ? {
            requestId,
            content: "follow-up prompt",
            selectedSkillIds: [],
            lifecycleState: "claimed",
            foldedIntoRequestId: null,
            origin: null,
            createdAt: "2026-09-28T02:49:28Z",
          }
        : null,
    context: {
      estimatedDurableTokens: 0,
      estimatedConversationTokens: 0,
      contextWindow: 8192,
      compactionThreshold: 0.8,
      compactionThresholdTokens: 6553,
      compactionStrategy: "summary",
      durableMessageCount: 0,
      providerMessageCount: 0,
      totalCompactedMessages: 0,
      compactions: [],
      lastRequest: null,
    },
    timelineItems: [],
  };
}

function startWait(
  observations: RequestDiagnosticsBundle[],
  snapshots: DesktopSessionSnapshot[],
) {
  let observationIndex = 0;
  let snapshotIndex = 0;
  const fetchRequestDiagnostics = vi.fn(
    async () => observations[Math.min(observationIndex++, observations.length - 1)],
  );
  const fetchSessionSnapshot = vi.fn<DesktopApiAdapter["fetchSessionSnapshot"]>(
    async () => snapshots[Math.min(snapshotIndex++, snapshots.length - 1)],
  );
  const settled = vi.fn();
  const completion = waitForRequestCompletion({
    request: target,
    adapter: { fetchSessionSnapshot },
    fetchRequestDiagnostics,
    getExitStatus: () => null,
    stdoutTail: () => "",
    stderrTail: () => "",
    timeoutMs: 5_000,
  });
  void completion.then(settled);
  return { completion, settled, fetchRequestDiagnostics, fetchSessionSnapshot };
}

describe("request completion wait", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("keeps waiting when prior terminal diagnostics are followed by the new claimed turn", async () => {
    const completed = session("completed");
    const wait = startWait(
      [diagnostics("interrupted", false), diagnostics("completed")],
      [session("running"), completed],
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(wait.settled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1_000);
    await expect(wait.completion).resolves.toEqual(completed);
    expect(wait.fetchRequestDiagnostics).toHaveBeenCalledWith(
      target.sessionId,
      target.requestId,
    );
  });

  it("does not return a terminal snapshot belonging to another request", async () => {
    const completed = session("completed");
    const wait = startWait(
      [diagnostics("completed")],
      [session("completed", "previous-request"), completed],
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(wait.settled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(500);
    await expect(wait.completion).resolves.toEqual(completed);
    expect(wait.fetchSessionSnapshot).toHaveBeenCalledWith(
      target.sessionId,
      target.agentDid,
      target.requestId,
    );
  });

  it("keeps waiting when exact terminal diagnostics race with a running snapshot", async () => {
    const completed = session("completed");
    const wait = startWait([diagnostics("completed")], [session("running"), completed]);
    await vi.advanceTimersByTimeAsync(0);
    expect(wait.fetchSessionSnapshot).toHaveBeenCalledTimes(1);
    expect(wait.settled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(500);
    await expect(wait.completion).resolves.toEqual(completed);
  });

  it("returns the requested terminal turn even when another request is session-latest", async () => {
    const observation = diagnostics("completed");
    observation.desktop.latestRequestId = "later-request";
    observation.remote.latestRequestId = "later-request";
    const completed = session("completed");
    const wait = startWait([observation], [completed]);
    await vi.advanceTimersByTimeAsync(0);
    expect(wait.settled).toHaveBeenCalledWith(completed);
    await expect(wait.completion).resolves.toEqual(completed);
  });

  it("does not treat a missing requested row as completed even with a terminal snapshot", async () => {
    const completed = session("completed");
    const wait = startWait(
      [diagnostics("completed", false), diagnostics("completed")],
      [completed],
    );
    await vi.advanceTimersByTimeAsync(0);
    expect(wait.settled).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(500);
    await expect(wait.completion).resolves.toEqual(completed);
  });

  it.each(["failed", "interrupted"])(
    "returns the requested %s terminal turn",
    async (state) => {
      const terminal = session(state);
      const wait = startWait([diagnostics(state)], [terminal]);
      await vi.advanceTimersByTimeAsync(0);
      await expect(wait.completion).resolves.toEqual(terminal);
    },
  );
});
