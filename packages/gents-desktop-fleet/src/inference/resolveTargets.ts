import type { DeploymentView } from "@source-inc/gents-desktop-client";

export function resolveTargets(deployment: DeploymentView) {
  const defaultBehaviorId = deployment.agentPrincipal.defaultBehaviorId;
  const behavior =
    deployment.behaviors.find(
      (entry) => entry.behaviorId === defaultBehaviorId,
    ) ?? null;
  const profile =
    deployment.inferenceProfiles.find(
      (entry) => entry.profile_id === behavior?.inferenceProfileId,
    ) ?? null;
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === profile?.backend_id,
    ) ?? null;
  const error = !defaultBehaviorId
    ? "AgentPrincipal has no default behavior binding"
    : !behavior
      ? `AgentPrincipal default behavior ${defaultBehaviorId} is not replicated`
      : !profile
        ? `Behavior ${behavior.behaviorId} inference profile is not replicated`
        : !backend
          ? `Inference profile ${profile.profile_id} backend is not replicated`
          : null;
  return {
    behavior,
    profile,
    backend,
    backendId: backend?.backendId ?? null,
    error,
  };
}
