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
  nodeDid: "did:test:agent",
  found: true,
  error: null,
  reread: false,
};

function deployment(overrides: Partial<DeploymentView> = {}): DeploymentView {
  return {
    nodeDid: "did:test:agent",
    dialSucceeded: true,
    runtimeConnection: "connected",
    chatSafe: true,
    source: "enrolled",
    lastError: null,
    runtime: null,
    node: {
      nodeDid: "did:test:agent",
      defaultAgentDid: "default",
    },
    agents: [
      {
        agentDid: "default",
        displayName: "Default",
        enabled: true,
        isDefault: true,
      },
    ],
    nodeReadiness: {
      source: { state: "current" },
      activeGeneration: 1,
      routerGeneration: 1,
      updatedAt: "2026-09-02T00:00:00Z",
      agents: [{ state: "ready", agentDid: "default" }],
    },
    ...overrides,
  } as DeploymentView;
}

function session(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    nodeDid: "did:test:agent",
    timelineItems: [],
    ...overrides,
  } as DesktopSessionSnapshot;
}

function project(
  overrides: Partial<Parameters<typeof projectSessionLoadingStatus>[0]> = {},
) {
  return projectSessionLoadingStatus({
    selectedSessionId: "session-1",
    selectedNodeDid: "did:test:agent",
    session: session(),
    sessionLoad: loaded,
    operationalState: projectDeploymentOperationalState(deployment()),
    ...overrides,
  });
}

describe("startup loading projection", () => {
  it("reports only lifecycle-owned startup work", () => {
    expect(projectStartupLoadingStatus("checking-managed-server", true)).toMatchObject({
      currentLabel: "Starting your local node",
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
        nodeDid: "did:test:other",
        found: null,
        error: null,
        reread: true,
      },
    });
    expect(status).toMatchObject({ layer: "localDatabase", phase: "loading" });
  });

  it("does not block a connected runtime while pairing is preparing", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({ source: "local-standard", chatSafe: false }),
        ),
      }),
    ).toBeNull();
  });

  it("treats an absent or hidden session as an empty result", () => {
    expect(
      project({
        session: null,
        sessionLoad: { ...loaded, found: false },
      }),
    ).toBeNull();
  });

  it("does not block a new enrolled chat solely because its readiness observation is old", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({
            nodeReadiness: {
              ...deployment().nodeReadiness,
              source: { state: "current" },
              updatedAt: "2000-01-01T00:00:00Z",
            },
          }),
          null,
          {
            state: "syncing",
            lastError: null,
            connectedPeerCount: 1,
            activationErrorCount: 0,
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
          nodeReadiness: {
            ...deployment().nodeReadiness,
            agents: [
              {
                state: "unavailable",
                agentDid: "default",
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

  it("does not mislabel a non-inference agent failure", () => {
    expect(
      project({
        operationalState: projectDeploymentOperationalState(
          deployment({
            nodeReadiness: {
              ...deployment().nodeReadiness,
              agents: [
                {
                  state: "unavailable",
                  agentDid: "default",
                  reason: "agent_disabled",
                },
              ],
            },
          }),
        ),
      }),
    ).toMatchObject({
      layer: "runtime",
      title: "This agent is unavailable",
      action: null,
    });
  });

  it("renders no wait when the exact session and agent are ready", () => {
    expect(project()).toBeNull();
  });
});

describe("background projection refresh", () => {
  it("keeps a visible conversation quiet while a newer projection is pending", () => {
    expect(
      project({
        sessionLoad: {
          ...loaded,
          phase: "pending",
          found: null,
          error: "refresh is pending",
        },
      }),
    ).toBeNull();
  });

  it("keeps initial projection loading visible while a re-read follows", () => {
    expect(
      project({
        session: null,
        sessionLoad: {
          ...loaded,
          phase: "pending",
          found: null,
          error: "refresh is pending",
          reread: true,
        },
      }),
    ).toMatchObject({ title: "Waiting for session update", action: null });
  });

  it("offers a retry once no re-read follows a pending projection", () => {
    expect(
      project({
        session: null,
        sessionLoad: {
          ...loaded,
          phase: "pending",
          found: null,
          error: "refresh is pending",
          reread: false,
        },
      }),
    ).toMatchObject({ title: "Waiting for session update", action: "retryLocal" });
  });
});
