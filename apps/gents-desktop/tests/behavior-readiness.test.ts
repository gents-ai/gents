import { describe, expect, it } from "vitest";

import type {
  AgentReadinessStatusView,
  AgentUnavailableReasonView,
  DeploymentView,
  AgentReadinessUnknownReasonView,
} from "@source-inc/gents-desktop-client";
import {
  nodeReadinessCanConfigureInference,
  projectDeploymentOperationalState,
  selectedAgentIdForDeployment,
  selectedNodeReadinessDecision,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";

function deployment(
  status: AgentReadinessStatusView,
  options: {
    chatSafe?: boolean;
    nodeDefault?: string | null;
    sourceReason?: AgentReadinessUnknownReasonView;
  } = {},
): DeploymentView {
  return {
    nodeDid: "did:key:z6MkRemote",
    dialSucceeded: true,
    chatSafe: options.chatSafe ?? true,
    node: {
      nodeDid: "did:key:z6MkRemote",
      defaultAgentId:
        options.nodeDefault === undefined ? "default" : options.nodeDefault,
    },
    nodeReadiness: {
      source: options.sourceReason
        ? { state: "unknown", reason: options.sourceReason }
        : { state: "current" },
      activeGeneration: 4,
      routerGeneration: 4,
      updatedAt: "2026-08-28T00:00:00Z",
      agents: [status],
    },
    agents: [
      {
        agentId: "default",
        displayName: "Default",
        enabled: false,
        isDefault: true,
      },
    ],
    // Remote clients do not receive backend configuration. A ready runtime
    // projection remains sufficient without reconstructing backend state.
    inferenceBackends: [],
  } as unknown as DeploymentView;
}

function unavailable(reason: AgentUnavailableReasonView): DeploymentView {
  return deployment({ state: "unavailable", agentId: "default", reason });
}

describe("selectedNodeReadinessDecision", () => {
  it.each([
    "backend_not_configured",
    "backend_disabled",
    "backend_temporarily_unavailable",
    "credentials_required",
    "inference_profile_invalid",
  ] satisfies AgentUnavailableReasonView[])(
    "offers inference configuration for %s",
    (reason) => {
      expect(
        nodeReadinessCanConfigureInference(
          selectedNodeReadinessDecision(unavailable(reason), null),
        ),
      ).toBe(true);
    },
  );

  it("does not mislabel missing readiness or non-inference failures", () => {
    const missing = deployment(
      { state: "ready", agentId: "default" },
      { sourceReason: "readiness_missing" },
    );
    expect(
      nodeReadinessCanConfigureInference(selectedNodeReadinessDecision(missing, null)),
    ).toBe(false);
    expect(
      nodeReadinessCanConfigureInference(
        selectedNodeReadinessDecision(unavailable("tool_surface_unavailable"), null),
      ),
    ).toBe(false);
    expect(projectDeploymentOperationalState(missing).agent.action).toBeNull();
    expect(
      projectDeploymentOperationalState(unavailable("agent_disabled")).agent.action,
    ).toBeNull();
  });

  it("uses runtime readiness as the sole agent authority", () => {
    const remote = deployment({ state: "ready", agentId: "default" });
    expect(remote.inferenceBackends).toEqual([]);
    expect(remote.agents[0]?.enabled).toBe(false);
    expect(selectedNodeReadinessDecision(remote, null)).toEqual({
      kind: "ready",
      agentId: "default",
      agentLabel: "Default",
    });
  });

  it.each([
    "agent_disabled",
    "runtime_configuration_invalid",
    "backend_not_configured",
    "backend_disabled",
    "backend_temporarily_unavailable",
    "credentials_required",
    "inference_profile_invalid",
    "tool_configuration_invalid",
    "tool_surface_unavailable",
    "executor_start_failed",
  ] satisfies AgentUnavailableReasonView[])(
    "blocks the typed unavailable reason %s",
    (reason) => {
      expect(selectedNodeReadinessDecision(unavailable(reason), null)).toEqual({
        kind: "unavailable",
        agentId: "default",
        agentLabel: "Default",
        reason,
      });
    },
  );

  it.each([
    "readiness_missing",
    "readiness_malformed",
    "readiness_version_unsupported",
    "process_not_ready",
    "router_generation_stale",
    "agent_not_assigned",
  ] satisfies AgentReadinessUnknownReasonView[])(
    "fails closed for the typed unknown reason %s",
    (reason) => {
      const unknown = deployment(
        { state: "ready", agentId: "default" },
        { sourceReason: reason },
      );
      expect(selectedNodeReadinessDecision(unknown, null)).toEqual({
        kind: "unknown",
        agentId: "default",
        reason,
      });
    },
  );

  it("keeps current readiness ready regardless of observation age", () => {
    const lagged = deployment({ state: "ready", agentId: "default" });
    lagged.nodeReadiness.updatedAt = "2000-01-01T00:00:00Z";
    expect(selectedNodeReadinessDecision(lagged, null)).toEqual({
      kind: "ready",
      agentId: "default",
      agentLabel: "Default",
    });
  });

  it("keeps an explicit unassigned selection unknown", () => {
    const current = deployment({ state: "ready", agentId: "default" });
    expect(selectedNodeReadinessDecision(current, "unassigned")).toEqual({
      kind: "unknown",
      agentId: "unassigned",
      reason: "agent_not_assigned",
    });
  });

  it("falls back to the gossiped agent when the node document is absent", () => {
    const noNodeDefault = deployment(
      { state: "ready", agentId: "default" },
      { nodeDefault: null },
    );
    expect(selectedNodeReadinessDecision(noNodeDefault, null)).toEqual({
      kind: "ready",
      agentId: "default",
      agentLabel: "Default",
    });
  });

  it("replaces an agent-scoped selection that the next deployment does not assign", () => {
    const nextNode = deployment({ state: "ready", agentId: "default" });
    expect(selectedAgentIdForDeployment(nextNode, "previous-agent")).toBe("default");
    expect(selectedAgentIdForDeployment(nextNode, "default")).toBe("default");
  });

  it.each([
    ["missing", { sourceReason: "readiness_missing" }, "agentUnavailable"],
    ["stale", { sourceReason: "router_generation_stale" }, "agentUnavailable"],
    ["disabled", {}, "agentUnavailable"],
    ["route-not-ready", { chatSafe: false }, "routeNotReady"],
    ["ready", {}, null],
  ] as const)(
    "gates the empty-backend remote topology when readiness is %s",
    (_name, options, blockedReason) => {
      const status: AgentReadinessStatusView =
        _name === "disabled"
          ? {
              state: "unavailable",
              agentId: "default",
              reason: "agent_disabled",
            }
          : { state: "ready", agentId: "default" };
      const remote = deployment(status, options);
      expect(remote.inferenceBackends).toEqual([]);
      const projection = projectChatShell({
        clientAvailable: true,
        selectedNodeDid: remote.nodeDid,
        selectedSessionId: null,
        sending: false,
        session: null,
        selectedSessionSummary: null,
        localWorkflow: { kind: "ready" },
        operationalState: projectDeploymentOperationalState(remote),
      });

      if (blockedReason === null) {
        expect(projection.nonEmptyContentSendStatus).toEqual({ kind: "ready" });
      } else {
        expect(projection.nonEmptyContentSendStatus).toMatchObject({
          kind: "disabled",
          reason: blockedReason,
        });
      }
    },
  );
});
