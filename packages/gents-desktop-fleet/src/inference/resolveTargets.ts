import type {
  BehaviorSaveRequest,
  BehaviorView,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

export function resolveTargets(deployment: DeploymentView) {
  const behavior =
    deployment.behaviors.find((entry) => entry.isDefault) ??
    deployment.behaviors[0] ??
    null;
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === behavior?.backendId,
    ) ??
    deployment.inferenceBackends[0] ??
    null;
  const backendId = backend?.backendId ?? behavior?.backendId ?? "default";
  return { behavior, backend, backendId };
}

export function behaviorSaveFrom(
  behavior: BehaviorView,
  agentDid: string,
  backendId: string,
): BehaviorSaveRequest {
  return {
    agentDid,
    behaviorId: behavior.behaviorId,
    displayName: behavior.displayName,
    systemPrompt: behavior.systemPrompt ?? "",
    backendId,
    toolSelectionId: behavior.toolSelectionId ?? null,
    inferenceProfileId: behavior.inferenceProfileId ?? null,
    compactionStrategy: behavior.compactionStrategy ?? null,
    compactionThreshold: behavior.compactionThreshold ?? null,
    enabled: behavior.enabled,
    skillRefs: behavior.skillRefs,
    skillExcludes: behavior.skillExcludes,
  };
}
