import type { BehaviorReadinessDecision } from "@source-inc/gents-desktop-chat";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

function behaviorLabel(deployment: DeploymentView, behaviorId: string): string {
  return (
    deployment.behaviors
      .find((behavior) => behavior.behaviorId === behaviorId)
      ?.displayName?.trim() || behaviorId
  );
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
