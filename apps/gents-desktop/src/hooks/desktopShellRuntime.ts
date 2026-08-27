import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type {
  DesktopSessionSnapshot,
  P2PHealth,
  SessionLiveDeltaView,
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
  responseOnly = false,
): DesktopUpdateRefreshScope {
  if (reason === "health") return "snapshot";
  if (reason === "hydration") return selectedSessionId ? "session" : "snapshot";
  if (selectedSessionId && selectedTrackedRequestId) {
    if (reason === "store" && responseOnly) return "sessionDelta";
    return "sessionEvent";
  }
  return "full";
}

export async function dismissMailboxItemAndClearMatchingRoute(
  itemId: string,
  dismiss: (itemId: string) => Promise<void>,
  currentRouteItemId: () => string | null,
  clearMatchingRoute: () => void,
) {
  await dismiss(itemId);
  if (currentRouteItemId() === itemId) {
    clearMatchingRoute();
  }
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
  if (!revision || session.latestRequestId !== requestId) return null;
  const response = session.activeResponseOverlay ?? session.latestResponse;
  const content = response?.content ?? "";
  const reasoning = response?.reasoning ?? "";
  return {
    sessionId: session.sessionId,
    agentDid: session.agentDid,
    requestId,
    baseReconcileVersion: revision.reconcileVersion,
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

/** Apply a bridge-checked response suffix without rebuilding historical rows. */
export function applySessionLiveDelta(
  current: DesktopSessionSnapshot,
  delta: SessionLiveDeltaView,
): DesktopSessionSnapshot | null {
  if (
    delta.outcome === "snapshotRequired" ||
    delta.requestId !== current.latestRequestId ||
    !current.projectionRevision ||
    delta.revision.reconcileVersion !== current.projectionRevision.reconcileVersion ||
    delta.revision.storeVersion < current.projectionRevision.storeVersion
  ) {
    return null;
  }
  if (delta.outcome === "unchanged") {
    return { ...current, projectionRevision: delta.revision };
  }
  if (delta.outcome !== "delta" || !delta.content || !delta.reasoning) {
    return null;
  }

  const active = current.activeResponseOverlay ?? current.latestResponse;
  if (!active) return null;
  const content = applyLiveTextPatch(active.content, delta.content);
  const reasoning = applyLiveTextPatch(active.reasoning, delta.reasoning);
  if (content === null && delta.content.byteLen > 0) return null;
  if (reasoning === null && delta.reasoning.byteLen > 0) return null;
  const liveIndex = current.timelineItems.findIndex(
    (item) => item.kind === "liveAssistant",
  );
  if (liveIndex < 0) return null;

  const nextResponse = {
    ...active,
    status: delta.status,
    content,
    reasoning,
  };
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
    latestResponse: current.latestResponse
      ? { ...current.latestResponse, ...nextResponse }
      : nextResponse,
    activeResponseOverlay: liveTailCleared ? null : nextResponse,
    timelineItems,
    projectionRevision: delta.revision,
  };
}

export async function delay(ms: number) {
  await new Promise((resolve) => setTimeout(resolve, ms));
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
