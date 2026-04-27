import { describe, expect, it } from "vitest";

import {
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
  type RequestDiagnosticsBundle,
} from "./live-bridge-runner";

function requestDiagnosticsBundle({
  desktopTurnState = "streaming",
  remoteTurnState = "completed",
}: {
  desktopTurnState?: string | null;
  remoteTurnState?: string | null;
} = {}): RequestDiagnosticsBundle {
  const makeDiagnostics = (source: string, turnState: string | null) => ({
    source,
    sessionId: "session-1",
    requestId: "request-1",
    turnState,
    latestRequestId: "request-1",
    conversationUpdatedAt: "2026-04-22T00:00:02Z",
    request: {
      status: turnState === "completed" ? "complete" : "processing",
      lifecycleState: turnState === "completed" ? "completed" : "claimed",
      failureReason: null,
      createdAt: "2026-04-22T00:00:00Z",
      claimedAt: "2026-04-22T00:00:01Z",
      interruptRequestedAt: null,
      validUntil: null,
    },
    response: {
      status: turnState === "completed" ? "complete" : "streaming",
      errorMessage: null,
      progressSeq: turnState === "completed" ? 15 : 14,
      materializedMessageSequence: turnState === "completed" ? 62 : 61,
      materializedAt:
        turnState === "completed" ? "2026-04-22T00:00:02Z" : null,
      completedAt: turnState === "completed" ? "2026-04-22T00:00:02Z" : null,
      contentLen: turnState === "completed" ? 8546 : 128,
      reasoningLen: 0,
    },
    toolCalls: {
      total: 22,
      completed: turnState === "completed" ? 22 : 21,
      pending: turnState === "completed" ? 0 : 1,
      latestToolName: "read_file",
      latestStatus: turnState === "completed" ? "completed" : "running",
      latestCompletedAt:
        turnState === "completed" ? "2026-04-22T00:00:02Z" : null,
    },
    toolResultCount: turnState === "completed" ? 22 : 21,
    messageCount: turnState === "completed" ? 62 : 61,
    timelineCount: turnState === "completed" ? 62 : 61,
    activeResponseOverlayContentLen: turnState === "completed" ? 0 : 128,
    activeResponseOverlayReasoningLen: 0,
  });

  return {
    desktop: makeDiagnostics("desktop", desktopTurnState),
    remote: makeDiagnostics("remote", remoteTurnState),
  };
}

describe("live bridge runner stall observer", () => {
  it("starts tracking when the remote is terminal but desktop is still streaming", () => {
    const observation = observeRemoteTerminalDesktopStall({
      diagnostics: requestDiagnosticsBundle(),
      previousStartedAt: null,
      now: 10_000,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: 10_000,
      stallMs: 0,
      exceededThreshold: false,
    });
  });

  it("flags the divergence once it persists past the threshold", () => {
    const firstObservation = observeRemoteTerminalDesktopStall({
      diagnostics: requestDiagnosticsBundle(),
      previousStartedAt: null,
      now: 10_000,
      thresholdMs: 30_000,
    });

    const secondObservation = observeRemoteTerminalDesktopStall({
      diagnostics: requestDiagnosticsBundle(),
      previousStartedAt: firstObservation.startedAt,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(secondObservation).toEqual({
      startedAt: 10_000,
      stallMs: 30_250,
      exceededThreshold: true,
    });
  });

  it("clears the stall timer once the desktop also reaches a terminal turn", () => {
    const observation = observeRemoteTerminalDesktopStall({
      diagnostics: requestDiagnosticsBundle({
        desktopTurnState: "completed",
        remoteTurnState: "completed",
      }),
      previousStartedAt: 10_000,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: null,
      stallMs: null,
      exceededThreshold: false,
    });
  });

  it("clears the stall timer when the remote is not terminal", () => {
    const observation = observeRemoteTerminalDesktopStall({
      diagnostics: requestDiagnosticsBundle({
        desktopTurnState: "streaming",
        remoteTurnState: "streaming",
      }),
      previousStartedAt: 10_000,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: null,
      stallMs: null,
      exceededThreshold: false,
    });
  });
});

describe("live bridge runner progress lag observer", () => {
  it("starts tracking when the remote advances ahead of a stale desktop", () => {
    const observation = observeRemoteAheadDesktopLag({
      diagnostics: requestDiagnosticsBundle(),
      desktopProgressed: false,
      previousStartedAt: null,
      now: 10_000,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: 10_000,
      lagMs: 0,
      exceededThreshold: false,
    });
  });

  it("flags the lag once the desktop stays stale past the threshold", () => {
    const firstObservation = observeRemoteAheadDesktopLag({
      diagnostics: requestDiagnosticsBundle(),
      desktopProgressed: false,
      previousStartedAt: null,
      now: 10_000,
      thresholdMs: 30_000,
    });

    const secondObservation = observeRemoteAheadDesktopLag({
      diagnostics: requestDiagnosticsBundle(),
      desktopProgressed: false,
      previousStartedAt: firstObservation.startedAt,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(secondObservation).toEqual({
      startedAt: 10_000,
      lagMs: 30_250,
      exceededThreshold: true,
    });
  });

  it("clears the lag timer when the desktop makes progress", () => {
    const observation = observeRemoteAheadDesktopLag({
      diagnostics: requestDiagnosticsBundle(),
      desktopProgressed: true,
      previousStartedAt: 10_000,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: null,
      lagMs: null,
      exceededThreshold: false,
    });
  });

  it("clears the lag timer when the remote is no longer ahead", () => {
    const observation = observeRemoteAheadDesktopLag({
      diagnostics: requestDiagnosticsBundle({
        desktopTurnState: "streaming",
        remoteTurnState: "streaming",
      }),
      desktopProgressed: false,
      previousStartedAt: 10_000,
      now: 40_250,
      thresholdMs: 30_000,
    });

    expect(observation).toEqual({
      startedAt: null,
      lagMs: null,
      exceededThreshold: false,
    });
  });
});
