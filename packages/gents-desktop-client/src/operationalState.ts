import type {
  BehaviorReadinessUnknownReasonView,
  BehaviorUnavailableReasonView,
  DeploymentView,
  SyncHealthView,
} from "./types.js";
import type { P2PHealth } from "./types/bootstrap.js";

export type OperationalStatusKind =
  "ready" | "working" | "waiting" | "syncing" | "blocked";

export type OperationalStatusLayer =
  | "client"
  | "selection"
  | "sync"
  | "p2p"
  | "route"
  | "runtime"
  | "inference"
  | "reconcile";

export type OperationalRecoveryAction =
  "reconnect" | "configureInference" | null;

/** Shared presentation-safe status. Surfaces choose density, not meaning. */
export type OperationalStatus = {
  kind: OperationalStatusKind;
  layer: OperationalStatusLayer;
  reason: string;
  label: string;
  shortLabel: string;
  detail: string;
  action: OperationalRecoveryAction;
  animated: boolean;
};

export type BehaviorReadinessDecision =
  | { kind: "ready"; behaviorId: string; behaviorLabel: string }
  | {
      kind: "unavailable";
      behaviorId: string;
      behaviorLabel: string;
      reason: BehaviorUnavailableReasonView;
    }
  | {
      kind: "unknown";
      behaviorId: string | null;
      reason: BehaviorReadinessUnknownReasonView;
    };

export type DeploymentOperationalState = {
  transport: OperationalStatus;
  route: OperationalStatus;
  behavior: OperationalStatus;
  reconcile: OperationalStatus;
  /** First status that prevents a chat request from being admitted. */
  admissionBlocker: OperationalStatus | null;
  /** Highest-priority fleet/header summary, including non-blocking reconcile. */
  summary: OperationalStatus;
  behaviorReadiness: BehaviorReadinessDecision;
};

function status(
  value: Omit<OperationalStatus, "action" | "animated"> &
    Partial<Pick<OperationalStatus, "action" | "animated">>,
): OperationalStatus {
  return {
    action: null,
    animated: value.kind === "waiting" || value.kind === "syncing",
    ...value,
  };
}

export function isLocalRuntimeSource(source?: string | null): boolean {
  return source === "local-standard";
}

function behaviorLabel(deployment: DeploymentView, behaviorId: string): string {
  return (
    deployment.behaviors
      .find((behavior) => behavior.behaviorId === behaviorId)
      ?.displayName?.trim() || behaviorId
  );
}

/** Keep an explicit selection only while the selected runtime assigns it. */
export function selectedBehaviorIdForDeployment(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): string | null {
  if (!deployment) return null;
  if (
    selectedBehaviorId !== null &&
    deployment.behaviorReadiness.behaviors.some(
      (behavior) => behavior.behaviorId === selectedBehaviorId,
    )
  ) {
    return selectedBehaviorId;
  }
  return deployment.behaviorReadiness.defaultBehaviorId;
}

/** Select one runtime-authored readiness verdict for admission and display. */
export function selectedBehaviorReadinessDecision(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): BehaviorReadinessDecision {
  if (!deployment) {
    return { kind: "unknown", behaviorId: null, reason: "readiness_missing" };
  }

  const readiness = deployment.behaviorReadiness;
  const behaviorId = selectedBehaviorId ?? readiness.defaultBehaviorId ?? null;
  if (readiness.source.state === "unknown") {
    return { kind: "unknown", behaviorId, reason: readiness.source.reason };
  }
  if (!behaviorId) {
    return {
      kind: "unknown",
      behaviorId: null,
      reason: "behavior_not_assigned",
    };
  }

  const readinessStatus = readiness.behaviors.find(
    (candidate) => candidate.behaviorId === behaviorId,
  );
  if (!readinessStatus) {
    return { kind: "unknown", behaviorId, reason: "behavior_not_assigned" };
  }
  if (readinessStatus.state === "ready") {
    return {
      kind: "ready",
      behaviorId,
      behaviorLabel: behaviorLabel(deployment, behaviorId),
    };
  }
  if (readinessStatus.state === "unknown") {
    return { kind: "unknown", behaviorId, reason: readinessStatus.reason };
  }
  return {
    kind: "unavailable",
    behaviorId,
    behaviorLabel: behaviorLabel(deployment, behaviorId),
    reason: readinessStatus.reason,
  };
}

export function behaviorReadinessIsInferenceFailure(
  decision: BehaviorReadinessDecision,
): boolean {
  if (decision.kind !== "unavailable") return false;
  return (
    decision.reason === "backend_not_configured" ||
    decision.reason === "backend_disabled" ||
    decision.reason === "backend_temporarily_unavailable" ||
    decision.reason === "credentials_required" ||
    decision.reason === "inference_profile_invalid"
  );
}

export function behaviorReadinessCanConfigureInference(
  decision: BehaviorReadinessDecision,
): boolean {
  return behaviorReadinessIsInferenceFailure(decision);
}

export function behaviorReadinessCanReconnect(
  decision: BehaviorReadinessDecision,
): boolean {
  return (
    decision.kind === "unknown" &&
    (decision.reason === "readiness_missing" ||
      decision.reason === "readiness_stale")
  );
}

export function behaviorReadinessDetail(
  decision: Exclude<BehaviorReadinessDecision, { kind: "ready" }>,
): string {
  if (decision.kind === "unknown") {
    switch (decision.reason) {
      case "readiness_missing":
        return "Waiting for the agent to report readiness";
      case "readiness_malformed":
        return "The agent reported invalid readiness data";
      case "readiness_version_unsupported":
        return "The agent uses an incompatible readiness format";
      case "readiness_stale":
        return "The agent stopped reporting readiness; reconnect it or restart its runtime";
      case "process_not_ready":
        return "The agent runtime is still starting";
      case "router_generation_stale":
        return "The agent is still applying its latest configuration";
      case "behavior_not_assigned":
        return "Behavior is not assigned to this runtime";
    }
  }

  switch (decision.reason) {
    case "behavior_disabled":
      return `Behavior “${decision.behaviorLabel}” is disabled`;
    case "runtime_configuration_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid runtime configuration`;
    case "backend_not_configured":
      return `Behavior “${decision.behaviorLabel}” has no inference backend configured`;
    case "backend_disabled":
      return `Behavior “${decision.behaviorLabel}” has a disabled inference backend`;
    case "backend_temporarily_unavailable":
      return `Inference backend for “${decision.behaviorLabel}” is temporarily unavailable`;
    case "credentials_required":
      return `Behavior “${decision.behaviorLabel}” requires inference credentials`;
    case "inference_profile_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid inference profile`;
    case "tool_configuration_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid tool configuration`;
    case "tool_surface_unavailable":
      return `Behavior “${decision.behaviorLabel}” cannot start its tool surface`;
    case "executor_start_failed":
      return `Behavior “${decision.behaviorLabel}” could not start`;
  }
}

export function projectBehaviorOperationalStatus(
  decision: BehaviorReadinessDecision,
  localRuntime: boolean,
): OperationalStatus {
  if (decision.kind === "ready") {
    return status({
      kind: "ready",
      layer: "runtime",
      reason: "behavior_ready",
      label: "Agent is ready",
      shortLabel: "Online",
      detail: `Behavior “${decision.behaviorLabel}” is ready to accept work.`,
    });
  }

  const inferenceFailure = behaviorReadinessIsInferenceFailure(decision);
  const incompatible =
    decision.kind === "unknown" &&
    (decision.reason === "readiness_malformed" ||
      decision.reason === "readiness_version_unsupported");
  const stale =
    decision.kind === "unknown" && decision.reason === "readiness_stale";
  const reconnect = behaviorReadinessCanReconnect(decision);
  return status({
    kind: decision.kind === "unknown" && !incompatible ? "waiting" : "blocked",
    layer: inferenceFailure ? "inference" : "runtime",
    reason: decision.reason,
    label: inferenceFailure
      ? "Inference is unavailable"
      : incompatible
        ? "Runtime is incompatible"
        : decision.kind === "unavailable"
          ? "This behavior is unavailable"
          : stale
            ? "Runtime readiness is stale"
            : "Waiting for the agent runtime",
    shortLabel: inferenceFailure
      ? "Inference unavailable"
      : incompatible
        ? "Runtime incompatible"
        : decision.kind === "unavailable"
          ? "Unavailable"
          : stale
            ? "Runtime stale"
            : "Waiting for runtime",
    detail: behaviorReadinessDetail(decision),
    action:
      inferenceFailure && localRuntime
        ? "configureInference"
        : reconnect
          ? "reconnect"
          : null,
  });
}

export function projectDeploymentTransportStatus(
  connected: boolean,
): OperationalStatus {
  return connected
    ? status({
        kind: "ready",
        layer: "p2p",
        reason: "connected",
        label: "Agent is connected",
        shortLabel: "Connected",
        detail: "The secure P2P transport is connected.",
      })
    : status({
        kind: "blocked",
        layer: "p2p",
        reason: "disconnected",
        label: "Agent connection is offline",
        shortLabel: "Not connected",
        detail: "Reconnect the secure P2P connection to continue.",
        action: "reconnect",
      });
}

export function projectRouteOperationalStatus(
  routeReady: boolean,
): OperationalStatus {
  return routeReady
    ? status({
        kind: "ready",
        layer: "route",
        reason: "route_ready",
        label: "Secure route is ready",
        shortLabel: "Route ready",
        detail: "The signed conversation route is ready.",
      })
    : status({
        kind: "waiting",
        layer: "route",
        reason: "route_not_ready",
        label: "Preparing the secure route",
        shortLabel: "Preparing",
        detail:
          "The agent is connected, but its signed conversation route is not ready yet.",
        action: "reconnect",
      });
}

export function projectDeploymentOperationalState(
  deployment: DeploymentView,
  selectedBehaviorId: string | null = null,
): DeploymentOperationalState {
  const transport = projectDeploymentTransportStatus(deployment.dialSucceeded);

  // A request route is usable only after both authenticated enrollment and the
  // runtime's final chat-safety projection agree. Inconsistent snapshots fail
  // closed while the newer half of the snapshot catches up.
  const routeReady = deployment.pairingReady && deployment.chatSafe;
  const route = projectRouteOperationalStatus(routeReady);

  const behaviorReadiness = selectedBehaviorReadinessDecision(
    deployment,
    selectedBehaviorId,
  );
  const behavior = projectBehaviorOperationalStatus(
    behaviorReadiness,
    isLocalRuntimeSource(deployment.source),
  );
  const reconcilePhase = deployment.runtime?.reconcilePhase ?? "unknown";
  const reconcile =
    reconcilePhase !== "idle" && reconcilePhase !== "unknown"
      ? status({
          kind: "syncing",
          layer: "reconcile",
          reason: reconcilePhase,
          label: "Syncing runtime configuration",
          shortLabel: "Syncing",
          detail: `Runtime reconciliation is ${reconcilePhase}.`,
        })
      : status({
          kind: "ready",
          layer: "reconcile",
          reason: reconcilePhase,
          label: "Runtime configuration is current",
          shortLabel: "Current",
          detail: "No runtime configuration reconciliation is pending.",
        });

  const admissionBlocker =
    transport.kind !== "ready"
      ? transport
      : route.kind !== "ready"
        ? route
        : behavior.kind !== "ready"
          ? behavior
          : null;

  const error =
    deployment.lastError ?? deployment.runtime?.lastReconcileError ?? null;
  const summary = error
    ? status({
        kind: "blocked",
        layer: "p2p",
        reason: "deployment_error",
        label: "Agent connection error",
        shortLabel: "Error",
        detail: error,
        action: "reconnect",
      })
    : (admissionBlocker ??
      (reconcile.kind !== "ready"
        ? reconcile
        : status({
            kind: "ready",
            layer: "runtime",
            reason: "online",
            label: "Agent is online",
            shortLabel: "Online",
            detail:
              "Transport, signed route, and runtime readiness are current.",
          })));

  return {
    transport,
    route,
    behavior,
    reconcile,
    admissionBlocker,
    summary,
    behaviorReadiness,
  };
}

export function projectClientOperationalStatus(
  clientAvailable: boolean,
  agentSelected: boolean,
): OperationalStatus | null {
  if (!clientAvailable) {
    return status({
      kind: "blocked",
      layer: "client",
      reason: "client_offline",
      label: "Secure client is offline",
      shortLabel: "Client offline",
      detail: "Secure client is not running",
    });
  }
  if (!agentSelected) {
    return status({
      kind: "blocked",
      layer: "selection",
      reason: "agent_not_selected",
      label: "Select an agent",
      shortLabel: "Select agent",
      detail: "Select an agent before sending",
    });
  }
  return null;
}

export function projectP2PTransportOperationalStatus(
  runtimeHealth: P2PHealth | null,
  configuredPeerCount: number,
  dialedPeerCount: number,
): OperationalStatus {
  const diagnostic = runtimeHealth
    ? `Transport ${runtimeHealth.status}; ${dialedPeerCount}/${configuredPeerCount} saved peers dialed; ${runtimeHealth.connectedPeerCount} active connections; ${runtimeHealth.replicatorCount} replicators`
    : `Checking P2P transport; ${dialedPeerCount}/${configuredPeerCount} saved peers dialed`;

  if (!runtimeHealth) {
    return status({
      kind: "waiting",
      layer: "p2p",
      reason: "transport_unknown",
      label: "Checking secure sync",
      shortLabel: "Checking sync",
      detail: diagnostic,
    });
  }
  if (runtimeHealth.status === "wedged") {
    return status({
      kind: "blocked",
      layer: "p2p",
      reason: "transport_wedged",
      label: "Secure sync is stalled",
      shortLabel: "P2P stalled",
      detail: diagnostic,
      action: "reconnect",
    });
  }
  if (runtimeHealth.status !== "healthy") {
    return status({
      kind: "syncing",
      layer: "p2p",
      reason: "transport_retrying",
      label: "Retrying secure sync",
      shortLabel: "P2P retrying",
      detail: diagnostic,
      action: "reconnect",
    });
  }
  if (configuredPeerCount === 0) {
    return status({
      kind: "ready",
      layer: "p2p",
      reason: "local_only",
      label: "Local runtime",
      shortLabel: "Local",
      detail: diagnostic,
    });
  }
  if (dialedPeerCount < configuredPeerCount) {
    return status({
      kind: "syncing",
      layer: "p2p",
      reason: "peers_reconnecting",
      label: `Reconnecting ${dialedPeerCount}/${configuredPeerCount} agents`,
      shortLabel: `Reconnecting ${dialedPeerCount}/${configuredPeerCount}`,
      detail: diagnostic,
      action: "reconnect",
    });
  }
  return status({
    kind: "ready",
    layer: "p2p",
    reason: "transport_healthy",
    label: "Secure sync is connected",
    shortLabel: "Connected",
    detail: diagnostic,
  });
}

export type SyncHealthStateName =
  "healthy" | "syncing" | "stalled" | "offline" | "failed";

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

export function formatElapsedSince(
  iso: string | null | undefined,
  now = Date.now(),
): string | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return iso;
  const elapsedMs = Math.max(0, now - then);
  const seconds = Math.floor(elapsedMs / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

export function projectSyncOperationalStatus(
  syncHealth: SyncHealthView | null | undefined,
  now = Date.now(),
): OperationalStatus | null {
  const state = syncHealthState(syncHealth);
  if (!state) return null;
  const since =
    state === "offline"
      ? formatElapsedSince(syncHealth?.offlineSince ?? syncHealth?.since, now)
      : state === "stalled"
        ? formatElapsedSince(syncHealth?.stalledSince ?? syncHealth?.since, now)
        : null;
  const label =
    state === "healthy"
      ? "Sync is healthy"
      : state === "syncing"
        ? "Syncing"
        : state === "stalled"
          ? since
            ? `Sync stalled for ${since}`
            : "Sync stalled"
          : state === "offline"
            ? since
              ? `Offline for ${since}`
              : "Offline"
            : "Sync failed";
  return status({
    kind:
      state === "healthy"
        ? "ready"
        : state === "syncing"
          ? "syncing"
          : "blocked",
    layer: "sync",
    reason: state,
    label,
    shortLabel: state === "healthy" ? "Sync healthy" : label,
    detail:
      syncHealth?.lastError ??
      `Collection and route synchronization is ${state}.`,
    action:
      state === "offline" || state === "stalled" || state === "failed"
        ? "reconnect"
        : null,
  });
}

export function syncHealthDiagnostics(
  syncHealth: SyncHealthView | null | undefined,
  deployments: DeploymentView[],
) {
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
