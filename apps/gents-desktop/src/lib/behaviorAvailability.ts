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

  // Persisted probe status is diagnostic history, not the runtime admission
  // decision. It can remain unknown after the runtime has measured a backend
  // healthy (and for provider kinds the prober intentionally skips), so using
  // it as a client-side gate can disagree with the authoritative runtime.
  return null;
}
