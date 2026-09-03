import type {
  BehaviorSaveRequest,
  BehaviorView,
  DeploymentView,
} from "@source-inc/gents-desktop-client";

export function resolveTargets(deployment: DeploymentView) {
  const defaultBehaviorId = deployment.agentPrincipal.defaultBehaviorId;
  if (!defaultBehaviorId) {
    return {
      behavior: null,
      backend: null,
      backendId: null,
      error: "AgentPrincipal has no default behavior binding",
    };
  }
  const behavior = deployment.behaviors.find(
    (entry) => entry.behaviorId === defaultBehaviorId,
  );
  if (!behavior) {
    return {
      behavior: null,
      backend: null,
      backendId: null,
      error: `AgentPrincipal default behavior ${defaultBehaviorId} is not replicated`,
    };
  }
  const backendId = behavior.backendId;
  if (!backendId) {
    return {
      behavior,
      backend: null,
      backendId: null,
      error: `Behavior ${behavior.behaviorId} has no backend binding`,
    };
  }
  const backend =
    deployment.inferenceBackends.find(
      (entry) => entry.backendId === backendId,
    ) ?? null;
  return { behavior, backend, backendId, error: null };
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
