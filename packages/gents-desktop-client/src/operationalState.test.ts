import { describe, expect, it } from "vitest";

import type {
  NodeReadinessSourceView,
  AgentReadinessStatusView,
  DeploymentView,
  SyncHealthView,
} from "./types.js";
import { projectDeploymentOperationalState } from "./operationalState.js";

function deployment(
  overrides: Partial<DeploymentView> & {
    readinessSource?: NodeReadinessSourceView;
    readinessStatus?: AgentReadinessStatusView;
  } = {},
): DeploymentView {
  const {
    readinessSource = { state: "current" },
    readinessStatus = { state: "ready", agentId: "default" },
    ...deploymentOverrides
  } = overrides;
  return {
    peerId: "peer-1",
    label: "Mandrake",
    nodeDid: "did:key:node",
    addr: "endpoint",
    source: "enrollment",
    graphql: null,
    dialSucceeded: true,
    chatSafe: true,
    routes: [],
    pairing: [],
    lastError: null,
    nodeConfig: null,
    agentConfigs: [],
    runtime: null,
    nodeReadiness: {
      source: readinessSource,
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      agents: [readinessStatus],
    },
    agents: [
      {
        agentId: "default",
        nodeDid: "did:key:node",
        displayName: "Default",
        description: null,
        contextId: null,
        inferenceProfileId: null,
        enabled: true,
        isDefault: true,
        tags: [],
        createdAt: null,
      },
    ],
    agentEnvironments: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    inferenceSampling: [],
    inferenceExecution: [],
    contexts: [],
    compactions: [],
    tools: [],
    toolServiceRegistries: [],
    agentTargets: [],
    datastoreToolSurfaces: [],
    chainKeyBindings: [],
    skills: [],
    tasks: [],
    schedules: [],
    eventSources: [],
    triggers: [],
    sessions: [],
    mailboxItems: [],
    node: {
      nodeDid: "did:key:node",
      defaultAgentId: "default",
    } as DeploymentView["node"],
    ...deploymentOverrides,
  };
}

function syncHealth(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    lastError: null,
    connectedPeerCount: 1,
    pendingDagCount: 0,
    persistedPendingDagCount: 0,
    pushRetryMarkerCount: 0,
    exhaustedFetchCount: 0,
    quarantinedDagCount: 0,
    ...overrides,
  };
}

describe("deployment operational state", () => {
  it("projects one shared offline wait while recovery runs automatically", () => {
    const state = projectDeploymentOperationalState(
      deployment({ dialSucceeded: false }),
    );

    expect(state.admissionBlocker).toBe(state.transport);
    expect(state.summary).toBe(state.transport);
    expect(state.transport).toMatchObject({
      layer: "p2p",
      kind: "waiting",
      shortLabel: "Not connected",
      action: null,
    });
  });

  it("keeps signed route preparation distinct from transport", () => {
    const state = projectDeploymentOperationalState(
      deployment({ chatSafe: false }),
    );

    expect(state.transport.kind).toBe("ready");
    expect(state.admissionBlocker).toBe(state.route);
    expect(state.route).toMatchObject({
      layer: "route",
      kind: "waiting",
      reason: "pairing_not_accepted",
      label: "Waiting for pairing request acceptance",
      shortLabel: "Waiting for pairing",
    });
  });

  it("does not wait for a gossiped Node after pairing", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        node: {
          nodeDid: "did:key:node",
          defaultAgentId: null,
        } as DeploymentView["node"],
        agents: [
          {
            agentId: "did:key:node:default",
            displayName: "Amy",
            enabled: true,
            isDefault: false,
          },
        ],
        readinessStatus: {
          state: "ready",
          agentId: "did:key:node:default",
        },
      }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.agent).toMatchObject({
      kind: "ready",
      shortLabel: "Online",
    });
    expect(state.agent.shortLabel).not.toBe("Waiting for runtime");
  });

  it("does not let a retained ready entry override an unknown generation", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "router_generation_stale" },
      }),
    );

    expect(state.admissionBlocker).toBe(state.agent);
    expect(state.agent).toMatchObject({
      kind: "waiting",
      reason: "router_generation_stale",
      shortLabel: "Waiting for runtime",
    });
  });

  it("keeps a local node waiting when its readiness source is missing", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        source: "local-standard",
        readinessSource: { state: "unknown", reason: "readiness_missing" },
      }),
    );

    expect(state.admissionBlocker).toBe(state.agent);
    expect(state.agent).toMatchObject({
      layer: "runtime",
      kind: "waiting",
      reason: "readiness_missing",
      shortLabel: "Waiting for runtime",
    });
  });

  it("blocks a disconnected local host despite retained Ready state", () => {
    const state = projectDeploymentOperationalState(
      deployment({ source: "local-standard", dialSucceeded: false }),
    );
    expect(state.agent.kind).toBe("ready");
    expect(state.admissionBlocker).toBe(state.transport);
    expect(state.summary).toBe(state.transport);
    expect(state.summary.shortLabel).toBe("Not connected");
  });

  it("does not let unrelated database work block current signed readiness", () => {
    const state = projectDeploymentOperationalState(
      deployment(),
      null,
      syncHealth({ state: "syncing", pendingDagCount: 2 }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.summary).toBe(state.sync);
    expect(state.summary).toMatchObject({
      layer: "sync",
      kind: "syncing",
      shortLabel: "Syncing",
    });
  });

  it("does not let database lag override current runtime readiness", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "current" },
      }),
      null,
      syncHealth({
        state: "syncing",
        pendingDagCount: 1,
        exhaustedFetchCount: 3,
      }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.agent.kind).toBe("ready");
    expect(state.summary).toBe(state.sync);
    expect(state.sync).toMatchObject({
      layer: "sync",
      kind: "syncing",
      shortLabel: "Syncing",
    });
  });

  it("surfaces a runtime version mismatch without offering a futile reconnect", () => {
    const detail =
      "this app and the agent runtime are different Gents versions; update the app and the runtime to the same version";
    const state = projectDeploymentOperationalState(
      deployment(),
      null,
      syncHealth({ state: "incompatible", lastError: detail }),
    );

    expect(state.summary).toBe(state.sync);
    expect(state.sync).toMatchObject({
      layer: "sync",
      kind: "blocked",
      reason: "incompatible",
      shortLabel: "Update required",
      detail,
      action: null,
    });
  });

  it("does not expire current runtime readiness based on its timestamp", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        nodeReadiness: {
          source: { state: "current" },
          activeGeneration: 1,
          routerGeneration: 1,
          updatedAt: "2000-01-01T00:00:00Z",
          agents: [{ state: "ready", agentId: "default" }],
        },
      }),
      null,
      syncHealth(),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.agent).toMatchObject({
      kind: "ready",
      shortLabel: "Online",
    });
  });

  it("offers backend configuration only on the host that owns it", () => {
    const readinessStatus = {
      state: "unavailable" as const,
      agentId: "default",
      reason: "backend_not_configured" as const,
    };
    const remote = projectDeploymentOperationalState(
      deployment({ readinessStatus }),
    );
    const local = projectDeploymentOperationalState(
      deployment({ source: "local-standard", readinessStatus }),
    );

    expect(remote.agent.action).toBeNull();
    expect(local.agent).toMatchObject({
      layer: "inference",
      kind: "blocked",
      action: "configureInference",
    });
  });

  it("does not call an explicitly unavailable agent online", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessStatus: {
          state: "unavailable",
          agentId: "default",
          reason: "agent_disabled",
        },
      }),
    );

    expect(state.summary).toBe(state.agent);
    expect(state.summary).toMatchObject({
      kind: "blocked",
      shortLabel: "Unavailable",
    });
  });
});
