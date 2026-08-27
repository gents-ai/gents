import type { DeploymentView, SyncHealthView } from "@source-inc/gents-desktop-client";

export type SyncHealthStateName =
  "healthy" | "syncing" | "stalled" | "offline" | "failed";

const STATE_LABEL: Record<SyncHealthStateName, string> = {
  healthy: "Sync healthy",
  syncing: "Syncing",
  stalled: "Sync stalled",
  offline: "Offline",
  failed: "Sync failed",
};

export function syncHealthState(
  syncHealth: SyncHealthView | null | undefined,
): SyncHealthStateName | null {
  switch (syncHealth?.state) {
    case "healthy":
    case "syncing":
    case "stalled":
    case "offline":
    case "failed":
      return syncHealth.state;
    default:
      return null;
  }
}

export function syncHealthLabel(
  syncHealth: SyncHealthView | null | undefined,
  now = Date.now(),
): string {
  const state = syncHealthState(syncHealth);
  if (!state) return "Sync unavailable";
  if (state === "offline") {
    const since = formatElapsedSince(
      syncHealth?.offlineSince ?? syncHealth?.since,
      now,
    );
    return since ? `Offline since ${since}` : STATE_LABEL.offline;
  }
  if (state === "stalled") {
    const since = formatElapsedSince(
      syncHealth?.stalledSince ?? syncHealth?.since,
      now,
    );
    return since ? `Sync stalled since ${since}` : STATE_LABEL.stalled;
  }
  if (state === "syncing") {
    const hydration = syncHealth?.hydration;
    if (
      hydration &&
      (hydration.phase === "requested" || hydration.phase === "serving") &&
      hydration.servedCount != null
    ) {
      return `Syncing · ${hydration.mergedCount} of ${hydration.servedCount}`;
    }
  }
  return STATE_LABEL[state];
}

export function formatElapsedSince(
  iso: string | null | undefined,
  now = Date.now(),
): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const elapsedMs = Math.max(0, now - then);
  const seconds = Math.floor(elapsedMs / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export function syncHealthDiagnostics(
  syncHealth: SyncHealthView | null | undefined,
  deployments: DeploymentView[],
) {
  const hydration = syncHealth?.hydration;
  return {
    state: syncHealthState(syncHealth),
    since: syncHealth?.since ?? null,
    offlineSince: syncHealth?.offlineSince ?? null,
    stalledSince: syncHealth?.stalledSince ?? null,
    lastErrorClass: syncHealth?.lastErrorClass ?? null,
    lastError: syncHealth?.lastError ?? null,
    pairingRetryCount: syncHealth?.pairingRetryCount ?? 0,
    routeRetryCount: syncHealth?.routeRetryCount ?? 0,
    connectedPeerCount: syncHealth?.connectedPeerCount ?? 0,
    hydration: hydration
      ? {
          sessionId: hydration.sessionId,
          phase: hydration.phase,
          mergedCount: hydration.mergedCount,
          servedCount: hydration.servedCount,
        }
      : null,
    peers: deployments.map((deployment) => ({
      label: deployment.label,
      agentDid: deployment.agentDid,
      dialSucceeded: deployment.dialSucceeded,
      lastError: deployment.lastError ?? null,
      pairing: deployment.pairing ?? [],
      routes: deployment.routes ?? [],
    })),
  };
}
