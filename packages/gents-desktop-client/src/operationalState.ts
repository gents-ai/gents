import type {
  BehaviorReadinessUnknownReasonView,
  BehaviorUnavailableReasonView,
  DeploymentView,
  SyncHealthView,
} from "./types.js";

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
  sync: OperationalStatus;
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

/** Keep an explicit selection only while its database behavior row exists. */
export function selectedBehaviorIdForDeployment(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): string | null {
  if (!deployment) return null;
  if (
    selectedBehaviorId !== null &&
    deployment.behaviors.some(
      (behavior) => behavior.behaviorId === selectedBehaviorId,
    )
  ) {
    return selectedBehaviorId;
  }
  return deployment.agentPrincipal.defaultBehaviorId;
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
  const behaviorId =
    selectedBehaviorId ?? deployment.agentPrincipal.defaultBehaviorId ?? null;
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
        return "The latest runtime readiness observation is older than expected";
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
            ? "Runtime is not reporting readiness"
            : "Waiting for the agent runtime",
    shortLabel: inferenceFailure
      ? "Inference unavailable"
      : incompatible
        ? "Runtime incompatible"
        : decision.kind === "unavailable"
          ? "Unavailable"
          : stale
            ? "Runtime unavailable"
            : "Waiting for runtime",
    detail: behaviorReadinessDetail(decision),
    action: inferenceFailure && localRuntime ? "configureInference" : null,
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
  syncHealth: SyncHealthView | null = null,
): DeploymentOperationalState {
  const transport = projectDeploymentTransportStatus(deployment.dialSucceeded);

  const route = projectRouteOperationalStatus(deployment.chatSafe);
  const localRuntime = isLocalRuntimeSource(deployment.source);
  const sync = projectSyncOperationalStatus(syncHealth);

  const behaviorReadiness = selectedBehaviorReadinessDecision(
    deployment,
    selectedBehaviorId,
  );
  const behavior = projectBehaviorOperationalStatus(
    behaviorReadiness,
    localRuntime,
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

  const behaviorBlocker =
    behavior.kind === "ready" ? null : behavior;
  const admissionBlocker =
    transport.kind !== "ready"
      ? transport
      : route.kind !== "ready"
        ? route
        : behaviorBlocker;

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
      (sync.kind !== "ready" ? sync : null) ??
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
    sync,
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

export type SyncHealthStateName = "healthy" | "syncing" | "offline" | "failed";

export function syncHealthState(
  syncHealth: SyncHealthView | null | undefined,
): SyncHealthStateName | null {
  switch (syncHealth?.state) {
    case "healthy":
    case "syncing":
    case "offline":
    case "failed":
      return syncHealth.state;
    default:
      return null;
  }
}

export function projectSyncOperationalStatus(
  syncHealth: SyncHealthView | null | undefined,
): OperationalStatus {
  const state = syncHealthState(syncHealth);
  if (!state) {
    return status({
      kind: "waiting",
      layer: "sync",
      reason: "sync_not_observed",
      label: "Checking database sync",
      shortLabel: "Checking sync",
      detail: "Waiting for the database sync coordinator's first observation.",
    });
  }
  const label =
    state === "healthy"
      ? "Sync is healthy"
      : state === "syncing"
        ? "Syncing"
        : state === "offline"
          ? "Offline"
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
      `Database synchronization is ${state}.`,
    action: state === "offline" || state === "failed" ? "reconnect" : null,
  });
}

export function syncHealthDiagnostics(
  syncHealth: SyncHealthView | null | undefined,
) {
  return {
    state: syncHealthState(syncHealth),
    lastError: syncHealth?.lastError ?? null,
    connectedPeerCount: syncHealth?.connectedPeerCount ?? 0,
    pendingDagCount: syncHealth?.pendingDagCount ?? null,
    persistedPendingDagCount: syncHealth?.persistedPendingDagCount ?? null,
    pushRetryMarkerCount: syncHealth?.pushRetryMarkerCount ?? null,
    exhaustedFetchCount: syncHealth?.exhaustedFetchCount ?? null,
    quarantinedDagCount: syncHealth?.quarantinedDagCount ?? null,
  };
}
