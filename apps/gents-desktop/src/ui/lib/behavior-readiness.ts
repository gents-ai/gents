import {
  selectedBehaviorReadinessDecision,
  type DeploymentView,
} from "@source-inc/gents-desktop-client";

const READINESS_REASON: Record<string, string> = {
  behavior_disabled: "behaviour is disabled",
  runtime_configuration_invalid: "runtime configuration is invalid",
  backend_not_configured: "no inference backend",
  backend_disabled: "inference backend is disabled",
  backend_temporarily_unavailable: "inference backend is unavailable right now",
  credentials_required: "inference credentials are required",
  inference_profile_invalid: "inference profile is invalid",
  tool_configuration_invalid: "tool configuration is invalid",
  tool_surface_unavailable: "tool surface is unavailable",
  executor_start_failed: "executor failed to start",
};

/** Preserve picker copy while delegating the readiness decision to its owner. */
export function behaviorReadiness(
  deployment: DeploymentView | null,
  behaviorId: string | null,
) {
  const decision = selectedBehaviorReadinessDecision(deployment, behaviorId);
  if (decision.kind === "ready") return { ready: true, reason: null };
  const reason = decision.reason;
  return {
    ready: false,
    reason: READINESS_REASON[reason] ?? reason.replace(/_/g, " "),
  };
}
