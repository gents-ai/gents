/* First-run is unfinished until a local agent can actually run inference.
   gents init writes a placeholder backend, so a non-empty backends list is
   not enough; a key, OAuth, env var, or a healthy probe is. */
import type {
  DeploymentView,
  DesktopClientSnapshot,
  InferenceBackendView,
} from "@source-inc/gents-desktop-client";

export function isLocalAgent(deployment: DeploymentView): boolean {
  const source = deployment.source ?? "";
  return (
    source === "local" || source === "local-standard" || source.startsWith("local")
  );
}

export function inferenceIsConfigured(deployment: DeploymentView): boolean {
  return deployment.inferenceBackends.some((backend) => backendIsConfigured(backend));
}

export function backendIsConfigured(backend: InferenceBackendView): boolean {
  if (backend.enabled === false) return false;
  if (backend.apiKeyConfigured) return true;
  if (backend.authKind === "principal_oauth" || backend.authKind === "environment") {
    return true;
  }
  return backend.probeStatus === "ok" || backend.probeStatus === "healthy";
}

export function needsFirstRunSetup(snapshot: DesktopClientSnapshot): boolean {
  const deployments = snapshot.client?.deployments ?? [];
  if (deployments.length === 0) return true;
  return deployments.some(
    (deployment) => isLocalAgent(deployment) && !inferenceIsConfigured(deployment),
  );
}
