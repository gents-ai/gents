import type {
  AgentReadinessUnknownReasonView,
  AgentUnavailableReasonView,
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

export type OperationalRecoveryAction = "configureInference" | null;

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

export type NodeReadinessDecision =
  | { kind: "ready"; agentDid: string; agentLabel: string }
  | {
      kind: "unavailable";
      agentDid: string;
      agentLabel: string;
      reason: AgentUnavailableReasonView;
    }
  | {
      kind: "unknown";
      agentDid: string | null;
      reason: AgentReadinessUnknownReasonView;
    };

export type DeploymentOperationalState = {
  transport: OperationalStatus;
  connection: OperationalStatus;
  route: OperationalStatus;
  sync: OperationalStatus;
  agent: OperationalStatus;
  reconcile: OperationalStatus;
  admissionBlocker: OperationalStatus | null;
  summary: OperationalStatus;
  nodeReadiness: NodeReadinessDecision;
};

type DeploymentSettings = Omit<DeploymentView, "sessions" | "mailboxItems">;

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

function agentLabel(deployment: DeploymentSettings, agentDid: string): string {
  return (
    deployment.agents
      .find((agent) => agent.agentDid === agentDid)
      ?.displayName?.trim() || agentDid
  );
}

function fallbackAgentDid(deployment: DeploymentSettings): string | null {
  const nodeDefault = deployment.node.defaultAgentDid?.trim();
  if (
    nodeDefault &&
    deployment.agents.some((agent) => agent.agentDid === nodeDefault)
  ) {
    return nodeDefault;
  }
  const markedDefault = deployment.agents.find((agent) => agent.isDefault);
  if (markedDefault) {
    return markedDefault.agentDid;
  }
  return deployment.agents.find((agent) => agent.enabled)?.agentDid ?? null;
}

export function selectedAgentDidForDeployment(
  deployment: DeploymentSettings | null,
  selectedAgentDid: string | null,
): string | null {
  if (!deployment) return null;
  if (
    selectedAgentDid !== null &&
    deployment.agents.some((agent) => agent.agentDid === selectedAgentDid)
  ) {
    return selectedAgentDid;
  }
  return fallbackAgentDid(deployment);
}

export function selectedNodeReadinessDecision(
  deployment: DeploymentSettings | null,
  selectedAgentDid: string | null,
): NodeReadinessDecision {
  if (!deployment) {
    return { kind: "unknown", agentDid: null, reason: "readiness_missing" };
  }

  const readiness = deployment.nodeReadiness;
  const agentDid = selectedAgentDid ?? fallbackAgentDid(deployment);
  if (readiness.source.state === "unknown") {
    return { kind: "unknown", agentDid, reason: readiness.source.reason };
  }
  if (!agentDid) {
    return {
      kind: "unknown",
      agentDid: null,
      reason: "agent_not_assigned",
    };
  }

  const readinessStatus = readiness.agents.find(
    (candidate) => candidate.agentDid === agentDid,
  );
  if (!readinessStatus) {
    return { kind: "unknown", agentDid, reason: "agent_not_assigned" };
  }
  if (readinessStatus.state === "ready") {
    return {
      kind: "ready",
      agentDid,
      agentLabel: agentLabel(deployment, agentDid),
    };
  }
  if (readinessStatus.state === "unknown") {
    return { kind: "unknown", agentDid, reason: readinessStatus.reason };
  }
  return {
    kind: "unavailable",
    agentDid,
    agentLabel: agentLabel(deployment, agentDid),
    reason: readinessStatus.reason,
  };
}

export function nodeReadinessIsInferenceFailure(
  decision: NodeReadinessDecision,
): boolean {
  if (decision.kind !== "unavailable") return false;
  return (
    decision.reason === "backend_not_configured" ||
    decision.reason === "backend_disabled" ||
    decision.reason === "backend_temporarily_unavailable" ||
    decision.reason === "credentials_required" ||
    decision.reason === "subscription_not_entitled" ||
    decision.reason === "inference_profile_invalid"
  );
}

export function nodeReadinessCanConfigureInference(
  decision: NodeReadinessDecision,
): boolean {
  return nodeReadinessIsInferenceFailure(decision);
}

export function nodeReadinessDetail(
  decision: Exclude<NodeReadinessDecision, { kind: "ready" }>,
): string {
  if (decision.kind === "unknown") {
    switch (decision.reason) {
      case "readiness_missing":
        return "Waiting for the agent to report readiness";
      case "readiness_malformed":
        return "The agent reported invalid readiness data";
      case "readiness_version_unsupported":
        return "The agent uses an incompatible readiness format";
      case "process_not_ready":
        return "The node runtime is still starting";
      case "router_generation_stale":
        return "The agent is still applying its latest configuration";
      case "agent_not_assigned":
        return "Agent is not assigned to this runtime";
    }
  }

  switch (decision.reason) {
    case "agent_disabled":
      return `Agent “${decision.agentLabel}” is disabled`;
    case "runtime_configuration_invalid":
      return `Agent “${decision.agentLabel}” has an invalid runtime configuration`;
    case "backend_not_configured":
      return `Agent “${decision.agentLabel}” has no inference backend configured`;
    case "backend_disabled":
      return `Agent “${decision.agentLabel}” has a disabled inference backend`;
    case "backend_temporarily_unavailable":
      return `Inference backend for “${decision.agentLabel}” is temporarily unavailable`;
    case "credentials_required":
      return `Agent “${decision.agentLabel}” needs a signed-in account. Open Inference to sign in or select another account.`;
    case "subscription_not_entitled":
      return `The subscription account for “${decision.agentLabel}” is not eligible for inference. Select an eligible account or configure an API key; signing in again will not change account eligibility.`;
    case "inference_profile_invalid":
      return `Agent “${decision.agentLabel}” has an invalid inference profile`;
    case "tool_configuration_invalid":
      return `Agent “${decision.agentLabel}” has an invalid tool configuration`;
    case "tool_surface_unavailable":
      return `Agent “${decision.agentLabel}” cannot start its tools. Check installed plugins and Tools configuration on its node.`;
    case "executor_start_failed":
      return `Agent “${decision.agentLabel}” could not start`;
  }
}

export function projectAgentOperationalStatus(
  decision: NodeReadinessDecision,
  localRuntime: boolean,
): OperationalStatus {
  if (decision.kind === "ready") {
    return status({
      kind: "ready",
      layer: "runtime",
      reason: "agent_ready",
      label: "Agent is ready",
      shortLabel: "Online",
      detail: `Agent “${decision.agentLabel}” is ready to accept work.`,
    });
  }

  const inferenceFailure = nodeReadinessIsInferenceFailure(decision);
  const incompatible =
    decision.kind === "unknown" &&
    (decision.reason === "readiness_malformed" ||
      decision.reason === "readiness_version_unsupported");
  return status({
    kind: decision.kind === "unknown" && !incompatible ? "waiting" : "blocked",
    layer: inferenceFailure ? "inference" : "runtime",
    reason: decision.reason,
    label:
      decision.reason === "credentials_required"
        ? "Sign in to continue"
        : decision.reason === "subscription_not_entitled"
          ? "Subscription access unavailable"
          : inferenceFailure
            ? "Inference is unavailable"
            : incompatible
              ? "Runtime is incompatible"
              : decision.kind === "unavailable"
                ? "This agent is unavailable"
                : "Waiting for the node runtime",
    shortLabel:
      decision.reason === "credentials_required"
        ? "Sign-in required"
        : decision.reason === "subscription_not_entitled"
          ? "Account unavailable"
          : inferenceFailure
            ? "Inference unavailable"
            : incompatible
              ? "Runtime incompatible"
              : decision.kind === "unavailable"
                ? "Unavailable"
                : "Waiting for runtime",
    detail: nodeReadinessDetail(decision),
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
        kind: "waiting",
        layer: "p2p",
        reason: "disconnected",
        label: "Agent connection is offline",
        shortLabel: "Not connected",
        detail: "The secure P2P connection is recovering automatically.",
      });
}

export function projectRouteOperationalStatus(
  routeReady: boolean,
  pairingPending = false,
): OperationalStatus {
  if (routeReady) {
    return status({
      kind: "ready",
      layer: "route",
      reason: "route_ready",
      label: "Peer pairing is ready",
      shortLabel: "Paired",
      detail: "The signed peer pairing is ready.",
    });
  }
  if (pairingPending) {
    return status({
      kind: "waiting",
      layer: "route",
      reason: "pairing_not_accepted",
      label: "Waiting for pairing request acceptance",
      shortLabel: "Waiting for pairing",
      detail:
        "The pairing request was sent and is waiting for the agent to accept it.",
    });
  }
  return status({
    kind: "waiting",
    layer: "route",
    reason: "route_not_ready",
    label: "Preparing peer pairing",
    shortLabel: "Preparing",
    detail: "Waiting for the signed peer pairing to become ready.",
  });
}

/** The native chat write fence's refusal, as the composer shows it: the
 * cause and next action come from the desktop core, never from a UI-local
 * reading of pairing or source. */
export function projectChatRefusalStatus(
  refusal: DeploymentView["chatRefusal"],
): OperationalStatus | null {
  if (!refusal) return null;
  const waiting = refusal.nextAction === "await_endpoint_advertisement";
  return status({
    kind: waiting ? "waiting" : "blocked",
    layer: "route",
    reason: refusal.cause,
    label: waiting ? "Waiting for the runtime endpoint" : "Runtime is not connected",
    shortLabel: waiting ? "Waiting for endpoint" : "Not connected",
    detail: refusal.detail,
  });
}

export function runtimeConnectionBlocksAdmission(connection: string | null | undefined): boolean {
  return connection !== "connected";
}

function projectRuntimeConnectionStatus(connection: string | null | undefined): OperationalStatus {
  if (connection === "connected") {
    return status({ kind: "ready", layer: "runtime", reason: "connected",
      label: "Runtime is connected", shortLabel: "Connected",
      detail: "The authenticated runtime connection is ready." });
  }
  if (connection === "denied") {
    return status({ kind: "blocked", layer: "runtime", reason: "permission_denied",
      label: "Runtime access denied", shortLabel: "Access denied",
      detail: "The runtime rejected this client's access." });
  }
  return status({ kind: "waiting", layer: "runtime", reason: connection === "disconnected" ? "disconnected" : "connecting",
    label: "Connecting to runtime", shortLabel: "Connecting",
    detail: "Waiting for the authenticated runtime connection." });
}

export function projectDeploymentOperationalState(
  deployment: DeploymentSettings,
  selectedAgentDid: string | null = null,
  syncHealth: SyncHealthView | null = null,
): DeploymentOperationalState {
  const transport = projectDeploymentTransportStatus(deployment.dialSucceeded);
  const connection = projectRuntimeConnectionStatus(deployment.runtimeConnection);

  const localRuntime = isLocalRuntimeSource(deployment.source);
  const route = projectRouteOperationalStatus(
    localRuntime || deployment.pairingReady === true,
  );
  const sync = projectSyncOperationalStatus(syncHealth);

  const nodeReadiness = selectedNodeReadinessDecision(
    deployment,
    selectedAgentDid,
  );
  const agent = projectAgentOperationalStatus(nodeReadiness, localRuntime);
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

  const agentBlocker = agent.kind === "ready" ? null : agent;
  const admissionBlocker =
    projectChatRefusalStatus(deployment.chatRefusal) ??
    (runtimeConnectionBlocksAdmission(deployment.runtimeConnection)
      ? connection
      : agentBlocker);

  const error =
    deployment.runtime?.lastReconcileError ?? null;
  const summary = error
    ? status({
        kind: "blocked",
        layer: "runtime",
        reason: "deployment_error",
        label: "Runtime configuration error",
        shortLabel: "Error",
        detail: error,
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
              "The runtime connection and agent readiness are current.",
          })));

  return {
    transport,
    connection,
    route,
    sync,
    agent,
    reconcile,
    admissionBlocker,
    summary,
    nodeReadiness,
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

export type SyncHealthStateName =
  "healthy" | "syncing" | "offline" | "failed" | "incompatible";

export function syncHealthState(
  syncHealth: SyncHealthView | null | undefined,
): SyncHealthStateName | null {
  switch (syncHealth?.state) {
    case "healthy":
    case "syncing":
    case "offline":
    case "failed":
    case "incompatible":
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
          : state === "incompatible"
            ? "Update required"
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
    detail: syncHealth?.lastError ?? `Database synchronization is ${state}.`,
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
