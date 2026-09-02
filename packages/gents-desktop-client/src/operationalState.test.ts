import { describe, expect, it } from "vitest";

import type {
  BehaviorReadinessSourceView,
  BehaviorReadinessStatusView,
  DeploymentView,
} from "./types.js";
import {
  projectDeploymentOperationalState,
  projectP2PTransportOperationalStatus,
} from "./operationalState.js";

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
    pairingReady: true,
    chatSafe: true,
    routes: [],
    pairing: [],
    lastError: null,
    defaultBehaviorId: "default",
    runtime: null,
    behaviorReadiness: {
      source: readinessSource,
      activeGeneration: 1,
      routerGeneration: 1,
      defaultBehaviorId: "default",
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
    agentPrincipal: {} as DeploymentView["agentPrincipal"],
    ...deploymentOverrides,
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
      deployment({ pairingReady: false, chatSafe: false }),
    );

    expect(state.transport.kind).toBe("ready");
    expect(state.admissionBlocker).toBe(state.route);
    expect(state.route).toMatchObject({
      layer: "route",
      kind: "waiting",
      shortLabel: "Preparing",
    });
  });

  it("fails closed while enrollment and chat route snapshots disagree", () => {
    const state = projectDeploymentOperationalState(
      deployment({ pairingReady: false, chatSafe: true }),
    );

    expect(state.route.shortLabel).toBe("Preparing");
    expect(state.admissionBlocker).toBe(state.route);
  });

  it("uses the same stale-runtime reason for admission, recovery, and summary", () => {
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
      shortLabel: "Runtime stale",
      action: "reconnect",
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

describe("P2P transport operational state", () => {
  it("projects header density from the same typed status", () => {
    expect(projectP2PTransportOperationalStatus(null, 2, 0)).toMatchObject({
      kind: "waiting",
      shortLabel: "Checking sync",
    });
    expect(
      projectP2PTransportOperationalStatus(
        {
          status: "healthy",
          connectedPeerCount: 1,
          replicatorCount: 1,
          consecutiveFailures: 0,
          lastError: null,
          lastOkAt: null,
          lastFailureAt: null,
        },
        2,
        1,
      ),
    ).toMatchObject({ kind: "syncing", shortLabel: "Reconnecting 1/2" });
  });
});
