import { spawn } from "node:child_process";

import { describe, expect, it } from "vitest";

import {
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
  type RequestDiagnosticsBundle,
} from "./live-bridge-runner";
import {
  terminateRunnerProcess,
  waitForReadyMessage,
} from "./live-bridge-runner/process";

function spawnRunnerFixture(script: string) {
  return spawn(process.execPath, ["-e", script], {
    detached: process.platform !== "win32",
    stdio: ["pipe", "pipe", "pipe"],
  });
}

describe("live bridge runner process lifecycle", () => {
  it("kills and awaits a runner process group when readiness times out", async () => {
    const child = spawnRunnerFixture(`
      const { spawn } = require("node:child_process");
      if (${process.platform !== "win32"}) {
        spawn(process.execPath, ["-e", "setInterval(() => {}, 1_000)"], {
          stdio: "ignore",
        });
      }
      process.stdout.write("cargo still compiling");
      process.stderr.write("compiler diagnostic");
      setInterval(() => {}, 1_000);
    `);
    const processGroupId = child.pid;

    let startupError: Error | null = null;
    try {
      await waitForReadyMessage(child, 500);
    } catch (error) {
      startupError = error as Error;
    }

    expect(startupError?.message).toContain(
      "bridge runner did not become ready within 500ms",
    );
    expect(startupError?.message).toContain("cargo still compiling");
    expect(startupError?.message).toContain("compiler diagnostic");
    expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
    if (process.platform !== "win32" && processGroupId !== undefined) {
      expect(() => process.kill(-processGroupId, 0)).toThrow();
    }
  });

  it("leaves a ready runner alive for normal construction", async () => {
    const readyMessage = {
      kind: "ready",
      baseUrl: "http://127.0.0.1:1234",
      deploymentLabel: "fixture",
      agentDid: "did:key:fixture",
      toolRoot: "/tmp/fixture",
    };
    const child = spawnRunnerFixture(`
      process.stdout.write(${JSON.stringify(`${JSON.stringify(readyMessage)}\n`)});
      setInterval(() => {}, 1_000);
    `);

    const ready = await waitForReadyMessage(child, 2_000);

    expect(ready.message).toEqual(readyMessage);
    expect(child.exitCode).toBeNull();
    expect(child.signalCode).toBeNull();
    await terminateRunnerProcess(child);
  });
});

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
      materializedAt: turnState === "completed" ? "2026-04-22T00:00:02Z" : null,
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
      latestCompletedAt: turnState === "completed" ? "2026-04-22T00:00:02Z" : null,
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
