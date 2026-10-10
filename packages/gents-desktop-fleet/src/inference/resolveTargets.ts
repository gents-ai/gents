import type { DeploymentView } from "@source-inc/gents-desktop-client";

export function resolveTargets(deployment: DeploymentView) {
  const defaultAgentId = deployment.node.defaultAgentId;
  const agent =
    deployment.agents.find((entry) => entry.agentId === defaultAgentId) ?? null;
  const profile =
    deployment.inferenceProfiles.find(
      (entry) => entry.profile_id === agent?.inferenceProfileId,
    ) ?? null;
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === profile?.backend_id,
    ) ?? null;
  const error = !defaultAgentId
    ? "Node has no default agent binding"
    : !agent
      ? `Node default agent ${defaultAgentId} is not replicated`
      : !profile
        ? `Agent ${agent.agentId} inference profile is not replicated`
        : !backend
          ? `Inference profile ${profile.profile_id} backend is not replicated`
          : null;
  return {
    agent,
    profile,
    backend,
    backendId: backend?.backendId ?? null,
    error,
  };
}
