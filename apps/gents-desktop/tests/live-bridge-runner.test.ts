import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { once } from "node:events";

import { describe, expect, it } from "vitest";

import {
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
  type RequestDiagnosticsBundle,
} from "./live-bridge-runner";
import {
  assertLiveBridgeRunnerPlatform,
  disposeRunnerProcess,
  terminateRunnerProcess,
  waitForReadyMessage,
} from "./live-bridge-runner/process";

function spawnRunnerFixture(script: string) {
  return spawn(process.execPath, ["-e", script], {
    detached: true,
    stdio: ["pipe", "pipe", "pipe"],
  });
}

async function forceCleanupRunnerFixture(child: ChildProcessWithoutNullStreams) {
  if (!child.stdin.destroyed && !child.stdin.writableEnded) {
    child.stdin.destroy();
  }
  child.stdout.resume();
  child.stderr.resume();

  if (Number.isInteger(child.pid) && child.pid! > 0) {
    try {
      process.kill(-child.pid!, "SIGKILL");
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") {
        throw error;
      }
    }
  }
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await Promise.race([
      once(child, "exit"),
      new Promise((resolve) => setTimeout(resolve, 2_000)),
    ]);
  }
}

function permissionDenied() {
  const error = new Error("operation not permitted") as NodeJS.ErrnoException;
  error.code = "EPERM";
  return error;
}

describe("live bridge runner platform contract", () => {
  it("fails early on Windows rather than claiming process-tree cleanup", () => {
    expect(() => assertLiveBridgeRunnerPlatform("win32")).toThrow(
      "requires POSIX process-group cleanup",
    );
    expect(() => assertLiveBridgeRunnerPlatform("darwin")).not.toThrow();
    expect(() => assertLiveBridgeRunnerPlatform("linux")).not.toThrow();
  });
});

describe.skipIf(process.platform === "win32")(
  "live bridge runner process lifecycle",
  () => {
    it("kills and awaits a runner process group when readiness times out", async () => {
      const child = spawnRunnerFixture(`
      const { spawn } = require("node:child_process");
      spawn(process.execPath, ["-e", "setInterval(() => {}, 1_000)"], {
        stdio: "ignore",
      });
      process.stdout.write("cargo still compiling");
      process.stderr.write("compiler diagnostic");
      setInterval(() => {}, 1_000);
    `);
      const processGroupId = child.pid;

      try {
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
        if (processGroupId !== undefined) {
          expect(() => process.kill(-processGroupId, 0)).toThrow();
        }
      } finally {
        await forceCleanupRunnerFixture(child);
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

      try {
        const ready = await waitForReadyMessage(child, 2_000);

        expect(ready.message).toEqual(readyMessage);
        expect(child.exitCode).toBeNull();
        expect(child.signalCode).toBeNull();
        await terminateRunnerProcess(child);
      } finally {
        await forceCleanupRunnerFixture(child);
      }
    });

    it("preserves graceful stdin disposal", async () => {
      const gracefulChild = spawnRunnerFixture(`
      process.stdin.resume();
      process.stdin.on("end", () => process.exit(0));
      process.stdout.write(${JSON.stringify(
        `${JSON.stringify({
          kind: "ready",
          baseUrl: "http://127.0.0.1:1234",
          deploymentLabel: "graceful-fixture",
          agentDid: "did:key:graceful-fixture",
          toolRoot: "/tmp/graceful-fixture",
        })}\n`,
      )});
    `);
      try {
        await waitForReadyMessage(gracefulChild, 2_000);
        await disposeRunnerProcess(gracefulChild, 500);

        expect(gracefulChild.exitCode).toBe(0);
        expect(gracefulChild.signalCode).toBeNull();
      } finally {
        await forceCleanupRunnerFixture(gracefulChild);
      }
    });

    it("terminates a stalled ready runner and its descendant during disposal", async () => {
      const stalledChild = spawnRunnerFixture(`
      const { spawn } = require("node:child_process");
      spawn(process.execPath, ["-e", "setInterval(() => {}, 1_000)"], {
        stdio: "ignore",
      });
      process.stdin.resume();
      process.stdout.write(${JSON.stringify(
        `${JSON.stringify({
          kind: "ready",
          baseUrl: "http://127.0.0.1:1234",
          deploymentLabel: "stalled-fixture",
          agentDid: "did:key:stalled-fixture",
          toolRoot: "/tmp/stalled-fixture",
        })}\n`,
      )});
      setInterval(() => {}, 1_000);
    `);
      const stalledGroupId = stalledChild.pid;
      try {
        await waitForReadyMessage(stalledChild, 2_000);
        await disposeRunnerProcess(stalledChild, 50);

        expect(stalledChild.exitCode !== null || stalledChild.signalCode !== null).toBe(
          true,
        );
        if (stalledGroupId !== undefined) {
          expect(() => process.kill(-stalledGroupId, 0)).toThrow();
        }
      } finally {
        await forceCleanupRunnerFixture(stalledChild);
      }
    });

    it("captures process errors and cleans up before rejecting", async () => {
      const child = spawnRunnerFixture(`
      process.stdout.write("startup stdout");
      process.stderr.write("startup stderr");
      setInterval(() => {}, 1_000);
    `);
      try {
        const stdoutSeen = once(child.stdout, "data");
        const stderrSeen = once(child.stderr, "data");
        const ready = waitForReadyMessage(child, 2_000);
        await Promise.all([stdoutSeen, stderrSeen]);
        const processError = new Error("synthetic EACCES") as NodeJS.ErrnoException;
        processError.code = "EACCES";
        child.emit("error", processError);

        await expect(ready).rejects.toThrow("synthetic EACCES");
        await expect(ready).rejects.toThrow("startup stdout");
        await expect(ready).rejects.toThrow("startup stderr");
        expect(child.exitCode !== null || child.signalCode !== null).toBe(true);
      } finally {
        await forceCleanupRunnerFixture(child);
      }
    });

    it("handles a spawn ENOENT without waiting for the readiness timeout", async () => {
      const child = spawn("gents-live-bridge-command-that-does-not-exist", [], {
        detached: true,
        stdio: ["pipe", "pipe", "pipe"],
      });

      await expect(waitForReadyMessage(child, 2_000)).rejects.toThrow("ENOENT");
    });

    it("ignores process-group EPERM only after the leader exits", async () => {
      const exitedChild = spawnRunnerFixture("process.exit(0)");
      const exitedPid = exitedChild.pid;
      try {
        await once(exitedChild, "exit");
        const exitedSignals: Array<[number, string | number]> = [];

        await terminateRunnerProcess(exitedChild, {
          killByPid: (pid, signal) => {
            exitedSignals.push([pid, signal]);
            throw permissionDenied();
          },
        });

        expect(exitedSignals).toEqual([[-exitedPid!, "SIGTERM"]]);

        const probeSignals: Array<[number, string | number]> = [];
        await terminateRunnerProcess(exitedChild, {
          graceMs: 1,
          killByPid: (pid, signal) => {
            probeSignals.push([pid, signal]);
            if (signal === "SIGTERM") {
              return true;
            }
            throw permissionDenied();
          },
        });
        expect(probeSignals[0]).toEqual([-exitedPid!, "SIGTERM"]);
        expect(probeSignals).toContainEqual([-exitedPid!, 0]);
        expect(probeSignals[probeSignals.length - 1]).toEqual([-exitedPid!, "SIGKILL"]);
      } finally {
        await forceCleanupRunnerFixture(exitedChild);
      }

      const liveChild = spawnRunnerFixture(`
      process.stdout.write(${JSON.stringify(
        `${JSON.stringify({
          kind: "ready",
          baseUrl: "http://127.0.0.1:1234",
          deploymentLabel: "eperm-fixture",
          agentDid: "did:key:eperm-fixture",
          toolRoot: "/tmp/eperm-fixture",
        })}\n`,
      )});
      setInterval(() => {}, 1_000);
    `);
      try {
        await waitForReadyMessage(liveChild, 2_000);
        await expect(
          terminateRunnerProcess(liveChild, {
            killByPid: () => {
              throw permissionDenied();
            },
          }),
        ).rejects.toMatchObject({ code: "EPERM" });
        expect(liveChild.exitCode).toBeNull();
        expect(liveChild.signalCode).toBeNull();
      } finally {
        await forceCleanupRunnerFixture(liveChild);
      }
    });
  },
);

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
