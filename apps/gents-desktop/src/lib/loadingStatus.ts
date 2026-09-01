import type { BehaviorReadinessDecision } from "@source-inc/gents-desktop-chat";
import { behaviorReadinessHint } from "@source-inc/gents-desktop-chat";
import type {
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { isLocalRuntimeSource } from "@source-inc/gents-desktop-fleet";

import {
  behaviorReadinessCanConfigureInference,
  behaviorReadinessCanReconnect,
  behaviorReadinessIsInferenceFailure,
} from "./behaviorReadiness";
import { sessionHydrationLabel, visibleSessionHydration } from "./sessionHydration";

export type DesktopStartupPhase =
  | "checking-managed-server"
  | "loading-configuration"
  | "starting-client"
  | "managed-server-error"
  | "configuration-error"
  | "client-error"
  | "ready";

export type LoadingStepState = "active" | "complete" | "pending" | "error";

export type StartupLoadingStatus = {
  failed: boolean;
  title: string;
  currentLabel: string;
  managedServerState: LoadingStepState | null;
  connectionState: LoadingStepState;
  clientState: LoadingStepState;
};

export function projectStartupLoadingStatus(
  phase: Exclude<DesktopStartupPhase, "ready">,
  managedServerSupported = false,
): StartupLoadingStatus {
  switch (phase) {
    case "checking-managed-server":
      return {
        failed: false,
        title: "Bringing Gents online",
        currentLabel: "Checking the hosted agent",
        managedServerState: "active",
        connectionState: "pending",
        clientState: "pending",
      };
    case "loading-configuration":
      return {
        failed: false,
        title: "Bringing Gents online",
        currentLabel: "Reading saved connections",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "active",
        clientState: "pending",
      };
    case "starting-client":
      return {
        failed: false,
        title: "Bringing Gents online",
        currentLabel: "Starting the secure client",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "complete",
        clientState: "active",
      };
    case "managed-server-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "The hosted agent could not be restored",
        managedServerState: "error",
        connectionState: "pending",
        clientState: "pending",
      };
    case "configuration-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "Saved connections could not be read",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "error",
        clientState: "pending",
      };
    case "client-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "The secure client could not start",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "complete",
        clientState: "error",
      };
  }
}

export type SessionLoadState = {
  phase: "idle" | "loading" | "loaded" | "failed";
  sessionId: string | null;
  agentDid: string | null;
  found: boolean | null;
  error: string | null;
};

export type ConversationLoadingLayer =
  "localDatabase" | "p2p" | "sessionSync" | "runtime" | "inference";

export type ConversationLoadingAction =
  "retryLocal" | "retryHydration" | "reconnect" | "configureInference";

export type ConversationLoadingStatus = {
  layer: ConversationLoadingLayer;
  phase: "loading" | "blocked" | "failed";
  title: string;
  detail: string;
  action: ConversationLoadingAction | null;
};

type ConversationLoadingInput = {
  selectedSessionId: string | null;
  selectedAgentDid: string | null;
  session: DesktopSessionSnapshot | null;
  sessionLoad: SessionLoadState;
  deployment: DeploymentView | null;
  behaviorReadiness: BehaviorReadinessDecision;
};

/**
 * Sole projection for user-visible conversation waits. It does not invent
 * progress: every state comes from an in-flight local read, signed hydration
 * progress, P2P route facts, or runtime-authored behavior readiness.
 */
export function projectConversationLoadingStatus({
  selectedSessionId,
  selectedAgentDid,
  session,
  sessionLoad,
  deployment,
  behaviorReadiness,
}: ConversationLoadingInput): ConversationLoadingStatus | null {
  if (!selectedSessionId) return null;

  const sessionMatches =
    session?.sessionId === selectedSessionId &&
    (!selectedAgentDid || !session.agentDid || session.agentDid === selectedAgentDid);
  const loadMatches =
    sessionLoad.sessionId === selectedSessionId &&
    (!selectedAgentDid ||
      !sessionLoad.agentDid ||
      sessionLoad.agentDid === selectedAgentDid);

  if (loadMatches && sessionLoad.phase === "loading" && !sessionMatches) {
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Loading conversation",
      detail: "Reading saved messages from the local database.",
      action: null,
    };
  }

  if (loadMatches && sessionLoad.phase === "failed") {
    return {
      layer: "localDatabase",
      phase: "failed",
      title: "Couldn’t load this conversation",
      detail: sessionLoad.error ?? "The local database read failed.",
      action: "retryLocal",
    };
  }

  if (!loadMatches && !sessionMatches) {
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Opening conversation",
      detail: "Preparing the exact local session read.",
      action: null,
    };
  }

  const hydration = visibleSessionHydration(
    sessionMatches ? session?.hydration : null,
    selectedSessionId,
    selectedAgentDid,
  );
  if (hydration?.phase === "failed") {
    return {
      layer: "sessionSync",
      phase: "failed",
      title: "Conversation sync failed",
      detail: "The agent could not finish sending the requested session history.",
      action: "retryHydration",
    };
  }
  if (hydration?.phase === "serving") {
    return {
      layer: "sessionSync",
      phase: "loading",
      title: "Syncing conversation history",
      detail: sessionHydrationLabel(hydration),
      action: null,
    };
  }
  if (hydration?.phase === "requested") {
    if (deployment && !deployment.dialSucceeded) {
      return disconnectedStatus();
    }
    if (deployment && !deployment.pairingReady) {
      return routePreparingStatus();
    }
    return {
      layer: "sessionSync",
      phase: "loading",
      title: "Requesting conversation history",
      detail: "Waiting for the enrolled agent to begin the secure transfer.",
      action: null,
    };
  }

  if (!sessionMatches) {
    if (deployment && !deployment.dialSucceeded) return disconnectedStatus();
    if (deployment && !deployment.pairingReady) return routePreparingStatus();
    return {
      layer: "sessionSync",
      phase: "blocked",
      title: "Waiting for conversation history",
      detail:
        "No local messages were found yet. Waiting for the enrolled agent’s session projection.",
      action: deployment ? "reconnect" : "retryLocal",
    };
  }

  if (behaviorReadiness.kind !== "ready") {
    const inferenceFailure = behaviorReadinessIsInferenceFailure(behaviorReadiness);
    const canConfigure =
      isLocalRuntimeSource(deployment?.source) &&
      behaviorReadinessCanConfigureInference(behaviorReadiness);
    const canReconnect = behaviorReadinessCanReconnect(behaviorReadiness);
    return {
      layer: inferenceFailure ? "inference" : "runtime",
      phase: "blocked",
      title: inferenceFailure
        ? "Inference is unavailable"
        : behaviorReadiness.kind === "unavailable"
          ? "This behavior is unavailable"
          : "Waiting for the agent runtime",
      detail: behaviorReadinessHint(behaviorReadiness),
      action: canConfigure ? "configureInference" : canReconnect ? "reconnect" : null,
    };
  }

  return null;
}

function disconnectedStatus(): ConversationLoadingStatus {
  return {
    layer: "p2p",
    phase: "blocked",
    title: "Agent connection is offline",
    detail:
      "Reconnect the secure P2P connection to continue loading this conversation.",
    action: "reconnect",
  };
}

function routePreparingStatus(): ConversationLoadingStatus {
  return {
    layer: "p2p",
    phase: "loading",
    title: "Preparing the secure route",
    detail:
      "The agent is connected, but its signed conversation route is not ready yet.",
    action: "reconnect",
  };
}
