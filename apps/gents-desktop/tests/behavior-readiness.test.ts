import { describe, expect, it } from "vitest";

import type {
  BehaviorReadinessStatusView,
  BehaviorReadinessUnknownReasonView,
  BehaviorUnavailableReasonView,
  DeploymentView,
} from "@source-inc/gents-desktop-client";
import {
  behaviorReadinessCanConfigureInference,
  projectDeploymentOperationalState,
  selectedBehaviorIdForDeployment,
  selectedBehaviorReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";

function deployment(
  status: BehaviorReadinessStatusView,
  options: {
    chatSafe?: boolean;
    principalDefault?: string | null;
    sourceReason?: BehaviorReadinessUnknownReasonView;
  } = {},
): DeploymentView {
  return {
    agentDid: "did:key:z6MkRemote",
    dialSucceeded: true,
    chatSafe: options.chatSafe ?? true,
    agentPrincipal: {
      agentDid: "did:key:z6MkRemote",
      defaultBehaviorId:
        options.principalDefault === undefined ? "default" : options.principalDefault,
    },
    behaviorReadiness: {
      source: options.sourceReason
        ? { state: "unknown", reason: options.sourceReason }
        : { state: "current" },
      activeGeneration: 4,
      routerGeneration: 4,
      updatedAt: "2026-08-28T00:00:00Z",
      behaviors: [status],
    },
    behaviors: [
      {
        behaviorId: "default",
        displayName: "Default",
        enabled: false,
        isDefault: true,
      },
    ],
    // Remote clients do not receive backend configuration. A ready runtime
    // projection remains sufficient without reconstructing backend state.
    inferenceBackends: [],
  } as DeploymentView;
}

function unavailable(reason: BehaviorUnavailableReasonView): DeploymentView {
  return deployment({ state: "unavailable", behaviorId: "default", reason });
}

describe("selectedBehaviorReadinessDecision", () => {
  it.each([
    "backend_not_configured",
    "backend_disabled",
    "backend_temporarily_unavailable",
    "credentials_required",
    "inference_profile_invalid",
  ] satisfies BehaviorUnavailableReasonView[])(
    "offers inference configuration for %s",
    (reason) => {
      expect(
        behaviorReadinessCanConfigureInference(
          selectedBehaviorReadinessDecision(unavailable(reason), null),
        ),
      ).toBe(true);
    },
  );

  it("does not mislabel missing readiness or non-inference failures", () => {
    const missing = deployment(
      { state: "ready", behaviorId: "default" },
      { sourceReason: "readiness_missing" },
    );
    expect(
      behaviorReadinessCanConfigureInference(
        selectedBehaviorReadinessDecision(missing, null),
      ),
    ).toBe(false);
    expect(
      behaviorReadinessCanConfigureInference(
        selectedBehaviorReadinessDecision(
          unavailable("tool_surface_unavailable"),
          null,
        ),
      ),
    ).toBe(false);
    expect(projectDeploymentOperationalState(missing).behavior.action).toBeNull();
    expect(
      projectDeploymentOperationalState(unavailable("backend_disabled")).behavior
        .action,
    ).toBeNull();
  });

  it("uses runtime readiness as the sole behavior authority", () => {
    const remote = deployment({ state: "ready", behaviorId: "default" });
    expect(remote.inferenceBackends).toEqual([]);
    expect(remote.behaviors[0]?.enabled).toBe(false);
    expect(selectedBehaviorReadinessDecision(remote, null)).toEqual({
      kind: "ready",
      behaviorId: "default",
      behaviorLabel: "Default",
    });
  });

  it.each([
    "behavior_disabled",
    "runtime_configuration_invalid",
    "backend_not_configured",
    "backend_disabled",
    "backend_temporarily_unavailable",
    "credentials_required",
    "inference_profile_invalid",
    "tool_configuration_invalid",
    "tool_surface_unavailable",
    "executor_start_failed",
  ] satisfies BehaviorUnavailableReasonView[])(
    "blocks the typed unavailable reason %s",
    (reason) => {
      expect(selectedBehaviorReadinessDecision(unavailable(reason), null)).toEqual({
        kind: "unavailable",
        behaviorId: "default",
        behaviorLabel: "Default",
        reason,
      });
    },
  );

  it.each([
    "readiness_missing",
    "readiness_malformed",
    "readiness_version_unsupported",
    "readiness_stale",
    "process_not_ready",
    "router_generation_stale",
    "behavior_not_assigned",
  ] satisfies BehaviorReadinessUnknownReasonView[])(
    "fails closed for the typed unknown reason %s",
    (reason) => {
      const unknown = deployment(
        { state: "ready", behaviorId: "default" },
        { sourceReason: reason },
      );
      expect(selectedBehaviorReadinessDecision(unknown, null)).toEqual({
        kind: "unknown",
        behaviorId: "default",
        reason,
      });
    },
  );

  it("uses only explicit selection or the database principal default", () => {
    const current = deployment({ state: "ready", behaviorId: "default" });
    expect(selectedBehaviorReadinessDecision(current, "unassigned")).toEqual({
      kind: "unknown",
      behaviorId: "unassigned",
      reason: "behavior_not_assigned",
    });

    const noPrincipalDefault = deployment(
      { state: "ready", behaviorId: "default" },
      { principalDefault: null },
    );
    expect(selectedBehaviorReadinessDecision(noPrincipalDefault, null)).toEqual({
      kind: "unknown",
      behaviorId: null,
      reason: "behavior_not_assigned",
    });
  });

  it("replaces an agent-scoped selection that the next deployment does not assign", () => {
    const nextAgent = deployment({ state: "ready", behaviorId: "default" });
    expect(selectedBehaviorIdForDeployment(nextAgent, "previous-agent-behavior")).toBe(
      "default",
    );
    expect(selectedBehaviorIdForDeployment(nextAgent, "default")).toBe("default");
  });

  it.each([
    ["missing", { sourceReason: "readiness_missing" }, "behaviorUnavailable"],
    ["stale", { sourceReason: "router_generation_stale" }, "behaviorUnavailable"],
    ["disabled", {}, "behaviorUnavailable"],
    ["route-not-ready", { chatSafe: false }, "routeNotReady"],
    ["ready", {}, null],
  ] as const)(
    "gates the empty-backend remote topology when readiness is %s",
    (_name, options, blockedReason) => {
      const status: BehaviorReadinessStatusView =
        _name === "disabled"
          ? {
              state: "unavailable",
              behaviorId: "default",
              reason: "behavior_disabled",
            }
          : { state: "ready", behaviorId: "default" };
      const remote = deployment(status, options);
      expect(remote.inferenceBackends).toEqual([]);
      const projection = projectChatShell({
        clientAvailable: true,
        selectedAgentDid: remote.agentDid,
        selectedSessionId: null,
        draft: "hello",
        sending: false,
        session: null,
        selectedConversation: null,
        localWorkflow: { kind: "ready" },
        operationalState: projectDeploymentOperationalState(remote),
      });

      if (blockedReason === null) {
        expect(projection.sendStatus).toEqual({ kind: "ready" });
      } else {
        expect(projection.sendStatus).toMatchObject({
          kind: "disabled",
          reason: blockedReason,
        });
      }
    },
  );
});
