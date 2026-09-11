import { describe, expect, it } from "vitest";

import type {
  BehaviorReadinessSourceView,
  BehaviorReadinessStatusView,
  DeploymentView,
  SyncHealthView,
} from "./types.js";
import { projectDeploymentOperationalState } from "./operationalState.js";

function deployment(
  overrides: Partial<DeploymentView> & {
    readinessSource?: BehaviorReadinessSourceView;
    readinessStatus?: BehaviorReadinessStatusView;
  } = {},
): DeploymentView {
  const {
    readinessSource = { state: "current" },
    readinessStatus = { state: "ready", behaviorId: "default" },
    ...deploymentOverrides
  } = overrides;
  return {
    peerId: "peer-1",
    label: "Mandrake",
    agentDid: "did:key:agent",
    addr: "endpoint",
    source: "enrollment",
    graphql: null,
    dialSucceeded: true,
    chatSafe: true,
    routes: [],
    pairing: [],
    lastError: null,
    principalConfig: null,
    behaviorConfigs: [],
    runtime: null,
    behaviorReadiness: {
      source: readinessSource,
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      behaviors: [readinessStatus],
    },
    behaviors: [
      {
        behaviorId: "default",
        agentDid: "did:key:agent",
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
    behaviorEnvironments: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    inferenceSampling: [],
    inferenceExecution: [],
    contexts: [],
    compactions: [],
    tools: [],
    toolServiceRegistries: [],
    subagentTargets: [],
    datastoreToolSurfaces: [],
    chainKeyBindings: [],
    skills: [],
    tasks: [],
    schedules: [],
    eventSources: [],
    triggers: [],
    sessions: [],
    mailboxItems: [],
    agentPrincipal: {
      agentDid: "did:key:agent",
      defaultBehaviorId: "default",
    } as DeploymentView["agentPrincipal"],
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
  it("projects one shared offline blocker and recovery action", () => {
    const state = projectDeploymentOperationalState(
      deployment({ dialSucceeded: false }),
    );

    expect(state.admissionBlocker).toBe(state.transport);
    expect(state.summary).toBe(state.transport);
    expect(state.transport).toMatchObject({
      layer: "p2p",
      kind: "blocked",
      shortLabel: "Not connected",
      action: "reconnect",
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

  it("does not wait for a gossiped AgentPrincipal after pairing", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        agentPrincipal: {
          agentDid: "did:key:agent",
          defaultBehaviorId: null,
        } as DeploymentView["agentPrincipal"],
        behaviors: [
          {
            behaviorId: "did:key:agent:default",
            displayName: "Amy",
            enabled: true,
            isDefault: false,
          },
        ],
        readinessStatus: {
          state: "ready",
          behaviorId: "did:key:agent:default",
        },
      }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.behavior).toMatchObject({
      kind: "ready",
      shortLabel: "Online",
    });
    expect(state.behavior.shortLabel).not.toBe("Waiting for runtime");
  });

  it("does not block an enrolled chat on a lagged ready replica", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.behavior).toMatchObject({
      kind: "ready",
      shortLabel: "Online",
    });
  });

  it("still accepts a legacy local readiness-stale verdict", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        source: "local-standard",
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
    );

    expect(state.admissionBlocker).toBe(state.behavior);
    expect(state.behavior).toMatchObject({
      layer: "runtime",
      kind: "waiting",
      reason: "readiness_stale",
      shortLabel: "Runtime unavailable",
    });
  });

  it("blocks a disconnected local host despite retained Ready state", () => {
    const state = projectDeploymentOperationalState(
      deployment({ source: "local-standard", dialSucceeded: false }),
    );
    expect(state.behavior.kind).toBe("ready");
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

  it("does not let replica lag block chat while database sync is catching up", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
      null,
      syncHealth({
        state: "syncing",
        pendingDagCount: 1,
        exhaustedFetchCount: 3,
      }),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.behavior.kind).toBe("ready");
    expect(state.summary).toBe(state.sync);
    expect(state.sync).toMatchObject({
      layer: "sync",
      kind: "syncing",
      shortLabel: "Syncing",
    });
  });

  it("keeps an enrolled agent online from last-known readiness when sync is healthy", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
      null,
      syncHealth(),
    );

    expect(state.admissionBlocker).toBeNull();
    expect(state.behavior).toMatchObject({
      kind: "ready",
      shortLabel: "Online",
    });
  });

  it("offers backend configuration only on the host that owns it", () => {
    const readinessStatus = {
      state: "unavailable" as const,
      behaviorId: "default",
      reason: "backend_not_configured" as const,
    };
    const remote = projectDeploymentOperationalState(
      deployment({ readinessStatus }),
    );
    const local = projectDeploymentOperationalState(
      deployment({ source: "local-standard", readinessStatus }),
    );

    expect(remote.behavior.action).toBeNull();
    expect(local.behavior).toMatchObject({
      layer: "inference",
      kind: "blocked",
      action: "configureInference",
    });
  });

  it("does not call an explicitly unavailable behavior online", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessStatus: {
          state: "unavailable",
          behaviorId: "default",
          reason: "behavior_disabled",
        },
      }),
    );

    expect(state.summary).toBe(state.behavior);
    expect(state.summary).toMatchObject({
      kind: "blocked",
      shortLabel: "Unavailable",
    });
  });
});
