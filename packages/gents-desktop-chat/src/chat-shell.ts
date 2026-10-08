import type {
  SessionSummary,
  DeploymentOperationalState,
  DesktopSessionSnapshot,
  OperationalStatus,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import {
  isTerminalTurnState,
  projectClientOperationalStatus,
  projectRouteOperationalStatus,
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
    case "workspaceBindingPending":
    case "pending":
      return { label: "Queued", animated: true };
    case "claimed":
      return { label: "Claimed", animated: true };
    case "processing":
      return { label: "Working", animated: true };
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
  | "running"
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
  | "sessionMissingFromSnapshot"
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

/** `queue`: the session's turn is not terminal, so a message sent now waits
    behind it and joins the turn that claims it (Lean `SendDecision.queue`). */
export type SendStatus =
  | { kind: "ready" }
  | { kind: "queue"; turnState: TurnState; hint: string }
  | { kind: "disabled"; reason: ChatBlockedReason; hint: string };

export type ChatActivityStatus = {
  kind: "working" | "waiting" | "syncing" | "blocked";
  label: string;
  detail: string;
  animated: boolean;
};

type ProjectionInput = {
  clientAvailable: boolean;
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  sending: boolean;
  /** The workflow reads these alone; transcript content never decides it. */
  session: Pick<
    DesktopSessionSnapshot,
    "sessionId" | "agentDid" | "turnState" | "latestRequestId" | "pendingTurn"
  > | null;
  selectedSessionSummary: SessionSummary | null;
  localWorkflow: ChatWorkflowState;
  operationalState: DeploymentOperationalState | null;
};

export type ChatShellProjection = {
  workflow: ChatWorkflowState;
  /** Whether a message with content could be sent now. The composer adds
      its own emptiness, so this does not change with each keystroke. */
  nonEmptyContentSendStatus: SendStatus;
  activityStatus: ChatActivityStatus | null;
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

  /* a submission observed as the turn, or queued behind the turn it then
     tracks */
  if (
    localWorkflow.kind === "awaitingObservation" &&
    projectedWorkflow.kind === "turnInProgress" &&
    localWorkflow.agentDid === projectedWorkflow.agentDid &&
    localWorkflow.sessionId === projectedWorkflow.sessionId
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

function isTurnState(value?: string | null): value is TurnState {
  return (
    value === "waitingForClaim" ||
    value === "running" ||
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

function queueHint(turnState: TurnState) {
  return turnState === "waitingForClaim"
    ? "Queued behind the message waiting to start"
    : "Queued behind the running turn";
}

function hintFor(reason: ChatBlockedReason) {
  switch (reason) {
    case "clientOffline":
      return "Secure client is not running";
    case "agentNotSelected":
      return "Select an agent before sending";
    case "routeNotReady":
      return "Secure route to the agent is not ready";
    case "behaviorUnavailable":
      return "The selected behavior is unavailable";
    case "composerEmpty":
      return "Type a message to send";
    case "submittingRequest":
      return "Submitting request";
    case "waitingForRequestObservation":
      return "Waiting for request observation";
    case "sessionMissingFromSnapshot":
      return "Session missing from snapshot";
    case "inconsistentTurnObservation":
      return "Waiting for consistent turn observation";
  }
}

function chatActivity(status: OperationalStatus): ChatActivityStatus | null {
  if (status.kind === "ready") return null;
  return {
    kind: status.kind,
    label: status.label,
    detail: status.detail,
    animated: status.animated,
  };
}

function activityStatusFor(
  sendStatus: SendStatus,
  admissionStatus: OperationalStatus | null,
): ChatActivityStatus | null {
  if (sendStatus.kind === "ready") return null;
  if (sendStatus.kind === "queue") {
    return sendStatus.turnState === "waitingForClaim"
      ? {
          kind: "waiting",
          label: "Waiting for the agent…",
          detail:
            "The agent has not started yet. Messages you send now wait behind it.",
          animated: true,
        }
      : {
          kind: "working",
          label: "Agent is working…",
          detail: "Messages you send now wait until this turn finishes.",
          animated: true,
        };
  }

  switch (sendStatus.reason) {
    case "composerEmpty":
      return null;
    case "clientOffline":
    case "agentNotSelected":
    case "routeNotReady":
    case "behaviorUnavailable":
      return admissionStatus ? chatActivity(admissionStatus) : null;
    case "submittingRequest":
      return {
        kind: "syncing",
        label: "Sending message…",
        detail: "Creating the request in the secure conversation.",
        animated: true,
      };
    case "waitingForRequestObservation":
      return {
        kind: "syncing",
        label: "Syncing message…",
        detail:
          "Your request was created; waiting for it to appear in the shared conversation.",
        animated: true,
      };
    case "sessionMissingFromSnapshot":
      return {
        kind: "syncing",
        label: "Loading session…",
        detail:
          "Reading local session state before another message can be sent.",
        animated: true,
      };
    case "inconsistentTurnObservation":
      return {
        kind: "syncing",
        label: "Syncing turn status…",
        detail: "Waiting for local and replicated turn records to agree.",
        animated: true,
      };
  }
}

export function projectChatShell(input: ProjectionInput): ChatShellProjection {
  const clientStatus = projectClientOperationalStatus(
    input.clientAvailable,
    Boolean(input.selectedAgentDid),
  );
  const deploymentStatus = input.selectedAgentDid
    ? (input.operationalState?.admissionBlocker ??
      (input.operationalState ? null : projectRouteOperationalStatus(false)))
    : null;
  const admissionStatus = clientStatus ?? deploymentStatus;
  const rawObservedTurnState =
    input.session?.turnState ?? input.selectedSessionSummary?.turnState ?? null;
  const observedTurnState: TurnState | null = isTurnState(rawObservedTurnState)
    ? rawObservedTurnState
    : null;

  const queuedRequestIds = new Set(
    (input.session?.queuedTurns ?? []).map((turn) => turn.requestId),
  );
  const foldedRequestIds = new Set(
    (input.session?.foldedInputs ?? []).map((folded) => folded.requestId),
  );
  /* Lean `trackedRequestForFrontend`: a submission observed as queued or
     folded is not the session's turn */
  const trackedRequestId =
    (input.localWorkflow.kind === "awaitingObservation" ||
      input.localWorkflow.kind === "turnInProgress") &&
    input.localWorkflow.agentDid === input.selectedAgentDid &&
    (input.selectedSessionId === input.localWorkflow.sessionId ||
      input.session?.sessionId === input.localWorkflow.sessionId) &&
    !(
      input.localWorkflow.kind === "awaitingObservation" &&
      (queuedRequestIds.has(input.localWorkflow.requestId) ||
        foldedRequestIds.has(input.localWorkflow.requestId))
    )
      ? (input.localWorkflow.requestId ?? null)
      : null;

  const observedLatestRequestId =
    input.session?.latestRequestId ??
    input.selectedSessionSummary?.latestRequestId ??
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
    const observedAsQueued =
      queuedRequestIds.has(input.localWorkflow.requestId) ||
      foldedRequestIds.has(input.localWorkflow.requestId);
    const requestObserved =
      observedLatestRequestId === input.localWorkflow.requestId ||
      pendingRequestId === input.localWorkflow.requestId ||
      observedAsQueued;

    if (selectedMatches) {
      if (!requestObserved) {
        workflow = input.localWorkflow;
      } else if (observedTurnState && !isTerminalTurnState(observedTurnState)) {
        workflow = {
          kind: "turnInProgress",
          agentDid: input.localWorkflow.agentDid,
          sessionId: input.localWorkflow.sessionId,
          requestId: observedAsQueued
            ? activeRequestId
            : input.localWorkflow.requestId,
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
      if (!input.session && !input.selectedSessionSummary) {
        workflow = blocked("sessionMissingFromSnapshot");
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

  function sendStatusFor(): SendStatus {
    if (admissionStatus) {
      const reason: ChatBlockedReason =
        admissionStatus.layer === "client"
          ? "clientOffline"
          : admissionStatus.layer === "selection"
            ? "agentNotSelected"
            : admissionStatus.layer === "p2p" ||
                admissionStatus.layer === "route"
              ? "routeNotReady"
              : "behaviorUnavailable";
      return {
        kind: "disabled",
        reason,
        hint: admissionStatus.detail,
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
        hint: hintFor(workflow.reason),
      };
    }
    if (
      workflow.kind === "turnInProgress" &&
      !isTerminalTurnState(workflow.turnState)
    ) {
      return {
        kind: "queue",
        turnState: workflow.turnState,
        hint: queueHint(workflow.turnState),
      };
    }
    return { kind: "ready" };
  }

  const nonEmptyContentSendStatus = sendStatusFor();

  return {
    workflow,
    nonEmptyContentSendStatus,
    activityStatus: activityStatusFor(
      nonEmptyContentSendStatus,
      admissionStatus,
    ),
    turnState: observedTurnState,
    activeRequestId,
  };
}
