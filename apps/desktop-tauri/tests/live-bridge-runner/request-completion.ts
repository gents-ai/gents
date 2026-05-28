import type { DesktopApiAdapter } from "../../src/lib/desktop-api";
import type {
  ChatSendResult,
  DesktopSessionSnapshot,
  TaskRunResult,
} from "../../src/lib/types";
import {
  isTerminalTurnState,
  observeRemoteAheadDesktopLag,
  observeRemoteTerminalDesktopStall,
  requestProgressSignature,
} from "./observations";
import type { RunnerExitStatus } from "./logs";
import type { RequestDiagnosticsBundle } from "./types";

const REQUEST_POLL_MS = 500;

export type RequestCompletionTarget = Pick<
  ChatSendResult | TaskRunResult,
  "agentDid" | "requestId" | "sessionId"
>;

export async function waitForRequestCompletion({
  request,
  adapter,
  fetchRequestDiagnostics,
  getExitStatus,
  stdoutTail,
  stderrTail,
  timeoutMs,
}: {
  request: RequestCompletionTarget;
  adapter: Pick<DesktopApiAdapter, "fetchSessionSnapshot">;
  fetchRequestDiagnostics: (
    sessionId: string,
    requestId: string,
  ) => Promise<RequestDiagnosticsBundle>;
  getExitStatus: () => RunnerExitStatus | null;
  stdoutTail: () => string;
  stderrTail: () => string;
  timeoutMs: number;
}): Promise<DesktopSessionSnapshot> {
  const deadline = Date.now() + timeoutMs;
  let lastObservedState = "no diagnostics observed yet";
  let lastError: string | null = null;
  const progressHistory: string[] = [];
  let lastProgressSignature = "";
  let lastDesktopProgressSignature = "";
  let remoteTerminalDesktopStallStartedAt: number | null = null;
  let remoteAheadDesktopLagStartedAt: number | null = null;

  while (Date.now() < deadline) {
    throwIfExited({
      exitStatus: getExitStatus(),
      context: `waiting for request ${request.requestId} to complete`,
      lastObservedState,
      lastError,
      progressHistory,
      stdoutTail,
      stderrTail,
    });
    try {
      const diagnostics = await fetchRequestDiagnostics(
        request.sessionId,
        request.requestId,
      );
      lastError = null;
      const progressSignature = JSON.stringify({
        desktop: diagnostics.desktop,
        remote: diagnostics.remote,
      });
      const desktopProgressSignature = requestProgressSignature(diagnostics.desktop);
      const desktopProgressed =
        desktopProgressSignature !== lastDesktopProgressSignature;
      if (desktopProgressed) {
        lastDesktopProgressSignature = desktopProgressSignature;
      }
      if (progressSignature !== lastProgressSignature) {
        lastProgressSignature = progressSignature;
        lastObservedState = progressSignature;
        progressHistory.push(progressSignature);
        if (progressHistory.length > 8) {
          progressHistory.shift();
        }
      }

      const remoteTerminalDesktopStall = observeRemoteTerminalDesktopStall({
        diagnostics,
        previousStartedAt: remoteTerminalDesktopStallStartedAt,
        now: Date.now(),
      });
      remoteTerminalDesktopStallStartedAt = remoteTerminalDesktopStall.startedAt;
      if (remoteTerminalDesktopStall.exceededThreshold) {
        throw new Error(
          `desktop stalled after remote terminal response for request ${request.requestId}; stallMs=${remoteTerminalDesktopStall.stallMs ?? 0}; diagnostics=${JSON.stringify({ desktop: diagnostics.desktop, remote: diagnostics.remote })}; runnerStdoutTail=${JSON.stringify(stdoutTail())}; runnerStderrTail=${JSON.stringify(stderrTail())}`,
        );
      }

      const remoteAheadDesktopLag = observeRemoteAheadDesktopLag({
        diagnostics,
        desktopProgressed,
        previousStartedAt: remoteAheadDesktopLagStartedAt,
        now: Date.now(),
      });
      remoteAheadDesktopLagStartedAt = remoteAheadDesktopLag.startedAt;
      if (remoteAheadDesktopLag.exceededThreshold) {
        throw new Error(
          `desktop stopped materializing progress while remote advanced for request ${request.requestId}; lagMs=${remoteAheadDesktopLag.lagMs ?? 0}; diagnostics=${JSON.stringify({ desktop: diagnostics.desktop, remote: diagnostics.remote })}; runnerStdoutTail=${JSON.stringify(stdoutTail())}; runnerStderrTail=${JSON.stringify(stderrTail())}`,
        );
      }

      if (isTerminalTurnState(diagnostics.desktop.turnState)) {
        const snapshot = await adapter.fetchSessionSnapshot(
          request.sessionId,
          request.agentDid,
          request.requestId,
        );
        if (snapshot) {
          return snapshot;
        }
      }
    } catch (error) {
      lastError = String(error);
      throwIfExited({
        exitStatus: getExitStatus(),
        context: `waiting for request ${request.requestId} to complete`,
        lastObservedState,
        lastError,
        progressHistory,
        stdoutTail,
        stderrTail,
      });
    }
    await new Promise((resolve) => setTimeout(resolve, REQUEST_POLL_MS));
  }

  throw new Error(
    `timed out waiting for request ${request.requestId} to complete; lastObservedState=${lastObservedState}; lastError=${lastError ?? "none"}; progressHistory=${JSON.stringify(progressHistory)}; runnerStderrTail=${JSON.stringify(stderrTail())}`,
  );
}

function throwIfExited({
  exitStatus,
  context,
  lastObservedState,
  lastError,
  progressHistory,
  stdoutTail,
  stderrTail,
}: {
  exitStatus: RunnerExitStatus | null;
  context: string;
  lastObservedState: string;
  lastError: string | null;
  progressHistory: string[];
  stdoutTail: () => string;
  stderrTail: () => string;
}) {
  if (!exitStatus) {
    return;
  }

  throw new Error(
    `bridge runner exited while ${context}; code=${exitStatus.code ?? "null"}; signal=${exitStatus.signal ?? "null"}; lastObservedState=${lastObservedState}; lastError=${lastError ?? "none"}; progressHistory=${JSON.stringify(progressHistory)}; runnerStdoutTail=${JSON.stringify(stdoutTail())}; runnerStderrTail=${JSON.stringify(stderrTail())}`,
  );
}
