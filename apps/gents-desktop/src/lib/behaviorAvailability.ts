import type { DeploymentView } from "@source-inc/gents-desktop-client";

function backendName(name: string | null, backendId: string): string {
  return name?.trim() || backendId;
}

/** Explain why the selected behavior cannot currently accept a request. */
export function selectedBehaviorUnavailableHint(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): string | null {
  if (!deployment) return null;

  const behaviorId =
    selectedBehaviorId ??
    deployment.defaultBehaviorId ??
    deployment.behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
    null;
  const behavior = deployment.behaviors.find(
    (candidate) => candidate.behaviorId === behaviorId,
  );
  if (!behavior) return "The selected behavior is unavailable";
  if (!behavior.enabled) return `Behavior “${behavior.displayName}” is disabled`;

  const backendId = behavior.backendId?.trim();
  if (!backendId) return `Behavior “${behavior.displayName}” has no backend`;
  const backend = deployment.inferenceBackends.find(
    (candidate) => candidate.backendId === backendId,
  );
  if (!backend) return `Backend “${backendId}” is unavailable`;

  const displayName = backendName(backend.name, backend.backendId);
  if (backend.enabled !== true) return `Backend “${displayName}” is disabled`;
  if (backend.probeStatus === "healthy") return null;
  if (!backend.probeStatus || backend.probeStatus === "unknown") {
    return `Backend “${displayName}” is still checking readiness`;
  }
  return `Backend “${displayName}” is unavailable (${backend.probeStatus.replace(/_/g, " ")})`;
}
