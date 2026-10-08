import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import {
  isTerminalTurnState,
  type DesktopSessionSnapshot,
  type P2PHealth,
  type SessionLiveDeltaView,
} from "@source-inc/gents-desktop-client";

export const SESSION_TIMELINE_PAGE_SIZE = 40;

export type DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: number;
  clientRestartMaxAttempts: number;
  clientRestartBackoffMs: number;
  activeSessionPollMs: number | null;
};

const DEFAULT_TIMING_CONFIG: DesktopShellTimingConfig = {
  p2pAutoRestartCooldownMs: 20_000,
  clientRestartMaxAttempts: 10,
  clientRestartBackoffMs: 250,
  activeSessionPollMs: 1_500,
};

let timingConfigOverrides: Partial<DesktopShellTimingConfig> | null = null;

export function timingConfig(): DesktopShellTimingConfig {
  return {
    ...DEFAULT_TIMING_CONFIG,
    ...timingConfigOverrides,
  };
}

export function setDesktopShellTimingConfigForTests(
  overrides: Partial<DesktopShellTimingConfig> | null,
) {
  timingConfigOverrides = overrides;
}

export function shouldAutoRestartP2P(
  previous: P2PHealth | null,
  next: P2PHealth | null,
  lastAttemptAt: number | null,
  now: number,
  cooldownMs: number,
) {
  if (!next || next.status !== "wedged") {
    return false;
  }

  if (lastAttemptAt !== null && now - lastAttemptAt < cooldownMs) {
    return false;
  }

  if (!previous) {
    return true;
  }

  return (
    previous.status !== "wedged" ||
    previous.consecutiveFailures !== next.consecutiveFailures ||
    previous.lastError !== next.lastError
  );
}

export type DesktopUpdateRefreshScope =
  "snapshot" | "sessionDelta" | "session" | "sessionEvent" | "full";

export function desktopUpdateRefreshScope(
  reason: string | undefined,
  selectedSessionId: string | null,
  selectedTrackedRequestId: string | null,
): DesktopUpdateRefreshScope {
  if (reason === "health") return "snapshot";
  if (selectedSessionId && selectedTrackedRequestId) {
    if (reason === "store") return "sessionDelta";
    return "sessionEvent";
  }
  return "full";
}

const utf8 = new TextEncoder();

function liveTextHash(value: string) {
  let hash = 0x811c9dc5;
  for (const byte of utf8.encode(value)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

export function sessionLiveDeltaRequest(
  session: DesktopSessionSnapshot,
  requestId: string,
) {
  const revision = session.projectionRevision;
  if (!revision || !session.liveCursor || session.latestRequestId !== requestId)
    return null;
  const live = session.timelineItems.find((item) => item.kind === "liveAssistant");
  const content = live?.content ?? "";
  const reasoning = live?.reasoning ?? "";
  return {
    sessionId: session.sessionId,
    agentDid: session.agentDid,
    requestId,
    baseLiveCursor: session.liveCursor,
    baseContentByteLen: utf8.encode(content).byteLength,
    baseContentHash: liveTextHash(content),
    baseReasoningByteLen: utf8.encode(reasoning).byteLength,
    baseReasoningHash: liveTextHash(reasoning),
  };
}

function applyLiveTextPatch(
  current: string | null | undefined,
  patch: NonNullable<SessionLiveDeltaView["content"]>,
) {
  const base = current ?? "";
  const next =
    patch.mode === "unchanged"
      ? base
      : patch.mode === "append"
        ? `${base}${patch.value}`
        : patch.mode === "replace"
          ? patch.value
          : null;
  if (next == null) return null;
  if (
    utf8.encode(next).byteLength !== patch.byteLen ||
    liveTextHash(next) !== patch.hash
  ) {
    return null;
  }
  return next || null;
}

/** Native adapter of ClientLiveDelta.accepts; identity is encoded by the bridge. */
export function acceptsLiveCursor(
  base: string | null | undefined,
  current: string | null | undefined,
  terminal: boolean,
): boolean {
  return !terminal && base != null && base === current;
}

/** Apply a bridge-checked response suffix without rebuilding historical rows. */
export function applySessionLiveDelta(
  current: DesktopSessionSnapshot,
  delta: SessionLiveDeltaView,
): DesktopSessionSnapshot | null {
  if (
    delta.outcome === "snapshotRequired" ||
    delta.requestId !== current.latestRequestId ||
    !current.projectionRevision ||
    !acceptsLiveCursor(
      current.liveCursor,
      delta.liveCursor,
      (!!delta.turnState && isTerminalTurnState(delta.turnState)) ||
        (!!current.turnState && isTerminalTurnState(current.turnState)),
    ) ||
    delta.revision.storeVersion < current.projectionRevision.storeVersion
  ) {
    return null;
  }
  if (delta.outcome === "unchanged") {
    return {
      ...current,
      turnState: delta.turnState,
      projectionRevision: delta.revision,
    };
  }
  if (delta.outcome !== "delta" || !delta.content || !delta.reasoning) {
    return null;
  }

  const liveIndex = current.timelineItems.findIndex(
    (item) => item.kind === "liveAssistant",
  );
  if (liveIndex < 0) return null;
  const live = current.timelineItems[liveIndex];
  if (live.kind !== "liveAssistant") return null;
  const content = applyLiveTextPatch(live.content, delta.content);
  const reasoning = applyLiveTextPatch(live.reasoning, delta.reasoning);
  if (content === null && delta.content.byteLen > 0) return null;
  if (reasoning === null && delta.reasoning.byteLen > 0) return null;

  const timelineItems = current.timelineItems.slice();
  const liveTailCleared = content === null && reasoning === null;
  if (liveTailCleared) {
    timelineItems.splice(liveIndex, 1);
  } else {
    timelineItems[liveIndex] = {
      kind: "liveAssistant",
      itemKey: timelineItems[liveIndex].itemKey,
      content,
      reasoning,
    };
  }
  return {
    ...current,
    turnState: delta.turnState,
    timelineItems,
    projectionRevision: delta.revision,
  };
}

export async function delay(ms: number) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

/** Keep async presentation effects attached to the UI intent that started them. */
export function acceptsAsyncResult(
  currentGeneration: number,
  capturedGeneration: number,
) {
  return currentGeneration === capturedGeneration;
}

export function logShellEvent(message: string) {
  console.info(`[live-tauri-shell] ${message}`);
}

export function trackedRequestIdForSession(
  sessionId: string | null,
  workflow: ChatWorkflowState,
) {
  if (!sessionId) {
    return null;
  }

  if (workflow.kind === "awaitingObservation" || workflow.kind === "turnInProgress") {
    return workflow.sessionId === sessionId ? (workflow.requestId ?? null) : null;
  }

  return null;
}

/** How a failed action is reported, once: what failed, as a person would say
    it, and why. */
export function actionFailure(label: string, error: unknown): string {
  return `Couldn’t ${label}: ${error instanceof Error ? error.message : String(error)}`;
}

/* Failures an action has already shown the person. A caller that catches
   one still learns that it failed, to keep what was typed, but does not
   report it again. The bridge rejects with strings, which cannot be
   marked, so those become errors with the same message. */
const shownFailures = new WeakSet<object>();

/** `error`, marked as already shown; rethrow this. */
export function shownFailure(error: unknown): object {
  const failure =
    typeof error === "object" && error !== null ? error : new Error(String(error));
  shownFailures.add(failure);
  return failure;
}

/** Whether an action already showed this failure. */
export function wasShown(error: unknown): boolean {
  return typeof error === "object" && error !== null && shownFailures.has(error);
}
