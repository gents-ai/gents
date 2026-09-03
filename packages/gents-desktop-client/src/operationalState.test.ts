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
        displayName: "Default",
        enabled: true,
        isDefault: true,
      },
    ],
    behaviorEnvironments: [],
    inferenceBackends: [],
    inferenceProfiles: [],
    toolSelections: [],
    toolServiceRegistries: [],
    skills: [],
    tasks: [],
    schedules: [],
    eventTriggers: [],
    conversations: [],
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
      shortLabel: "Preparing",
    });
  });

  it("keeps stale runtime readiness attributed to the runtime", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
    );

    expect(state.admissionBlocker).toBe(state.behavior);
    expect(state.summary).toBe(state.behavior);
    expect(state.behavior).toMatchObject({
      layer: "runtime",
      kind: "waiting",
      reason: "readiness_stale",
      shortLabel: "Runtime unavailable",
      action: null,
    });
    expect(state.sync).toMatchObject({
      layer: "sync",
      reason: "sync_not_observed",
      shortLabel: "Checking sync",
    });
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

  it("keeps database work separate from stale runtime readiness", () => {
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

    expect(state.admissionBlocker).toBe(state.behavior);
    expect(state.summary).toBe(state.behavior);
    expect(state.behavior).toMatchObject({
      layer: "runtime",
      reason: "readiness_stale",
      shortLabel: "Runtime unavailable",
    });
    expect(state.sync).toMatchObject({
      layer: "sync",
      kind: "syncing",
      reason: "syncing",
      shortLabel: "Syncing",
    });
  });

  it("keeps stale runtime reporting distinct when database sync is healthy", () => {
    const state = projectDeploymentOperationalState(
      deployment({
        readinessSource: { state: "unknown", reason: "readiness_stale" },
      }),
      null,
      syncHealth(),
    );

    expect(state.admissionBlocker).toBe(state.behavior);
    expect(state.behavior).toMatchObject({
      layer: "runtime",
      shortLabel: "Runtime unavailable",
      action: null,
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
