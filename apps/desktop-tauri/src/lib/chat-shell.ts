import type { ConversationSummary, DesktopSessionSnapshot } from "./types";

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
  | "composerEmpty"
  | "submittingRequest"
  | "waitingForRequestObservation"
  | "conversationMissingFromSnapshot"
  | "awaitingTurnTerminality"
  | "inconsistentTurnObservation";

export type ChatWorkflowState =
  | { kind: "ready" }
  | { kind: "submittingRequest"; agentDid: string; sessionId?: string | null }
  | { kind: "awaitingObservation"; sessionId: string; requestId: string }
  | {
      kind: "turnInProgress";
      sessionId: string;
      requestId?: string | null;
      turnState: TurnState;
    }
  | { kind: "blocked"; reason: ChatBlockedReason; turnState?: TurnState | null };

export type SendStatus =
  | { kind: "ready" }
  | { kind: "disabled"; reason: ChatBlockedReason; hint: string };

type ProjectionInput = {
  clientAvailable: boolean;
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  draft: string;
  sending: boolean;
  session: DesktopSessionSnapshot | null;
  selectedConversation: ConversationSummary | null;
  localWorkflow: ChatWorkflowState;
};

export type ChatShellProjection = {
  workflow: ChatWorkflowState;
  sendStatus: SendStatus;
  turnState: TurnState | null;
  activeRequestId: string | null;
};

export function isTerminalTurnState(turnState?: string | null): turnState is
  | "completed"
  | "failed"
  | "superseded"
  | "interrupted" {
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

function blocked(reason: ChatBlockedReason, turnState?: TurnState | null): ChatWorkflowState {
  return { kind: "blocked", reason, turnState };
}

function hintFor(reason: ChatBlockedReason, turnState?: TurnState | null) {
  switch (reason) {
    case "clientOffline":
      return "Client offline";
    case "agentNotSelected":
      return "Select an agent before sending";
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

export function projectChatShell(input: ProjectionInput): ChatShellProjection {
  const rawObservedTurnState =
    input.session?.turnState ?? input.selectedConversation?.turnState ?? null;
  const observedTurnState: TurnState | null = isTurnState(rawObservedTurnState)
    ? rawObservedTurnState
    : null;

  const trackedRequestId =
    input.localWorkflow.kind === "awaitingObservation" ||
    input.localWorkflow.kind === "turnInProgress"
      ? input.localWorkflow.requestId ?? null
      : null;

  const observedLatestRequestId =
    input.session?.latestRequestId ?? input.selectedConversation?.latestRequestId ?? null;
  const pendingRequestId = input.session?.pendingTurn?.requestId ?? null;
  const activeRequestId = trackedRequestId ?? pendingRequestId ?? observedLatestRequestId;

  let workflow: ChatWorkflowState = input.localWorkflow;

  if (input.localWorkflow.kind === "awaitingObservation") {
    const selectedMatches =
      input.selectedSessionId === input.localWorkflow.sessionId ||
      input.session?.sessionId === input.localWorkflow.sessionId;
    const requestObserved =
      observedLatestRequestId === input.localWorkflow.requestId ||
      pendingRequestId === input.localWorkflow.requestId;

    if (selectedMatches) {
      if (!requestObserved) {
        workflow = input.localWorkflow;
      } else if (observedTurnState && !isTerminalTurnState(observedTurnState)) {
        workflow = {
          kind: "turnInProgress",
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
    if (
      observedTurnState &&
      activeRequestId === (input.localWorkflow.requestId ?? activeRequestId)
    ) {
      workflow = isTerminalTurnState(observedTurnState)
        ? { kind: "ready" }
        : {
            kind: "turnInProgress",
            sessionId: input.localWorkflow.sessionId,
            requestId: input.localWorkflow.requestId,
            turnState: observedTurnState,
          };
    } else if (!observedTurnState && activeRequestId === input.localWorkflow.requestId) {
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

  let sendStatus: SendStatus;
  if (!input.clientAvailable) {
    sendStatus = {
      kind: "disabled",
      reason: "clientOffline",
      hint: hintFor("clientOffline"),
    };
  } else if (!input.selectedAgentDid) {
    sendStatus = {
      kind: "disabled",
      reason: "agentNotSelected",
      hint: hintFor("agentNotSelected"),
    };
  } else if (!input.draft.trim()) {
    sendStatus = {
      kind: "disabled",
      reason: "composerEmpty",
      hint: hintFor("composerEmpty"),
    };
  } else if (input.sending || input.localWorkflow.kind === "submittingRequest") {
    sendStatus = {
      kind: "disabled",
      reason: "submittingRequest",
      hint: hintFor("submittingRequest"),
    };
  } else if (
    workflow.kind === "awaitingObservation" &&
    activeRequestId === workflow.requestId &&
    pendingRequestId !== workflow.requestId &&
    observedLatestRequestId !== workflow.requestId
  ) {
    sendStatus = {
      kind: "disabled",
      reason: "waitingForRequestObservation",
      hint: hintFor("waitingForRequestObservation"),
    };
  } else if (workflow.kind === "blocked") {
    sendStatus = {
      kind: "disabled",
      reason: workflow.reason,
      hint: hintFor(workflow.reason, workflow.turnState),
    };
  } else if (
    workflow.kind === "turnInProgress" &&
    !isTerminalTurnState(workflow.turnState)
  ) {
    sendStatus = {
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint: hintFor("awaitingTurnTerminality", workflow.turnState),
    };
  } else {
    sendStatus = { kind: "ready" };
  }

  return {
    workflow,
    sendStatus,
    turnState: observedTurnState,
    activeRequestId,
  };
}
