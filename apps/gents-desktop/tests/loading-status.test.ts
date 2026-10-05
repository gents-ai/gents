import { describe, expect, it } from "vitest";

import type {
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { projectDeploymentOperationalState } from "@source-inc/gents-desktop-client";
import {
  projectSessionLoadingStatus,
  projectStartupLoadingStatus,
  type SessionLoadState,
} from "../src/lib/loadingStatus";

const loaded: SessionLoadState = {
  phase: "loaded",
  sessionId: "session-1",
  agentDid: "did:test:agent",
  found: true,
  error: null,
};

function deployment(overrides: Partial<DeploymentView> = {}): DeploymentView {
  return {
    agentDid: "did:test:agent",
    dialSucceeded: true,
    chatSafe: true,
    source: "enrolled",
    lastError: null,
    runtime: null,
    agentPrincipal: {
      agentDid: "did:test:agent",
      defaultBehaviorId: "default",
    },
    behaviors: [
      {
        behaviorId: "default",
        displayName: "Default",
        enabled: true,
        isDefault: true,
      },
    ],
    behaviorReadiness: {
      source: { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      behaviors: [{ state: "ready", behaviorId: "default" }],
    },
    ...overrides,
  } as DeploymentView;
}

function session(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:test:agent",
    timelineItems: [],
    ...overrides,
  } as DesktopSessionSnapshot;
}

function project(
  overrides: Partial<Parameters<typeof projectSessionLoadingStatus>[0]> = {},
) {
  return projectSessionLoadingStatus({
    selectedSessionId: "session-1",
    selectedAgentDid: "did:test:agent",
    session: session(),
    sessionLoad: loaded,
    operationalState: projectDeploymentOperationalState(deployment()),
    ...overrides,
  });
}

describe("startup loading projection", () => {
  it("reports only lifecycle-owned startup work", () => {
    expect(projectStartupLoadingStatus("checking-managed-server", true)).toMatchObject({
      currentLabel: "Checking the background agent",
      managedServerState: "active",
      connectionState: "pending",
      clientState: "pending",
    });
    expect(projectStartupLoadingStatus("loading-configuration")).toMatchObject({
      currentLabel: "Reading saved connections",
      connectionState: "active",
      clientState: "pending",
    });
    expect(projectStartupLoadingStatus("managed-server-error", true)).toMatchObject({
      failed: true,
      managedServerState: "error",
      connectionState: "pending",
    });
    expect(projectStartupLoadingStatus("starting-client")).toMatchObject({
      currentLabel: "Starting the secure client",
      connectionState: "complete",
      clientState: "active",
    });
  });
});

describe("session loading projection", () => {
  it("distinguishes the exact local database read and its failure", () => {
    expect(
      project({
        session: null,
        sessionLoad: { ...loaded, phase: "loading", found: null },
      }),
    ).toMatchObject({ layer: "localDatabase", phase: "loading", action: null });

    expect(
      project({
        session: null,
        sessionLoad: {
          ...loaded,
          phase: "failed",
          found: null,
          error: "database unavailable",
        },
      }),
    ).toMatchObject({
      layer: "localDatabase",
      phase: "failed",
      detail: "database unavailable",
      action: "retryLocal",
    });
  });

  it("keeps a failed refresh visible while retrying over an existing transcript", () => {
    expect(
      project({
        sessionLoad: { ...loaded, phase: "loading", error: "read timed out" },
      }),
    ).toMatchObject({
      title: "Retrying conversation update",
      detail: "The last update failed. Displayed messages may be out of date.",
      action: null,
    });
  });

  it("ignores reordered load state and session state from another target", () => {
    const status = project({
      session: session({ sessionId: "session-old" }),
      sessionLoad: {
        phase: "loading",
        sessionId: "session-old",
        agentDid: "did:test:other",
        found: null,
        error: null,
      },
    });
    expect(status).toMatchObject({ layer: "localDatabase", phase: "loading" });
  });

  it("attributes signed hydration progress to session sync using covered counts", () => {
    expect(
      project({
        session: session({
          hydration: {
            sessionId: "session-1",
            agentDid: "did:test:agent",
            phase: "serving",
            mergedCount: 124,
            coveredCount: 47,
            servedCount: 47,
          },
        }),
      }),
    ).toMatchObject({
      layer: "sessionSync",
      phase: "loading",
      detail: "Fetching session history · 47 of 47",
    });
  });

  it("presents a session this client cannot read without a retry", () => {
    expect(
      project({
        session: session({
          hydration: {
            sessionId: "session-1",
            agentDid: "did:test:agent",
            phase: "unreadable",
            mergedCount: 0,
            coveredCount: 0,
            servedCount: null,
            detail:
              "Started by the agent itself and owned by its node, so this client cannot read it.",
          },
        }),
      }),
    ).toEqual({
      layer: "sessionSync",
      phase: "blocked",
      title: "Not readable from this client",
      detail:
        "Started by the agent itself and owned by its node, so this client cannot read it.",
      action: null,
    });
  });

  it("shows the signed refusal reason on a retryable sync failure", () => {
    expect(
      project({
        session: session({
          hydration: {
            sessionId: "session-1",
            agentDid: "did:test:agent",
            phase: "failed",
            mergedCount: 0,
            coveredCount: 0,
            servedCount: null,
            detail: "peer pairing does not match requester and agent",
          },
        }),
      }),
    ).toMatchObject({
      phase: "failed",
      detail:
        "The agent refused the session history: peer pairing does not match requester and agent.",
      action: "retryHydration",
    });
  });

  it("waits for automatic P2P recovery during approved session hydration", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({ dialSucceeded: false }),
        ),
        session: session({
          hydration: {
            sessionId: "session-1",
            agentDid: "did:test:agent",
            phase: "requested",
            mergedCount: 0,
            coveredCount: 0,
            servedCount: null,
          },
        }),
      }),
    ).toMatchObject({ layer: "p2p", phase: "loading", action: null });
  });

  it("waits for automatic route preparation without requiring reconnect", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({ source: "local-standard", chatSafe: false }),
        ),
      }),
    ).toMatchObject({ layer: "p2p", phase: "loading", action: null });
  });

  it("does not require reconnect while an approved session snapshot arrives", () => {
    expect(project({ session: null })).toMatchObject({
      layer: "sessionSync",
      action: null,
    });
    expect(project({ session: null, operationalState: null })).toMatchObject({
      action: "retryLocal",
    });
  });

  it("does not block a new enrolled chat on a lagged ready replica", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({
            behaviorReadiness: {
              ...deployment().behaviorReadiness,
              source: { state: "unknown", reason: "readiness_stale" },
            },
          }),
          null,
          {
            state: "syncing",
            lastError: null,
            connectedPeerCount: 1,
            pendingDagCount: 1,
            persistedPendingDagCount: 1,
            pushRetryMarkerCount: 0,
            exhaustedFetchCount: 1,
            quarantinedDagCount: 0,
          },
        ),
      }),
    ).toBeNull();
  });

  it("offers inference configuration only for an explicit local backend failure", () => {
    const unavailable = (source: string) =>
      projectDeploymentOperationalState(
        deployment({
          source,
          behaviorReadiness: {
            ...deployment().behaviorReadiness,
            behaviors: [
              {
                state: "unavailable",
                behaviorId: "default",
                reason: "backend_not_configured",
              },
            ],
          },
        }),
      );
    expect(
      project({
        operationalState: unavailable("local-standard"),
      }),
    ).toMatchObject({ layer: "inference", action: "configureInference" });
    expect(project({ operationalState: unavailable("enrollment") })).toMatchObject({
      layer: "inference",
      action: null,
    });
  });

  it("does not mislabel a non-inference behavior failure", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({
            behaviorReadiness: {
              ...deployment().behaviorReadiness,
              behaviors: [
                {
                  state: "unavailable",
                  behaviorId: "default",
                  reason: "behavior_disabled",
                },
              ],
            },
          }),
        ),
      }),
    ).toMatchObject({
      layer: "runtime",
      title: "This behavior is unavailable",
      action: null,
    });
  });

  it("renders no wait when the exact session and behavior are ready", () => {
    expect(project()).toBeNull();
  });
});
