import type { BehaviorReadinessDecision } from "@source-inc/gents-desktop-chat";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

function behaviorLabel(deployment: DeploymentView, behaviorId: string): string {
  return (
    deployment.behaviors
      .find((behavior) => behavior.behaviorId === behaviorId)
      ?.displayName?.trim() || behaviorId
  );
}

/** Keep an explicit selection only while the selected runtime assigns it. */
export function selectedBehaviorIdForDeployment(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): string | null {
  if (!deployment) return null;
  if (
    selectedBehaviorId !== null &&
    deployment.behaviorReadiness.behaviors.some(
      (behavior) => behavior.behaviorId === selectedBehaviorId,
    )
  ) {
    return selectedBehaviorId;
  }
  return deployment.behaviorReadiness.defaultBehaviorId;
}

/** Select one runtime-authored readiness verdict for chat admission. */
export function selectedBehaviorReadinessDecision(
  deployment: DeploymentView | null,
  selectedBehaviorId: string | null,
): BehaviorReadinessDecision {
  if (!deployment) {
    return { kind: "unknown", behaviorId: null, reason: "readiness_missing" };
  }

  const readiness = deployment.behaviorReadiness;
  const behaviorId = selectedBehaviorId ?? readiness.defaultBehaviorId ?? null;
  if (readiness.source.state === "unknown") {
    return { kind: "unknown", behaviorId, reason: readiness.source.reason };
  }
  if (!behaviorId) {
    return { kind: "unknown", behaviorId: null, reason: "behavior_not_assigned" };
  }

  const status = readiness.behaviors.find(
    (candidate) => candidate.behaviorId === behaviorId,
  );
  if (!status) {
    return { kind: "unknown", behaviorId, reason: "behavior_not_assigned" };
  }
  if (status.state === "ready") {
    return {
      kind: "ready",
      behaviorId,
      behaviorLabel: behaviorLabel(deployment, behaviorId),
    };
  }
  if (status.state === "unknown") {
    return { kind: "unknown", behaviorId, reason: status.reason };
  }
  return {
    kind: "unavailable",
    behaviorId,
    behaviorLabel: behaviorLabel(deployment, behaviorId),
    reason: status.reason,
  };
}

/** Offer backend configuration only when the runtime names an inference
 * configuration failure. Missing or stale readiness is a transport/runtime
 * recovery problem and must not masquerade as a backend setup problem. */
export function behaviorReadinessCanConfigureInference(
  decision: BehaviorReadinessDecision,
): boolean {
  return behaviorReadinessIsInferenceFailure(decision);
}

/** Classify inference only from an explicit runtime-authored backend verdict. */
export function behaviorReadinessIsInferenceFailure(
  decision: BehaviorReadinessDecision,
): boolean {
  if (decision.kind !== "unavailable") return false;
  return (
    decision.reason === "backend_not_configured" ||
    decision.reason === "backend_disabled" ||
    decision.reason === "backend_temporarily_unavailable" ||
    decision.reason === "credentials_required" ||
    decision.reason === "inference_profile_invalid"
  );
}

/** A P2P redial can refresh missing or stale runtime-authored readiness. Other
 * unknown states require the runtime to finish starting or applying config. */
export function behaviorReadinessCanReconnect(
  decision: BehaviorReadinessDecision,
): boolean {
  return (
    decision.kind === "unknown" &&
    (decision.reason === "readiness_missing" || decision.reason === "readiness_stale")
  );
}
