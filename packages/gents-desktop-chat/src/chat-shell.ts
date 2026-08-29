import type {
  BehaviorReadinessUnknownReasonView,
  BehaviorUnavailableReasonView,
  ConversationSummary,
  DesktopSessionSnapshot,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";

export type OptimisticPendingTurn = PendingTurnView & { sessionId: string };

export type RequestProgressPresentation = {
  label: string;
  animated: boolean;
};

export function requestProgressPresentation(
  lifecycleState?: string | null,
): RequestProgressPresentation | null {
  switch (lifecycleState) {
    case "pending":
      return { label: "Queued", animated: true };
    case "claimed":
      return { label: "Claimed", animated: true };
    case "processing":
      return { label: "Working", animated: true };
    case "inputRequired":
      return { label: "Waiting for input", animated: false };
    case "completed":
      return { label: "Completed", animated: false };
    case "failed":
      return { label: "Failed", animated: false };
    case "superseded":
      return { label: "Superseded", animated: false };
    case "dead":
      return { label: "Expired", animated: false };
    case "interrupted":
      return { label: "Interrupted", animated: false };
    default:
      return null;
  }
}

export type TurnState =
  | "waitingForClaim"
  | "streaming"
  | "completed"
  | "failed"
  | "superseded"
  | "interrupted";

export type ChatBlockedReason =
  | "clientOffline"
  | "agentNotSelected"
  | "routeNotReady"
  | "behaviorUnavailable"
  | "composerEmpty"
  | "submittingRequest"
  | "waitingForRequestObservation"
  | "conversationMissingFromSnapshot"
  | "awaitingTurnTerminality"
  | "inconsistentTurnObservation";

export type ChatWorkflowState =
  | { kind: "ready" }
  | { kind: "submittingRequest"; agentDid: string; sessionId?: string | null }
  | {
      kind: "awaitingObservation";
      agentDid: string;
      sessionId: string;
      requestId: string;
    }
  | {
      kind: "turnInProgress";
      agentDid: string;
      sessionId: string;
      requestId?: string | null;
      turnState: TurnState;
    }
  | {
      kind: "blocked";
      reason: ChatBlockedReason;
      turnState?: TurnState | null;
    };

export type SendStatus =
  | { kind: "ready" }
  | { kind: "disabled"; reason: ChatBlockedReason; hint: string };

export type BehaviorReadinessDecision =
  | { kind: "ready"; behaviorId: string; behaviorLabel: string }
  | {
      kind: "unavailable";
      behaviorId: string;
      behaviorLabel: string;
      reason: BehaviorUnavailableReasonView;
    }
  | {
      kind: "unknown";
      behaviorId: string | null;
      reason: BehaviorReadinessUnknownReasonView;
    };

type ProjectionInput = {
  clientAvailable: boolean;
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  draft: string;
  sending: boolean;
  session: DesktopSessionSnapshot | null;
  selectedConversation: ConversationSummary | null;
  localWorkflow: ChatWorkflowState;
  chatSafe: boolean;
  behaviorReadiness: BehaviorReadinessDecision;
};

export type ChatShellProjection = {
  workflow: ChatWorkflowState;
  sendStatus: SendStatus;
  nonEmptyContentSendStatus: SendStatus;
  turnState: TurnState | null;
  activeRequestId: string | null;
};

/**
 * Commit authoritative projection transitions back into the hook's local
 * workflow state.
 *
 * `projectChatShell` is intentionally pure, but the request id it tracks is
 * also used to select the next session snapshot. If a terminal projection is
 * rendered without retiring that local id, the following snapshot refresh can
 * pin the completed request again and resurrect the interrupt control.
 */
export function reconcileProjectedWorkflow(
  localWorkflow: ChatWorkflowState,
  projectedWorkflow: ChatWorkflowState,
): ChatWorkflowState {
  if (
    (localWorkflow.kind === "awaitingObservation" ||
      localWorkflow.kind === "turnInProgress") &&
    projectedWorkflow.kind === "ready"
  ) {
    return projectedWorkflow;
  }

  if (
    localWorkflow.kind === "awaitingObservation" &&
    projectedWorkflow.kind === "turnInProgress" &&
    localWorkflow.agentDid === projectedWorkflow.agentDid &&
    localWorkflow.sessionId === projectedWorkflow.sessionId &&
    localWorkflow.requestId === projectedWorkflow.requestId
  ) {
    return projectedWorkflow;
  }

  if (
    localWorkflow.kind === "turnInProgress" &&
    projectedWorkflow.kind === "turnInProgress" &&
    localWorkflow.agentDid === projectedWorkflow.agentDid &&
    localWorkflow.sessionId === projectedWorkflow.sessionId &&
    localWorkflow.requestId === projectedWorkflow.requestId &&
    localWorkflow.turnState !== projectedWorkflow.turnState
  ) {
    return projectedWorkflow;
  }

  return localWorkflow;
}

export function isTerminalTurnState(
  turnState?: string | null,
): turnState is "completed" | "failed" | "superseded" | "interrupted" {
  return (
    turnState === "completed" ||
    turnState === "failed" ||
    turnState === "superseded" ||
    turnState === "interrupted"
  );
}

function isTurnState(value?: string | null): value is TurnState {
  return (
    value === "waitingForClaim" ||
    value === "streaming" ||
    value === "completed" ||
    value === "failed" ||
    value === "superseded" ||
    value === "interrupted"
  );
}

function blocked(
  reason: ChatBlockedReason,
  turnState?: TurnState | null,
): ChatWorkflowState {
  return { kind: "blocked", reason, turnState };
}

function hintFor(reason: ChatBlockedReason, turnState?: TurnState | null) {
  switch (reason) {
    case "clientOffline":
      return "Client offline";
    case "agentNotSelected":
      return "Select an agent before sending";
    case "routeNotReady":
      return "Agent route is not ready";
    case "behaviorUnavailable":
      return "The selected behavior is unavailable";
    case "composerEmpty":
      return "Type a message to send";
    case "submittingRequest":
      return "Submitting request";
    case "waitingForRequestObservation":
      return "Waiting for request observation";
    case "conversationMissingFromSnapshot":
      return "Conversation missing from snapshot";
    case "awaitingTurnTerminality":
      if (turnState === "waitingForClaim") {
        return "Waiting for the active turn to start";
      }
      if (turnState === "streaming") {
        return "Turn still streaming";
      }
      return "Waiting for terminal turn reconciliation";
    case "inconsistentTurnObservation":
      return "Waiting for consistent turn observation";
  }
}

function behaviorReadinessHint(decision: Exclude<BehaviorReadinessDecision, { kind: "ready" }>) {
  if (decision.kind === "unknown") {
    switch (decision.reason) {
      case "readiness_missing":
        return "Runtime readiness is not available yet";
      case "readiness_malformed":
        return "Runtime readiness is invalid";
      case "readiness_version_unsupported":
        return "Runtime readiness uses an unsupported format";
      case "process_not_ready":
        return "Runtime is not ready";
      case "router_generation_stale":
        return "Runtime is still applying its latest configuration";
      case "behavior_not_assigned":
        return "Behavior is not assigned to this runtime";
    }
  }

  switch (decision.reason) {
    case "behavior_disabled":
      return `Behavior “${decision.behaviorLabel}” is disabled`;
    case "runtime_configuration_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid runtime configuration`;
    case "backend_not_configured":
      return `Behavior “${decision.behaviorLabel}” has no inference backend configured`;
    case "backend_disabled":
      return `Behavior “${decision.behaviorLabel}” has a disabled inference backend`;
    case "backend_temporarily_unavailable":
      return `Behavior “${decision.behaviorLabel}” is temporarily unavailable`;
    case "credentials_required":
      return `Behavior “${decision.behaviorLabel}” requires inference credentials`;
    case "inference_profile_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid inference profile`;
    case "tool_configuration_invalid":
      return `Behavior “${decision.behaviorLabel}” has an invalid tool configuration`;
    case "tool_surface_unavailable":
      return `Behavior “${decision.behaviorLabel}” cannot start its tool surface`;
    case "executor_start_failed":
      return `Behavior “${decision.behaviorLabel}” could not start`;
  }
}

export function projectChatShell(input: ProjectionInput): ChatShellProjection {
  const rawObservedTurnState =
    input.session?.turnState ?? input.selectedConversation?.turnState ?? null;
  const observedTurnState: TurnState | null = isTurnState(rawObservedTurnState)
    ? rawObservedTurnState
    : null;

  const trackedRequestId =
    (input.localWorkflow.kind === "awaitingObservation" ||
      input.localWorkflow.kind === "turnInProgress") &&
    input.localWorkflow.agentDid === input.selectedAgentDid &&
    (input.selectedSessionId === input.localWorkflow.sessionId ||
      input.session?.sessionId === input.localWorkflow.sessionId)
      ? (input.localWorkflow.requestId ?? null)
      : null;

  const observedLatestRequestId =
    input.session?.latestRequestId ??
    input.selectedConversation?.latestRequestId ??
    null;
  const pendingRequestId = input.session?.pendingTurn?.requestId ?? null;
  const activeRequestId =
    trackedRequestId ?? pendingRequestId ?? observedLatestRequestId;

  let workflow: ChatWorkflowState = input.localWorkflow;

  if (input.localWorkflow.kind === "awaitingObservation") {
    const selectedMatches =
      input.localWorkflow.agentDid === input.selectedAgentDid &&
      (input.selectedSessionId === input.localWorkflow.sessionId ||
        (input.session?.sessionId === input.localWorkflow.sessionId &&
          input.session.agentDid === input.localWorkflow.agentDid));
    const requestObserved =
      observedLatestRequestId === input.localWorkflow.requestId ||
      pendingRequestId === input.localWorkflow.requestId;

    if (selectedMatches) {
      if (!requestObserved) {
        workflow = input.localWorkflow;
      } else if (observedTurnState && !isTerminalTurnState(observedTurnState)) {
        workflow = {
          kind: "turnInProgress",
          agentDid: input.localWorkflow.agentDid,
          sessionId: input.localWorkflow.sessionId,
          requestId: input.localWorkflow.requestId,
          turnState: observedTurnState,
        };
      } else if (observedTurnState && isTerminalTurnState(observedTurnState)) {
        workflow = { kind: "ready" };
      } else {
        workflow = blocked("inconsistentTurnObservation");
      }
    } else {
      workflow = { kind: "ready" };
    }
  } else if (input.localWorkflow.kind === "turnInProgress") {
    const selectedMatches =
      input.localWorkflow.agentDid === input.selectedAgentDid &&
      (input.selectedSessionId === input.localWorkflow.sessionId ||
        (input.session?.sessionId === input.localWorkflow.sessionId &&
          input.session.agentDid === input.localWorkflow.agentDid));

    if (!selectedMatches) {
      workflow = { kind: "ready" };
    } else if (
      observedTurnState &&
      activeRequestId === (input.localWorkflow.requestId ?? activeRequestId)
    ) {
      workflow = isTerminalTurnState(observedTurnState)
        ? { kind: "ready" }
        : {
            kind: "turnInProgress",
            agentDid: input.localWorkflow.agentDid,
            sessionId: input.localWorkflow.sessionId,
            requestId: input.localWorkflow.requestId,
            turnState: observedTurnState,
          };
    } else if (
      !observedTurnState &&
      activeRequestId === input.localWorkflow.requestId
    ) {
      workflow = blocked("inconsistentTurnObservation");
    }
  } else if (input.localWorkflow.kind !== "submittingRequest") {
    if (!input.clientAvailable) {
      workflow = blocked("clientOffline");
    } else if (!input.selectedAgentDid) {
      workflow = blocked("agentNotSelected");
    } else if (input.selectedSessionId) {
      if (!input.session && !input.selectedConversation) {
        workflow = blocked("conversationMissingFromSnapshot");
      } else if (observedTurnState && !isTerminalTurnState(observedTurnState)) {
        workflow = {
          kind: "turnInProgress",
          agentDid: input.selectedAgentDid,
          sessionId: input.selectedSessionId,
          requestId: activeRequestId,
          turnState: observedTurnState,
        };
      } else if (!observedTurnState && activeRequestId) {
        workflow = blocked("inconsistentTurnObservation");
      } else {
        workflow = { kind: "ready" };
      }
    } else {
      workflow = { kind: "ready" };
    }
  }

  function sendStatusFor(composerEmpty: boolean): SendStatus {
    if (!input.clientAvailable) {
      return {
        kind: "disabled",
        reason: "clientOffline",
        hint: hintFor("clientOffline"),
      };
    }
    if (!input.selectedAgentDid) {
      return {
        kind: "disabled",
        reason: "agentNotSelected",
        hint: hintFor("agentNotSelected"),
      };
    }
    if (!input.chatSafe) {
      return {
        kind: "disabled",
        reason: "routeNotReady",
        hint: hintFor("routeNotReady"),
      };
    }
    if (input.behaviorReadiness.kind !== "ready") {
      return {
        kind: "disabled",
        reason: "behaviorUnavailable",
        hint: behaviorReadinessHint(input.behaviorReadiness),
      };
    }
    if (composerEmpty) {
      return {
        kind: "disabled",
        reason: "composerEmpty",
        hint: hintFor("composerEmpty"),
      };
    }
    if (input.sending || input.localWorkflow.kind === "submittingRequest") {
      return {
        kind: "disabled",
        reason: "submittingRequest",
        hint: hintFor("submittingRequest"),
      };
    }
    if (
      workflow.kind === "awaitingObservation" &&
      activeRequestId === workflow.requestId &&
      pendingRequestId !== workflow.requestId &&
      observedLatestRequestId !== workflow.requestId
    ) {
      return {
        kind: "disabled",
        reason: "waitingForRequestObservation",
        hint: hintFor("waitingForRequestObservation"),
      };
    }
    if (workflow.kind === "blocked") {
      return {
        kind: "disabled",
        reason: workflow.reason,
        hint: hintFor(workflow.reason, workflow.turnState),
      };
    }
    if (
      workflow.kind === "turnInProgress" &&
      !isTerminalTurnState(workflow.turnState)
    ) {
      return {
        kind: "disabled",
        reason: "awaitingTurnTerminality",
        hint: hintFor("awaitingTurnTerminality", workflow.turnState),
      };
    }
    return { kind: "ready" };
  }

  const sendStatus = sendStatusFor(!input.draft.trim());
  const nonEmptyContentSendStatus = sendStatusFor(false);

  return {
    workflow,
    sendStatus,
    nonEmptyContentSendStatus,
    turnState: observedTurnState,
    activeRequestId,
  };
}
