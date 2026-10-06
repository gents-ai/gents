/* First-run is unfinished until a local agent can actually run inference.
   gents init writes a placeholder backend, so a non-empty backends list is
   not enough; a key, OAuth, env var, or a healthy probe is. */
import type {
  DeploymentView,
  DesktopClientSnapshot,
  InferenceBackendView,
} from "@source-inc/gents-desktop-client";
import { agentOf } from "./agents";

export function isLocalAgent(
  deployment: Pick<DeploymentView, "agentDid" | "source">,
  initAgentDid?: string | null,
): boolean {
  const source = deployment.source ?? "";
  return (
    deployment.agentDid === initAgentDid ||
    source === "local" ||
    source === "local-standard" ||
    source.startsWith("local")
  );
}

export function inferenceIsConfigured(deployment: DeploymentView): boolean {
  const behavior = deployment.behaviorConfigs.find(
    (row) => row.behavior_id === deployment.agentPrincipal.defaultBehaviorId,
  );
  const profile = deployment.inferenceProfiles.find(
    (row) => row.profile_id === behavior?.inference_profile_id,
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
  if (backend.authKind === "principal_oauth" || backend.authKind === "environment") {
    return true;
  }
  return backend.probeStatus === "ok" || backend.probeStatus === "healthy";
}

export function shouldRebindSetupDefault(
  deployment: DeploymentView,
  addingExtra: boolean,
): boolean {
  if (!addingExtra) return true;
  const defaultBehavior = agentOf(
    deployment,
    deployment.agentPrincipal.defaultBehaviorId,
  );
  const defaultProfile = deployment.inferenceProfiles.find(
    (profile) => profile.profile_id === defaultBehavior?.inferenceProfileId,
  );
  return defaultProfile?.backend_id === `${deployment.agentDid}:backend`;
}

/** The node first-run setup configured: the one this machine runs, else the
    first listed. Setup also runs when remote nodes are already paired and
    only this machine's agent lacks inference, so the first listed node may
    be a remote one. */
export function nodeSetUp(snapshot: DesktopClientSnapshot) {
  const nodes = snapshot.client?.deployments ?? [];
  return (
    nodes.find((node) => isLocalAgent(node, snapshot.bootstrap.initAgentDid)) ??
    nodes[0] ??
    null
  );
}

export function needsFirstRunSetup(snapshot: DesktopClientSnapshot): boolean {
  const deployments = snapshot.client?.deployments ?? [];
  if (deployments.length === 0) return true;
  return deployments.some(
    (deployment) =>
      isLocalAgent(deployment, snapshot.bootstrap.initAgentDid) &&
      !inferenceIsConfigured(deployment),
  );
}
