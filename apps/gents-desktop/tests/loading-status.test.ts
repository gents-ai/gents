import { describe, expect, it } from "vitest";

import type { BehaviorReadinessDecision } from "@source-inc/gents-desktop-chat";
import type {
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import {
  projectConversationLoadingStatus,
  projectStartupLoadingStatus,
  type SessionLoadState,
} from "../src/lib/loadingStatus";

const ready: BehaviorReadinessDecision = {
  kind: "ready",
  behaviorId: "default",
  behaviorLabel: "Default",
};

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
    pairingReady: true,
    source: "enrolled",
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
  overrides: Partial<Parameters<typeof projectConversationLoadingStatus>[0]> = {},
) {
  return projectConversationLoadingStatus({
    selectedSessionId: "session-1",
    selectedAgentDid: "did:test:agent",
    session: session(),
    sessionLoad: loaded,
    deployment: deployment(),
    behaviorReadiness: ready,
    ...overrides,
  });
}

describe("startup loading projection", () => {
  it("reports only lifecycle-owned startup work", () => {
    expect(projectStartupLoadingStatus("checking-managed-server", true)).toMatchObject({
      currentLabel: "Checking the hosted agent",
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

describe("conversation loading projection", () => {
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

  it("attributes requested hydration to P2P when the enrolled agent is offline", () => {
    expect(
      project({
        deployment: deployment({ dialSucceeded: false }),
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
    ).toMatchObject({ layer: "p2p", phase: "blocked", action: "reconnect" });
  });

  it("attributes missing or stale runtime readiness to runtime recovery", () => {
    expect(
      project({
        behaviorReadiness: {
          kind: "unknown",
          behaviorId: "default",
          reason: "readiness_stale",
        },
      }),
    ).toMatchObject({ layer: "runtime", action: "reconnect" });
  });

  it("offers inference configuration only for an explicit local backend failure", () => {
    const unavailable: BehaviorReadinessDecision = {
      kind: "unavailable",
      behaviorId: "default",
      behaviorLabel: "Default",
      reason: "backend_not_configured",
    };
    expect(
      project({
        deployment: deployment({ source: "local-standard" }),
        behaviorReadiness: unavailable,
      }),
    ).toMatchObject({ layer: "inference", action: "configureInference" });
    expect(project({ behaviorReadiness: unavailable })).toMatchObject({
      layer: "inference",
      action: null,
    });
  });

  it("does not mislabel a non-inference behavior failure", () => {
    expect(
      project({
        behaviorReadiness: {
          kind: "unavailable",
          behaviorId: "default",
          behaviorLabel: "Default",
          reason: "behavior_disabled",
        },
      }),
    ).toMatchObject({
      layer: "runtime",
      title: "This behavior is unavailable",
      action: null,
    });
  });

  it("renders no wait when the exact conversation and behavior are ready", () => {
    expect(project()).toBeNull();
  });
});
