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
      case "storage_unavailable":
        return "The node runtime cannot write to its storage. Free disk space, then restart this node with the same history folder.";
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

/** The chat write route (`ClientChatReadiness`): an authenticated runtime
 * endpoint means ready, and the native fence's refusal names its cause and
 * next action. Pairing is a P2P observation and is not read here. */
export function projectRouteOperationalStatus(
  refusal: DeploymentView["chatRefusal"],
): OperationalStatus {
  return (
    projectChatRefusalStatus(refusal) ??
    status({
      kind: "ready",
      layer: "route",
      reason: "route_ready",
      label: "Runtime endpoint is ready",
      shortLabel: "Ready",
      detail: "Chat writes go to the authenticated runtime endpoint.",
    })
  );
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

/** `RuntimeConnectionObservation.presentation`: each status names its own
    reason, so an outage or a refusal never reads as a connection attempt.
    `refused` is the runtime's own answer that it does not accept this
    client's credentials; nothing it merely hides is shown as one. A null
    status is a peer the observer has not reconciled yet; the bridge never
    withholds a reconciled status, since admission reads it. */
export function projectRuntimeConnectionStatus(
  connection: string | null | undefined,
): OperationalStatus {
  switch (connection) {
    case "connected":
      return status({ kind: "ready", layer: "runtime", reason: "connected",
        label: "Runtime is connected", shortLabel: "Connected",
        detail: "The authenticated runtime connection is ready." });
    case "noEndpoint":
      return status({ kind: "blocked", layer: "runtime", reason: "no_endpoint",
        label: "Runtime has no address", shortLabel: "No address",
        detail: "This node has no runtime endpoint this app can connect to." });
    case "refused":
      return status({ kind: "blocked", layer: "runtime", reason: "refused",
        label: "Runtime refused this app", shortLabel: "Refused",
        detail: "The runtime does not accept this app's credentials. Ask its owner to restore this device's access; it reconnects by itself once they do." });
    case "disconnected":
      return status({ kind: "waiting", layer: "runtime", reason: "disconnected",
        label: "Runtime unreachable", shortLabel: "Reconnecting",
        detail: "The runtime connection was lost; reconnecting." });
    default:
      return status({ kind: "waiting", layer: "runtime", reason: "connecting",
        label: "Connecting to runtime", shortLabel: "Connecting",
        detail: "Waiting for the authenticated runtime connection." });
  }
}

export function projectDeploymentOperationalState(
  deployment: DeploymentSettings,
  selectedAgentDid: string | null = null,
  syncHealth: SyncHealthView | null = null,
): DeploymentOperationalState {
  const transport = projectDeploymentTransportStatus(deployment.dialSucceeded);
  const connection = projectRuntimeConnectionStatus(deployment.runtimeConnection);

  const localRuntime = isLocalRuntimeSource(deployment.source);
  const route = projectRouteOperationalStatus(deployment.chatRefusal);
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

export type SyncHealthStateName = "healthy" | "waiting" | "blocked";

export function syncHealthState(
  syncHealth: SyncHealthView | null | undefined,
): SyncHealthStateName | null {
  switch (syncHealth?.state) {
    case "healthy":
    case "waiting":
    case "blocked":
      return syncHealth.state;
    default:
      return null;
  }
}

type SyncCauseText = { label: string; shortLabel: string; detail: string };

/** Presentation of each `ClientSyncHealth.Cause`; the bridge derives the cause. */
const SYNC_CAUSE_TEXT: Record<string, SyncCauseText> = {
  runtime_connecting: {
    label: "Connecting to runtime",
    shortLabel: "Connecting",
    detail: "Waiting for the authenticated runtime connection.",
  },
  runtime_disconnected: {
    label: "Runtime unreachable",
    shortLabel: "Reconnecting",
    detail: "The runtime is not answering; it may be stopped. Reconnecting.",
  },
  runtime_refused: {
    label: "Runtime refused this app",
    shortLabel: "Refused",
    detail:
      "The runtime does not accept this app's credentials. Ask its owner to restore this device's access; it reconnects by itself once they do.",
  },
  runtime_no_endpoint: {
    label: "Runtime has no endpoint",
    shortLabel: "No endpoint",
    detail:
      "This runtime has no endpoint this app can read. Connect the runtime again.",
  },
  enrollment_authentication: {
    label: "Enrollment failed",
    shortLabel: "Enroll again",
    detail:
      "The enrolled runtime could not be authenticated. Enroll this device again.",
  },
  enrollment_route: {
    label: "Finishing enrollment",
    shortLabel: "Enrolling",
    detail: "The enrollment route is not applied yet; retrying.",
  },
  discovery_degraded: {
    label: "Discovery degraded",
    shortLabel: "Discovery degraded",
    detail: "The peer-to-peer discovery probe is failing; retrying.",
  },
  discovery_offline: {
    label: "Discovery offline",
    shortLabel: "Discovery offline",
    detail: "No configured peer can be reached for discovery; retrying.",
  },
};

export function projectSyncOperationalStatus(
  syncHealth: SyncHealthView | null | undefined,
): OperationalStatus {
  const state = syncHealthState(syncHealth);
  if (!state) {
    return status({
      kind: "waiting",
      layer: "sync",
      reason: "sync_not_observed",
      label: "Checking connection",
      shortLabel: "Checking connection",
      detail: "Waiting for the first runtime connection check.",
    });
  }
  if (state === "healthy") {
    return status({
      kind: "ready",
      layer: "sync",
      reason: "healthy",
      label: "Connection is healthy",
      shortLabel: "Connected",
      detail: "Every runtime is connected and enrollment is current.",
    });
  }
  const cause = syncHealth?.cause ?? "unknown";
  const text = SYNC_CAUSE_TEXT[cause] ?? {
    label: "Connection needs attention",
    shortLabel: "Connection",
    detail: `Connection is ${state}.`,
  };
  return status({
    kind: state,
    layer: "sync",
    reason: cause,
    label: text.label,
    shortLabel: text.shortLabel,
    detail: syncHealth?.lastError ?? text.detail,
  });
}

export function syncHealthDiagnostics(
  syncHealth: SyncHealthView | null | undefined,
) {
  return {
    state: syncHealthState(syncHealth),
    cause: syncHealth?.cause ?? null,
    nextAction: syncHealth?.nextAction ?? null,
    lastError: syncHealth?.lastError ?? null,
    runtimeCount: syncHealth?.runtimeCount ?? 0,
    connectedRuntimeCount: syncHealth?.connectedRuntimeCount ?? 0,
    activationErrorCount: syncHealth?.activationErrorCount ?? 0,
  };
}
