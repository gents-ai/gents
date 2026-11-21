import type { DeploymentView } from "./generated/DeploymentView.js";
import type { DesktopClientSnapshot } from "./generated/DesktopClientSnapshot.js";
import type { InferenceBackendView } from "./generated/InferenceBackendView.js";
import type { SavedRuntimeView } from "./generated/SavedRuntimeView.js";

type NodeView = Omit<DeploymentView, "sessions" | "mailboxItems">;

export function isLocalNode(
  deployment: Pick<NodeView, "nodeDid" | "local">,
  initNodeDid?: string | null,
): boolean {
  return (
    deployment.nodeDid === initNodeDid || deployment.local
  );
}

export function inferenceIsConfigured(
  deployment: Pick<
    NodeView,
    "node" | "agentConfigs" | "inferenceProfiles" | "inferenceBackends"
  >,
): boolean {
  const agent = deployment.agentConfigs.find(
    (row) => row.agent_did === deployment.node.defaultAgentDid,
  );
  const profile = deployment.inferenceProfiles.find(
    (row) => row.profile_id === agent?.inference_profile_id,
  );
  return (
    Boolean(profile?.model_name?.trim()) &&
    deployment.inferenceBackends.some(
      (backend) =>
        backend.backendId === profile?.backend_id &&
        backendIsConfigured(backend),
    )
  );
}

export function backendIsConfigured(backend: InferenceBackendView): boolean {
  if (backend.enabled === false) return false;
  if (backend.apiKeyConfigured) return true;
  if (
    backend.authKind === "node_oauth" ||
    backend.authKind === "node_oauth_pool" ||
    backend.authKind === "environment"
  ) {
    return true;
  }
  return backend.probeStatus === "ok" || backend.probeStatus === "healthy";
}

export function shouldRebindSetupDefault(
  deployment: Pick<
    NodeView,
    "nodeDid" | "node" | "agents" | "inferenceProfiles"
  >,
  addingExtra: boolean,
): boolean {
  if (!addingExtra) return true;
  const defaultAgent = deployment.agents.find(
    (agent) => agent.agentDid === deployment.node.defaultAgentDid,
  );
  const defaultProfile = deployment.inferenceProfiles.find(
    (profile) => profile.profile_id === defaultAgent?.inferenceProfileId,
  );
  return defaultProfile?.backend_id === `${deployment.nodeDid}:backend`;
}

export function nodeSetUp(snapshot: DesktopClientSnapshot) {
  const nodes = snapshot.client?.deployments ?? [];
  return (
    nodes.find((node) => isLocalNode(node, snapshot.bootstrap.initNodeDid)) ??
    nodes[0] ??
    null
  );
}

export function configIsRead(
  snapshot: Pick<DesktopClientSnapshot, "client">,
  nodeDid: string | null | undefined,
): boolean {
  return Boolean(
    nodeDid &&
    snapshot.client?.operatorConfigAuthoritativeNodeDids?.includes(nodeDid),
  );
}

export function needsFirstRunSetup(snapshot: DesktopClientSnapshot): boolean {
  const deployments = snapshot.client?.deployments ?? [];
  if (deployments.length === 0) return true;
  return deployments.some(
    (deployment) =>
      isLocalNode(deployment, snapshot.bootstrap.initNodeDid) &&
      !inferenceIsConfigured(deployment),
  );
}

export type FirstRunPhase = "unknown" | "active" | "done";

export function isSavedRemote(
  peer: SavedRuntimeView,
  initNodeDid?: string | null,
): boolean {
  return (
    !peer.local &&
    !isLocalNode(peer, initNodeDid)
  );
}

export function hasSavedRemote(
  snapshot: Pick<DesktopClientSnapshot, "bootstrap">,
): boolean {
  return snapshot.bootstrap.savedRuntimes.some((peer) =>
    isSavedRemote(peer, snapshot.bootstrap.initNodeDid),
  );
}

export function projectFirstRunPhase(
  phase: FirstRunPhase,
  snapshotReady: boolean,
  initConfigStatus: DesktopClientSnapshot["bootstrap"]["initConfigStatus"],
  needsSetup: boolean,
  authoritative: boolean,
  localRuntimeAllowed = true,
  remoteConfigured = false,
  localHomeLost = false,
): FirstRunPhase {
  if (phase !== "unknown") return phase;
  if (!snapshotReady) return "unknown";
  if (initConfigStatus !== "absent" && initConfigStatus !== "configured")
    return "unknown";
  if (!localRuntimeAllowed || initConfigStatus === "absent")
    return remoteConfigured && !localHomeLost ? "done" : "active";
  if (!authoritative) return "unknown";
  return needsSetup ? "active" : "done";
}
