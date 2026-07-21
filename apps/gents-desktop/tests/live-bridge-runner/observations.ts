import type {
  RemoteAheadDesktopLagObservation,
  RemoteTerminalDesktopStallObservation,
  RequestDiagnostics,
  RequestDiagnosticsBundle,
} from "./types";

const REMOTE_TERMINAL_DESKTOP_STALL_MS = 30_000;
const REMOTE_AHEAD_DESKTOP_LAG_MS = 30_000;

export function isTerminalTurnState(value?: string | null) {
  return (
    value === "completed" ||
    value === "failed" ||
    value === "superseded" ||
    value === "interrupted"
  );
}

export function observeRemoteTerminalDesktopStall({
  diagnostics,
  previousStartedAt,
  now,
  thresholdMs = REMOTE_TERMINAL_DESKTOP_STALL_MS,
}: {
  diagnostics: RequestDiagnosticsBundle;
  previousStartedAt: number | null;
  now: number;
  thresholdMs?: number;
}): RemoteTerminalDesktopStallObservation {
  if (
    !isTerminalTurnState(diagnostics.remote.turnState) ||
    isTerminalTurnState(diagnostics.desktop.turnState)
  ) {
    return {
      startedAt: null,
      stallMs: null,
      exceededThreshold: false,
    };
  }

  const startedAt = previousStartedAt ?? now;
  const stallMs = now - startedAt;
  return {
    startedAt,
    stallMs,
    exceededThreshold: previousStartedAt !== null && stallMs >= thresholdMs,
  };
}

function progressNumber(value?: number | null) {
  return value ?? 0;
}

export function requestProgressSignature(diagnostics: RequestDiagnostics) {
  return JSON.stringify({
    turnState: diagnostics.turnState ?? null,
    latestRequestId: diagnostics.latestRequestId ?? null,
    requestStatus: diagnostics.request?.status ?? null,
    requestLifecycleState: diagnostics.request?.lifecycleState ?? null,
    responseStatus: diagnostics.response?.status ?? null,
    responseProgressSeq: progressNumber(diagnostics.response?.progressSeq),
    materializedMessageSequence: progressNumber(
      diagnostics.response?.materializedMessageSequence,
    ),
    responseContentLen: progressNumber(diagnostics.response?.contentLen),
    responseReasoningLen: progressNumber(diagnostics.response?.reasoningLen),
    toolCallsCompleted: diagnostics.toolCalls.completed,
    toolCallsPending: diagnostics.toolCalls.pending,
    toolResultCount: diagnostics.toolResultCount,
    messageCount: diagnostics.messageCount,
    timelineCount: diagnostics.timelineCount,
    activeResponseOverlayContentLen: diagnostics.activeResponseOverlayContentLen,
    activeResponseOverlayReasoningLen: diagnostics.activeResponseOverlayReasoningLen,
  });
}

function isRemoteAheadOfDesktop(diagnostics: RequestDiagnosticsBundle) {
  return (
    progressNumber(diagnostics.remote.response?.progressSeq) >
      progressNumber(diagnostics.desktop.response?.progressSeq) ||
    progressNumber(diagnostics.remote.response?.materializedMessageSequence) >
      progressNumber(diagnostics.desktop.response?.materializedMessageSequence) ||
    progressNumber(diagnostics.remote.response?.contentLen) >
      progressNumber(diagnostics.desktop.response?.contentLen) ||
    diagnostics.remote.toolCalls.completed > diagnostics.desktop.toolCalls.completed ||
    diagnostics.remote.toolResultCount > diagnostics.desktop.toolResultCount ||
    diagnostics.remote.messageCount > diagnostics.desktop.messageCount ||
    diagnostics.remote.timelineCount > diagnostics.desktop.timelineCount ||
    diagnostics.remote.activeResponseOverlayContentLen >
      diagnostics.desktop.activeResponseOverlayContentLen
  );
}

export function observeRemoteAheadDesktopLag({
  diagnostics,
  desktopProgressed,
  previousStartedAt,
  now,
  thresholdMs = REMOTE_AHEAD_DESKTOP_LAG_MS,
}: {
  diagnostics: RequestDiagnosticsBundle;
  desktopProgressed: boolean;
  previousStartedAt: number | null;
  now: number;
  thresholdMs?: number;
}): RemoteAheadDesktopLagObservation {
  if (
    desktopProgressed ||
    isTerminalTurnState(diagnostics.desktop.turnState) ||
    !isRemoteAheadOfDesktop(diagnostics)
  ) {
    return {
      startedAt: null,
      lagMs: null,
      exceededThreshold: false,
    };
  }

  const startedAt = previousStartedAt ?? now;
  const lagMs = now - startedAt;
  return {
    startedAt,
    lagMs,
    exceededThreshold: previousStartedAt !== null && lagMs >= thresholdMs,
  };
}
