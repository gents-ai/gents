// First-run is unfinished until a local node can actually run inference:
// `gents init` writes a placeholder backend, so a non-empty backends list
// is not enough; a key, OAuth, env var, or a healthy probe is.
import type { NodeView } from "../../hooks/fleetStore";
import type {
  DesktopClientSnapshot,
  InferenceBackendView,
} from "@source-inc/gents-desktop-client";
import { agentOf } from "./agents";

export function isLocalNode(
  deployment: Pick<NodeView, "nodeDid" | "source">,
  initNodeDid?: string | null,
): boolean {
  const source = deployment.source ?? "";
  return (
    deployment.nodeDid === initNodeDid ||
    source === "local" ||
    source === "local-standard" ||
    source.startsWith("local")
  );
}

export function inferenceIsConfigured(deployment: NodeView): boolean {
  const agent = deployment.agentConfigs.find(
    (row) => row.agent_id === deployment.node.defaultAgentId,
  );
  const profile = deployment.inferenceProfiles.find(
    (row) => row.profile_id === agent?.inference_profile_id,
  );
  return (
    Boolean(profile?.model_name?.trim()) &&
    deployment.inferenceBackends.some(
      (backend) =>
        backend.backendId === profile?.backend_id && backendIsConfigured(backend),
    )
  );
}

export function backendIsConfigured(backend: InferenceBackendView): boolean {
  if (backend.enabled === false) return false;
  if (backend.apiKeyConfigured) return true;
  if (backend.authKind === "node_oauth" || backend.authKind === "environment") {
    return true;
  }
  return backend.probeStatus === "ok" || backend.probeStatus === "healthy";
}

export function shouldRebindSetupDefault(
  deployment: NodeView,
  addingExtra: boolean,
): boolean {
  if (!addingExtra) return true;
  const defaultAgent = agentOf(deployment, deployment.node.defaultAgentId);
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
    nodeDid && snapshot.client?.operatorConfigAuthoritativeNodeDids?.includes(nodeDid),
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

export function projectFirstRunPhase(
  phase: FirstRunPhase,
  snapshotReady: boolean,
  initConfigStatus: DesktopClientSnapshot["bootstrap"]["initConfigStatus"],
  needsSetup: boolean,
  authoritative: boolean,
): FirstRunPhase {
  if (phase !== "unknown") return phase;
  if (!snapshotReady) return "unknown";
  if (initConfigStatus === "absent") return "active";
  if (initConfigStatus !== "configured") return "unknown";
  if (!authoritative) return "unknown";
  return needsSetup ? "active" : "done";
}
