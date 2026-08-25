import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type {
  DesktopSessionSnapshot,
  P2PHealth,
  RenderedTimelineItem,
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
  "snapshot" | "sessionDelta" | "session" | "full";

export function desktopUpdateRefreshScope(
  reason: string | undefined,
  selectedSessionId: string | null,
  selectedTrackedRequestId: string | null,
  responseOnly = false,
): DesktopUpdateRefreshScope {
  if (reason === "health") return "snapshot";
  if (selectedSessionId && selectedTrackedRequestId) {
    if (reason === "store" && responseOnly) return "sessionDelta";
    return "session";
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
  timelineItems[liveIndex] = {
    kind: "liveAssistant",
    itemKey: timelineItems[liveIndex].itemKey,
    content,
    reasoning,
  };
  return {
    ...current,
    turnState: delta.turnState,
    latestResponse: current.latestResponse
      ? { ...current.latestResponse, ...nextResponse }
      : nextResponse,
    activeResponseOverlay: nextResponse,
    timelineItems,
    projectionRevision: delta.revision,
  };
}

export async function delay(ms: number) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

export type TrailingRefreshQueue = {
  request: () => Promise<void>;
  dispose: () => void;
};

export function createTrailingRefreshQueue(
  refresh: () => Promise<void>,
): TrailingRefreshQueue {
  let active: Promise<void> | null = null;
  let disposed = false;
  let pending = false;

  const drain = async () => {
    do {
      pending = false;
      try {
        await refresh();
      } catch (error) {
        logShellEvent(`coalesced refresh failed error=${String(error)}`);
      }
    } while (!disposed && pending);
  };

  return {
    request() {
      if (disposed) {
        return Promise.resolve();
      }

      pending = true;
      if (!active) {
        // Start in the next microtask so a synchronous event wave becomes one
        // bridge read. Events received while that read is active still retain
        // exactly one trailing pass.
        active = Promise.resolve()
          .then(drain)
          .finally(() => {
            active = null;
          });
      }
      return active;
    },
    dispose() {
      disposed = true;
      pending = false;
    },
  };
}

function timelineItemIdentity(item: RenderedTimelineItem) {
  return `${item.kind}:${item.itemKey}`;
}

function sameOptionalStrings(left: string[] | undefined, right: string[] | undefined) {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

function timelineItemUnchanged(
  previous: RenderedTimelineItem,
  next: RenderedTimelineItem,
) {
  if (previous.kind !== next.kind || previous.itemKey !== next.itemKey) return false;
  switch (previous.kind) {
    case "userMessage":
      return (
        next.kind === "userMessage" &&
        previous.requestId === next.requestId &&
        previous.sequence === next.sequence &&
        previous.content === next.content &&
        previous.timestamp === next.timestamp
      );
    case "assistantMessage":
      return (
        next.kind === "assistantMessage" &&
        previous.sequence === next.sequence &&
        previous.content === next.content &&
        previous.reasoning === next.reasoning &&
        previous.timestamp === next.timestamp
      );
    case "pendingUserTurn":
      return (
        next.kind === "pendingUserTurn" &&
        previous.requestId === next.requestId &&
        previous.content === next.content &&
        previous.lifecycleState === next.lifecycleState &&
        previous.createdAt === next.createdAt &&
        sameOptionalStrings(previous.selectedSkillIds, next.selectedSkillIds)
      );
    case "liveAssistant":
      return (
        next.kind === "liveAssistant" &&
        previous.content === next.content &&
        previous.reasoning === next.reasoning
      );
    case "toolGroup":
      return (
        next.kind === "toolGroup" && JSON.stringify(previous) === JSON.stringify(next)
      );
  }
}

function reuseUnchangedTimelineItems(
  previous: RenderedTimelineItem[],
  next: RenderedTimelineItem[],
) {
  const previousByIdentity = new Map(
    previous.map((item) => [timelineItemIdentity(item), item]),
  );
  return next.map((item) => {
    const existing = previousByIdentity.get(timelineItemIdentity(item));
    return existing && timelineItemUnchanged(existing, item) ? existing : item;
  });
}

/**
 * Merge an authoritative tip page into any older pages the reader explicitly
 * loaded. Rows inside the incoming tip are replaced, rows before its cursor
 * retain object identity, and stale live-tail rows disappear.
 */
export function mergeSessionTipSnapshot(
  current: DesktopSessionSnapshot | null,
  next: DesktopSessionSnapshot,
): DesktopSessionSnapshot {
  if (
    !current ||
    current.sessionId !== next.sessionId ||
    !next.timelinePage ||
    next.timelinePage.hasNewer
  ) {
    return next;
  }

  const firstIncoming = next.timelineItems[0];
  const overlapIndex = firstIncoming
    ? current.timelineItems.findIndex(
        (item) => timelineItemIdentity(item) === timelineItemIdentity(firstIncoming),
      )
    : -1;
  const retainedPrefix =
    next.timelinePage.hasOlder && overlapIndex >= 0
      ? current.timelineItems.slice(0, overlapIndex)
      : [];
  const timelineItems = reuseUnchangedTimelineItems(current.timelineItems, [
    ...retainedPrefix,
    ...next.timelineItems,
  ]);

  return {
    ...next,
    timelineItems,
    timelinePage: {
      ...next.timelinePage,
      pageItems: timelineItems.length,
      oldestItemKey: timelineItems[0]?.itemKey ?? null,
    },
  };
}

/** Merge an explicit older page without allowing its older metadata to regress the live tip. */
export function mergeOlderSessionTimelinePage(
  current: DesktopSessionSnapshot | null,
  older: DesktopSessionSnapshot,
): DesktopSessionSnapshot {
  if (!current || current.sessionId !== older.sessionId || !older.timelinePage) {
    return current ?? older;
  }
  const currentIdentities = new Set(current.timelineItems.map(timelineItemIdentity));
  const prefix = older.timelineItems.filter(
    (item) => !currentIdentities.has(timelineItemIdentity(item)),
  );
  const timelineItems = [...prefix, ...current.timelineItems];
  const currentPage = current.timelinePage ?? older.timelinePage;
  const totalItemsExact =
    (currentPage.totalItemsExact ?? true) &&
    (older.timelinePage.totalItemsExact ?? true);
  return {
    ...current,
    timelineItems,
    timelinePage: {
      ...currentPage,
      totalItems: totalItemsExact
        ? Math.max(currentPage.totalItems, older.timelinePage.totalItems)
        : -1,
      totalItemsExact,
      pageItems: timelineItems.length,
      hasOlder: older.timelinePage.hasOlder,
      hasNewer: false,
      oldestItemKey: timelineItems[0]?.itemKey ?? null,
      newestItemKey: timelineItems[timelineItems.length - 1]?.itemKey ?? null,
      queryCount: (currentPage.queryCount ?? 0) + (older.timelinePage.queryCount ?? 0),
      queriedRows:
        (currentPage.queriedRows ?? 0) + (older.timelinePage.queriedRows ?? 0),
      messageQueryLimit: Math.max(
        currentPage.messageQueryLimit ?? 0,
        older.timelinePage.messageQueryLimit ?? 0,
      ),
      toolCallQueryLimit: Math.max(
        currentPage.toolCallQueryLimit ?? 0,
        older.timelinePage.toolCallQueryLimit ?? 0,
      ),
    },
  };
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
